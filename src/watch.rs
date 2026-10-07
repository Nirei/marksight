//! Live reload: filesystem watching with a trailing debounce.
//!
//! The parent directory is watched (not the file) so editor save-via-rename
//! is seen too. Events drain into the main loop through a plain channel; the
//! reload callback fires once the file has been quiet for [`DEBOUNCE_MS`],
//! so save storms trigger a single re-read.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use gtk::glib;
use notify::{Event, RecursiveMode, Watcher as _};

const DEBOUNCE_MS: Duration = Duration::from_millis(150);
const POLL: Duration = Duration::from_millis(50);

pub struct FileWatcher {
    _watcher: notify::RecommendedWatcher,
}

impl FileWatcher {
    pub fn new(path: &Path, changed: impl Fn() + 'static) -> notify::Result<Self> {
        let dir: PathBuf = path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
        let name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();

        let (tx, rx) = mpsc::channel::<()>();
        let mut watcher = notify::recommended_watcher(move |event: Result<Event, notify::Error>| {
            if let Ok(event) = event {
                if event.paths.iter().any(|p| p.file_name() == Some(&name)) {
                    let _ = tx.send(());
                }
            }
        })?;
        watcher.watch(&dir, RecursiveMode::NonRecursive)?;

        let mut last_event: Option<Instant> = None;
        glib::timeout_add_local(POLL, move || {
            let mut seen = false;
            while rx.try_recv().is_ok() {
                seen = true;
            }
            if seen {
                last_event = Some(Instant::now());
            }
            if let Some(at) = last_event {
                if at.elapsed() >= DEBOUNCE_MS {
                    last_event = None;
                    changed();
                }
            }
            glib::ControlFlow::Continue
        });

        Ok(FileWatcher { _watcher: watcher })
    }
}
