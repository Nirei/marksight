//! Application shell: window, header bar, TOC sidebar, search bar, states.
//!
//! Owns the [`Document`] and the [`RenderedDoc`]; parsing happens on a worker
//! thread and lands here through a main-loop poller, keeping the UI thread
//! responsive for rebuilds of arbitrarily large files. markview is a
//! single-window application, so the [`App`] handle is kept in a thread-local
//! registry shared by the activate and open signals.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use gtk::{self as gtk};

use crate::doc::{self, Document, TocEntry};
use crate::find::{self, Match, TextIndex};
use crate::markup;
use crate::render::{self, RenderCtx, RenderedDoc};
use crate::watch::FileWatcher;

pub const APP_ID: &str = "io.github.markview.Markview";

const CSS: &str = include_str!("../resources/markview.css");
const FIND_DEBOUNCE: Duration = Duration::from_millis(120);
const FIND_POLL: Duration = Duration::from_millis(40);
const LOAD_POLL: Duration = Duration::from_millis(40);

enum LoadMsg {
    Loaded { path: PathBuf, doc: Document },
    Failed { path: PathBuf, message: String },
}

struct Ui {
    window: gtk::ApplicationWindow,
    stack: gtk::Stack,
    scrolled: gtk::ScrolledWindow,
    toc_list: gtk::ListBox,
    toc_column: gtk::Widget,
    search_bar: gtk::SearchBar,
    search_entry: gtk::SearchEntry,
    match_counter: gtk::Label,
    probe_hit: gtk::Label,
    probe_active: gtk::Label,
    toc_button: gtk::ToggleButton,
    error_label: gtk::Label,
    title_label: gtk::Label,
    path_label: gtk::Label,
}

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
        }
    }
}

pub struct App {
    ui: Ui,
    state: RefCell<State>,
    loader: mpsc::Sender<LoadMsg>,
}

thread_local! {
    static INSTANCE: RefCell<Option<Rc<App>>> = const { RefCell::new(None) };
}

pub fn run() -> glib::ExitCode {
    let app = gtk::Application::builder()
        .application_id(APP_ID)
        .flags(gtk::gio::ApplicationFlags::HANDLES_OPEN)
        .build();
    app.connect_activate(|app| {
        ensure_window(app);
    });
    app.connect_open(|app, files, _| {
        let application = ensure_window(app);
        if let Some(path) = files.first().and_then(|f| f.path()) {
            application.open_path(&path);
        }
    });
    app.run()
}

pub fn ensure_window(app: &gtk::Application) -> Rc<App> {
    let existing = INSTANCE.with(|cell| cell.borrow().clone());
    if let Some(application) = existing {
        return application;
    }
    let (sender, receiver) = mpsc::channel::<LoadMsg>();
    let application = build(app, sender);
    INSTANCE.with(|cell| *cell.borrow_mut() = Some(application.clone()));

    let poll_app = application.clone();
    glib::timeout_add_local(LOAD_POLL, move || {
        while let Ok(msg) = receiver.try_recv() {
            match msg {
                LoadMsg::Loaded { path, doc } => apply_doc(&poll_app, &path, doc),
                LoadMsg::Failed { path, message } => show_error(&poll_app, &path, &message),
            }
        }
        glib::ControlFlow::Continue
    });

    let find_app = application.clone();
    glib::timeout_add_local(FIND_POLL, move || {
        let pending = find_app.state.borrow_mut().find_pending.take();
        if let Some(at) = pending {
            if at.elapsed() < FIND_DEBOUNCE {
                find_app.state.borrow_mut().find_pending = Some(at);
            } else {
                let needle = find_app.ui.search_entry.text().to_string();
                run_find(&find_app, &needle);
            }
        }
        glib::ControlFlow::Continue
    });

    application
}

