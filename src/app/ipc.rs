//! Second-launch forwarding over D-Bus: hand files to an already-running
//! primary instance and recover the launch's startup-notification id.

use std::path::PathBuf;

use gtk::glib;
use gtk::prelude::*;

use super::APP_ID;

const APP_OBJECT_PATH: &str = "/io/github/cinzatech/marksight";

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
    let name = glib::Variant::tuple_from_iter([APP_ID.to_variant()]);
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
    let platform = glib::VariantDict::new(None);
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
    platform: &glib::VariantDict,
) -> (&'static str, glib::Variant) {
    if files.is_empty() {
        ("Activate", glib::Variant::tuple_from_iter([platform.end()]))
    } else {
        let uris: Vec<String> = files
            .iter()
            .map(|path| gtk::gio::File::for_path(path).uri().to_string())
            .collect();
        (
            "Open",
            glib::Variant::tuple_from_iter([uris.to_variant(), hint.to_variant(), platform.end()]),
        )
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
