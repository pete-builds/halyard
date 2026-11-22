//! Live reload: watch the config file and hand back the new configuration or the error.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use notify::event::CreateKind;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::config::{Config, ConfigError, Loaded, themes_dir_for};

/// What a reload produced: the new configuration, or why it could not be loaded.
pub type ConfigEvent = Result<Loaded, ConfigError>;

/// The longest a reload can be postponed by a constant stream of events, counted from
/// the first event of a burst. Without it, a file that is rewritten every few
/// milliseconds would never reload.
pub const MAX_DEBOUNCE: Duration = Duration::from_millis(500);

/// Watches a config file and sends a [`ConfigEvent`] after each burst of changes settles.
///
/// Only events for the exact config path, or for the `themes/` directory next to it, start
/// a reload; a `config.toml` somewhere else under a watched ancestor does not.
///
/// The parent directory is watched, not the file, so editors that save by
/// replacing the file (write a temp file, rename over it) are noticed. If the
/// directory does not exist yet, the nearest existing ancestor is watched
/// non-recursively, and the watch moves one level deeper each time the next
/// directory on the way to the file appears. (notify's non-recursive mode reports
/// only direct children, so a recursive watch of something like `$HOME` is never needed.)
/// A deleted file reloads as the defaults.
///
/// After the first event of a burst the reload waits until no further event has
/// arrived for `debounce`, but never longer than [`MAX_DEBOUNCE`] in total.
///
/// Dropping the watcher stops it and its worker thread.
#[derive(Debug)]
pub struct ConfigWatcher {
    // Held so the OS watch stays alive; dropping it closes the raw channel, which ends the worker.
    _watcher: Arc<Mutex<RecommendedWatcher>>,
    rx: Receiver<ConfigEvent>,
}

/// What the notify callback tells the worker thread.
enum Raw {
    /// The config file or themes directory changed.
    Changed,
    /// A directory on the way to the config file appeared: move the watch deeper.
    Rewatch,
}

/// Canonicalize the longest existing prefix of `path` and re-append the rest, so a path
/// that does not exist yet still compares equal to the real paths file systems report
/// (`/var` against `/private/var` on macOS).
fn canon_lenient(path: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut cur = path;
    loop {
        if let Ok(c) = cur.canonicalize() {
            return rest.iter().rev().fold(c, |acc, part| acc.join(part));
        }
        match (cur.parent(), cur.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                cur = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// The directories to watch for `path`, all non-recursive: the config's parent (or the
/// nearest existing ancestor of it) and the themes directory when it exists.
fn watch_roots(path: &Path) -> Vec<PathBuf> {
    let parent =
        path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    let root = parent.ancestors().find(|a| a.is_dir()).unwrap_or_else(|| Path::new("."));
    let mut roots = vec![root.to_path_buf()];
    let themes = themes_dir_for(path);
    if themes.is_dir() {
        roots.push(themes);
    }
    roots
}

fn rewatch(watcher: &Mutex<RecommendedWatcher>, old: &mut Vec<PathBuf>, path: &Path) {
    let Ok(mut w) = watcher.lock() else { return };
    let new = watch_roots(path);
    for r in old.iter().filter(|r| !new.contains(r)) {
        // The directory may be gone already; there is nothing to undo then.
        let _ = w.unwatch(r);
    }
    for r in new.iter().filter(|r| !old.contains(r)) {
        // Best effort: a directory that vanished again is picked up by the next event.
        let _ = w.watch(r, RecursiveMode::NonRecursive);
    }
    *old = new;
}

impl ConfigWatcher {
    /// Start watching `path`. Changes are debounced: the reload runs once no
    /// further change has arrived for `debounce`, or [`MAX_DEBOUNCE`] after the
    /// first change of a burst, whichever comes first.
    ///
    /// # Errors
    /// Returns the underlying [`notify::Error`] when the OS watch cannot be set up.
    pub fn new(path: &Path, debounce: Duration) -> notify::Result<Self> {
        let target = canon_lenient(path);
        let themes = canon_lenient(&themes_dir_for(path));
        let (raw_tx, raw_rx) = mpsc::channel::<Raw>();
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
                    if !relevant {
                        return;
                    }
                    // A newly created directory on the way to the file: inotify registers a
                    // watch on it only after it exists, so move the watch there.
                    let dir_created =
                        matches!(ev.kind, EventKind::Create(CreateKind::Folder | CreateKind::Any));
                    let mut msg = None;
                    for p in &ev.paths {
                        let p = canon_lenient(p);
                        if p == target || p == themes || p.starts_with(&themes) {
                            msg = Some(Raw::Changed);
                            break;
                        }
                        if dir_created && target.starts_with(&p) {
                            msg = Some(Raw::Rewatch);
                        }
                    }
                    if let Some(m) = msg {
                        // A send error means the worker already stopped; nothing to do.
                        let _ = raw_tx.send(m);
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
        let mut roots = watch_roots(path);
        for r in &roots {
            watcher.watch(r, RecursiveMode::NonRecursive)?;
        }
        let watcher = Arc::new(Mutex::new(watcher));

        let owned: PathBuf = path.to_path_buf();
        let shared = Arc::clone(&watcher);
        thread::Builder::new().name("halyard-config-watch".into()).spawn(move || {
            while let Ok(first) = raw_rx.recv() {
                let mut reload = matches!(first, Raw::Changed);
                if matches!(first, Raw::Rewatch) {
                    rewatch(&shared, &mut roots, &owned);
                    // The file may have been written before the new watch existed.
                    reload = owned.exists();
                    if !reload {
                        continue;
                    }
                }
                let started = Instant::now();
                loop {
                    let left = MAX_DEBOUNCE.saturating_sub(started.elapsed());
                    if left.is_zero() {
                        break;
                    }
                    match raw_rx.recv_timeout(debounce.min(left)) {
                        Ok(Raw::Changed) => {}
                        Ok(Raw::Rewatch) => rewatch(&shared, &mut roots, &owned),
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