fn build(app: &gtk::Application, loader: mpsc::Sender<LoadMsg>) -> Rc<App> {
    load_css();

    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .default_width(920)
        .default_height(700)
        .title("markview")
        .icon_name(APP_ID)
        .build();

    let open_button = gtk::Button::with_label("Open");
    open_button.set_tooltip_text(Some("Open a markdown file (Ctrl+O)"));

    let toc_button = gtk::ToggleButton::with_label("Contents");
    toc_button.set_tooltip_text(Some("Toggle the table of contents (F9)"));

    let menu = gtk::gio::Menu::new();
    let section = gtk::gio::Menu::new();
    section.append(Some("About markview"), Some("app.about"));
    section.append(Some("Quit"), Some("app.quit"));
    menu.append_section(None, &section);
    let menu_button = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .menu_model(&menu)
        .build();

    let title_label = gtk::Label::new(Some("markview"));
    title_label.set_css_classes(&["mv-title"]);
    title_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let path_label = gtk::Label::new(None);
    path_label.set_css_classes(&["mv-path"]);
    path_label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    let title_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    title_box.append(&title_label);
    title_box.append(&path_label);

    let header = gtk::HeaderBar::new();
    header.set_title_widget(Some(&title_box));
    header.pack_start(&open_button);
    header.pack_start(&toc_button);
    header.pack_end(&menu_button);
    window.set_titlebar(Some(&header));

    let stack = gtk::Stack::new();
    stack.add_named(&welcome_page(), Some("welcome"));

    let error_page = error_page();
    stack.add_named(&error_page.0, Some("error"));

    let scrolled = gtk::ScrolledWindow::new();
    scrolled.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scrolled.set_hexpand(true);
    scrolled.set_vexpand(true);
    let viewport = gtk::Viewport::new(None::<&gtk::Adjustment>, None::<&gtk::Adjustment>);
    viewport.set_child(Some(&gtk::Label::new(None)));
    scrolled.set_child(Some(&viewport));
    stack.add_named(&scrolled, Some("doc"));
    stack.set_visible_child_name("welcome");

    let toc_list = gtk::ListBox::new();
    toc_list.set_css_classes(&["mv-toc"]);
    toc_list.set_selection_mode(gtk::SelectionMode::None);
    let toc_scrolled = gtk::ScrolledWindow::new();
    toc_scrolled.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    toc_scrolled.set_child(Some(&toc_list));
    let toc_column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    toc_column.set_css_classes(&["mv-toc-column"]);
    toc_column.set_size_request(230, -1);
    toc_column.append(&toc_scrolled);

    let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
    paned.set_start_child(Some(&toc_column));
    paned.set_end_child(Some(&stack));
    paned.set_position(230);
    paned.set_shrink_start_child(false);

    let search_entry = gtk::SearchEntry::new();
    search_entry.set_hexpand(true);
    search_entry.set_placeholder_text(Some("Find in document"));
    let match_counter = gtk::Label::new(None);
    match_counter.set_css_classes(&["mv-match-counter"]);
    let search_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    search_box.set_css_classes(&["mv-search-box"]);
    search_box.append(&search_entry);
    search_box.append(&match_counter);
    let search_bar = gtk::SearchBar::new();
    search_bar.set_child(Some(&search_box));
    search_bar.set_key_capture_widget(Some(&window));

    let probe_hit = gtk::Label::new(None);
    probe_hit.set_css_classes(&["mv-probe-hit"]);
    probe_hit.set_visible(false);
    let probe_active = gtk::Label::new(None);
    probe_active.set_css_classes(&["mv-probe-active"]);
    probe_active.set_visible(false);

    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    column.append(&search_bar);
    column.append(&paned);
    column.append(&probe_hit);
    column.append(&probe_active);
    window.set_child(Some(&column));

    let ui = Ui {
        window,
        stack,
        scrolled,
        toc_list,
        toc_column: toc_column.upcast(),
        search_bar,
        search_entry,
        match_counter,
        probe_hit,
        probe_active,
        toc_button,
        error_label: error_page.1,
        title_label,
        path_label,
    };

    let application = App {
        ui,
        state: RefCell::new(State::new()),
        loader,
    };
    let shared = Rc::new(application);
    wire(&shared, app, &open_button);
    shared
}

fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_data(CSS);
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

