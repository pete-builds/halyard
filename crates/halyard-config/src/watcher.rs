//! Live reload: watch the config file and hand back the new configuration or the error.
//!
//! The source of truth is a fingerprint, polled on a dedicated thread about once a second.
//! File system notifications ([`notify`]) are only a hint that wakes the poll early, so a
//! missing directory, a failed watch, a symlink swap or a directory moved into place can delay
//! a reload by at most one poll interval but can never lose one.

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::config::{Config, ConfigError, ConfigWarning, Loaded, themes_dir_for};

/// What a reload produced: the new configuration, or why it could not be loaded.
pub type ConfigEvent = Result<Loaded, ConfigError>;

/// The longest a reload can be postponed by a constant stream of changes, counted from the
/// first change of a burst. Without it, a file that is rewritten every few milliseconds would
/// never reload.
pub const MAX_DEBOUNCE: Duration = Duration::from_millis(500);

/// How often the fingerprint is checked when no notification arrives.
pub const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// The state of one file: where its symlinks lead right now, and what is in it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum FileState {
    /// Absent, a dangling symlink, or not a regular file.
    Missing,
    /// Present but not readable; loading reports the I/O error.
    Unreadable,
    Present {
        /// The path after following every symlink, resolved fresh each time.
        resolved: PathBuf,
        size: u64,
        mtime: Option<SystemTime>,
        /// Hash of the content. Config files are tiny, so hashing every poll is cheap.
        hash: u64,
    },
}

fn file_state(path: &Path) -> FileState {
    let Ok(resolved) = fs::canonicalize(path) else { return FileState::Missing };
    let Ok(meta) = fs::metadata(&resolved) else { return FileState::Missing };
    if !meta.is_file() {
        return FileState::Missing;
    }
    let Ok(bytes) = fs::read(&resolved) else { return FileState::Unreadable };
    let mut h = DefaultHasher::new();
    bytes.hash(&mut h);
    FileState::Present { resolved, size: meta.len(), mtime: meta.modified().ok(), hash: h.finish() }
}

/// Everything a reload depends on: the config file, and the names and state of every
/// `*.toml` in the themes directory.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Fingerprint {
    config: FileState,
    themes: Vec<(String, FileState)>,
}

fn fingerprint(config: &Path) -> Fingerprint {
    let mut themes = Vec::new();
    if let Ok(rd) = fs::read_dir(themes_dir_for(config)) {
        for e in rd.filter_map(Result::ok) {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "toml") {
                themes.push((e.file_name().to_string_lossy().into_owned(), file_state(&p)));
            }
        }
    }
    themes.sort_by(|a, b| a.0.cmp(&b.0));
    Fingerprint { config: file_state(config), themes }
}

