//! Document loading: bounded reads, the worker thread, and the streaming
//! render that applies a parsed document to the window.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use gtk::{self as gtk};

use super::App;
use super::links::link_handler;
use super::search::run_find;
use super::window::window_by_id;
use crate::doc::{self, Document, TocEntry};
use crate::find::TextIndex;
use crate::i18n::{gettext, ngettext};
use crate::markup;
use crate::render;
use crate::timing;
use crate::watch::FileWatcher;

/// Upper bound on document bytes read into memory. One limit for every
/// untrusted input: the same 64 MiB cap the image decoder uses
/// ([`crate::render::IMAGE_MAX_FILE_BYTES`]).
const MAX_DOC_BYTES: u64 = crate::render::IMAGE_MAX_FILE_BYTES;
/// Content arrives by characters of document text, not by build time: layout
/// is the cost, layout scales with wrapped text, and characters count every
/// structure alike. The first slice aims at roughly a screen of text; each
/// stream tick adds a bounded amount on top while the window is visible.
const INITIAL_CHARS: usize = 3500;
const STREAM_CHARS: usize = 1200;
const STREAM_INTERVAL: Duration = Duration::from_millis(16);

enum LoadMsg {
    Loaded { path: PathBuf, doc: Document },
    Failed { path: PathBuf, message: String },
}

impl App {
    pub fn reload_path(self: &Rc<Self>, path: &Path) {
        if self.closed.get() {
            return;
        }
        if self.state.borrow().path.as_deref() == Some(path) {
            self.open_path(path);
        }
    }

    pub fn open_path(self: &Rc<Self>, path: &Path) {
        timing::mark("open_path");
        {
            let mut state = self.state.borrow_mut();
            // A reload of the path already on screen must keep the last
            // good render: if this read fails, show_error treats it as a
            // transient reload failure instead of killing the watcher. A
            // different path drops everything derived from the previous
            // document: it belongs to whatever lands next, not to this one.
            if state.path.as_deref() == Some(path) {
                state.reload_started();
            } else {
                state.document_changed();
            }
            state.path = Some(path.to_path_buf());
        }
        self.start_watcher(path);
        let id = self.id;
        let path = path.to_path_buf();
        std::thread::spawn(move || {
            let msg = match read_bounded(&path) {
                Ok(bytes) => {
                    timing::mark("read-done");
                    let doc = doc::parse(&String::from_utf8_lossy(&bytes));
                    timing::mark("parse-done");
                    LoadMsg::Loaded {
                        path: path.clone(),
                        doc,
                    }
                }
                Err(message) => LoadMsg::Failed {
                    path: path.clone(),
                    message,
                },
            };
            // Deliver straight onto the main context: the callback runs on
            // the main loop's next pass, with no timer in between. Routed by
            // window id, so the load lands in the window that started it and
            // a window closed mid-load simply drops it.
            glib::MainContext::default().invoke(move || {
                if let Some(app) = window_by_id(id) {
                    match msg {
                        LoadMsg::Loaded { path, doc } => apply_doc(&app, &path, doc),
                        LoadMsg::Failed { path, message } => show_error(&app, &path, &message),
                    }
                }
            });
        });
    }

    fn start_watcher(self: &Rc<Self>, path: &Path) {
        // Weak: a closed window must not keep reloading a document nobody is
        // looking at, and App -> watcher -> callback -> App would otherwise
        // be a reference cycle that outlives the window.
        let app_for_reload = Rc::downgrade(self);
        let watched = path.to_path_buf();
        let watcher = FileWatcher::new(path, move || {
            if let Some(app) = app_for_reload.upgrade() {
                app.reload_path(&watched);
            }
        });
        match watcher {
            Ok(watcher) => self.state.borrow_mut().watcher = Some(watcher),
            Err(err) => eprintln!("marksight: live reload unavailable: {err}"),
        }
    }
}

/// Reads at most [`MAX_DOC_BYTES`] from a regular file, with clear errors
/// for everything else. The metadata check runs before the file is opened:
/// opening a FIFO with no writer, or a device node, would park the reader
/// thread indefinitely. `take` holds the bound even if the file grows
/// between the stat and the read.
fn read_bounded(path: &Path) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let metadata = std::fs::metadata(path).map_err(|err| io_error_message(&err))?;
    if !metadata.is_file() {
        return Err(gettext("not a regular file"));
    }
    let len = metadata.len();
    if len > MAX_DOC_BYTES {
        return Err(ngettext(
            "file too large: {size} byte (the limit is {limit} bytes)",
            "file too large: {size} bytes (the limit is {limit} bytes)",
            u32::try_from(len).unwrap_or(u32::MAX),
        )
        .replace("{size}", &len.to_string())
        .replace("{limit}", &MAX_DOC_BYTES.to_string()));
    }
    let file = std::fs::File::open(path).map_err(|err| io_error_message(&err))?;
    let mut bytes = Vec::with_capacity(len.min(4 << 20) as usize);
    file.take(MAX_DOC_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|err| io_error_message(&err))?;
    Ok(bytes)
}