fn wire(app: &Rc<App>, gtk_app: &gtk::Application, open_button: &gtk::Button) {
    let window = app.ui.window.clone();

    let about = gtk::gio::SimpleAction::new("about", None);
    about.connect_activate({
        let window = window.clone();
        move |_, _| show_about(&window)
    });
    gtk_app.add_action(&about);

    let quit = gtk::gio::SimpleAction::new("quit", None);
    quit.connect_activate({
        let gtk_app = gtk_app.clone();
        move |_, _| gtk_app.quit()
    });
    gtk_app.add_action(&quit);

    let controller = gtk::ShortcutController::new();
    controller.set_scope(gtk::ShortcutScope::Managed);
    for accel in ["<control>o", "F9", "<control>w", "<control>q"] {
        if let Some(trigger) = gtk::ShortcutTrigger::parse_string(accel) {
            let shortcut = gtk::Shortcut::new(
                Some(trigger),
                Some(gtk::CallbackAction::new({
                    let app = app.clone();
                    let accel = accel.to_owned();
                    move |_, _| {
                        key_pressed(&app, &accel);
                        glib::Propagation::Stop
                    }
                })),
            );
            controller.add_shortcut(shortcut);
        }
    }
    window.add_controller(controller);

    let win_open = gtk::gio::SimpleAction::new("open", None);
    win_open.connect_activate({
        let app = app.clone();
        move |_, _| open_dialog(&app)
    });
    window.add_action(&win_open);

    let win_toc = gtk::gio::SimpleAction::new("toggle-toc", None);
    win_toc.connect_activate({
        let app = app.clone();
        move |_, _| toggle_toc(&app)
    });
    window.add_action(&win_toc);

    open_button.connect_clicked({
        let app = app.clone();
        move |_| open_dialog(&app)
    });

    app.ui.toc_button.connect_toggled({
        let app = app.clone();
        move |button| {
            app.ui.toc_column.set_visible(button.is_active());
        }
    });

    app.ui.toc_list.connect_row_activated({
        let app = app.clone();
        move |_, row| {
            let index = row.index() as usize;
            let target = app.state.borrow().toc_targets.get(index).cloned();
            if let Some(target) = target {
                scroll_to(&app.ui.scrolled, &target);
            }
        }
    });

    app.ui.search_entry.connect_search_changed({
        let app = app.clone();
        move |_| schedule_find(&app)
    });
    app.ui.search_entry.connect_next_match({
        let app = app.clone();
        move |_| cycle_find(&app, true)
    });
    app.ui.search_entry.connect_previous_match({
        let app = app.clone();
        move |_| cycle_find(&app, false)
    });
    app.ui.search_entry.connect_stop_search({
        let app = app.clone();
        move |_| {
            app.ui.search_bar.set_search_mode(false);
            clear_find(&app);
        }
    });

    window.present();
}

fn key_pressed(app: &Rc<App>, accel: &str) {
    match accel {
        "<control>o" => open_dialog(app),
        "F9" => toggle_toc(app),
        "<control>w" => app.ui.window.close(),
        "<control>q" => app
            .ui
            .window
            .application()
            .expect("window attached to application")
            .quit(),
        _ => {}
    }
}

fn toggle_toc(app: &Rc<App>) {
    let visible = !app.ui.toc_column.is_visible();
    app.ui.toc_column.set_visible(visible);
    app.ui.toc_button.set_active(visible);
}

fn show_about(window: &gtk::ApplicationWindow) {
    let about = gtk::AboutDialog::new();
    about.set_transient_for(Some(window));
    about.set_modal(true);
    about.set_program_name(Some("markview"));
    about.set_version(Some(env!("CARGO_PKG_VERSION")));
    about.set_comments(Some("A minimal, theme-native markdown viewer"));
    about.set_license(Some(
        "Licensed under either of MIT or Apache-2.0, at your option.",
    ));
    about.set_logo_icon_name(Some(APP_ID));
    about.set_website(Some("https://github.com/markview/markview"));
    about.present();
}

