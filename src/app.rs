//! Application shell: window, header bar, TOC sidebar, search bar, states.
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

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use gtk::{self as gtk};

use crate::doc::{self, Document, TocEntry};
use crate::find::{self, Match, TextIndex};
use crate::markup;
use crate::render::{self, RenderedDoc};
use crate::timing;
use crate::watch::FileWatcher;

pub const APP_ID: &str = "io.github.cinzatech.marksight";

const CSS: &str = include_str!("../resources/marksight.css");
const LOGO_SVG: &str = include_str!("../data/logo/marksight-logo.svg");
/// Upper bound on document bytes read into memory. One limit for every
/// untrusted input: the same 64 MiB cap the image decoder uses
/// ([`crate::render::IMAGE_MAX_FILE_BYTES`]).
const MAX_DOC_BYTES: u64 = crate::render::IMAGE_MAX_FILE_BYTES;
const FIND_DEBOUNCE: Duration = Duration::from_millis(120);
const FIND_POLL: Duration = Duration::from_millis(40);
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
    find_needle: String,
    find_active: Option<usize>,
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
            find_active: None,
            scroll_generation: 0,
            render_generation: 0,
            render_complete: false,
            pending_startup_id: None,
        }
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

thread_local! {
    /// Live windows, most recent last. Windows are removed when the user
    /// closes them; the underlying widget tree stays parked in GTK until
    /// process exit, but a closed window is no longer routed any work.
    static WINDOWS: RefCell<Vec<Weak<App>>> = const { RefCell::new(Vec::new()) };
}

static NEXT_WINDOW_ID: AtomicU64 = AtomicU64::new(0);

/// All windows whose widget tree is still alive; dead entries are pruned.
fn alive_windows() -> Vec<Rc<App>> {
    WINDOWS.with(|cell| {
        let mut list = cell.borrow_mut();
        list.retain(|weak| weak.strong_count() > 0);
        list.iter().filter_map(Weak::upgrade).collect()
    })
}