/// Make `path` absolute against `cwd` and drop `.` and `..` components lexically.
fn absolutize(cwd: &Path, path: &Path) -> PathBuf {
    let joined = if path.is_absolute() { path.to_path_buf() } else { cwd.join(path) };
    let mut out = PathBuf::new();
    for c in joined.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[derive(Debug)]
struct Signals {
    stop: bool,
    hint: bool,
}

/// Wakes the worker: a stop request, or a hint that something may have changed.
#[derive(Debug)]
struct Shared {
    signals: Mutex<Signals>,
    cv: Condvar,
}

enum Wake {
    Stop,
    Hint,
    Timeout,
}

impl Shared {
    fn hint(&self) {
        if let Ok(mut s) = self.signals.lock() {
            s.hint = true;
        }
        self.cv.notify_all();
    }

    fn stop(&self) {
        if let Ok(mut s) = self.signals.lock() {
            s.stop = true;
        }
        self.cv.notify_all();
    }

    /// Sleep up to `dur`, returning early for a stop or a hint (a hint is consumed).
    fn wait(&self, dur: Duration) -> Wake {
        let deadline = Instant::now() + dur;
        let Ok(mut g) = self.signals.lock() else { return Wake::Stop };
        loop {
            if g.stop {
                return Wake::Stop;
            }
            if g.hint {
                g.hint = false;
                return Wake::Hint;
            }
            let now = Instant::now();
            if now >= deadline {
                return Wake::Timeout;
            }
            match self.cv.wait_timeout(g, deadline - now) {
                Ok((ng, _)) => g = ng,
                Err(_) => return Wake::Stop,
            }
        }
    }
}

/// Watches a config file and sends a [`ConfigEvent`] after each change settles.
///
/// A change is a different fingerprint: for `config.toml` the path it resolves to after
/// following every symlink (fresh each time), its size, modification time and a hash of its
/// content; for the `themes/` folder next to it the sorted names of the `*.toml` files with
/// the same data for each. So a symlink swapped by rename or by unlink and link, an edit made
/// through a symlink, a directory moved into place or away, and a file that appears, vanishes
/// or is recreated are all noticed, and unrelated files in the same directories are not. A missing file is
/// a state like any other: the event holds the defaults and a warning saying so.
///
/// Notifications from the OS only wake the check early, on the config's directory and
/// `themes/` where they exist. If they cannot be set up, or the directories do not exist yet,
/// the poll (every [`POLL_INTERVAL`]) still finds every change.
///
/// After a change is first seen, the reload waits until nothing has changed for `debounce`, but
/// never longer than [`MAX_DEBOUNCE`] in total. A relative path is made absolute against the
/// working directory when the watcher is created.
///
/// Dropping the watcher stops its thread and waits for it to finish.
#[derive(Debug)]
pub struct ConfigWatcher {
    shared: Arc<Shared>,
    rx: Receiver<ConfigEvent>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for ConfigWatcher {
    fn drop(&mut self) {
        self.shared.stop();
        if let Some(h) = self.handle.take() {
            // A panic in the worker has nothing left to clean up here.
            let _ = h.join();
        }
    }
}

impl ConfigWatcher {
    /// Start watching `path`, polling every [`POLL_INTERVAL`].
    ///
    /// # Errors
    /// Returns an error when the working directory or the worker thread cannot be set up.
    /// A failure to register OS notifications is not an error: polling still works.
    pub fn new(path: &Path, debounce: Duration) -> notify::Result<Self> {
        Self::start(path, debounce, POLL_INTERVAL, true)
    }

    /// Like [`ConfigWatcher::new`] with a chosen poll interval.
    ///
    /// # Errors
    /// Same as [`ConfigWatcher::new`].
    pub fn with_poll_interval(
        path: &Path,
        debounce: Duration,
        poll: Duration,
    ) -> notify::Result<Self> {
        Self::start(path, debounce, poll, true)
    }

    /// Poll only, with no OS notifications at all. This is the fallback path that must work
    /// alone, exposed so a test can prove it does.
    ///
    /// # Errors
    /// Same as [`ConfigWatcher::new`].
    #[doc(hidden)]
    pub fn poll_only(path: &Path, debounce: Duration, poll: Duration) -> notify::Result<Self> {
        Self::start(path, debounce, poll, false)
    }

    fn start(
        path: &Path,
        debounce: Duration,
        poll: Duration,
        use_notify: bool,
    ) -> notify::Result<Self> {
        let path = absolutize(&std::env::current_dir().map_err(notify::Error::io)?, path);
        let shared = Arc::new(Shared {
            signals: Mutex::new(Signals { stop: false, hint: false }),
            cv: Condvar::new(),
        });
        // The baseline is taken before this returns, so an edit made right after `new` is
        // a change and not part of the starting state.
        let baseline = fingerprint(&path);
        let (out_tx, out_rx) = mpsc::channel::<ConfigEvent>();
        let worker_shared = Arc::clone(&shared);
        let handle =
            thread::Builder::new().name("halyard-config-watch".into()).spawn(move || {
                run(&path, baseline, debounce, poll, use_notify, &worker_shared, &out_tx);
            })?;
        Ok(Self { shared, rx: out_rx, handle: Some(handle) })
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

/// OS notification handle plus the directories it currently watches. Owned by the worker, so
/// it is dropped when the worker ends.
struct Hints {
    watcher: Option<RecommendedWatcher>,
    watched: Vec<PathBuf>,
}

impl Hints {
    fn new(use_notify: bool, shared: &Arc<Shared>) -> Self {
        let watcher = use_notify
            .then(|| {
                let shared = Arc::clone(shared);
                notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                    // Reading the file produces Access events on Linux; reacting to them
                    // would make every check wake the next one.
                    if res.is_ok_and(|ev| !matches!(ev.kind, EventKind::Access(_))) {
                        shared.hint();
                    }
                })
                .ok()
            })
            .flatten();
        Self { watcher, watched: Vec::new() }
    }

    /// Best effort: watch the config's directory, where its symlink leads, and `themes/`,
    /// each only where it exists. `force` drops and re-adds everything, which revives a
    /// watch whose directory was deleted and recreated.
    fn sync(&mut self, config: &Path, force: bool) {
        let Some(w) = self.watcher.as_mut() else { return };
        let mut want: Vec<PathBuf> = Vec::new();
        let mut add = |d: Option<&Path>| {
            if let Some(d) = d.filter(|d| d.is_dir()) {
                let d = d.to_path_buf();
                if !want.contains(&d) {
                    want.push(d);
                }
            }
        };
        add(config.parent());
        add(fs::canonicalize(config).ok().as_deref().and_then(Path::parent));
        add(Some(&themes_dir_for(config)));
        if force {
            for d in self.watched.drain(..) {
                let _ = w.unwatch(&d);
            }
        } else {
            self.watched.retain(|d| {
                let keep = want.contains(d);
                if !keep {
                    // The directory may be gone already; there is nothing to undo then.
                    let _ = w.unwatch(d);
                }
                keep
            });
        }
        for d in want {
            if !self.watched.contains(&d) && w.watch(&d, RecursiveMode::NonRecursive).is_ok() {
                self.watched.push(d);
            }
        }
    }
}

fn run(
    path: &Path,
    baseline: Fingerprint,
    debounce: Duration,
    poll: Duration,
    use_notify: bool,
    shared: &Arc<Shared>,
    out: &mpsc::Sender<ConfigEvent>,
) {
    let mut hints = Hints::new(use_notify, shared);
    hints.sync(path, true);
    let mut last = baseline;
    loop {
        match shared.wait(poll) {
            Wake::Stop => return,
            Wake::Hint => {}
            Wake::Timeout => {
                hints.sync(path, false);
                if fingerprint(path) == last {
                    continue;
                }
            }
        }
        // Something may have changed: wait for it to settle, but not past the cap.
        let started = Instant::now();
        loop {
            let left = MAX_DEBOUNCE.saturating_sub(started.elapsed());
            if left.is_zero() {
                break;
            }
            match shared.wait(debounce.min(left)) {
                Wake::Stop => return,
                Wake::Hint => {}
                Wake::Timeout => break,
            }
        }
        let now = fingerprint(path);
        if now == last {
            continue;
        }
        hints.sync(path, true);
        let mut event = Config::load(path);
        if now.config == FileState::Missing
            && let Ok(loaded) = &mut event
        {
            loaded.warnings.push(ConfigWarning {
                key: "config".into(),
                line: None,
                message: "the config file does not exist; using the defaults".into(),
            });
        }
        last = now;
        if out.send(event).is_err() {
            return;
        }
    }
}