fn welcome_page() -> gtk::Widget {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 12);
    column.set_valign(gtk::Align::Center);
    column.set_halign(gtk::Align::Center);
    let title = gtk::Label::new(Some("markview"));
    title.set_css_classes(&["mv-welcome-title"]);
    let subtitle = gtk::Label::new(Some("Open a Markdown file to view it here"));
    subtitle.set_css_classes(&["mv-welcome-subtitle"]);
    let button = gtk::Button::with_label("Open a file…");
    button.set_css_classes(&["mv-welcome-button"]);
    button.set_action_name(Some("win.open"));
    let hint = gtk::Label::new(Some(
        "Ctrl+O open · F9 contents · Ctrl+F find · live reload on save",
    ));
    hint.set_css_classes(&["mv-welcome-hint"]);
    column.append(&title);
    column.append(&subtitle);
    column.append(&button);
    column.append(&hint);
    column.upcast()
}

fn error_page() -> (gtk::Widget, gtk::Label) {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 12);
    column.set_valign(gtk::Align::Center);
    column.set_halign(gtk::Align::Center);
    let title = gtk::Label::new(Some("Could not open file"));
    title.set_css_classes(&["mv-welcome-title"]);
    let label = gtk::Label::new(None);
    label.set_css_classes(&["mv-welcome-subtitle"]);
    label.set_wrap(true);
    label.set_selectable(true);
    let button = gtk::Button::with_label("Open another file…");
    button.set_css_classes(&["mv-welcome-button"]);
    button.set_action_name(Some("win.open"));
    column.append(&title);
    column.append(&label);
    column.append(&button);
    (column.upcast(), label)
}

fn open_dialog(app: &Rc<App>) {
    let dialog = gtk::FileDialog::builder().title("Open Markdown").build();
    dialog.open(Some(&app.ui.window), None::<&gtk::gio::Cancellable>, {
        let app = app.clone();
        move |result| {
            if let Ok(file) = result {
                if let Some(path) = file.path() {
                    app.open_path(&path);
                }
            }
        }
    });
}

impl App {
    pub fn search_set(&self, text: &str) {
        self.ui.search_entry.set_text(text);
    }

    pub fn search_counter(&self) -> String {
        self.ui.match_counter.text().to_string()
    }

    pub fn toc_row_count(&self) -> i32 {
        self.ui.toc_list.observe_children().n_items() as i32
    }

    pub fn toc_list(&self) -> &gtk::ListBox {
        &self.ui.toc_list
    }

    pub fn scroll_value(&self) -> f64 {
        self.ui.scrolled.vadjustment().value()
    }

    pub fn label_group_count(&self) -> usize {
        self.state
            .borrow()
            .rendered
            .as_ref()
            .map(|r| r.groups.len())
            .unwrap_or(0)
    }

    pub fn visible_page(&self) -> String {
        self.ui
            .stack
            .visible_child_name()
            .map(|s| s.to_string())
            .unwrap_or_default()
    }

    pub fn current_doc_len(&self) -> usize {
        self.state.borrow().doc.blocks.len()
    }

    pub fn open_path(&self, path: &Path) {
        {
            let mut state = self.state.borrow_mut();
            state.path = Some(path.to_path_buf());
            state.matches.clear();
            state.active = None;
            state.find_pending = None;
            state.watcher = None;
        }
        let path = path.to_path_buf();
        let sender = self.loader.clone();
        std::thread::spawn(move || {
            let msg = match std::fs::read(&path) {
                Ok(bytes) => LoadMsg::Loaded {
                    path: path.clone(),
                    doc: doc::parse(&String::from_utf8_lossy(&bytes)),
                },
                Err(err) => LoadMsg::Failed {
                    path: path.clone(),
                    message: err.to_string(),
                },
            };
            let _ = sender.send(msg);
        });
    }
}

fn show_error(app: &Rc<App>, path: &Path, message: &str) {
    if app.state.borrow().path.as_deref() != Some(path) {
        return;
    }
    app.ui
        .error_label
        .set_text(&format!("{}\n\n{}", path.display(), message));
    app.ui.stack.set_visible_child_name("error");
    app.ui.window.set_title(Some("markview"));
    app.ui.title_label.set_text("markview");
    app.ui.path_label.set_text("");
    let mut state = app.state.borrow_mut();
    state.path = None;
    state.watcher = None;
    state.rendered = None;
}