/// The two I/O failures users can actually act on, as short translatable
/// messages; anything else keeps the raw error text, which the error page
/// shows and the reload path logs.
fn io_error_message(err: &std::io::Error) -> String {
    match err.kind() {
        std::io::ErrorKind::NotFound => gettext("file not found"),
        std::io::ErrorKind::PermissionDenied => gettext("permission denied"),
        _ => err.to_string(),
    }
}

fn show_error(app: &Rc<App>, path: &Path, message: &str) {
    if app.state.borrow().path.as_deref() != Some(path) {
        return;
    }
    // A reload that fails while this document is already on screen is
    // almost always an editor's save-via-rename: the Remove half of the
    // rename briefly leaves the file missing. Killing the watcher and the
    // path here (the first-open behavior) permanently broke live reload.
    // Instead keep everything and re-read on the next event; the missing
    // retry in watch.rs usually bridges the gap without this path at all.
    if app.state.borrow().rendered.is_some() {
        eprintln!("marksight: reload failed, keeping the displayed document: {message}");
        return;
    }
    app.ui
        .error_label
        .set_text(&format!("{}\n\n{}", path.display(), message));
    app.ui.stack.set_visible_child_name("error");
    app.ui.window.set_title(Some("marksight"));
    app.ui.title_label.set_text("marksight");
    app.ui.path_label.set_text("");
    let mut state = app.state.borrow_mut();
    state.path = None;
    state.watcher = None;
    state.document_changed();
    drop(state);
    app.present_if_hidden();
}

fn apply_doc(app: &Rc<App>, path: &Path, parsed: Document) {
    timing::mark("apply_doc (doorbell)");
    if app.state.borrow().path.as_deref() != Some(path) {
        return;
    }
    let anchors = Rc::new(RefCell::new(HashMap::new()));
    let toc_targets = Rc::new(RefCell::new(Vec::new()));
    let link_handler = link_handler(app, &anchors);

    let base_dir = path.parent().map(Path::to_path_buf);
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "marksight".to_owned());
    let toc_rows = toc_row_texts(&parsed, &parsed.headings);
    let has_toc = !parsed.headings.is_empty();

    let mut renderer = render::ChunkedRenderer::new(
        parsed,
        base_dir,
        link_handler,
        anchors.clone(),
        toc_targets.clone(),
    );
    let generation = {
        let mut state = app.state.borrow_mut();
        state.render_generation += 1;
        state.render_generation
    };
    renderer.render_units(INITIAL_CHARS);
    timing::mark("first-slice-built");

    if let Some(viewport) = app.ui.scrolled.child().and_downcast::<gtk::Viewport>() {
        viewport.set_child(Some(renderer.root()));
    }
    app.ui.scrolled.vadjustment().set_value(0.0);

    {
        let mut state = app.state.borrow_mut();
        // The old render is detached (the streaming root is already in the
        // viewport) and the stale matches go with it; the needle and match
        // position survive so a live reload's search continues where it
        // was. `run_find` re-runs when the stream completes.
        state.rendered = None;
        state.matches.clear();
        state.toc_targets.clear();
        state.render_complete = false;
    }

    app.ui.stack.set_visible_child_name("doc");
    app.ui.window.set_title(Some(&file_name));
    app.ui.title_label.set_text(&file_name);
    app.ui.path_label.set_text(&path.display().to_string());
    // The complete sidebar is built from the parsed headings before anything
    // streams, so it is there in full from the first frame; only the scroll
    // targets of not-yet-rendered headings are pending, and their rows simply
    // do not scroll until the stream lands.
    rebuild_toc(app, &toc_rows);
    app.ui.toc_column.set_visible(has_toc);
    app.ui.toc_button.set_active(has_toc);
    app.ui.toc_button.set_visible(has_toc);
    app.ui.match_counter.set_text("");
    app.present_if_hidden();
    timing::mark("present-called");

    let stream_weak = Rc::downgrade(app);
    let stream_path = path.to_path_buf();
    glib::timeout_add_local(STREAM_INTERVAL, move || {
        let Some(stream_app) = stream_weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        if stream_app.closed.get() {
            return glib::ControlFlow::Break;
        }
        {
            let state = stream_app.state.borrow();
            let superseded = state.path.as_deref() != Some(&stream_path)
                || state.render_generation != generation;
            if superseded || state.render_complete {
                return glib::ControlFlow::Break;
            }
        }
        if !renderer.render_units(STREAM_CHARS) {
            return glib::ControlFlow::Continue;
        }
        let (doc, rendered) = renderer.finish();
        timing::mark("render-complete");
        // The same pure decomposition the tests and fuzz targets exercise
        // (markup::text_units): the renderer creates one label group per
        // unit, in this exact order — the GUI smoke suite asserts the two
        // stay in lockstep, so a match's entry points at its label.
        let index = TextIndex::build(&doc);
        {
            let mut state = stream_app.state.borrow_mut();
            state.doc = doc;
            state.index = index;
            state.rendered = Some(rendered);
            state.render_complete = true;
            state.toc_targets = toc_targets.borrow().clone();
        }
        // TOC rows were built in full before the first slice; heading
        // targets exist as the stream passes them.
        let needle = stream_app.ui.search_entry.text().to_string();
        if stream_app.ui.search_bar.is_search_mode() && !needle.trim().is_empty() {
            run_find(&stream_app, &needle);
        }
        glib::ControlFlow::Break
    });
}

