// Gated to nothing without the ui feature: the whole file needs GTK.
#![cfg(feature = "ui")]

//! GUI smoke tests: boot the real GTK application against a virtual display.
//!
//! The whole suite is one test function so all GTK interaction happens on a
//! single thread. Skipped automatically when no display is available, so a
//! plain `cargo test` on a headless box stays green; CI exports DISPLAY via
//! Xvfb (plus a window manager so windows get sized).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gtk::glib;
use gtk::prelude::*;

use marksight::app;

static SKIP: AtomicBool = AtomicBool::new(false);

fn pump_until(timeout: Duration, condition: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        while glib::MainContext::default().iteration(false) {}
        std::thread::sleep(Duration::from_millis(5));
    }
    condition()
}

fn pump(duration: Duration) {
    let _ = pump_until(duration, || false);
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn gui_smoke_suite() {
    if gtk::init().is_err() {
        SKIP.store(true, Ordering::SeqCst);
        eprintln!("skipping gui smoke: no display available");
        return;
    }

    let gtk_app = gtk::Application::new(None::<&str>, Default::default());
    let mv = app::ensure_window(&gtk_app);

    welcome_page_visible(&mv);
    open_document(&mv);
    toc_matches_headings(&mv);
    toc_sidebar_fills_pane(&mv);
    search_finds_and_cycles(&mv);
    search_clears_highlights(&mv);
    unknown_anchor_link_is_handled_without_launching(&mv);
    search_inside_every_structure(&mv);
    scroll_targets_move_viewport(&mv);
    pathological_documents_do_not_crash(&mv);
    image_keeps_space_in_tall_document(&mv);
    live_reload_replaces_content(&mv);
    live_reload_survives_save_via_rename(&mv);
    deep_nesting_opens_within_budget(&mv);
    huge_document_within_budget(&mv);
    error_state_on_missing_file(&mv);
    oversize_file_shows_size_error(&mv);
    control_characters_do_not_crash_the_viewer(&mv);
    second_file_opens_its_own_window(&mv, &gtk_app);
    same_file_reuses_its_window(&mv, &gtk_app);
}

fn welcome_page_visible(mv: &std::rc::Rc<app::App>) {
    assert_eq!(
        mv.visible_page(),
        "welcome",
        "fresh app shows the welcome page"
    );
}

fn open_document(mv: &std::rc::Rc<app::App>) {
    mv.open_path(&fixture("kitchen-sink.md"));
    assert!(
        pump_until(Duration::from_secs(5), || mv.visible_page() == "doc"
            && mv.rendering_complete()),
        "document page never appeared or never finished streaming"
    );
    assert!(
        mv.label_group_count() > 5,
        "kitchen sink should produce many label groups, got {}",
        mv.label_group_count()
    );
    assert_eq!(mv.scroll_value(), 0.0);
    // The window is first presented when the document lands; let it map and
    // allocate before layout-dependent assertions read pane sizes.
    pump(Duration::from_millis(200));
}

fn toc_matches_headings(mv: &std::rc::Rc<app::App>) {
    let doc = marksight::doc::parse(&std::fs::read_to_string(fixture("kitchen-sink.md")).unwrap());
    assert_eq!(mv.toc_row_count() as usize, doc.headings.len());
}

fn toc_sidebar_fills_pane(mv: &std::rc::Rc<app::App>) {
    let ancestor = mv
        .toc_list()
        .ancestor(gtk::ScrolledWindow::static_type())
        .expect("TOC list has no ScrolledWindow ancestor");
    let scrolled = ancestor.downcast_ref::<gtk::ScrolledWindow>().unwrap();
    let page = scrolled.vadjustment().page_size();
    assert!(
        page >= 400.0,
        "TOC sidebar should fill its pane vertically (visible page {page}px in a ~700px window)"
    );
}

fn search_finds_and_cycles(mv: &std::rc::Rc<app::App>) {
    mv.search_set("marksight");
    assert!(
        pump_until(Duration::from_secs(2), || !mv.search_counter().is_empty()),
        "search counter never populated"
    );
    let counter = mv.search_counter();
    assert!(
        counter.contains('/'),
        "counter should be active/total, got {counter:?}"
    );

    mv.search_set("missing-needle-xyz");
    assert!(
        pump_until(Duration::from_secs(2), || mv.search_counter() == "0/0"),
        "no-match search should report zero, got {:?}",
        mv.search_counter()
    );

    mv.search_set("footnote");
    assert!(
        pump_until(Duration::from_secs(2), || mv
            .search_counter()
            .contains("/2")),
        "kitchen sink has two 'footnote' hits, got {:?}",
        mv.search_counter()
    );

    let highlighted = mv.debug_label_states();
    assert_eq!(
        highlighted.len(),
        2,
        "exactly two labels should be highlighted, got {highlighted:?}"
    );
    for markup in &highlighted {
        let plain = strip_tags(markup);
        assert!(
            plain.to_lowercase().contains("footnote"),
            "highlight landed on text without the needle (phantom match): {markup:?}"
        );
    }

    assert_eq!(mv.search_counter(), "1/2");
    mv.search_emit_activate();
    pump(Duration::from_millis(200));
    assert_eq!(
        mv.search_counter(),
        "2/2",
        "Enter should cycle to the second match"
    );
    mv.search_emit_activate();
    pump(Duration::from_millis(200));
    assert_eq!(
        mv.search_counter(),
        "1/2",
        "Enter should wrap back to the first match"
    );
}

fn search_clears_highlights(mv: &std::rc::Rc<app::App>) {
    mv.search_set("");
    pump_until(Duration::from_secs(2), || mv.search_counter().is_empty());
    assert!(mv.search_counter().is_empty());
}

fn search_inside_every_structure(mv: &std::rc::Rc<app::App>) {
    // The shipped index and the tested index (markup::text_units) must be
    // the same decomposition: one entry per rendered label. Searching for
    // a needle that occurs inside exactly one structure must highlight
    // exactly one label, and that label must contain the needle — no
    // phantom matches from misaligned entries.
    let markdown = "\
- list item with unitlist inside

| cell one | unittable in a cell |
|---|---|
| x | y |

> a quote holding unitquote

> [!NOTE]
> an alert carrying unitalert

paragraph referencing unitnote[^f]

```rust
// code block with unitcode
```

# heading containing unithead

[^f]: the footnote defines unitfootnote
";
    let path = std::env::temp_dir().join("marksight-smoke-structures.md");
    std::fs::write(&path, markdown).unwrap();
    mv.open_path(&path);
    assert!(
        pump_until(Duration::from_secs(5), || mv.visible_page() == "doc"
            && mv.rendering_complete()),
        "structures document never finished rendering"
    );
    let doc = marksight::doc::parse(markdown);
    assert_eq!(
        mv.label_group_count(),
        marksight::markup::text_units(&doc).len(),
        "label groups must mirror the find index's text units one to one"
    );

    for needle in [
        "unitlist",
        "unittable",
        "unitquote",
        "unitalert",
        "unitcode",
        "unithead",
        "unitfootnote",
        "unitnote",
    ] {
        mv.search_set(needle);
        // Wait for the highlight itself, not the counter: consecutive
        // needles all report 1/1, so the counter alone cannot show that
        // this needle's search has actually run.
        let landed = pump_until(Duration::from_secs(2), || {
            let highlighted = mv.debug_label_states();
            highlighted.len() == 1 && strip_tags(&highlighted[0]).to_lowercase().contains(needle)
        });
        let highlighted = mv.debug_label_states();
        assert!(
            landed,
            "needle {needle} should highlight exactly one label containing it, got {highlighted:?}"
        );
        assert_eq!(
            mv.search_counter(),
            "1/1",
            "needle {needle} should have exactly one hit"
        );
    }
    mv.search_set("");
    pump(Duration::from_millis(200));
    let _ = std::fs::remove_file(&path);
}

fn unknown_anchor_link_is_handled_without_launching(mv: &std::rc::Rc<app::App>) {
    // Regression: an unknown in-document anchor used to fall through to
    // GTK's default URI launcher (returning false from activate-link);
    // every # URI must be fully handled as a no-op instead.
    let label = mv
        .heading_widget(0)
        .and_downcast::<gtk::Label>()
        .expect("first heading is a label");
    let handled: bool = label.emit_by_name("activate-link", &[&"#definitely-not-an-anchor"]);
    assert!(
        handled,
        "a # URI must always be handled, also when its anchor is unknown"
    );
    assert_eq!(mv.visible_page(), "doc", "unknown anchor must not navigate");
}

fn scroll_targets_move_viewport(mv: &std::rc::Rc<app::App>) {
    let doc_text = (1..=400)
        .map(|i| format!("## heading {i}\n\ndocument body {i}\n"))
        .collect::<String>();
    let path = std::env::temp_dir().join("marksight-smoke-tall.md");
    std::fs::write(&path, &doc_text).unwrap();
    mv.open_path(&path);
    assert!(
        pump_until(Duration::from_secs(5), || mv.visible_page() == "doc"
            && mv.toc_row_count() == 400),
        "tall document never rendered: page={} toc={}",
        mv.visible_page(),
        mv.toc_row_count()
    );
    assert_eq!(
        mv.scroll_value(),
        0.0,
        "opening a document resets the scroll position"
    );

    let toc = mv.toc_list();
    let row_at = |index: i32| -> gtk::ListBoxRow {
        let mut child = toc.first_child().unwrap();
        for _ in 0..index {
            child = child.next_sibling().unwrap();
        }
        child.downcast::<gtk::ListBoxRow>().unwrap()
    };
    let click_row = |index: i32| {
        let row = row_at(index);
        toc.emit_by_name::<()>("row-activated", &[&row]);
        pump(Duration::from_millis(320));
    };

    click_row(399);
    assert!(
        mv.scroll_value() > 100.0,
        "activating the last TOC entry should scroll far down, got {}",
        mv.scroll_value()
    );
    assert_last_row_visible(mv);

    click_row(0);
    assert!(
        mv.scroll_value() < 50.0,
        "activating the first TOC entry should scroll back up, got {}",
        mv.scroll_value()
    );

    click_row(200);
    let first_landing = mv.scroll_value();
    click_row(50);
    click_row(200);
    let second_landing = mv.scroll_value();
    assert!(
        (first_landing - second_landing).abs() <= 1.0,
        "clicking the same TOC entry must land identically regardless of prior scroll ({first_landing} vs {second_landing})"
    );

    mv.search_set("heading 350");
    assert!(pump_until(Duration::from_secs(2), || mv
        .search_counter()
        .contains("/1")));
    pump(Duration::from_millis(300));
    assert!(
        mv.scroll_value() > 1000.0,
        "finding a late heading should scroll far down, got {}",
        mv.scroll_value()
    );
    let _ = std::fs::remove_file(&path);
}

fn strip_tags(markup: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for ch in markup.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out
}

fn assert_last_row_visible(mv: &std::rc::Rc<app::App>) {
    let scrolled = mv.doc_scrolled();
    let viewport = scrolled
        .child()
        .unwrap()
        .downcast::<gtk::Viewport>()
        .unwrap();
    let page = scrolled.vadjustment().page_size();
    let target = mv.heading_widget(399).expect("last heading widget");
    let point = target
        .compute_point(&viewport, &gtk::graphene::Point::new(0.0, 0.0))
        .expect("last heading position");
    assert!(
        point.y() >= -5.0 && point.y() as f64 <= page,
        "last heading should be inside the viewport after jumping to it (y={}, page={page})",
        point.y()
    );
}

fn pathological_documents_do_not_crash(mv: &std::rc::Rc<app::App>) {
    let mut huge_table = String::from("| a | b | c |\n|---|---|---|\n");
    for row in 0..200 {
        huge_table.push_str(&format!("| r{row} <b>& | cell \\| pipe | `x{row}` |\n"));
    }
    let deep = "> ".repeat(120) + "bottom";
    let unclosed = "`".repeat(50);
    let pathological = format!("{huge_table}\n\n{deep}\n\n{unclosed}\n\n| broken | table\n|:-|\n");
    let path = std::env::temp_dir().join("marksight-smoke-pathological.md");
    std::fs::write(&path, &pathological).unwrap();
    mv.open_path(&path);
    assert!(
        pump_until(Duration::from_secs(5), || mv.label_group_count() > 100),
        "pathological document never applied (groups={})",
        mv.label_group_count()
    );
    let _ = std::fs::remove_file(&path);
}

fn image_keeps_space_in_tall_document(mv: &std::rc::Rc<app::App>) {
    // Regression: in a document taller than the window, the scrolled window
    // allocates the content its minimum height, and a shrinkable picture
    // reports a zero minimum, so images used to collapse to nothing.
    let dir = std::env::temp_dir().join("marksight-smoke-img");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(fixture("red-dot.png"), dir.join("red-dot.png")).unwrap();
    let mut markdown = String::new();
    for i in 0..60 {
        markdown.push_str(&format!(
            "## filler {i}\n\nparagraph of filler text to push the image far down {i}\n\n"
        ));
    }
    markdown.push_str("![red dot](red-dot.png)\n");
    let path = dir.join("doc.md");
    std::fs::write(&path, markdown).unwrap();
    mv.open_path(&path);
    assert!(
        pump_until(Duration::from_secs(5), || mv.visible_page() == "doc"
            && mv.rendering_complete()),
        "image document never finished rendering"
    );
    pump(Duration::from_millis(300));
    let sizes = mv.picture_sizes();
    assert_eq!(
        sizes.len(),
        1,
        "expected exactly one picture, got {sizes:?}"
    );
    let (width, height) = sizes[0];
    assert!(
        width > 0 && height > 0,
        "picture collapsed to {width}x{height} in a tall document"
    );
    let _ = std::fs::remove_file(&path);
}

fn live_reload_replaces_content(mv: &std::rc::Rc<app::App>) {
    let path = std::env::temp_dir().join("marksight-smoke-reload.md");
    std::fs::write(&path, "# first\n\none paragraph\n").unwrap();
    mv.open_path(&path);
    assert!(
        pump_until(Duration::from_secs(5), || mv.current_doc_len() == 2),
        "first reload document never applied (len={})",
        mv.current_doc_len()
    );

    std::fs::write(&path, "# second\n\n1\n\n2\n\n3\n\n4\n\n5\n").unwrap();
    assert!(
        pump_until(Duration::from_secs(10), || mv.current_doc_len() == 6),
        "watcher-driven reload never changed the document (len={})",
        mv.current_doc_len()
    );
    let _ = std::fs::remove_file(&path);
}

fn live_reload_survives_save_via_rename(mv: &std::rc::Rc<app::App>) {
    // Regression: a reload triggered by the Remove half of an editor's
    // save-via-rename used to hit show_error, which dropped the watcher
    // and the path, permanently killing live reload. The remove and the
    // recreate below sit further apart than one debounce window, so the
    // watcher really does observe a missing file.
    let path = std::env::temp_dir().join("marksight-smoke-rename.md");
    std::fs::write(&path, "# first\n\none paragraph\n").unwrap();
    mv.open_path(&path);
    assert!(
        pump_until(Duration::from_secs(5), || mv.current_doc_len() == 2),
        "first rename document never applied (len={})",
        mv.current_doc_len()
    );

    std::fs::remove_file(&path).unwrap();
    std::thread::sleep(Duration::from_millis(400));
    // While the file is gone the last good render must stay on screen.
    assert_eq!(
        mv.visible_page(),
        "doc",
        "a transiently missing file must not leave the document page"
    );
    assert_eq!(mv.current_doc_len(), 2, "last good render must be kept");

    std::fs::write(&path, "# second\n\n1\n\n2\n\n3\n\n4\n\n5\n").unwrap();
    assert!(
        pump_until(Duration::from_secs(10), || mv.current_doc_len() == 6),
        "watcher died on a transient remove: document never updated (len={})",
        mv.current_doc_len()
    );
    assert_eq!(
        mv.visible_page(),
        "doc",
        "reload after recreate must stay on the document page"
    );
    let _ = std::fs::remove_file(&path);
}

fn deep_nesting_opens_within_budget(mv: &std::rc::Rc<app::App>) {
    // MAX_CONTAINER_DEPTH is 256, so a document at the cap is legal input:
    // it must open, stream and finish within a sane budget. Quotes at the
    // cap plus 64 nested lists. Runs before the huge-document test on
    // purpose: replacing a viewport child whose tree holds ten thousand
    // widgets costs seconds of teardown on the main loop, and this test
    // must measure the deep document, not the previous one's disposal.
    let quotes = "> ".repeat(256) + "bottom of the quotes\n";
    let mut lists = String::new();
    for depth in 0..64 {
        lists.push_str(&"  ".repeat(depth));
        lists.push_str(&format!("- item {depth}\n"));
    }
    let markdown = format!("{quotes}\n\n{lists}");
    let path = std::env::temp_dir().join("marksight-smoke-deep.md");
    std::fs::write(&path, markdown).unwrap();
    let started = Instant::now();
    mv.open_path(&path);
    assert!(
        pump_until(Duration::from_secs(10), || mv.visible_page() == "doc"
            && mv.rendering_complete()),
        "deeply nested document never finished rendering"
    );
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(5),
        "256-deep document took {elapsed:?} to open, above the budget"
    );
    assert!(mv.current_doc_len() >= 2, "deep content must be present");
    let _ = std::fs::remove_file(&path);
}

