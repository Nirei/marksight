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

use markview::app;

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
    scroll_targets_move_viewport(&mv);
    pathological_documents_do_not_crash(&mv);
    live_reload_replaces_content(&mv);
    huge_document_within_budget(&mv);
    error_state_on_missing_file(&mv);
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
        pump_until(Duration::from_secs(5), || mv.visible_page() == "doc"),
        "document page never appeared"
    );
    assert!(
        mv.label_group_count() > 5,
        "kitchen sink should produce many label groups, got {}",
        mv.label_group_count()
    );
    assert_eq!(mv.scroll_value(), 0.0);
}

fn toc_matches_headings(mv: &std::rc::Rc<app::App>) {
    let doc = markview::doc::parse(&std::fs::read_to_string(fixture("kitchen-sink.md")).unwrap());
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
    mv.search_set("markview");
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

fn scroll_targets_move_viewport(mv: &std::rc::Rc<app::App>) {
    let doc_text = (1..=400)
        .map(|i| format!("## heading {i}\n\ndocument body {i}\n"))
        .collect::<String>();
    let path = std::env::temp_dir().join("markview-smoke-tall.md");
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
    let path = std::env::temp_dir().join("markview-smoke-pathological.md");
    std::fs::write(&path, &pathological).unwrap();
    mv.open_path(&path);
    assert!(
        pump_until(Duration::from_secs(5), || mv.label_group_count() > 100),
        "pathological document never applied (groups={})",
        mv.label_group_count()
    );
    let _ = std::fs::remove_file(&path);
}

fn live_reload_replaces_content(mv: &std::rc::Rc<app::App>) {
    let path = std::env::temp_dir().join("markview-smoke-reload.md");
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

fn huge_document_within_budget(mv: &std::rc::Rc<app::App>) {
    let mut text = String::with_capacity(400_000);
    for i in 0..1100 {
        text.push_str(&format!("## section {i}\nsome *emphasis* and `code` and [link](https://example.com/{i}) text\n\n- item one\n- item two with **bold**\n\n> a quote line\n\n```rust\nfn f{i}() -> u32 {{ {i} }}\n```\n\n"));
    }
    let path = std::env::temp_dir().join("markview-smoke-huge.md");
    std::fs::write(&path, &text).unwrap();

    let parsed = std::time::Instant::now();
    let doc = markview::doc::parse(&text);
    let parse_ms = parsed.elapsed().as_millis();
    let indexed = std::time::Instant::now();
    let index = markview::find::TextIndex::build(&doc);
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
    mv.open_path(PathBuf::from("/nonexistent/markview/does-not-exist.md").as_path());
    assert!(
        pump_until(Duration::from_secs(5), || mv.visible_page() == "error"),
        "missing file should land on the error page, got {:?}",
        mv.visible_page()
    );
}
