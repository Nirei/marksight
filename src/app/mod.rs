//! Application shell: the shared per-window state and the entry points.
//!
//! Owns the [`Document`] and the [`RenderedDoc`] per window; parsing happens
//! on a worker thread and is delivered back through the main context the
//! moment it is ready, keeping the UI thread responsive for rebuilds of
//! arbitrarily large files. Every file handed to the application (command
//! line, file manager, or a second running instance arriving over D-Bus)
//! opens in a window of its own; live windows are tracked weakly in a
//! thread-local registry, and worker results are routed back by window id.
//!
//! A window stays hidden until its first page is ready: the `open` launch
//! defers `present()` to the moment the document (or load error) is applied,
//! so startup never paints an empty frame, and presenting the new window is
//! what completes startup notification.
//!
//! The shell is split by concern: [`window`] builds windows and owns the
//! live-window registry, [`loading`] opens and streams documents, [`search`]
//! drives find-in-document, [`links`] handles link activation and in-document
//! scrolling, and [`ipc`] forwards second launches to the primary over D-Bus.

mod ipc;
mod links;
mod loading;
mod search;
mod window;

pub use self::ipc::{forward_files, launch_startup_id};
pub use self::window::{ensure_window, open_window, quit_or_close, window_count};

use std::cell::RefCell;
use std::path::PathBuf;

use gtk::glib;
use gtk::prelude::*;
use gtk::{self as gtk};

use crate::doc::Document;
use crate::find::{Match, TextIndex};
use crate::render::RenderedDoc;
use crate::timing;
use crate::watch::FileWatcher;

use self::window::Ui;

pub const APP_ID: &str = "io.github.cinzatech.marksight";

struct State {
    path: Option<PathBuf>,
    doc: Document,
    index: TextIndex,
    rendered: Option<RenderedDoc>,
    matches: Vec<Match>,
    active: Option<usize>,
    watcher: Option<FileWatcher>,
    toc_targets: Vec<gtk::Widget>,
    find_pending: Option<std::time::Instant>,
    find_needle: String,
    scroll_generation: u64,
    render_generation: u64,
    render_complete: bool,
    /// Startup-notification id of the launch that opened this window,
    /// consumed the moment the window is presented: presenting with the id
    /// is what ends the desktop's launch feedback.
    pending_startup_id: Option<String>,
}

impl State {
    fn new() -> Self {
        State {
            path: None,
            doc: Document::default(),
            index: TextIndex::default(),
            rendered: None,
            matches: Vec::new(),
            active: None,
            watcher: None,
            toc_targets: Vec::new(),
            find_pending: None,
            find_needle: String::new(),
            scroll_generation: 0,
            render_generation: 0,
            render_complete: false,
            pending_startup_id: None,
        }
    }

    /// Forgets everything derived from the document on screen. A different
    /// document is being loaded: the render, the find state and the scroll
    /// targets all describe the old document, and none of it may leak into
    /// the next one (a stale needle or match position would otherwise
    /// resurface when the next search runs). The path and watcher are
    /// assigned by the loader around this.
    fn document_changed(&mut self) {
        self.rendered = None;
        self.render_complete = false;
        self.toc_targets.clear();
        self.matches.clear();
        self.active = None;
        self.find_pending = None;
        self.find_needle.clear();
    }

    /// A re-read of the document already on screen (live reload). The last
    /// good render stays so a failing read reads as transient (see the
    /// loader's `show_error`); the matches go because they index the
    /// previous parse. The needle and match position survive, so the
    /// search that re-runs when the new parse lands continues where the
    /// reader was; every reader of `active` validates it against the
    /// fresh match list first.
    fn reload_started(&mut self) {
        self.matches.clear();
        self.find_pending = None;
    }
}

pub struct App {
    id: u64,
    ui: Ui,
    state: RefCell<State>,
    /// Set when the user closes this window. GTK keeps a closed window's
    /// widget tree (and with it every closure capturing the App) alive for
    /// the rest of the process, so Rc liveness cannot signal closure;
    /// this flag does. A closed window stops watching, streaming, finding
    /// and receiving loads, even though its memory stays parked until the
    /// process exits.
    closed: std::cell::Cell<bool>,
}

pub fn run() -> glib::ExitCode {
    let app = gtk::Application::builder()
        .application_id(APP_ID)
        .flags(gtk::gio::ApplicationFlags::HANDLES_OPEN)
        .build();
    timing::mark("app-object-built");
    app.connect_activate(|app| {
        timing::mark("activate-signal");
        match window::alive_windows().last() {
            Some(existing) => existing.ui.window.present(),
            None => ensure_window(app).present_if_hidden(),
        }
    });
    app.connect_open(|app, files, hint| {
        timing::mark("open-signal");
        handle_open(app, files, hint);
    });
    app.connect_startup(|_| timing::mark("startup-signal"));
    app.run()
}

/// Shared handling for files arriving at the application (from the open
/// signal, in the primary): every distinct file gets a window of its own,
/// while a file that is already on screen just gets its window raised.
///
/// `hint` is the open signal's hint argument; marksight's forwarding
/// process uses it to carry the launching desktop's startup-notification
/// id, so that presenting the window ends the launch feedback (busy cursor,
/// placeholder taskbar icon) instead of letting it time out.
#[doc(hidden)]
pub fn handle_open(app: &gtk::Application, files: &[gtk::gio::File], hint: &str) {
    if files.is_empty() {
        match window::alive_windows().last() {
            Some(existing) => existing.present_existing(None),
            None => ensure_window(app).present_if_hidden(),
        }
        return;
    }
    let startup_id = (!hint.is_empty()).then_some(hint);
    for file in files {
        let Some(path) = file.path() else {
            eprintln!("marksight: cannot open a file without a local path");
            continue;
        };
        match window::alive_windows()
            .into_iter()
            .find(|window| window.shows_path(&path))
        {
            Some(existing) => {
                existing.present_existing(startup_id);
            }
            None => {
                window::open_window_with_startup(app, &path, startup_id);
            }
        }
    }
}