fn huge_document_within_budget(mv: &std::rc::Rc<app::App>) {
    let mut text = String::with_capacity(400_000);
    for i in 0..1100 {
        text.push_str(&format!("## section {i}\nsome *emphasis* and `code` and [link](https://example.com/{i}) text\n\n- item one\n- item two with **bold**\n\n> a quote line\n\n```rust\nfn f{i}() -> u32 {{ {i} }}\n```\n\n"));
    }
    let path = std::env::temp_dir().join("marksight-smoke-huge.md");
    std::fs::write(&path, &text).unwrap();

    let parsed = std::time::Instant::now();
    let doc = marksight::doc::parse(&text);
    let parse_ms = parsed.elapsed().as_millis();
    let indexed = std::time::Instant::now();
    let index = marksight::find::TextIndex::build(&doc);
    let index_ms = indexed.elapsed().as_millis();
    assert!(!index.is_empty());

    mv.open_path(&path);
    assert!(pump_until(Duration::from_secs(15), || mv.visible_page()
        == "doc"
        && mv.current_doc_len() == doc.blocks.len()));
    assert!(parse_ms < 100, "10k-line parse took {parse_ms}ms");
    assert!(index_ms < 100, "index build took {index_ms}ms");
    let _ = std::fs::remove_file(&path);
}

