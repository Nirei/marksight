//! Window construction and the live-window registry.
//!
//! [`build`] assembles the widget tree (header bar, TOC sidebar, search
//! bar, the welcome/error/document stack) and wires every action and
//! shortcut to it. Live windows are tracked weakly in a thread-local
//! registry so second launches can raise an existing window and worker
//! results can be routed back by window id.

use std::cell::RefCell;
use std::path::Path;
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicU64, Ordering};

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use gtk::{self as gtk};

use super::links::scroll_to;
use super::search::{clear_find, cycle_find, install_find_poll, schedule_find};
use super::{APP_ID, App, State};
use crate::i18n::gettext;
use crate::timing;

const CSS: &str = include_str!("../../resources/marksight.css");
const LOGO_SVG: &str = include_str!("../../data/logo/marksight-logo.svg");

pub(super) struct Ui {
    pub(super) window: gtk::ApplicationWindow,
    pub(super) stack: gtk::Stack,
    pub(super) scrolled: gtk::ScrolledWindow,
    pub(super) toc_list: gtk::ListBox,
    pub(super) toc_column: gtk::Widget,
    pub(super) search_bar: gtk::SearchBar,
    pub(super) search_entry: gtk::SearchEntry,
    pub(super) match_counter: gtk::Label,
    pub(super) probe_hit: gtk::Label,
    pub(super) probe_active: gtk::Label,
    pub(super) toc_button: gtk::ToggleButton,
    pub(super) error_label: gtk::Label,
    pub(super) title_label: gtk::Label,
    pub(super) path_label: gtk::Label,
}

thread_local! {
    /// Live windows, most recent last. Windows are removed when the user
    /// closes them; the underlying widget tree stays parked in GTK until
    /// process exit, but a closed window is no longer routed any work.
    static WINDOWS: RefCell<Vec<Weak<App>>> = const { RefCell::new(Vec::new()) };
}

static NEXT_WINDOW_ID: AtomicU64 = AtomicU64::new(0);

/// All windows whose widget tree is still alive; dead entries are pruned.
pub(super) fn alive_windows() -> Vec<Rc<App>> {
    WINDOWS.with(|cell| {
        let mut list = cell.borrow_mut();
        list.retain(|weak| weak.strong_count() > 0);
        list.iter().filter_map(Weak::upgrade).collect()
    })
}

pub(super) fn window_by_id(id: u64) -> Option<Rc<App>> {
    alive_windows().into_iter().find(|app| app.id == id)
}

fn register_window(app: &Rc<App>) {
    WINDOWS.with(|cell| cell.borrow_mut().push(Rc::downgrade(app)));
}

fn unregister_window(id: u64) {
    WINDOWS.with(|cell| {
        cell.borrow_mut()
            .retain(|weak| weak.upgrade().is_none_or(|app| app.id != id))
    });
}

/// Marks `app`'s window closed and makes it inert: the file watcher is
/// dropped right away (a hidden window must not keep re-reading its file),
/// pending loads are invalidated, and the window leaves the live registry
/// so nothing routes work to it anymore.
fn close_window(app: &Rc<App>) {
    app.closed.set(true);
    let mut state = app.state.borrow_mut();
    state.path = None;
    state.watcher = None;
    drop(state);
    unregister_window(app.id);
}

#[doc(hidden)]
pub fn window_count() -> usize {
    alive_windows().len()
}

/// Returns the most recently created live window, building a fresh welcome
/// window when none exists. Used by `activate` and by the smoke tests, which
/// drive a single window directly.
pub fn ensure_window(app: &gtk::Application) -> Rc<App> {
    if let Some(existing) = alive_windows().last() {
        return existing.clone();
    }
    let window = build(app);
    register_window(&window);
    timing::mark("window-built");
    window
}

/// Opens `path` in a new window and returns its handle. The window stays
/// hidden until its first page (or load error) is ready; presenting it then
/// completes startup notification for whoever launched the file.
pub fn open_window(app: &gtk::Application, path: &Path) -> Rc<App> {
    open_window_with_startup(app, path, None)
}

