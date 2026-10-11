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
        // `Path::parent()` yields an empty path for a bare relative
        // filename; watching "" only works by accident of the inotify
        // backend treating it as the working directory, so say "." plainly.
        let dir: PathBuf = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
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

        // Runs on the main loop. After the first event, wait out
        // DEBOUNCE_MS, then drain everything that piled up during the wait
        // in one pass; only when a whole quiet window passes with nothing
        // new does the callback fire. Draining in bulk keeps an event storm
        // to one trailing window instead of one window per queued event.
        let receiver = glib::spawn_future_local(async move {
            loop {
                if rx.recv().await.is_err() {
                    return; // sender dropped: watcher is gone
                }
                loop {
                    glib::timeout_future(DEBOUNCE_MS).await;
                    if rx.is_closed() {
                        return;
                    }
                    let mut more = false;
                    while rx.try_recv().is_ok() {
                        more = true;
                    }
                    if !more {
                        break;
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