/// Heading levels and texts for the sidebar, resolved against the whole
/// parsed document so the TOC can be complete before any content streams.
fn toc_row_texts(doc: &Document, headings: &[TocEntry]) -> Vec<(u8, String)> {
    headings
        .iter()
        .map(|entry| {
            let text = doc
                .resolve(&entry.path)
                .and_then(|block| match block {
                    doc::Block::Heading { inlines, .. } => {
                        Some(markup::walk_inlines(inlines, doc, None).text)
                    }
                    _ => None,
                })
                .unwrap_or_default();
            (entry.level, text)
        })
        .collect()
}

fn rebuild_toc(app: &Rc<App>, rows: &[(u8, String)]) {
    while let Some(row) = app.ui.toc_list.first_child() {
        app.ui.toc_list.remove(&row);
    }
    for (level, text) in rows {
        let label = gtk::Label::new(Some(text));
        label.set_xalign(0.0);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_margin_start(6 + level.saturating_sub(1) as i32 * 14);
        label.set_css_classes(&["mv-toc-row"]);
        let row = gtk::ListBoxRow::new();
        row.set_child(Some(&label));
        row.set_css_classes(&["mv-toc-item"]);
        let row_text = if text.is_empty() {
            gettext("(untitled heading)")
        } else {
            text.clone()
        };
        row.update_property(&[gtk::accessible::Property::Label(
            &gettext("Jump to {heading}").replace("{heading}", &row_text),
        )]);
        app.ui.toc_list.append(&row);
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_DOC_BYTES, read_bounded};
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("marksight-read-{tag}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn regular_file_within_bound_reads() {
        let dir = scratch("ok");
        let path = dir.join("doc.md");
        fs::write(&path, b"# hello").unwrap();
        assert_eq!(read_bounded(&path).unwrap(), b"# hello");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn oversize_file_is_refused_without_reading_it() {
        let dir = scratch("big");
        let path = dir.join("huge.md");
        // A sparse file: 64 MiB + 1 of apparent length, almost none of it on
        // disk, so the test is instant.
        let file = fs::File::create(&path).unwrap();
        file.set_len(MAX_DOC_BYTES + 1).unwrap();
        drop(file);
        let err = read_bounded(&path).unwrap_err();
        assert!(err.contains("file too large"), "got {err}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn directory_is_refused_as_not_a_regular_file() {
        let dir = scratch("dir");
        let err = read_bounded(&dir).unwrap_err();
        assert_eq!(err, "not a regular file");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn fifo_is_refused_without_opening_it() {
        // Opening a FIFO blocks until a writer appears; the is_file check
        // must keep this test (and the reader thread) from ever hanging.
        let dir = scratch("fifo");
        let fifo = dir.join("pipe.md");
        Command::new("mkfifo").arg(&fifo).status().unwrap();
        let err = read_bounded(&fifo).unwrap_err();
        assert_eq!(err, "not a regular file");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_reports_the_io_error() {
        let err =
            read_bounded(PathBuf::from("/nonexistent/marksight/none.md").as_path()).unwrap_err();
        assert!(!err.is_empty());
    }
}