pub(super) fn open_window_with_startup(
    app: &gtk::Application,
    path: &Path,
    startup_id: Option<&str>,
) -> Rc<App> {
    let window = build(app);
    register_window(&window);
    if let Some(id) = startup_id.filter(|id| !id.is_empty()) {
        window
            .state
            .borrow_mut()
            .pending_startup_id
            .replace(id.to_string());
    }
    window.open_path(path);
    window
}

/// True when both paths name the same file, resolving symlinks when the
/// files exist and falling back to plain equality when they do not.
fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

fn build(app: &gtk::Application) -> Rc<App> {
    load_css();

    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .default_width(920)
        .default_height(700)
        .title("marksight")
        .icon_name(APP_ID)
        .build();
    window.connect_map(|_| timing::mark("window-mapped"));
    window.connect_realize(|_| timing::mark("window-realized"));

    let open_button = gtk::Button::from_icon_name("document-open-symbolic");
    open_button.set_tooltip_text(Some(&gettext("Open a markdown file (Ctrl+O)")));
    open_button.update_property(&[gtk::accessible::Property::Label(&gettext(
        "Open markdown file",
    ))]);

    let toc_button = gtk::ToggleButton::with_label(&gettext("Contents"));
    toc_button.set_tooltip_text(Some(&gettext("Toggle the table of contents (F9)")));

    let menu = gtk::gio::Menu::new();
    let section = gtk::gio::Menu::new();
    section.append(
        Some(gettext("About marksight").as_str()),
        Some("win.about"),
    );
    section.append(Some(gettext("Quit").as_str()), Some("win.quit"));
    menu.append_section(None, &section);
    let menu_button = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .menu_model(&menu)
        .build();

    let title_label = gtk::Label::new(Some("marksight"));
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
    // Size the content by its natural height: in documents taller than the
    // window, the default MINIMUM policy collapses zero-minimum widgets
    // (shrinkable pictures) to nothing.
    viewport.set_vscroll_policy(gtk::ScrollablePolicy::Natural);
    viewport.set_child(Some(&gtk::Label::new(None)));
    scrolled.set_child(Some(&viewport));
    stack.add_named(&scrolled, Some("doc"));
    stack.set_visible_child_name("welcome");

    let toc_list = gtk::ListBox::new();
    toc_list.set_css_classes(&["mv-toc"]);
    toc_list.set_selection_mode(gtk::SelectionMode::None);
    let toc_scrolled = gtk::ScrolledWindow::new();
    toc_scrolled.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    toc_scrolled.set_hexpand(true);
    toc_scrolled.set_vexpand(true);
    toc_scrolled.set_child(Some(&toc_list));
    let toc_column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    toc_column.set_css_classes(&["mv-toc-column"]);
    toc_column.set_size_request(230, -1);
    toc_column.append(&toc_scrolled);

    let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
    paned.set_vexpand(true);
    paned.set_hexpand(true);
    paned.set_start_child(Some(&toc_column));
    paned.set_end_child(Some(&stack));
    paned.set_position(230);
    paned.set_shrink_start_child(false);

    let search_entry = gtk::SearchEntry::new();
    search_entry.set_hexpand(true);
    search_entry.set_placeholder_text(Some(&gettext("Find in document")));
    let match_counter = gtk::Label::new(None);
    match_counter.set_css_classes(&["mv-match-counter"]);
    let search_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    search_box.set_css_classes(&["mv-search-box"]);
    search_box.append(&search_entry);
    search_box.append(&match_counter);
    let search_bar = gtk::SearchBar::new();
    search_bar.set_child(Some(&search_box));
    search_bar.set_key_capture_widget(Some(&window));
    search_bar.connect_entry(&search_entry);

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

    let id = NEXT_WINDOW_ID.fetch_add(1, Ordering::Relaxed);
    let application = App {
        id,
        ui,
        state: RefCell::new(State::new()),
        closed: std::cell::Cell::new(false),
    };
    let shared = Rc::new(application);
    wire(&shared, app, &open_button);

    // Window lifetime: Rc drops never happen for closed windows (GTK keeps
    // the widget tree, and every signal closure holds the App), so closure
    // is tracked explicitly and everything the window keeps doing is
    // stopped here.
    {
        let closing = Rc::downgrade(&shared);
        shared.ui.window.connect_close_request(move |_| {
            if let Some(app) = closing.upgrade() {
                close_window(&app);
            }
            glib::Propagation::Proceed
        });
    }

    install_find_poll(&shared);

    shared
}

