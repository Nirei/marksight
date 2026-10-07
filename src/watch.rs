//! Live reload: filesystem watching with a trailing debounce.
//!
//! The parent directory is watched (not the file) so editor save-via-rename
//! is seen too. Only content-changing events count — read access does not.
//! Events drain into the main loop through a plain channel; the reload
//! callback fires once the file has been quiet for [`DEBOUNCE_MS`].
//!
//! Dropping the [`FileWatcher`] cancels any still-pending reload, so a
//! viewer that has moved on to another file is never re-opened by a stale
//! trailing event.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use gtk::glib;
use notify::{Event, EventKind, RecursiveMode, Watcher as _};

pub const DEBOUNCE_MS: Duration = Duration::from_millis(150);
const POLL: Duration = Duration::from_millis(50);

pub struct FileWatcher {
    _watcher: notify::RecommendedWatcher,
    poll: Option<glib::SourceId>,
}

impl Drop for FileWatcher {
    fn drop(&mut self) {
        if let Some(id) = self.poll.take() {
            id.remove();
        }
    }
}

impl FileWatcher {
    pub fn new(path: &Path, changed: impl Fn() + 'static) -> notify::Result<Self> {
        let dir: PathBuf = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let name = path
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_default();

        let (tx, rx) = mpsc::channel::<()>();
        let mut watcher =
            notify::recommended_watcher(move |event: Result<Event, notify::Error>| {
                if let Ok(event) = event {
                    let content_changed = matches!(
                        event.kind,
                        EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
                    );
                    if content_changed && event.paths.iter().any(|p| p.file_name() == Some(&name)) {
                        let _ = tx.send(());
                    }
                }
            })?;
        watcher.watch(&dir, RecursiveMode::NonRecursive)?;

        let mut last_event: Option<Instant> = None;
        let poll = glib::timeout_add_local(POLL, move || {
            let mut seen = false;
            loop {
                match rx.try_recv() {
                    Ok(()) => seen = true,
                    Err(mpsc::TryRecvError::Disconnected) => return glib::ControlFlow::Break,
                    Err(mpsc::TryRecvError::Empty) => break,
                }
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

        Ok(FileWatcher {
            _watcher: watcher,
            poll: Some(poll),
        })
    }
}
