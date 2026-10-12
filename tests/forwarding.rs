//! Second-launch forwarding over a real D-Bus: the startup-notification id
//! of the launching desktop must travel with the Open call, or the desktop
//! keeps its launch feedback (busy cursor, placeholder taskbar entry) until
//! its own timeout when a file is opened into an already-running instance.
//!
//! The suite runs the real binary as a child of a private dbus-daemon and
//! plays the running instance itself, so the whole path is exercised: the
//! launch environment, the forwarder child, and the Open body it sends.
//! Skipped when no dbus-daemon is available, mirroring how the GUI smoke
//! suite skips without a display.

#![cfg(feature = "ui")]

use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;

const APP_ID: &str = "io.github.marksight.Marksight";
const OBJECT_PATH: &str = "/io/github/marksight/Marksight";

const NODE_XML: &str = "
<node>
  <interface name='org.gtk.Application'>
    <method name='Activate'>
      <arg type='a{sv}' name='platform-data' direction='in'/>
    </method>
    <method name='Open'>
      <arg type='as' name='uris' direction='in'/>
      <arg type='s' name='hint' direction='in'/>
      <arg type='a{sv}' name='platform-data' direction='in'/>
    </method>
  </interface>
</node>
";

/// One Open call as the mock primary received it.
#[derive(Debug)]
struct Received {
    uris: Vec<String>,
    hint: String,
    startup_id: Option<String>,
}

fn pump(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
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

#[test]
fn forwarded_open_carries_the_launch_startup_id() {
    let Ok(mut daemon) = Command::new("dbus-daemon")
        .args(["--session", "--nofork", "--print-address=1", "--nopidfile"])
        .stdout(Stdio::piped())
        .spawn()
    else {
        eprintln!("skipping forwarding test: no dbus-daemon available");
        return;
    };
    let address = {
        use std::io::BufRead;
        let stdout = daemon.stdout.take().expect("piped stdout");
        let mut line = String::new();
        std::io::BufReader::new(stdout)
            .read_line(&mut line)
            .expect("dbus-daemon prints its address");
        line.trim().to_string()
    };

    // The mock "running instance": owns the application name and answers
    // Open, recording what the forwarder actually sent.
    let connection = gio::DBusConnection::for_address_sync(
        &address,
        gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
            | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
        None::<&gio::DBusAuthObserver>,
        None::<&gio::Cancellable>,
    )
    .expect("connect to the private bus");
    connection
        .call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "RequestName",
            Some(&glib::Variant::tuple_from_iter([
                APP_ID.to_variant(),
                4u32.to_variant(),
            ])),
            None,
            gio::DBusCallFlags::NONE,
            -1,
            None::<&gio::Cancellable>,
        )
        .expect("own the application name");

    let (sender, receiver) = mpsc::channel::<Received>();
    let node = gio::DBusNodeInfo::for_xml(NODE_XML).expect("valid interface xml");
    let interface = node
        .lookup_interface("org.gtk.Application")
        .expect("interface");
    let registration = connection
        .register_object(OBJECT_PATH, &interface)
        .method_call(
            move |_connection, _sender, _path, _interface, method, params, invocation| {
                if method == "Open" {
                    let uris: Vec<String> = params.child_value(0).get().unwrap();
                    let hint: String = params.child_value(1).get().unwrap();
                    let platform = glib::VariantDict::new(Some(&params.child_value(2)));
                    let startup_id: Option<String> = platform
                        .lookup::<String>("desktop-startup-id")
                        .ok()
                        .flatten();
                    sender
                        .send(Received {
                            uris,
                            hint,
                            startup_id,
                        })
                        .expect("recorder channel alive");
                }
                invocation.return_value(None);
            },
        )
        .build()
        .expect("register mock application object");

    // The launch: the environment a file manager gives, including the
    // startup-notification id. GTK's constructor strips the variable before
    // the binary's main runs, so receiving it on the bus proves the
    // exec-time recovery in the forwarder works. The reply to the Open call
    // only goes out while this test pumps its main context, so the child
    // runs concurrently with the pump instead of through output().
    let doc = std::env::temp_dir().join("marksight-forwarding-test.md");
    std::fs::write(&doc, "# forwarding\n").unwrap();
    let mut launched = Command::new(env!("CARGO_BIN_EXE_marksight"))
        .arg(&doc)
        .env("DBUS_SESSION_BUS_ADDRESS", &address)
        .env("DESKTOP_STARTUP_ID", "test-launch-51-time4242")
        .spawn()
        .expect("run the real binary against the private bus");

    assert!(
        pump(Duration::from_secs(10), || {
            launched.try_wait().map(|s| s.is_some()).unwrap_or(false) || receiver.try_recv().is_ok()
        }),
        "neither the launch nor the Open call finished in time"
    );
    let status = launched.wait().expect("launch child is waitable");
    let received = receiver.try_recv();
    assert!(
        received.is_ok(),
        "the launch exited (status {status}) without the mock primary seeing the Open call"
    );
    let received = received.expect("Open call recorded");

    connection.unregister_object(registration).unwrap();
    drop(connection);

    assert!(
        status.success(),
        "the launch should hand its file to the running instance and exit 0"
    );
    assert_eq!(
        received.uris,
        vec![gio::File::for_path(&doc).uri().to_string()],
        "the file's URI must be forwarded"
    );
    assert_eq!(
        received.hint, "test-launch-51-time4242",
        "the Open hint must carry the launch's startup id"
    );
    assert_eq!(
        received.startup_id.as_deref(),
        Some("test-launch-51-time4242"),
        "the platform data must carry the launch's startup id"
    );

    let _ = std::fs::remove_file(&doc);
    let _ = daemon.kill();
    let _ = daemon.wait();
    if let Some(path) = address.strip_prefix("unix:path=") {
        let _ = std::fs::remove_file(path);
    }
}