fn load_css() {
    static LOADED: std::sync::Once = std::sync::Once::new();
    LOADED.call_once(|| {
        let provider = gtk::CssProvider::new();
        provider.load_from_data(CSS);
        if let Some(display) = gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
    });
}

fn wire(app: &Rc<App>, _gtk_app: &gtk::Application, open_button: &gtk::Button) {
    let window = app.ui.window.clone();

    // Both actions live on the window, not the application: their closures
    // capture the window, and an app-level action added per window would
    // pin every closed window (and its App) for the application's lifetime.
    let about = gtk::gio::SimpleAction::new("about", None);
    about.connect_activate({
        let window = window.clone();
        move |_, _| show_about(&window)
    });
    window.add_action(&about);

    let quit = gtk::gio::SimpleAction::new("quit", None);
    quit.connect_activate({
        let window = window.clone();
        move |_, _| window.close()
    });
    window.add_action(&quit);

    let controller = gtk::ShortcutController::new();
    controller.set_scope(gtk::ShortcutScope::Managed);
    for accel in ["<control>o", "<control>f", "F9", "<control>w", "<control>q"] {
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
                scroll_to(&app, &target);
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
    app.ui.search_entry.connect_activate({
        let app = app.clone();
        move |_| cycle_find(&app, true)
    });
    let key_controller = gtk::EventControllerKey::new();
    key_controller.connect_key_pressed({
        let app = app.clone();
        move |_, keyval, _, state| {
            if keyval == gtk::gdk::Key::Return && state.contains(gtk::gdk::ModifierType::SHIFT_MASK)
            {
                cycle_find(&app, false);
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        }
    });
    app.ui.search_entry.add_controller(key_controller);
    app.ui.search_entry.connect_stop_search({
        let app = app.clone();
        move |_| {
            app.ui.search_bar.set_search_mode(false);
            super::search::clear_find(&app);
        }
    });
}

fn collect_pictures(widget: &gtk::Widget, out: &mut Vec<(i32, i32)>) {
    if let Some(picture) = widget.downcast_ref::<gtk::Picture>() {
        out.push((picture.width(), picture.height()));
    }
    let mut child = widget.first_child();
    while let Some(c) = child {
        collect_pictures(&c, out);
        child = c.next_sibling();
    }
}

fn key_pressed(app: &Rc<App>, accel: &str) {
    match accel {
        "<control>o" => open_dialog(app),
        "<control>f" => toggle_find(app),
        "F9" => toggle_toc(app),
        "<control>w" => app.ui.window.close(),
        "<control>q" => quit_or_close(&app.ui.window),
        _ => {}
    }
}

/// Ctrl+Q: quit through the owning application when there is one, and
/// close the window when there is not — a detached window (no application
/// attached) must degrade to closing, not panic. Test hook for the smoke
/// suite; production reaches it through the shortcut controller.
#[doc(hidden)]
pub fn quit_or_close(window: &gtk::ApplicationWindow) {
    match window.application() {
        Some(gtk_app) => gtk_app.quit(),
        None => window.close(),
    }
}

fn toggle_toc(app: &Rc<App>) {
    let visible = !app.ui.toc_column.is_visible();
    app.ui.toc_column.set_visible(visible);
    app.ui.toc_button.set_active(visible);
}

/// Ctrl+F: toggle the search bar. The bar's key capture only intercepts
/// plain typing (it opens the bar and feeds the entry); GTK leaves the
/// Ctrl+F shortcut to the application, so it belongs here with the other
/// window shortcuts. Closing clears the search, mirroring what Escape
/// does through `stop_search`.
fn toggle_find(app: &Rc<App>) {
    if !app.ui.search_bar.is_search_mode() {
        app.ui.search_bar.set_search_mode(true);
        app.ui.search_entry.grab_focus();
    } else {
        app.ui.search_bar.set_search_mode(false);
        clear_find(app);
    }
}

fn show_about(window: &gtk::ApplicationWindow) {
    let about = gtk::AboutDialog::new();
    about.set_transient_for(Some(window));
    about.set_modal(true);
    about.set_program_name(Some("marksight"));
    about.set_version(Some(env!("CARGO_PKG_VERSION")));
    about.set_comments(Some(&gettext("A minimal, theme-native markdown viewer")));
    about.set_license(Some(
        "Licensed under either of MIT or Apache-2.0, at your option.",
    ));
    match gdk::Texture::from_bytes(&glib::Bytes::from_static(LOGO_SVG.as_bytes())) {
        Ok(texture) => about.set_logo(Some(&texture)),
        Err(_) => about.set_logo_icon_name(Some(APP_ID)),
    }
    about.set_website(Some("https://github.com/cinzatech/marksight"));
    about.present();
}

fn welcome_page() -> gtk::Widget {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 12);
    column.set_valign(gtk::Align::Center);
    column.set_halign(gtk::Align::Center);
    let title = gtk::Label::new(Some("marksight"));
    title.set_css_classes(&["mv-welcome-title"]);
    let subtitle = gtk::Label::new(Some(&gettext("Open a Markdown file to view it here")));
    subtitle.set_css_classes(&["mv-welcome-subtitle"]);
    let button = gtk::Button::with_label(&gettext("Open a file…"));
    button.set_css_classes(&["mv-welcome-button"]);
    button.set_action_name(Some("win.open"));
    let hint = gtk::Label::new(Some(&gettext(
        "Ctrl+O open · F9 contents · Ctrl+F find · live reload on save",
    )));
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
    let title = gtk::Label::new(Some(&gettext("Could not open file")));
    title.set_css_classes(&["mv-welcome-title"]);
    let label = gtk::Label::new(None);
    label.set_css_classes(&["mv-welcome-subtitle"]);
    label.set_wrap(true);
    label.set_selectable(true);
    let button = gtk::Button::with_label(&gettext("Open another file…"));
    button.set_css_classes(&["mv-welcome-button"]);
    button.set_action_name(Some("win.open"));
    column.append(&title);
    column.append(&label);
    column.append(&button);
    (column.upcast(), label)
}

fn open_dialog(app: &Rc<App>) {
    // No custom title: with the property unset, GTK shows its own
    // translated default ("Pick a File") from the gtk40 message catalog,
    // which an app-supplied English string would replace.
    let dialog = gtk::FileDialog::new();
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
    // Test hooks for the GUI smoke suite: they poke at widgets and state in
    // ways no library user should rely on, so they are hidden from the docs
    // and excluded from the semver surface.
    #[doc(hidden)]
    pub fn search_set(&self, text: &str) {
        self.ui.search_entry.set_text(text);
    }

    /// Toggles the search bar the way the Ctrl+F shortcut does; test hook
    /// for the GUI smoke suite (key events cannot be synthesized there).
    #[doc(hidden)]
    pub fn toggle_find(self: &Rc<Self>) {
        toggle_find(self);
    }

    #[doc(hidden)]
    pub fn search_mode(&self) -> bool {
        self.ui.search_bar.is_search_mode()
    }

    #[doc(hidden)]
    pub fn search_counter(&self) -> String {
        self.ui.match_counter.text().to_string()
    }

    #[doc(hidden)]
    pub fn toc_row_count(&self) -> i32 {
        self.ui.toc_list.observe_children().n_items() as i32
    }

    #[doc(hidden)]
    pub fn toc_list(&self) -> &gtk::ListBox {
        &self.ui.toc_list
    }

    #[doc(hidden)]
    pub fn heading_widget(&self, index: usize) -> Option<gtk::Widget> {
        self.state.borrow().toc_targets.get(index).cloned()
    }

    #[doc(hidden)]
    pub fn doc_scrolled(&self) -> gtk::ScrolledWindow {
        self.ui.scrolled.clone()
    }

    #[doc(hidden)]
    pub fn debug_label_states(&self) -> Vec<String> {
        let state = self.state.borrow();
        state
            .rendered
            .as_ref()
            .map(|r| r.debug_highlighted())
            .unwrap_or_default()
    }

    #[doc(hidden)]
    pub fn search_emit_activate(&self) {
        self.ui.search_entry.emit_by_name::<()>("activate", &[]);
    }

    #[doc(hidden)]
    pub fn scroll_value(&self) -> f64 {
        self.ui.scrolled.vadjustment().value()
    }

    #[doc(hidden)]
    pub fn label_group_count(&self) -> usize {
        self.state
            .borrow()
            .rendered
            .as_ref()
            .map(|r| r.groups.len())
            .unwrap_or(0)
    }

    #[doc(hidden)]
    pub fn visible_page(&self) -> String {
        self.ui
            .stack
            .visible_child_name()
            .map(|s| s.to_string())
            .unwrap_or_default()
    }

    /// Text currently shown on the error page; test hook for asserting
    /// which failure a load reported.
    #[doc(hidden)]
    pub fn error_text(&self) -> String {
        self.ui.error_label.text().to_string()
    }

    /// True once the progressive render has built the whole document and
    /// find/TOC are available.
    #[doc(hidden)]
    pub fn rendering_complete(&self) -> bool {
        self.state.borrow().render_complete
    }

    /// Allocated (width, height) of every picture in the current document;
    /// test hook for image-layout regressions.
    #[doc(hidden)]
    pub fn picture_sizes(&self) -> Vec<(i32, i32)> {
        let mut out = Vec::new();
        if let Some(viewport) = self.ui.scrolled.child().and_downcast::<gtk::Viewport>() {
            if let Some(content) = viewport.child() {
                collect_pictures(&content, &mut out);
            }
        }
        out
    }

    #[doc(hidden)]
    pub fn current_doc_len(&self) -> usize {
        self.state.borrow().doc.blocks.len()
    }

    /// Present the window on first content so its first paint already shows
    /// a page instead of an empty shell. No-op afterwards: later opens never
    /// raise or focus an already-visible window.
    pub(super) fn present_if_hidden(&self) {
        self.finish_startup_notification();
        if !self.ui.window.is_visible() {
            self.ui.window.present();
        }
    }

    /// Raise and focus this window, ending the launching desktop's startup
    /// feedback if an id was given.
    pub(super) fn present_existing(&self, startup_id: Option<&str>) {
        if let Some(id) = startup_id.filter(|id| !id.is_empty()) {
            self.notify_startup_complete(id);
        }
        self.ui.window.present();
    }

    /// True when this window is currently showing `path` (symlinks and
    /// relative paths resolved away on both sides).
    pub(super) fn shows_path(&self, path: &Path) -> bool {
        let Some(shown) = self.state.borrow().path.clone() else {
            return false;
        };
        same_file(&shown, path)
    }

    /// Consumes the pending startup-notification id, attaching it to the
    /// window and telling the desktop the launch finished.
    fn finish_startup_notification(&self) {
        let Some(id) = self.state.borrow_mut().pending_startup_id.take() else {
            return;
        };
        self.notify_startup_complete(&id);
    }

    fn notify_startup_complete(&self, id: &str) {
        // gtk_window_set_startup_id is the sanctioned way to finish a
        // startup-notification sequence (the explicit display-level call
        // was deprecated in GTK 4.10 in its favor): it stores the id on a
        // hidden window and completes at map time, or completes
        // immediately for a window that is already on screen.
        self.ui.window.set_startup_id(id);
    }
}