fn window_by_id(id: u64) -> Option<Rc<App>> {
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

pub fn run() -> glib::ExitCode {
    let app = gtk::Application::builder()
        .application_id(APP_ID)
        .flags(gtk::gio::ApplicationFlags::HANDLES_OPEN)
        .build();
    timing::mark("app-object-built");
    app.connect_activate(|app| {
        timing::mark("activate-signal");
        match alive_windows().last() {
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
        match alive_windows().last() {
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
        match alive_windows()
            .into_iter()
            .find(|window| window.shows_path(&path))
        {
            Some(existing) => {
                existing.present_existing(startup_id);
            }
            None => {
                open_window_with_startup(app, &path, startup_id);
            }
        }
    }
}

/// Entry point of the throwaway forwarding process started by `main` with
/// `--__forward-probe`. Returns true when a running instance was found and
/// the files were handed over; false means "no primary yet, become one".
/// The forwarding must happen in a child process: creating the session-bus
/// connection before GtkApplication initializes leaves GApplication's own
/// registration waiting forever, so the would-be primary must stay clean.
pub fn forward_files(files: &[std::ffi::OsString]) -> bool {
    let files: Vec<PathBuf> = files.iter().map(PathBuf::from).collect();
    forward_to_running_instance(&files)
}

const APP_OBJECT_PATH: &str = "/io/github/cinzatech/marksight";

/// Forwards `files` to an already-running instance and reports whether one
/// was found. Deliberately low-level: a forwarding process must not run
/// GtkApplication's remote machinery, which initializes GTK (pulling in the
/// accessibility bridge) and then waits for the primary to answer a
/// DescribeAll handshake, so a primary stuck behind a stalled portal or
/// slow a11y activation keeps the forwarding process, and with it the file
/// manager's busy cursor and placeholder taskbar icon, alive for tens of
/// seconds.
///
/// Name ownership is answered by the dbus-daemon itself, in microseconds,
/// whatever the owning application is doing; the Open call carries the
/// launch's startup id so the primary can finish the desktop's startup
/// feedback when it presents the new window, and it is sent with a short
/// timeout: delivery does not depend on the caller waiting for the reply,
/// so a busy primary delays this process by at most a quarter second.
fn forward_to_running_instance(files: &[PathBuf]) -> bool {
    let Ok(bus) =
        gtk::gio::bus_get_sync(gtk::gio::BusType::Session, None::<&gtk::gio::Cancellable>)
    else {
        return false; // no session bus: a standalone primary is fine
    };
    // D-Bus method bodies are always tuples: a lone string is ("s",).
    let name = gtk::glib::Variant::tuple_from_iter([APP_ID.to_variant()]);
    let owner = bus.call_sync(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "GetNameOwner",
        Some(&name),
        None,
        gtk::gio::DBusCallFlags::NONE,
        2000,
        None::<&gtk::gio::Cancellable>,
    );
    // An error here is the normal "no primary yet" answer (NameHasNoOwner),
    // and covers bus hiccups too: falling back to a standalone launch is
    // always a safe next move.
    if owner.is_err() {
        return false;
    }

    let startup_id = launch_startup_id().unwrap_or_default();
    let platform = gtk::glib::VariantDict::new(None);
    if !startup_id.is_empty() {
        // Both names carry the id, mirroring what GtkApplication itself
        // sends: the X11 and Wayland backends read different keys.
        platform.insert_value("desktop-startup-id", &startup_id.to_variant());
        platform.insert_value("activation-token", &startup_id.to_variant());
    }
    let (method, body) = forward_body(files, &startup_id, &platform);
    // A failed delivery must not be swallowed: report it and report "not
    // forwarded", so the caller falls back to opening the file in a
    // primary of its own instead of quietly doing nothing.
    // Two seconds, not a quarter: a reply that never comes means the only
    // other delivery path (a fresh GtkApplication handing the file over)
    // loses the startup id, so waiting longer than one main-loop hiccup of
    // the primary is what keeps the launch feedback short in the worst case;
    // a healthy primary still answers in milliseconds.
    match bus.call_sync(
        Some(APP_ID),
        APP_OBJECT_PATH,
        "org.gtk.Application",
        method,
        Some(&body),
        None,
        gtk::gio::DBusCallFlags::NONE,
        2000,
        None::<&gtk::gio::Cancellable>,
    ) {
        Ok(_) => true,
        Err(err) => {
            eprintln!("marksight: could not hand the file to the running instance: {err}");
            false
        }
    }
}

/// The startup-notification id of the launch that started this process.
///
/// A launch from the desktop sets `DESKTOP_STARTUP_ID` in the environment,
/// but GTK's library constructor moves that variable (and
/// `XDG_ACTIVATION_TOKEN`) into private storage and removes it from the
/// environment before `main` runs, so the live variable is empty by the
/// time any code here looks. The kernel keeps the exec-time environment
/// for `/proc/self/environ`, which no library constructor can touch, so the
/// id is recovered from there when the live variable is gone. Without this
/// recovery a forwarded launch can never name its startup sequence, and the
/// desktop shows launch feedback until its own timeout: a busy cursor and a
/// ghost taskbar entry for seconds after the file is already on screen.
///
/// Returns nothing on systems without `/proc` (there the plain environment
/// is the only source) and for launches that carried no id, such as one
/// from a terminal.
pub fn launch_startup_id() -> Option<String> {
    if let Some(id) = std::env::var_os("DESKTOP_STARTUP_ID") {
        let id = id.to_string_lossy().into_owned();
        return (!id.is_empty()).then_some(id);
    }
    let environ = std::fs::read("/proc/self/environ").ok()?;
    let id = startup_id_in_environ("DESKTOP_STARTUP_ID", &environ)
        .or_else(|| startup_id_in_environ("XDG_ACTIVATION_TOKEN", &environ))?;
    (!id.is_empty()).then_some(id)
}

/// Reads `name` out of a NUL-separated `KEY=VALUE` block such as
/// `/proc/self/environ`. An empty value counts as absent.
fn startup_id_in_environ(name: &str, environ: &[u8]) -> Option<String> {
    let prefix = format!("{name}=");
    let value = environ
        .split(|byte| *byte == 0)
        .find_map(|entry| entry.strip_prefix(prefix.as_bytes()))?;
    let text = String::from_utf8_lossy(value).into_owned();
    (!text.is_empty()).then_some(text)
}

/// Builds the method call that activates a running instance. The signatures
/// are fixed by GApplication's D-Bus API and pinned by a unit test:
/// `org.gtk.Application.Open(as uris, s hint, a{sv} platform-data)` and
/// `Activate(a{sv} platform-data)`. A mismatch is silently rejected by the
/// D-Bus layer, so getting these wrong means files that never open.
///
/// The hint argument carries the launching desktop's startup-notification
/// id to the primary, which uses it to end the launch feedback when it
/// presents the window.
fn forward_body(
    files: &[PathBuf],
    hint: &str,
    platform: &gtk::glib::VariantDict,
) -> (&'static str, gtk::glib::Variant) {
    if files.is_empty() {
        (
            "Activate",
            gtk::glib::Variant::tuple_from_iter([platform.end()]),
        )
    } else {
        let uris: Vec<String> = files
            .iter()
            .map(|path| gtk::gio::File::for_path(path).uri().to_string())
            .collect();
        (
            "Open",
            gtk::glib::Variant::tuple_from_iter([
                uris.to_variant(),
                hint.to_variant(),
                platform.end(),
            ]),
        )
    }
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

fn open_window_with_startup(
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
    open_button.set_tooltip_text(Some("Open a markdown file (Ctrl+O)"));
    open_button.update_property(&[gtk::accessible::Property::Label("Open markdown file")]);

    let toc_button = gtk::ToggleButton::with_label("Contents");
    toc_button.set_tooltip_text(Some("Toggle the table of contents (F9)"));

    let menu = gtk::gio::Menu::new();
    let section = gtk::gio::Menu::new();
    section.append(Some("About marksight"), Some("win.about"));
    section.append(Some("Quit"), Some("win.quit"));
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

    // Per-window find poll. Held weakly: a closed window stops paying for
    // its own timer instead of keeping the App alive forever.
    let find_weak = Rc::downgrade(&shared);
    glib::timeout_add_local(FIND_POLL, move || {
        let Some(find_app) = find_weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        if find_app.closed.get() {
            return glib::ControlFlow::Break;
        }
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
            clear_find(&app);
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
    about.set_program_name(Some("marksight"));
    about.set_version(Some(env!("CARGO_PKG_VERSION")));
    about.set_comments(Some("A minimal, theme-native markdown viewer"));
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
    // Test hooks for the GUI smoke suite: they poke at widgets and state in
    // ways no library user should rely on, so they are hidden from the docs
    // and excluded from the semver surface.
    #[doc(hidden)]
    pub fn search_set(&self, text: &str) {
        self.ui.search_entry.set_text(text);
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
    fn present_if_hidden(&self) {
        self.finish_startup_notification();
        if !self.ui.window.is_visible() {
            self.ui.window.present();
        }
    }

    /// Raise and focus this window, ending the launching desktop's startup
    /// feedback if an id was given.
    fn present_existing(&self, startup_id: Option<&str>) {
        if let Some(id) = startup_id.filter(|id| !id.is_empty()) {
            self.notify_startup_complete(id);
        }
        self.ui.window.present();
    }

    /// True when this window is currently showing `path` (symlinks and
    /// relative paths resolved away on both sides).
    fn shows_path(&self, path: &Path) -> bool {
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
            // different path drops the old render — it belongs to the
            // previous document, not to whatever lands next.
            let reload = state.path.as_deref() == Some(path);
            if !reload {
                state.rendered = None;
                state.render_complete = false;
            }
            state.path = Some(path.to_path_buf());
            state.matches.clear();
            state.active = None;
            state.find_pending = None;
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
}

/// Reads at most [`MAX_DOC_BYTES`] from a regular file, with clear errors
/// for everything else. The metadata check runs before the file is opened:
/// opening a FIFO with no writer, or a device node, would park the reader
/// thread indefinitely. `take` holds the bound even if the file grows
/// between the stat and the read.
fn read_bounded(path: &Path) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let metadata = std::fs::metadata(path).map_err(|err| err.to_string())?;
    if !metadata.is_file() {
        return Err("not a regular file".to_string());
    }
    let len = metadata.len();
    if len > MAX_DOC_BYTES {
        return Err(format!(
            "file too large: {len} bytes (limit {MAX_DOC_BYTES} bytes)"
        ));
    }
    let file = std::fs::File::open(path).map_err(|err| err.to_string())?;
    let mut bytes = Vec::with_capacity(len.min(4 << 20) as usize);
    file.take(MAX_DOC_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|err| err.to_string())?;
    Ok(bytes)
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
    state.rendered = None;
    state.render_complete = false;
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
    let link_handler: render::LinkHandler = {
        let anchors = anchors.clone();
        let app_for_links = app.clone();
        Rc::new(move |uri: &str| {
            if let Some(anchor) = uri.strip_prefix('#') {
                // In-document targets: footnote rows ("#fn-N") and heading
                // anchors from explicit {#id} attributes or generated slugs
                // share one map. Always handled, also for unknown anchors:
                // falling through would hand "#whatever" to GTK's default
                // launcher, which has no meaningful way to open it.
                let target = anchors.borrow().get(anchor).cloned();
                if let Some(target) = target {
                    scroll_to(&app_for_links, &target);
                }
                return true;
            }
            // Always stop propagation, also for refused schemes: letting
            // GTK fall through would hand the URI to the default launcher
            // and bypass the allowlist.
            launch_uri(uri);
            true
        })
    };

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
        state.rendered = None;
        state.matches.clear();
        state.active = None;
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
        let index = TextIndex {
            entries: rendered.groups.iter().map(|g| g.text(&doc)).collect(),
        };
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

impl App {
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
            "(untitled heading)".to_owned()
        } else {
            text.clone()
        };
        row.update_property(&[gtk::accessible::Property::Label(&format!(
            "Jump to {}",
            row_text
        ))]);
        app.ui.toc_list.append(&row);
    }
}

fn scroll_to(app: &Rc<App>, target: &impl IsA<gtk::Widget>) {
    let target = target.upcast_ref::<gtk::Widget>().clone();
    let generation = {
        let mut state = app.state.borrow_mut();
        state.scroll_generation += 1;
        state.scroll_generation
    };
    apply_scroll(&app.ui.scrolled, &target);
    for delay in [80, 240] {
        let app = app.clone();
        let target = target.clone();
        glib::timeout_add_local_once(Duration::from_millis(delay), move || {
            if app.state.borrow().scroll_generation == generation {
                apply_scroll(&app.ui.scrolled, &target);
            }
        });
    }
}

fn apply_scroll(scrolled: &gtk::ScrolledWindow, target: &gtk::Widget) {
    let Some(viewport) = scrolled.child().and_downcast::<gtk::Viewport>() else {
        return;
    };
    let Some(content) = viewport.child() else {
        return;
    };
    let Some(point) = target.compute_point(&content, &gtk::graphene::Point::new(0.0, 0.0)) else {
        return;
    };
    let adjustment = scrolled.vadjustment();
    let desired = point.y() as f64 - 12.0;
    let max = (adjustment.upper() - adjustment.page_size()).max(adjustment.lower());
    adjustment.set_value(desired.clamp(adjustment.lower(), max));
}

fn launch_uri(uri: &str) {
    if !is_launchable_uri(uri) {
        let shown: String = uri.chars().take(120).collect();
        eprintln!("marksight: refusing to open link (scheme not allowed): {shown}");
        return;
    }
    gtk::UriLauncher::new(uri).launch(None::<&gtk::Window>, None::<&gtk::gio::Cancellable>, |_| {});
}

/// Documents arrive from untrusted sources, so external links go through
/// a short allowlist. Only these schemes are handed to the system launcher;
/// everything else (javascript:, data:, file:, custom handlers, relative
/// paths) is refused, because a viewer that renders no HTML and runs no
/// scripts should not become a trampoline for them either.
fn is_launchable_uri(uri: &str) -> bool {
    let Some((scheme, _rest)) = uri.split_once(':') else {
        return false;
    };
    matches!(
        scheme.to_ascii_lowercase().as_str(),
        "http" | "https" | "mailto"
    )
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
    } else if needle == state.find_needle && state.find_active.is_some_and(|a| a < matches.len()) {
        state.find_active
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
        scroll_to(app, &target);
    }
    let mut state = app.state.borrow_mut();
    state.matches = matches;
    state.active = active;
    state.find_needle = needle.to_string();
    state.find_active = active;
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
        scroll_to(app, &target);
    }
    let mut state = app.state.borrow_mut();
    state.active = active;
    state.find_active = active;
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
#[cfg(test)]
mod tests {
    use super::{MAX_DOC_BYTES, is_launchable_uri, read_bounded};
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

    #[test]
    fn uri_allowlist_accepts_only_web_and_mail() {
        assert!(is_launchable_uri("https://example.com/a?b=c"));
        assert!(is_launchable_uri("http://example.com"));
        assert!(is_launchable_uri("HTTP://EXAMPLE.COM"));
        assert!(is_launchable_uri("mailto:someone@example.com"));
        assert!(!is_launchable_uri("javascript:alert(1)"));
        assert!(!is_launchable_uri("JaVaScRiPt:alert(1)"));
        assert!(!is_launchable_uri("data:text/html;base64,PHNjcmlwdD4="));
        assert!(!is_launchable_uri("file:///etc/passwd"));
        assert!(!is_launchable_uri("gopher://example.com"));
        assert!(!is_launchable_uri("example.com/relative"));
        assert!(!is_launchable_uri(""));
        assert!(!is_launchable_uri("#fragment"));
    }
}

#[cfg(test)]
mod forward_tests {
    use super::{forward_body, startup_id_in_environ};
    use gtk::glib::VariantDict;
    use std::path::PathBuf;

    #[test]
    fn exec_environ_lookup_finds_the_startup_id() {
        let environ = b"PATH=/usr/bin\0DESKTOP_STARTUP_ID=nemo-42-time7\0TERM=xterm\0";
        assert_eq!(
            startup_id_in_environ("DESKTOP_STARTUP_ID", environ).as_deref(),
            Some("nemo-42-time7")
        );
    }

    #[test]
    fn exec_environ_lookup_matches_whole_keys_only() {
        // A key that merely ends in the name must not match.
        let environ = b"XDESKTOP_STARTUP_ID=wrong\0DESKTOP_STARTUP_ID=right\0";
        assert_eq!(
            startup_id_in_environ("DESKTOP_STARTUP_ID", environ).as_deref(),
            Some("right")
        );
    }

    #[test]
    fn exec_environ_lookup_keeps_values_with_equals() {
        let environ = b"DESKTOP_STARTUP_ID=id=with=signs\0";
        assert_eq!(
            startup_id_in_environ("DESKTOP_STARTUP_ID", environ).as_deref(),
            Some("id=with=signs")
        );
    }

    #[test]
    fn exec_environ_lookup_reports_absent_and_empty() {
        assert_eq!(
            startup_id_in_environ("DESKTOP_STARTUP_ID", b"PATH=/bin\0"),
            None
        );
        assert_eq!(startup_id_in_environ("DESKTOP_STARTUP_ID", b""), None);
        // An empty value is not an id: treat it like an absent one.
        assert_eq!(
            startup_id_in_environ("DESKTOP_STARTUP_ID", b"DESKTOP_STARTUP_ID=\0"),
            None
        );
    }

    #[test]
    fn open_body_matches_gapplication_signature() {
        // org.gtk.Application.Open(as uris, s hint, a{sv} platform-data).
        // D-Bus rejects a mismatched body without ever reaching the
        // application, so this exact type string is a contract.
        let platform = VariantDict::new(None);
        let (method, body) = forward_body(&[PathBuf::from("/tmp/x.md")], "", &platform);
        assert_eq!(method, "Open");
        assert_eq!(body.type_().to_string(), "(assa{sv})");
    }

    #[test]
    fn hint_carries_the_startup_notification_id() {
        // The forwarding launch's startup id travels in the Open hint, so
        // the primary can end the desktop's launch feedback on present.
        let platform = VariantDict::new(None);
        let (_, body) = forward_body(&[PathBuf::from("/tmp/x.md")], "mystic-12345", &platform);
        let hint: String = body.child_value(1).get().unwrap();
        assert_eq!(hint, "mystic-12345");
    }

    #[test]
    fn activate_body_matches_gapplication_signature() {
        // org.gtk.Application.Activate(a{sv} platform-data).
        let platform = VariantDict::new(None);
        let (method, body) = forward_body(&[], "", &platform);
        assert_eq!(method, "Activate");
        assert_eq!(body.type_().to_string(), "(a{sv})");
    }
}