fn apply_doc(app: &Rc<App>, path: &Path, parsed: Document) {
    if app.state.borrow().path.as_deref() != Some(path) {
        return;
    }
    let anchors = Rc::new(RefCell::new(HashMap::new()));
    let toc_targets = Rc::new(RefCell::new(Vec::new()));
    let link_handler: render::LinkHandler = {
        let anchors = anchors.clone();
        let scrolled = app.ui.scrolled.clone();
        Rc::new(move |uri: &str| {
            if let Some(anchor) = uri.strip_prefix("#fn-") {
                let target = anchors.borrow().get(anchor).cloned();
                if let Some(target) = target {
                    scroll_to(&scrolled, &target);
                    return true;
                }
                return false;
            }
            if uri.starts_with('#') {
                return false;
            }
            launch_uri(uri);
            true
        })
    };

    let base_dir = path.parent().map(Path::to_path_buf);
    let ctx = RenderCtx {
        doc: &parsed,
        base_dir: base_dir.as_deref(),
        link_handler,
        anchors: anchors.clone(),
        toc_targets: toc_targets.clone(),
    };
    let rendered = render::render(&ctx);
    if let Some(viewport) = app.ui.scrolled.child().and_downcast::<gtk::Viewport>() {
        viewport.set_child(Some(&rendered.root));
    }
    app.ui.scrolled.vadjustment().set_value(0.0);

    let index = TextIndex::build(&parsed);
    let headings = parsed.headings.clone();
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "markview".to_owned());

    {
        let mut state = app.state.borrow_mut();
        state.path = Some(path.to_path_buf());
        state.doc = parsed;
        state.index = index;
        state.rendered = Some(rendered);
        state.matches.clear();
        state.active = None;
        state.toc_targets = toc_targets.borrow().clone();
    }

    rebuild_toc(app, &headings);
    app.ui.stack.set_visible_child_name("doc");
    app.ui.window.set_title(Some(&file_name));
    app.ui.title_label.set_text(&file_name);
    app.ui.path_label.set_text(&path.display().to_string());
    let has_toc = !headings.is_empty();
    app.ui.toc_column.set_visible(has_toc);
    app.ui.toc_button.set_active(has_toc);
    app.ui.toc_button.set_visible(has_toc);
    app.ui.match_counter.set_text("");

    start_watcher(app, path);

    let needle = app.ui.search_entry.text().to_string();
    if app.ui.search_bar.is_search_mode() && !needle.trim().is_empty() {
        run_find(app, &needle);
    }
}

fn start_watcher(app: &Rc<App>, path: &Path) {
    let app_for_reload = app.clone();
    let watched = path.to_path_buf();
    let watcher = FileWatcher::new(path, move || {
        app_for_reload.open_path(&watched);
    });
    match watcher {
        Ok(watcher) => app.state.borrow_mut().watcher = Some(watcher),
        Err(err) => eprintln!("markview: live reload unavailable: {err}"),
    }
}

fn rebuild_toc(app: &Rc<App>, headings: &[TocEntry]) {
    while let Some(row) = app.ui.toc_list.first_child() {
        app.ui.toc_list.remove(&row);
    }
    let doc = &app.state.borrow().doc;
    for entry in headings {
        let text = doc
            .resolve(&entry.path)
            .and_then(|block| match block {
                doc::Block::Heading { inlines, .. } => {
                    Some(markup::walk_inlines(inlines, doc, None).text)
                }
                _ => None,
            })
            .unwrap_or_default();
        let label = gtk::Label::new(Some(&text));
        label.set_xalign(0.0);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_margin_start(6 + entry.level.saturating_sub(1) as i32 * 14);
        label.set_css_classes(&["mv-toc-row"]);
        let row = gtk::ListBoxRow::new();
        row.set_child(Some(&label));
        row.set_css_classes(&["mv-toc-item"]);
        app.ui.toc_list.append(&row);
    }
}