fn error_state_on_missing_file(mv: &std::rc::Rc<app::App>) {
    mv.open_path(PathBuf::from("/nonexistent/marksight/does-not-exist.md").as_path());
    assert!(
        pump_until(Duration::from_secs(5), || mv.visible_page() == "error"),
        "missing file should land on the error page, got {:?}",
        mv.visible_page()
    );
}

fn oversize_file_shows_size_error(mv: &std::rc::Rc<app::App>) {
    // End-to-end wiring of the 64 MiB read cap: the worker's refusal must
    // reach the error page with the size message, not a parse of a huge
    // buffer. A sparse file keeps the test instant.
    let path = std::env::temp_dir().join("marksight-smoke-huge-file.md");
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(64 * 1024 * 1024 + 1).unwrap();
    drop(file);
    mv.open_path(&path);
    // The stack may already sit on "error" from the previous test, so wait
    // for this failure's own message rather than the page name.
    assert!(
        pump_until(Duration::from_secs(5), || mv
            .error_text()
            .contains("file too large")),
        "oversize file should report the size problem, got page {:?} text {:?}",
        mv.visible_page(),
        mv.error_text()
    );
    assert_eq!(mv.visible_page(), "error");
    let _ = std::fs::remove_file(&path);
}

fn control_characters_do_not_crash_the_viewer(mv: &std::rc::Rc<app::App>) {
    // Regression: a raw NUL byte in a file used to reach set_markup and
    // panic glib's Rust→C string conversion; the model boundary now
    // replaces it (and other controls) with U+FFFD before rendering.
    let path = std::env::temp_dir().join("marksight-smoke-controls.md");
    std::fs::write(&path, b"before \0 middle \x01 \x7f after\n").unwrap();
    mv.open_path(&path);
    assert!(
        pump_until(Duration::from_secs(5), || mv.visible_page() == "doc"
            && mv.rendering_complete()),
        "control-laden document never rendered, page={}",
        mv.visible_page()
    );
    assert_eq!(mv.current_doc_len(), 1);
    let _ = std::fs::remove_file(&path);
}

