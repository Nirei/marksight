//! Live reload: filesystem watching with a trailing debounce.
//!
//! The parent directory is watched (not the file) so editor save-via-rename
//! is seen too. Only content-changing events count — read access does not.
//! Events cross threads through an async channel; the receiver runs on the
//! main loop and waits out [`DEBOUNCE_MS`] after every event, so the reload
//! callback fires only once the file has gone quiet.
//!
//! Dropping the [`FileWatcher`] drops the sender, which ends the receiver
//! task, so a viewer that has moved on to another file is never re-opened by
//! a stale trailing event.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gtk::glib;
use notify::{Event, EventKind, RecursiveMode, Watcher as _};

pub const DEBOUNCE_MS: Duration = Duration::from_millis(150);

pub struct FileWatcher {
    _watcher: notify::RecommendedWatcher,
    _receiver: glib::JoinHandle<()>,
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

        let (tx, rx) = async_channel::unbounded::<()>();
        let mut watcher =
            notify::recommended_watcher(move |event: Result<Event, notify::Error>| {
                if let Ok(event) = event {
                    let content_changed = matches!(
                        event.kind,
                        EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
                    );
                    if content_changed && event.paths.iter().any(|p| p.file_name() == Some(&name)) {
                        let _ = tx.try_send(());
                    }
                }
            })?;
        watcher.watch(&dir, RecursiveMode::NonRecursive)?;

        // Runs on the main loop. Each event starts a fresh DEBOUNCE_MS wait;
        // an event landing during the wait restarts it, so the callback fires
        // only after the file has been quiet for the whole window.
        let receiver = glib::spawn_future_local(async move {
            loop {
                if rx.recv().await.is_err() {
                    return; // sender dropped: watcher is gone
                }
                loop {
                    glib::timeout_future(DEBOUNCE_MS).await;
                    match rx.try_recv() {
                        Ok(()) => continue,
                        Err(async_channel::TryRecvError::Empty) => break,
                        Err(async_channel::TryRecvError::Closed) => return,
                    }
                }
                changed();
            }
        });

        Ok(FileWatcher {
            _watcher: watcher,
            _receiver: receiver,
        })
    }
}
