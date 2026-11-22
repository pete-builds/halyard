//! Live reload: watch the config file and hand back the new configuration or the error.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

use notify::event::CreateKind;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::config::{Config, ConfigError, Loaded};

/// What a reload produced: the new configuration, or why it could not be loaded.
pub type ConfigEvent = Result<Loaded, ConfigError>;

/// Watches a config file and sends a [`ConfigEvent`] after each burst of changes settles.
///
/// The parent directory is watched, not the file, so editors that save by
/// replacing the file (write a temp file, rename over it) are noticed. If the
/// directory does not exist yet, the nearest existing ancestor is watched
/// recursively, so creating `~/.config/halyard/config.toml` for the first time
/// is noticed too. A deleted file reloads as the defaults.
///
/// Dropping the watcher stops it and its worker thread.
#[derive(Debug)]
pub struct ConfigWatcher {
    // Held so the OS watch stays alive; dropping it closes the raw channel, which ends the worker.
    _watcher: RecommendedWatcher,
    rx: Receiver<ConfigEvent>,
}

fn watch_root(path: &Path) -> (PathBuf, RecursiveMode) {
    let parent =
        path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    if parent.is_dir() {
        return (parent.to_path_buf(), RecursiveMode::NonRecursive);
    }
    let ancestor = parent.ancestors().find(|a| a.is_dir()).unwrap_or_else(|| Path::new("."));
    (ancestor.to_path_buf(), RecursiveMode::Recursive)
}

impl ConfigWatcher {
    /// Start watching `path`. Changes are debounced: the reload runs once no
    /// further change has arrived for `debounce`.
    ///
    /// # Errors
    /// Returns the underlying [`notify::Error`] when the OS watch cannot be set up.
    pub fn new(path: &Path, debounce: Duration) -> notify::Result<Self> {
        let file_name = path.file_name().map(std::ffi::OsStr::to_os_string);
        let target = path.to_path_buf();
        let (raw_tx, raw_rx) = mpsc::channel::<()>();
        let (out_tx, out_rx) = mpsc::channel::<ConfigEvent>();
        let err_tx: Sender<ConfigEvent> = out_tx.clone();
        let mut watcher =
            notify::recommended_watcher(move |res: notify::Result<Event>| match res {
                Ok(ev) => {
                    // Reading the file produces Access events on Linux; reacting to them would loop.
                    let relevant = matches!(
                        ev.kind,
                        EventKind::Create(_)
                            | EventKind::Modify(_)
                            | EventKind::Remove(_)
                            | EventKind::Any
                    );
                    // A newly created ancestor directory counts too: inotify registers a watch
                    // on it only after it exists, so a file written straight away can be missed.
                    let dir_created =
                        matches!(ev.kind, EventKind::Create(CreateKind::Folder | CreateKind::Any));
                    let ours = ev.paths.iter().any(|p| {
                        p.file_name() == file_name.as_deref()
                            || (dir_created && target.starts_with(p))
                    });
                    if relevant && ours {
                        // A send error means the worker already stopped; nothing to do.
                        let _ = raw_tx.send(());
                    }
                }
                Err(e) => {
                    let _ = err_tx.send(Err(ConfigError {
                        path: None,
                        key: None,
                        line: None,
                        message: format!("file watcher error: {e}"),
                    }));
                }
            })?;
        let (root, mode) = watch_root(path);
        watcher.watch(&root, mode)?;

        let owned: PathBuf = path.to_path_buf();
        thread::Builder::new().name("halyard-config-watch".into()).spawn(move || {
            while raw_rx.recv().is_ok() {
                loop {
                    match raw_rx.recv_timeout(debounce) {
                        Ok(()) => {}
                        Err(RecvTimeoutError::Timeout) => break,
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
                if out_tx.send(Config::load(&owned)).is_err() {
                    return;
                }
            }
        })?;
        Ok(Self { _watcher: watcher, rx: out_rx })
    }

    /// Wait up to `timeout` for the next reload result.
    #[must_use]
    pub fn recv_timeout(&self, timeout: Duration) -> Option<ConfigEvent> {
        self.rx.recv_timeout(timeout).ok()
    }

    /// The next reload result if one is ready.
    #[must_use]
    pub fn try_recv(&self) -> Option<ConfigEvent> {
        self.rx.try_recv().ok()
    }
}