fn second_file_opens_its_own_window(mv: &std::rc::Rc<app::App>, gtk_app: &gtk::Application) {
    // Regression: a second file used to replace the document of the
    // existing window, and because no new window was ever presented the
    // desktop's startup feedback (busy cursor, placeholder taskbar icon)
    // never completed.
    mv.open_path(&fixture("kitchen-sink.md"));
    assert!(
        pump_until(Duration::from_secs(5), || mv.visible_page() == "doc"
            && mv.rendering_complete()),
        "first window never finished rendering"
    );
    let first_len = mv.current_doc_len();

    let second = app::open_window(gtk_app, &fixture("code.md"));
    assert!(
        pump_until(Duration::from_secs(5), || second.visible_page() == "doc"
            && second.rendering_complete()),
        "second window never finished rendering"
    );
    assert_eq!(app::window_count(), 2, "each file must own a window");
    assert_eq!(
        mv.current_doc_len(),
        first_len,
        "opening a second file must not touch the first window's document"
    );
    assert!(second.current_doc_len() > 0);

    // Closing the extra window drops its App; the first window lives on.
    // The root() handle is a strong reference, so it is scoped to the close
    // call and released before the assertion polls for teardown.
    {
        let window = second
            .doc_scrolled()
            .root()
            .and_downcast::<gtk::ApplicationWindow>()
            .expect("second window has an application window root");
        window.close();
    }
    drop(second);
    assert!(
        pump_until(Duration::from_secs(5), || app::window_count() == 1),
        "closing a window must drop its App"
    );
    assert_eq!(mv.current_doc_len(), first_len);
}

fn same_file_reuses_its_window(mv: &std::rc::Rc<app::App>, gtk_app: &gtk::Application) {
    // Opening a file that is already shown raises its window instead of
    // creating another one.
    mv.open_path(&fixture("kitchen-sink.md"));
    assert!(
        pump_until(Duration::from_secs(5), || mv.visible_page() == "doc"
            && mv.rendering_complete()),
        "first window never finished rendering"
    );
    assert_eq!(app::window_count(), 1);

    let same = gtk::gio::File::for_path(fixture("kitchen-sink.md"));
    app::handle_open(gtk_app, &[same], "");
    pump(Duration::from_millis(300));
    assert_eq!(
        app::window_count(),
        1,
        "re-opening the same file must reuse the existing window"
    );

    let other = gtk::gio::File::for_path(fixture("code.md"));
    app::handle_open(gtk_app, &[other], "");
    pump(Duration::from_millis(300));
    assert_eq!(
        app::window_count(),
        2,
        "a different file must still get its own window"
    );
}