fn scroll_to(scrolled: &gtk::ScrolledWindow, target: &impl IsA<gtk::Widget>) {
    let target = target.upcast_ref::<gtk::Widget>();
    let Some(viewport) = scrolled.child().and_downcast::<gtk::Viewport>() else {
        return;
    };
    let Some(point) = target.compute_point(&viewport, &gtk::graphene::Point::new(0.0, 0.0)) else {
        return;
    };
    let adjustment = scrolled.vadjustment();
    let desired = point.y() as f64 - 12.0;
    let max = (adjustment.upper() - adjustment.page_size()).max(adjustment.lower());
    adjustment.set_value(desired.clamp(adjustment.lower(), max));
}

fn launch_uri(uri: &str) {
    gtk::UriLauncher::new(uri).launch(None::<&gtk::Window>, None::<&gtk::gio::Cancellable>, |_| {});
}

fn schedule_find(app: &Rc<App>) {
    if let Ok(mut state) = app.state.try_borrow_mut() {
        state.find_pending = Some(std::time::Instant::now());
    }
}

fn clear_find(app: &Rc<App>) {
    if !app.ui.search_entry.text().is_empty() {
        app.ui.search_entry.set_text("");
    } else {
        run_find(app, "");
    }
}

fn run_find(app: &Rc<App>, needle: &str) {
    let (hit, active_color) = probe_colors(app);
    let state = app.state.borrow();
    let Some(rendered) = &state.rendered else {
        return;
    };
    let matches = find::find(&state.index, needle);
    let active = if matches.is_empty() {
        None
    } else {
        find::cycle(&matches, None, true)
    };
    render::apply_highlight(
        &state.doc,
        rendered,
        |entry| {
            (
                find::entry_ranges(&matches, entry),
                find::active_range(&matches, active, entry),
            )
        },
        &hit,
        &active_color,
    );
    let counter = if needle.trim().is_empty() {
        String::new()
    } else {
        format!("{}/{}", active.map(|a| a + 1).unwrap_or(0), matches.len())
    };
    app.ui.match_counter.set_text(&counter);
    let scroll_target = active
        .and_then(|a| matches.get(a))
        .and_then(|m| rendered.first_label(m.entry).cloned());
    drop(state);
    if let Some(target) = scroll_target {
        scroll_to(&app.ui.scrolled, &target);
    }
    let mut state = app.state.borrow_mut();
    state.matches = matches;
    state.active = active;
}

fn cycle_find(app: &Rc<App>, forward: bool) {
    let (hit, active_color) = probe_colors(app);
    let state = app.state.borrow();
    let active = find::cycle(&state.matches, state.active, forward);
    let Some(rendered) = &state.rendered else {
        return;
    };
    render::apply_highlight(
        &state.doc,
        rendered,
        |entry| {
            (
                find::entry_ranges(&state.matches, entry),
                find::active_range(&state.matches, active, entry),
            )
        },
        &hit,
        &active_color,
    );
    if !state.matches.is_empty() {
        app.ui.match_counter.set_text(&format!(
            "{}/{}",
            active.map(|a| a + 1).unwrap_or(0),
            state.matches.len()
        ));
    }
    let scroll_target = active
        .and_then(|a| state.matches.get(a))
        .and_then(|m| rendered.first_label(m.entry).cloned());
    drop(state);
    if let Some(target) = scroll_target {
        scroll_to(&app.ui.scrolled, &target);
    }
    app.state.borrow_mut().active = active;
}

fn probe_colors(app: &Rc<App>) -> (String, String) {
    (
        rgba_hex(app.ui.probe_hit.color()),
        rgba_hex(app.ui.probe_active.color()),
    )
}

fn rgba_hex(color: gdk::RGBA) -> String {
    let to_byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!(
        "#{:02x}{:02x}{:02x}{:02x}",
        to_byte(color.red()),
        to_byte(color.green()),
        to_byte(color.blue()),
        to_byte(color.alpha())
    )
}
