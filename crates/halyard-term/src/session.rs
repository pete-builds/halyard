//! A program running in a PTY, with its VT engine on a dedicated thread.
//!
//! Threads per session:
//!
//! - **reader**: blocking reads from the PTY master, forwarding chunks over a
//!   bounded channel (backpressure: a flood of output cannot grow memory
//!   without bound).
//! - **session**: owns the [`Engine`] (which is `!Send`), consumes output and
//!   commands, and publishes [`Frame`]s at most once per [`FRAME_INTERVAL`].
//! - **writer**: blocking writes to the PTY master, so a program that is not
//!   reading its input (a large paste into `sleep`) never stalls rendering.
//! - **waiter**: blocks on the child process and reports its exit status.
//!
//! The [`Session`] handle is `Send + Sync`; the UI talks to it from any thread.

use std::ffi::{CStr, OsStr, OsString};
use std::fmt;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, bounded, never, select, unbounded};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::engine::{Engine, EngineError};
use crate::frame::Frame;
use crate::input::{KeyInput, MouseInput, ScrollRequest};
use crate::palette::Palette;

/// Minimum time between two published frames while output is streaming.
pub const FRAME_INTERVAL: Duration = Duration::from_millis(8);

/// How long to keep reading output after the child exits, so its last words
/// reach the screen before [`SessionEvent::Exited`] is reported.
const EXIT_DRAIN: Duration = Duration::from_millis(200);

/// A callback the session calls (from its own thread) after queueing an
/// event, so the UI can wake up and drain [`Session::events`].
pub type NotifyFn = Arc<dyn Fn() + Send + Sync>;

/// A tap that sees every raw PTY output chunk before the engine parses it.
/// It runs on the session thread and must return quickly.
pub type OutputTap = Box<dyn FnMut(&[u8]) + Send>;

/// Sees raw PTY output on the session thread and learns where the cursor was at
/// chosen points in it, for shell integration marks that need the row they
/// occurred on (see `halyard_shell::scanner`, "Locating events").
///
/// For each chunk the session calls [`OutputObserver::scan`] first. When the
/// observer asks for no marks the chunk goes to the engine in one piece, so an
/// observer that finds nothing costs one scan per chunk. Otherwise the engine is
/// fed up to each mark, [`OutputObserver::at_mark`] gets the cursor's absolute
/// row there, and [`OutputObserver::chunk_done`] runs after the rest of the chunk.
/// Every call runs on the session thread and must return quickly.
pub trait OutputObserver: Send {
    /// Looks at a chunk before the engine does. Pushes onto `marks` the offsets
    /// (ascending, each at most `chunk.len()`) after which the cursor row is
    /// wanted.
    fn scan(&mut self, chunk: &[u8], marks: &mut Vec<usize>);
    /// The engine has consumed the chunk up to mark number `index` (in the order
    /// pushed by `scan`); `row` is the cursor's row counted from the top of
    /// scrollback (see [`Engine::cursor_screen_row`]) plus the rows dropped from
    /// the top of scrollback so far ([`History::dropped`]), so it keeps naming
    /// the same line as old rows are dropped. Subtract `dropped` for a row of
    /// [`Session::screen_rows_text`] or [`crate::ScrollRequest::Row`].
    fn at_mark(&mut self, index: usize, row: u64);
    /// The whole chunk has been consumed. `history` describes the primary
    /// screen's scrollback now (`None` while the alternate screen is shown).
    fn chunk_done(&mut self, history: Option<History>);
}

/// The primary screen's scrollback after a chunk, for an [`OutputObserver`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct History {
    /// Scrollback rows now. When this shrinks, scrollback was cleared (`CSI 3 J`,
    /// as `clear` sends, or a reset) and rows recorded before point at other text.
    pub rows: u64,
    /// Rows dropped from the top of scrollback since the session started.
    pub dropped: u64,
    /// True when, since the last chunk, more rows were dropped than could be
    /// counted (more than the scrollback holds went by): `dropped` is then too
    /// small and every row recorded before is unreliable.
    pub lost: bool,
}

/// How to start a session.
pub struct SessionConfig {
    /// Program to run. `None` runs the user's login shell (see
    /// [`default_shell`]) as a login shell.
    pub program: Option<PathBuf>,
    /// Arguments for `program`. Ignored when `program` is `None`.
    pub args: Vec<String>,
    /// Working directory. `None` uses `$HOME`.
    pub cwd: Option<PathBuf>,
    /// Extra environment variables, applied after Halyard's own.
    pub env: Vec<(String, String)>,
    /// Environment variables to remove from the inherited environment.
    pub env_remove: Vec<String>,
    /// Initial width in cells.
    pub cols: u16,
    /// Initial height in cells.
    pub rows: u16,
    /// Cell width in pixels.
    pub cell_width: u32,
    /// Cell height in pixels.
    pub cell_height: u32,
    /// Lines of scrollback to keep (at least; see [`crate::engine::scrollback_bytes`]).
    pub scrollback_lines: usize,
    /// Colors.
    pub palette: Palette,
    /// Treat the macOS Option key as Alt (see [`Engine::set_option_as_alt`]).
    pub option_as_alt: bool,
    /// Called after each event is queued.
    pub notify: Option<NotifyFn>,
    /// Sees raw output before the engine (for shell-integration scanners).
    pub output_tap: Option<OutputTap>,
    /// Sees raw output and the cursor row at the points it asks for.
    pub observer: Option<Box<dyn OutputObserver>>,
    /// The `HALYARD_SESSION_ID` to give the program, when the caller needs to
    /// know it before the session starts (see [`new_session_id`]). Must be 1 to
    /// 128 characters from `A-Z a-z 0-9 . _ -`; anything else is replaced by a
    /// fresh id. `None` makes a fresh id.
    pub session_id: Option<String>,
    /// A directory to add to the end of the program's `PATH`, so helpers
    /// shipped beside Halyard, such as `halyard-msg`, resolve in the pane
    /// without shadowing anything the user installed. Nothing is added when
    /// `PATH` is unset or empty, or already holds the directory. Only for a
    /// program Halyard runs itself: a program started on the host through
    /// Flatpak never gets it, because the directory is a sandbox path the
    /// host cannot see.
    pub path_append: Option<PathBuf>,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            program: None,
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            env_remove: Vec::new(),
            cols: 80,
            rows: 24,
            cell_width: 8,
            cell_height: 16,
            scrollback_lines: 10_000,
            palette: Palette::default(),
            option_as_alt: false,
            notify: None,
            output_tap: None,
            observer: None,
            session_id: None,
            path_append: None,
        }
    }
}

impl fmt::Debug for SessionConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionConfig")
            .field("program", &self.program)
            .field("args", &self.args)
            .field("cwd", &self.cwd)
            .field("env", &self.env)
            .field("env_remove", &self.env_remove)
            .field("cols", &self.cols)
            .field("rows", &self.rows)
            .field("cell_width", &self.cell_width)
            .field("cell_height", &self.cell_height)
            .field("scrollback_lines", &self.scrollback_lines)
            .field("palette", &self.palette)
            .field("option_as_alt", &self.option_as_alt)
            .field("notify", &self.notify.is_some())
            .field("output_tap", &self.output_tap.is_some())
            .field("observer", &self.observer.is_some())
            .field("session_id", &self.session_id)
            .field("path_append", &self.path_append)
            .finish()
    }
}

/// Something the UI may want to react to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionEvent {
    /// A new frame is available from [`Session::latest_frame`].
    FrameReady,
    /// The program set the window title.
    TitleChanged(String),
    /// The program rang the bell.
    Bell,
    /// The program exited. `Some(code)` for a normal exit, `None` when it was
    /// killed by a signal or its status could not be read.
    Exited(Option<i32>),
}

/// Failure to start a session.
#[derive(Debug)]
pub enum SessionError {
    /// Opening the PTY or spawning the program failed.
    Spawn(String),
    /// The VT engine could not be created.
    Engine(EngineError),
    /// A thread could not be started.
    Thread(std::io::Error),
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(e) => write!(f, "could not start the program: {e}"),
            Self::Engine(e) => write!(f, "{e}"),
            Self::Thread(e) => write!(f, "could not start a session thread: {e}"),
        }
    }
}

impl std::error::Error for SessionError {}

enum Command {
    Write(Vec<u8>),
    Key(KeyInput),
    Mouse(MouseInput),
    Paste(String),
    Resize { cols: u16, rows: u16, cell_width: u32, cell_height: u32 },
    Scroll(ScrollRequest),
    SelectStart(u16, u16),
    SelectExtend(u16, u16),
    SelectWord(u16, u16),
    SelectClear,
    SelectionText(Sender<Option<String>>),
    RowsText(u64, u64, Sender<Option<String>>),
    SetPalette(Palette),
    Shutdown,
}

/// A running terminal session. Dropping it terminates the program.
pub struct Session {
    id: String,
    pid: Option<u32>,
    commands: Sender<Command>,
    events: Receiver<SessionEvent>,
    frame: Arc<Mutex<Arc<Frame>>>,
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session").field("id", &self.id).field("pid", &self.pid).finish()
    }
}

/// The user's login shell: `$SHELL` if it names an executable, else the
/// shell in the password database, else `/bin/sh`.
#[must_use]
pub fn default_shell() -> PathBuf {
    if let Some(shell) = std::env::var_os("SHELL").map(PathBuf::from)
        && is_executable(&shell)
    {
        return shell;
    }
    if let Some(shell) = passwd_shell()
        && is_executable(&shell)
    {
        return shell;
    }
    PathBuf::from("/bin/sh")
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.is_absolute()
        && std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

fn passwd_shell() -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    let mut buf: Vec<libc::c_char> = vec![0; 4096];
    loop {
        // SAFETY: passwd is plain old data; an all-zero value is valid.
        let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: every pointer refers to live, writable storage of the
        // stated size, and getpwuid_r is the reentrant variant.
        let rc = unsafe {
            libc::getpwuid_r(uid, &raw mut pwd, buf.as_mut_ptr(), buf.len(), &raw mut result)
        };
        if rc == libc::ERANGE && buf.len() < (1 << 20) {
            buf.resize(buf.len() * 2, 0);
            continue;
        }
        if rc != 0 || result.is_null() || pwd.pw_shell.is_null() {
            return None;
        }
        // SAFETY: on success pw_shell points to a NUL-terminated string
        // inside `buf`, which is still alive.
        let shell = unsafe { CStr::from_ptr(pwd.pw_shell) };
        let bytes = shell.to_bytes();
        return (!bytes.is_empty()).then(|| PathBuf::from(std::ffi::OsStr::from_bytes(bytes)));
    }
}

/// A fresh pane id for `HALYARD_SESSION_ID`: 32 lowercase hex characters.
#[must_use]
pub fn new_session_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// True if `id` is a valid `HALYARD_SESSION_ID` (`docs/protocol.md`).
fn valid_session_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Inherited variables that describe the terminal Halyard was launched from
/// (tmux, iTerm2, Ghostty, kitty, WezTerm, VTE, Windows Terminal, Alacritty)
/// or a fixed size. Left in place they make programs believe they run there.
const SCRUBBED_VARS: &[&str] = &[
    "TMUX",
    "TMUX_PANE",
    "STY",
    "ZELLIJ",
    "ZELLIJ_SESSION_NAME",
    "ZELLIJ_PANE_ID",
    "TERM_FEATURES",
    "TERMINAL_EMULATOR",
    "ITERM_SESSION_ID",
    "ITERM_PROFILE",
    "LC_TERMINAL",
    "LC_TERMINAL_VERSION",
    "TERM_SESSION_ID",
    "COLUMNS",
    "LINES",
    "VTE_VERSION",
    "WT_SESSION",
];

/// Prefixes of inherited variables to remove, as for [`SCRUBBED_VARS`].
const SCRUBBED_PREFIXES: &[&str] = &["GHOSTTY_", "KITTY_", "WEZTERM_", "ALACRITTY_", "KONSOLE_"];

/// True if an inherited variable belongs to another terminal and must not
/// reach programs run by Halyard.
fn is_foreign_terminal_var(name: &str) -> bool {
    SCRUBBED_VARS.contains(&name) || SCRUBBED_PREFIXES.iter().any(|p| name.starts_with(p))
}

/// Which signals closing a session sends.
///
/// Once the child has exited and been reaped, its pid and process group id can be reused by
/// unrelated processes, so a pane left on "process exited" and closed later must not signal
/// anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ShutdownPlan {
    /// SIGHUP to the PTY's foreground process group.
    hangup_group: bool,
    /// SIGHUP to the child (the shell).
    hangup_child: bool,
    /// SIGKILL to the child if it is still running after the grace period.
    force_kill_after_grace: bool,
}

impl ShutdownPlan {
    /// The plan when a session closes. `child_running` is the run loop's view; `exit_pending`
    /// is true when the waiter has already reaped the child and queued its exit status, which
    /// the run loop has not received yet. Either way a reaped child gets no signal.
    const fn at_close(child_running: bool, exit_pending: bool) -> Self {
        let signal = child_running && !exit_pending;
        Self { hangup_group: signal, hangup_child: signal, force_kill_after_grace: signal }
    }
}

/// Where a session's program runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Placement {
    /// In Halyard's own environment (every normal install).
    Local,
    /// On the host, through `flatpak-spawn --host`, because Halyard itself
    /// runs inside a Flatpak sandbox (see [`crate::flatpak`]).
    FlatpakHost,
}

impl Placement {
    fn detect() -> Self {
        if crate::flatpak::in_flatpak() { Self::FlatpakHost } else { Self::Local }
    }
}

/// The variables Halyard sets for every program, in the order they apply:
/// Halyard's own first, then the session's extras, which may override them.
fn session_env(config: &SessionConfig, session_id: &str) -> Vec<(OsString, OsString)> {
    let own = [
        ("TERM", "xterm-256color"),
        ("COLORTERM", "truecolor"),
        ("TERM_PROGRAM", "Halyard"),
        ("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION")),
        ("HALYARD_SESSION_ID", session_id),
    ];
    own.iter()
        .map(|(k, v)| (OsString::from(k), OsString::from(v)))
        .chain(config.env.iter().map(|(k, v)| (OsString::from(k), OsString::from(v))))
        .collect()
}

/// A command ready to spawn, plus the environment file it reads when it runs
/// on the host through Flatpak (see [`crate::flatpak::EnvFile`]).
struct Built {
    cmd: CommandBuilder,
    env_file: Option<crate::flatpak::EnvFile>,
}

fn build_command(config: &SessionConfig, session_id: &str) -> std::io::Result<Built> {
    build_command_for(config, session_id, Placement::detect())
}

fn build_command_for(
    config: &SessionConfig,
    session_id: &str,
    placement: Placement,
) -> std::io::Result<Built> {
    let mut env = session_env(config, session_id);
    if placement == Placement::FlatpakHost {
        // The same UTF-8 locale default as a local program gets, judged on the
        // variables the program will see: the session's, else the sandbox's
        // (inherited from the desktop session) unless the session removes them.
        let lookup = |k: &str| {
            env.iter().rev().find(|(name, _)| name == k).map(|(_, v)| v.clone()).or_else(|| {
                if config.env_remove.iter().any(|r| r == k) { None } else { std::env::var_os(k) }
            })
        };
        if let Some(lang) = default_locale(lookup, cfg!(target_os = "macos")) {
            env.push(("LANG".into(), lang.into()));
        }
        let home = std::env::var_os("HOME");
        let shell = std::env::var_os("SHELL");
        let env_path = crate::flatpak::EnvFile::path_in(&crate::flatpak::env_file_dir());
        let launch = crate::flatpak::host_launch(&crate::flatpak::HostSpawn {
            program: config.program.as_deref(),
            args: &config.args,
            cwd: config.cwd.as_deref(),
            home: home.as_deref(),
            shell: shell.as_deref(),
            env: &env,
            unset: &config.env_remove,
            env_file: &env_path,
        });
        let env_file = crate::flatpak::EnvFile::create(env_path, &launch.env_block)?;
        // The wrapper and flatpak-spawn run in the sandbox, in portable-pty's
        // default directory ($HOME); the host program gets the session's
        // directory through --directory, which may not exist inside the
        // sandbox. Their own environment is the sandbox's: the host program's
        // comes only from the env file.
        let mut cmd = CommandBuilder::new(launch.program);
        cmd.args(launch.args);
        return Ok(Built { cmd, env_file: Some(env_file) });
    }
    let mut cmd = if let Some(program) = &config.program {
        let mut cmd = CommandBuilder::new(program);
        cmd.args(&config.args);
        cmd
    } else {
        // portable-pty runs the builder's $SHELL as a login shell
        // (argv[0] = "-zsh") when no program is given.
        let mut cmd = CommandBuilder::new_default_prog();
        cmd.env("SHELL", default_shell());
        cmd
    };
    for (key, _) in std::env::vars_os() {
        if key.to_str().is_some_and(is_foreign_terminal_var) {
            cmd.env_remove(key);
        }
    }
    for key in &config.env_remove {
        cmd.env_remove(key);
    }
    for (k, v) in &env {
        cmd.env(k, v);
    }
    if let Some(dir) = &config.path_append
        && let Some(path) = path_with_appended(cmd.get_env("PATH"), dir)
    {
        cmd.env("PATH", path);
    }
    if let Some(cwd) = &config.cwd {
        cmd.cwd(cwd);
    } else if let Some(home) = cmd.get_env("HOME").filter(|h| !Path::new(h).is_dir()) {
        // Without a cwd, portable-pty starts the child in $HOME and the spawn fails
        // outright when that directory does not exist (containers, service accounts,
        // build sandboxes). A shell in "/" is better than no shell.
        // Debug formatting escapes control characters in a hostile HOME.
        eprintln!("halyard-term: HOME {home:?} is not a directory; starting in /");
        cmd.cwd("/");
    }
    if let Some(lang) =
        default_locale(|k| cmd.get_env(k).map(OsString::from), cfg!(target_os = "macos"))
    {
        cmd.env("LANG", lang);
    }
    Ok(Built { cmd, env_file: None })
}

/// `path` with `dir` added as its last entry, or `None` to leave `PATH` as it is.
///
/// Nothing changes when `PATH` is unset or empty (setting it would replace the
/// system's default search path with one directory), when `dir` is not absolute,
/// when it is already an entry, or when it cannot be written into `PATH` (it
/// contains a `:`). Last, not first, so it never shadows a program of the same
/// name the user installed, and macOS's `path_helper`, which login shells run,
/// keeps entries it does not know at the end.
#[must_use]
fn path_with_appended(path: Option<&OsStr>, dir: &Path) -> Option<OsString> {
    let path = path.filter(|p| !p.is_empty())?;
    if !dir.is_absolute() || std::env::split_paths(path).any(|entry| entry == dir) {
        return None;
    }
    let mut entries: Vec<PathBuf> = std::env::split_paths(path).collect();
    entries.push(dir.to_path_buf());
    std::env::join_paths(entries).ok()
}

/// The `LANG` to add when the child's environment names no locale at all.
///
/// Apps started from the macOS Dock or Finder, and many service and container
/// environments, have no `LANG`, `LC_CTYPE` or `LC_ALL`. Shells then run in the C
/// locale and mishandle UTF-8 (zsh line editing, paths with non-ASCII names).
/// Terminal.app and iTerm2 fill in a UTF-8 locale the same way. A locale the user
/// set anywhere (even an empty-valued one is treated as unset) is never replaced.
fn default_locale(env: impl Fn(&str) -> Option<OsString>, macos: bool) -> Option<&'static str> {
    let set = |k: &str| env(k).is_some_and(|v| !v.is_empty());
    if set("LC_ALL") || set("LC_CTYPE") || set("LANG") {
        return None;
    }
    // macOS has no C.UTF-8; glibc 2.35+ and musl have it built in.
    Some(if macos { "en_US.UTF-8" } else { "C.UTF-8" })
}

fn pty_size(cols: u16, rows: u16, cell_width: u32, cell_height: u32) -> PtySize {
    let px = |cells: u16, cell: u32| {
        u16::try_from(u32::from(cells).saturating_mul(cell)).unwrap_or(u16::MAX)
    };
    PtySize {
        rows: rows.max(1),
        cols: cols.max(1),
        pixel_width: px(cols, cell_width),
        pixel_height: px(rows, cell_height),
    }
}

fn spawn_thread(
    name: &str,
    f: impl FnOnce() + Send + 'static,
) -> Result<thread::JoinHandle<()>, SessionError> {
    thread::Builder::new().name(name.to_owned()).spawn(f).map_err(SessionError::Thread)
}

impl Session {
    /// Start `config.program` (or the login shell) in a new PTY.
    pub fn spawn(mut config: SessionConfig) -> Result<Self, SessionError> {
        let id = config
            .session_id
            .take()
            .filter(|id| valid_session_id(id))
            .unwrap_or_else(new_session_id);
        let spawn_err = |e: &dyn fmt::Display| SessionError::Spawn(format!("{e:#}"));
        let size = pty_size(config.cols, config.rows, config.cell_width, config.cell_height);
        let pair = native_pty_system().openpty(size).map_err(|e| spawn_err(&e))?;
        // Everything that can fail without side effects happens before the
        // child starts.
        let writer = pair.master.take_writer().map_err(|e| spawn_err(&e))?;
        let master_fd =
            pair.master.as_raw_fd().ok_or_else(|| SessionError::Spawn("PTY has no fd".into()))?;
        let reader = dup_cloexec(master_fd).map_err(|e| spawn_err(&e))?;
        let (wake_read, wake_write) = wake_pipe().map_err(|e| spawn_err(&e))?;
        let Built { cmd, env_file } = build_command(&config, &id).map_err(|e| spawn_err(&e))?;
        // On failure the env file (if any) is dropped and deleted here.
        let mut child = pair.slave.spawn_command(cmd).map_err(|e| spawn_err(&e))?;
        // The wrapper in the child deletes it now; see `flatpak::EnvFile`.
        if let Some(env_file) = env_file {
            env_file.handed_over();
        }
        // The parent must not hold the slave open, or the reader never sees
        // end-of-file when the program exits.
        drop(pair.slave);
        let pid = child.process_id();
        // From here on, a failure must not leave the program running.
        let mut guard = KillOnDrop { pid };
        let killer = child.clone_killer();

        let (out_tx, out_rx) = bounded::<Vec<u8>>(32);
        let (cmd_tx, cmd_rx) = unbounded::<Command>();
        let (write_tx, write_rx) = unbounded::<Vec<u8>>();
        let (exit_tx, exit_rx) = bounded::<Option<i32>>(1);
        let (event_tx, event_rx) = unbounded::<SessionEvent>();
        let frame = Arc::new(Mutex::new(Arc::new(Frame::default())));
        let (ready_tx, ready_rx) = bounded::<Result<(), EngineError>>(1);

        spawn_thread("halyard-pty-reader", move || read_loop(reader, &wake_read, &out_tx))?;
        spawn_thread("halyard-pty-writer", move || write_loop(writer, &write_rx))?;
        spawn_thread("halyard-child-wait", move || {
            let code = match child.wait() {
                Ok(status) if status.signal().is_none() => i32::try_from(status.exit_code()).ok(),
                Ok(_) => None,
                Err(e) => {
                    eprintln!("halyard-term: waiting for the child failed: {e}");
                    None
                }
            };
            // The session may already be gone; nothing to report then.
            exit_tx.send(code).ok();
        })?;

        let mut shared = SessionThread {
            master: pair.master,
            killer,
            pid,
            wake: Some(wake_write),
            out_rx,
            cmd_rx,
            write_tx,
            exit_rx,
            event_tx,
            frame: Arc::clone(&frame),
            notify: config.notify.take(),
            tap: config.output_tap.take(),
            observer: config.observer.take(),
            marks: Vec::new(),
        };
        let (cols, rows, cw, ch) =
            (config.cols, config.rows, config.cell_width, config.cell_height);
        let (scrollback, palette) = (config.scrollback_lines, config.palette.clone());
        let option_as_alt = config.option_as_alt;
        spawn_thread("halyard-session", move || {
            let engine = Engine::new(cols, rows, scrollback, &palette)
                .and_then(|mut e| e.resize(cols, rows, cw, ch).map(|()| e))
                .map(|mut e| {
                    e.set_option_as_alt(option_as_alt);
                    e
                });
            match engine {
                Ok(engine) => {
                    // The receiver waits for this message; if it is gone the
                    // spawn call already failed and the loop exits on its own.
                    ready_tx.send(Ok(())).ok();
                    shared.run(engine);
                }
                Err(e) => {
                    ready_tx.send(Err(e)).ok();
                    shared.shutdown(true);
                }
            }
        })?;
        let ready = ready_rx.recv();
        // The session thread owns the child now and stops it on any failure.
        guard.pid = None;
        match ready {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(SessionError::Engine(e)),
            Err(_) => {
                return Err(SessionError::Spawn(
                    "the session thread stopped during startup".into(),
                ));
            }
        }
        Ok(Self { id, pid, commands: cmd_tx, events: event_rx, frame })
    }

    /// The value of `HALYARD_SESSION_ID` given to the program.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The child's process id, if the platform reported one.
    #[must_use]
    pub const fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// Events from the session. Drain with `try_recv` after a notify call,
    /// or block with `recv_timeout`.
    #[must_use]
    pub const fn events(&self) -> &Receiver<SessionEvent> {
        &self.events
    }

    /// The most recently published frame.
    #[must_use]
    pub fn latest_frame(&self) -> Arc<Frame> {
        Arc::clone(&self.frame.lock().unwrap_or_else(PoisonError::into_inner))
    }

    fn send(&self, cmd: Command) {
        // A send only fails after the session thread stopped, which happens
        // once the session is shut down; input to a dead session is dropped.
        self.commands.send(cmd).ok();
    }

    /// Write raw bytes to the program's input.
    pub fn write(&self, bytes: impl Into<Vec<u8>>) {
        self.send(Command::Write(bytes.into()));
    }

    /// Send a key, encoded for the program's current keyboard modes.
    pub fn send_key(&self, key: KeyInput) {
        self.send(Command::Key(key));
    }

    /// Send a mouse event. Nothing is sent unless the program enabled mouse
    /// tracking (see [`Frame::mouse_tracking`]).
    pub fn send_mouse(&self, mouse: MouseInput) {
        self.send(Command::Mouse(mouse));
    }

    /// Paste text: unsafe control bytes are neutralized and the text is
    /// bracketed when the program enabled bracketed paste. Check
    /// [`crate::paste_is_safe`] first if you want to confirm multi-line
    /// pastes with the user.
    pub fn paste(&self, text: impl Into<String>) {
        self.send(Command::Paste(text.into()));
    }

    /// Resize the grid (the engine reflows) and the PTY (the program gets
    /// `SIGWINCH`). Cell sizes are in pixels.
    pub fn resize(&self, cols: u16, rows: u16, cell_width: u32, cell_height: u32) {
        self.send(Command::Resize { cols, rows, cell_width, cell_height });
    }

    /// Move the viewport through scrollback.
    pub fn scroll(&self, request: ScrollRequest) {
        self.send(Command::Scroll(request));
    }

    /// Start a selection at a viewport cell (clears the old one).
    pub fn selection_start(&self, col: u16, row: u16) {
        self.send(Command::SelectStart(col, row));
    }

    /// Extend the selection to a viewport cell.
    pub fn selection_extend(&self, col: u16, row: u16) {
        self.send(Command::SelectExtend(col, row));
    }

    /// Select the word at a viewport cell.
    pub fn select_word(&self, col: u16, row: u16) {
        self.send(Command::SelectWord(col, row));
    }

    /// Clear the selection.
    pub fn selection_clear(&self) {
        self.send(Command::SelectClear);
    }

    /// The selected text, or `None` when nothing is selected. Blocks until
    /// the session thread answers (at most one second).
    #[must_use]
    pub fn selection_text(&self) -> Option<String> {
        let (tx, rx) = bounded(1);
        self.send(Command::SelectionText(tx));
        rx.recv_timeout(Duration::from_secs(1)).ok().flatten()
    }

    /// The text of rows `first..=last`, counted from the top of scrollback as
    /// [`Engine::cursor_screen_row`] counts (the rows the shell integration
    /// marks carry), or `None` when out of range. Blocks until the session
    /// thread answers (at most one second).
    #[must_use]
    pub fn screen_rows_text(&self, first: u64, last: u64) -> Option<String> {
        let (tx, rx) = bounded(1);
        self.send(Command::RowsText(first, last, tx));
        rx.recv_timeout(Duration::from_secs(1)).ok().flatten()
    }

    /// Change the colors.
    pub fn set_palette(&self, palette: Palette) {
        self.send(Command::SetPalette(palette));
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.send(Command::Shutdown);
    }
}

/// Kills the child with SIGKILL when dropped, unless `pid` was cleared.
struct KillOnDrop {
    pid: Option<u32>,
}

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if let Some(pid) = self.pid.and_then(|p| libc::pid_t::try_from(p).ok()) {
            // SAFETY: kill has no memory-safety preconditions.
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    }
}

fn last_os_error<T>() -> std::io::Result<T> {
    Err(std::io::Error::last_os_error())
}

/// Duplicate `fd` with close-on-exec set, so later children do not inherit it.
fn dup_cloexec(fd: RawFd) -> std::io::Result<OwnedFd> {
    // SAFETY: fcntl(F_DUPFD_CLOEXEC) only reads `fd` and returns a new fd.
    let new = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if new < 0 {
        return last_os_error();
    }
    // SAFETY: `new` is a freshly created fd that nothing else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(new) })
}

/// A pipe used to wake the reader thread. Closing the write end (when the
/// session thread stops) makes the read end readable with POLLHUP.
fn wake_pipe() -> std::io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: `fds` has room for the two descriptors pipe writes.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return last_os_error();
    }
    // SAFETY: both fds were just created by pipe and are owned here.
    let (r, w) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    for fd in [&r, &w] {
        // SAFETY: setting FD_CLOEXEC on an fd we own.
        if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
            return last_os_error();
        }
    }
    Ok((r, w))
}

/// A read shorter than this means output is trickling in: a program writing
/// one line per `write` (line-buffered stdio on a terminal) reaches the reader
/// a few bytes at a time.
const SMALL_READ: usize = 512;

/// After a [`SMALL_READ`], the reader waits this long before polling again so
/// that more output collects in the PTY. Without it, `yes | head` made one
/// poll, read, channel send and session wakeup per 8 bytes on average and the
/// reader thread spent all its time in system calls (see `docs/PERFORMANCE.md`).
/// The bytes already read are handed over first, so the first output of a
/// burst (a key echo) is never delayed; later output waits at most this long.
/// Kept short so a macOS PTY, which holds only a few hundred bytes of small
/// writes before the program blocks, never sits full for long.
const SMALL_READ_PAUSE: Duration = Duration::from_micros(50);

/// Whether the reader pauses after a [`SMALL_READ`]. Only measured on macOS,
/// where a PTY read returns at most 1 KiB. On Linux it stays off until a real
/// Linux before and after exists (`docs/PERFORMANCE.md`).
const PAUSE_AFTER_SMALL_READ: bool = cfg!(target_os = "macos");

/// Read PTY output until end-of-file, an error, or the session stops.
///
/// Waits with poll on the PTY and the wake pipe, so a session that is
/// dropped while a background job keeps the PTY open (`nohup sleep 30 &`)
/// still ends this thread and closes its descriptor.
fn read_loop(pty: OwnedFd, wake: &OwnedFd, out: &Sender<Vec<u8>>) {
    let mut file = std::fs::File::from(pty);
    let mut buf = vec![0u8; 64 * 1024];
    let mut trickle = false;
    loop {
        if trickle {
            thread::sleep(SMALL_READ_PAUSE);
        }
        let mut fds = [
            libc::pollfd { fd: file.as_raw_fd(), events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: wake.as_raw_fd(), events: libc::POLLIN, revents: 0 },
        ];
        // SAFETY: `fds` is a valid array of two pollfd structs.
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
        if rc < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return;
        }
        if fds[1].revents != 0 || fds[0].revents & libc::POLLNVAL != 0 {
            return;
        }
        if fds[0].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) == 0 {
            continue;
        }
        match file.read(&mut buf) {
            // End of file, or EIO once the program closed the PTY: done.
            Ok(0) => return,
            Ok(n) => {
                trickle = PAUSE_AFTER_SMALL_READ && n < SMALL_READ;
                if out.send(buf[..n].to_vec()).is_err() {
                    return;
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
                ) => {}
            Err(_) => return,
        }
    }
}

fn write_loop(mut writer: Box<dyn Write + Send>, input: &Receiver<Vec<u8>>) {
    for bytes in input {
        if let Err(e) = writer.write_all(&bytes).and_then(|()| writer.flush()) {
            eprintln!("halyard-term: writing to the PTY failed: {e}");
            return;
        }
    }
}

struct SessionThread {
    master: Box<dyn MasterPty + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    pid: Option<u32>,
    /// Write end of the reader's wake pipe; dropping it stops the reader.
    wake: Option<OwnedFd>,
    out_rx: Receiver<Vec<u8>>,
    cmd_rx: Receiver<Command>,
    write_tx: Sender<Vec<u8>>,
    exit_rx: Receiver<Option<i32>>,
    event_tx: Sender<SessionEvent>,
    frame: Arc<Mutex<Arc<Frame>>>,
    notify: Option<NotifyFn>,
    tap: Option<OutputTap>,
    observer: Option<Box<dyn OutputObserver>>,
    /// Reused buffer for the observer's marks.
    marks: Vec<usize>,
}

impl SessionThread {
    fn emit(&self, event: SessionEvent) {
        // The UI may have stopped listening; the session keeps running.
        if self.event_tx.send(event).is_ok()
            && let Some(notify) = &self.notify
        {
            notify();
        }
    }

    fn publish(&self, engine: &mut Engine) {
        match engine.frame() {
            Ok(frame) => {
                *self.frame.lock().unwrap_or_else(PoisonError::into_inner) = Arc::new(frame);
                self.emit(SessionEvent::FrameReady);
            }
            Err(e) => eprintln!("halyard-term: {e}"),
        }
    }

    fn to_pty(&self, bytes: Vec<u8>) {
        if !bytes.is_empty() {
            // The writer thread stops only after a write error, which it
            // already reported.
            self.write_tx.send(bytes).ok();
        }
    }

    /// Stop the program the way closing a terminal does: SIGHUP to the
    /// foreground process group and to the shell, stop the reader, then
    /// SIGKILL the shell if it is still running after a grace period.
    fn shutdown(&mut self, child_running: bool) {
        // The waiter thread reaps the child and then reports the exit. An exit that is reported
        // but not yet received means the pid is already reaped and may be reused.
        let plan = ShutdownPlan::at_close(child_running, !self.exit_rx.is_empty());
        if plan.hangup_group
            && let Some(pgrp) = self.master.process_group_leader()
            && pgrp > 0
        {
            // SAFETY: killpg has no memory-safety preconditions.
            unsafe { libc::killpg(pgrp, libc::SIGHUP) };
        }
        if plan.hangup_child
            && let Err(e) = self.killer.kill()
            && e.raw_os_error() != Some(libc::ESRCH)
        {
            eprintln!("halyard-term: stopping the child: {e}");
        }
        // Stop the reader: close its wake pipe, and drop the output
        // receiver in case it is blocked handing over a chunk.
        self.wake = None;
        self.out_rx = never();
        if plan.force_kill_after_grace
            && self.exit_rx.recv_timeout(Duration::from_secs(2)).is_err()
            && let Some(pid) = self.pid.and_then(|p| libc::pid_t::try_from(p).ok())
        {
            // Still not reaped (the waiter thread reaps it), so the pid is
            // still ours: force it.
            // SAFETY: kill has no memory-safety preconditions.
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    }

    /// Feeds a chunk to the engine, stopping at the observer's marks to read the
    /// cursor row.
    fn feed_engine(&mut self, engine: &mut Engine, chunk: &[u8]) {
        let Some(observer) = self.observer.as_mut() else {
            engine.write(chunk);
            return;
        };
        self.marks.clear();
        observer.scan(chunk, &mut self.marks);
        let mut done = 0;
        let mut lost = false;
        let mut dropped;
        for (index, &mark) in self.marks.iter().enumerate() {
            let mark = mark.clamp(done, chunk.len());
            engine.write(&chunk[done..mark]);
            done = mark;
            dropped = engine.dropped_rows().unwrap_or_else(|| {
                lost = true;
                engine.dropped_total()
            });
            observer.at_mark(index, engine.cursor_screen_row().saturating_add(dropped));
        }
        engine.write(&chunk[done..]);
        dropped = engine.dropped_rows().unwrap_or_else(|| {
            lost = true;
            engine.dropped_total()
        });
        let history = engine.primary_history_rows().map(|rows| History { rows, dropped, lost });
        observer.chunk_done(history);
    }

    fn run(mut self, mut engine: Engine) {
        self.publish(&mut engine);
        let mut last_publish = Instant::now();
        let mut dirty = false;
        let mut reader_open = true;
        let mut exit = ExitTracker::default();
        let mut child_running = true;

        loop {
            let now = Instant::now();
            let mut timeout = Duration::from_secs(3600);
            if dirty {
                timeout = timeout.min(FRAME_INTERVAL.saturating_sub(now - last_publish));
            }
            if let Some(deadline) = exit.deadline() {
                timeout = timeout.min(deadline.saturating_duration_since(now));
            }
            let out_rx = if reader_open { self.out_rx.clone() } else { never() };
            let exit_rx = if child_running { self.exit_rx.clone() } else { never() };

            select! {
                recv(out_rx) -> msg => match msg {
                    Ok(chunk) => {
                        if let Some(tap) = self.tap.as_mut() {
                            tap(&chunk);
                        }
                        self.feed_engine(&mut engine, &chunk);
                        let fx = engine.take_effects();
                        self.to_pty(fx.pty_reply);
                        if fx.bell {
                            self.emit(SessionEvent::Bell);
                        }
                        if fx.title_changed {
                            self.emit(SessionEvent::TitleChanged(engine.title()));
                        }
                        dirty = true;
                    }
                    Err(_) => {
                        reader_open = false;
                        exit.reader_closed(Instant::now());
                    }
                },
                recv(exit_rx) -> msg => {
                    child_running = false;
                    exit.child_exited(msg.unwrap_or(None), reader_open, Instant::now());
                },
                recv(self.cmd_rx) -> msg => match msg {
                    Ok(Command::Shutdown) | Err(_) => break,
                    Ok(cmd) => {
                        if self.handle(&mut engine, cmd) {
                            dirty = true;
                        }
                    }
                },
                default(timeout) => {},
            }

            if dirty && last_publish.elapsed() >= FRAME_INTERVAL {
                self.publish(&mut engine);
                last_publish = Instant::now();
                dirty = false;
            }
            if let Some(code) = exit.take_due(Instant::now()) {
                if dirty {
                    self.publish(&mut engine);
                    last_publish = Instant::now();
                    dirty = false;
                }
                self.emit(SessionEvent::Exited(code));
            }
        }
        self.shutdown(child_running);
    }

    /// Apply a command. Returns true if the screen may have changed.
    fn handle(&self, engine: &mut Engine, cmd: Command) -> bool {
        let report = |r: Result<(), EngineError>| {
            if let Err(e) = r {
                eprintln!("halyard-term: {e}");
            }
        };
        match cmd {
            Command::Write(bytes) => {
                self.to_pty(bytes);
                false
            }
            Command::Key(key) => {
                match engine.encode_key(&key) {
                    Ok(bytes) => self.to_pty(bytes),
                    Err(e) => eprintln!("halyard-term: {e}"),
                }
                // Typing snaps the viewport back to the live screen.
                if key.action != crate::input::KeyAction::Release {
                    engine.scroll(ScrollRequest::Bottom);
                    return true;
                }
                false
            }
            Command::Mouse(mouse) => {
                match engine.encode_mouse(&mouse) {
                    Ok(bytes) => self.to_pty(bytes),
                    Err(e) => eprintln!("halyard-term: {e}"),
                }
                false
            }
            Command::Paste(text) => {
                match engine.encode_paste(&text) {
                    Ok(bytes) => self.to_pty(bytes),
                    Err(e) => eprintln!("halyard-term: {e}"),
                }
                engine.scroll(ScrollRequest::Bottom);
                true
            }
            Command::Resize { cols, rows, cell_width, cell_height } => {
                report(engine.resize(cols, rows, cell_width, cell_height));
                if let Err(e) = self.master.resize(pty_size(cols, rows, cell_width, cell_height)) {
                    eprintln!("halyard-term: resizing the PTY failed: {e:#}");
                }
                true
            }
            Command::Scroll(request) => {
                engine.scroll(request);
                true
            }
            Command::SelectStart(col, row) => {
                report(engine.selection_start(col, row));
                true
            }
            Command::SelectExtend(col, row) => {
                report(engine.selection_extend(col, row));
                true
            }
            Command::SelectWord(col, row) => {
                report(engine.select_word(col, row));
                true
            }
            Command::SelectClear => {
                report(engine.selection_clear());
                true
            }
            Command::SelectionText(reply) => {
                let text = engine.selection_text().unwrap_or_else(|e| {
                    eprintln!("halyard-term: {e}");
                    None
                });
                // The caller may have timed out and gone away.
                reply.send(text).ok();
                false
            }
            Command::RowsText(first, last, reply) => {
                let text = engine.screen_rows_text(first, last).unwrap_or_else(|e| {
                    eprintln!("halyard-term: {e}");
                    None
                });
                // The caller may have timed out and gone away.
                reply.send(text).ok();
                false
            }
            Command::SetPalette(palette) => {
                report(engine.set_palette(&palette));
                true
            }
            Command::Shutdown => false,
        }
    }
}

/// When the session reports the child's exit. The loop sleeps until
/// [`ExitTracker::deadline`]; a deadline left behind after the report would
/// make that sleep zero forever, so only `take_due` may clear it and nothing
/// may set it once the exit is reported.
#[derive(Default)]
struct ExitTracker {
    code: Option<Option<i32>>,
    deadline: Option<Instant>,
    reported: bool,
}

impl ExitTracker {
    fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    fn child_exited(&mut self, code: Option<i32>, reader_open: bool, now: Instant) {
        self.code = Some(code);
        if !self.reported {
            self.deadline = Some(if reader_open { now + EXIT_DRAIN } else { now });
        }
    }

    fn reader_closed(&mut self, now: Instant) {
        if self.code.is_some() && !self.reported {
            self.deadline = Some(now);
        }
    }

    /// The exit code, once, when the drain deadline has passed.
    fn take_due(&mut self, now: Instant) -> Option<Option<i32>> {
        if self.reported {
            return None;
        }
        let (code, deadline) = (self.code?, self.deadline?);
        if now < deadline {
            return None;
        }
        self.reported = true;
        self.deadline = None;
        Some(code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn reported_exit_leaves_no_deadline_when_reader_closes_late() {
        // A background job holds the PTY open past the shell's exit: the exit is
        // reported after the drain, then the reader hits EOF.
        let t0 = Instant::now();
        let mut exit = ExitTracker::default();
        exit.child_exited(Some(1), true, t0);
        assert_eq!(exit.take_due(t0), None);
        assert_eq!(exit.take_due(t0 + EXIT_DRAIN), Some(Some(1)));
        assert_eq!(exit.deadline(), None);
        exit.reader_closed(t0 + Duration::from_secs(5));
        assert_eq!(exit.deadline(), None, "a reported exit must not arm the loop timer");
        assert_eq!(exit.take_due(t0 + Duration::from_secs(5)), None);
    }

    #[test]
    fn exit_is_reported_once_when_reader_closes_first() {
        let t0 = Instant::now();
        let mut exit = ExitTracker::default();
        exit.reader_closed(t0);
        assert_eq!(exit.deadline(), None);
        exit.child_exited(None, false, t0);
        assert_eq!(exit.take_due(t0), Some(None));
        assert_eq!(exit.deadline(), None);
        assert_eq!(exit.take_due(t0 + Duration::from_secs(1)), None);
    }

    #[test]
    fn reader_closing_during_drain_reports_immediately() {
        let t0 = Instant::now();
        let mut exit = ExitTracker::default();
        exit.child_exited(Some(0), true, t0);
        exit.reader_closed(t0 + Duration::from_millis(10));
        assert_eq!(exit.take_due(t0 + Duration::from_millis(10)), Some(Some(0)));
        assert_eq!(exit.deadline(), None);
    }

    #[test]
    fn session_handle_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Session>();
    }

    #[test]
    fn default_shell_is_an_executable_absolute_path() {
        let shell = default_shell();
        assert!(shell.is_absolute(), "{shell:?}");
        assert!(is_executable(&shell), "{shell:?}");
    }

    #[test]
    fn passwd_lookup_finds_a_shell() {
        // Every CI and dev account has a passwd entry with a shell.
        assert!(passwd_shell().is_some());
    }

    #[test]
    fn pty_size_saturates_pixels() {
        let s = pty_size(1000, 1000, 1000, 1000);
        assert_eq!((s.cols, s.rows), (1000, 1000));
        assert_eq!(s.pixel_width, u16::MAX);
        assert_eq!(pty_size(0, 0, 8, 16).cols, 1);
    }

    #[test]
    fn a_utf8_locale_is_added_only_when_none_is_set() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| OsString::from(v))
        };
        assert_eq!(default_locale(env(&[]), true), Some("en_US.UTF-8"));
        assert_eq!(default_locale(env(&[]), false), Some("C.UTF-8"));
        assert_eq!(default_locale(env(&[("LANG", "")]), false), Some("C.UTF-8"));
        assert_eq!(default_locale(env(&[("LANG", "de_DE.UTF-8")]), true), None);
        assert_eq!(default_locale(env(&[("LC_CTYPE", "UTF-8")]), true), None);
        assert_eq!(default_locale(env(&[("LC_ALL", "C")]), false), None);
    }

    #[test]
    fn the_child_gets_a_locale_even_when_halyard_has_none() {
        let config = SessionConfig {
            env_remove: vec!["LANG".into(), "LC_ALL".into(), "LC_CTYPE".into()],
            ..SessionConfig::default()
        };
        let cmd = build_command(&config, "abc123").unwrap().cmd;
        let lang = cmd.get_env("LANG").and_then(|v| v.to_str()).map(str::to_owned);
        assert!(lang.as_deref().is_some_and(|l| l.ends_with("UTF-8")), "{lang:?}");

        let config = SessionConfig {
            env: vec![("LANG".into(), "de_DE.UTF-8".into())],
            ..SessionConfig::default()
        };
        let cmd = build_command(&config, "abc123").unwrap().cmd;
        assert_eq!(cmd.get_env("LANG").and_then(|v| v.to_str()), Some("de_DE.UTF-8"));
    }

    #[test]
    fn command_sets_halyard_environment() {
        let config = SessionConfig {
            env: vec![("FOO".into(), "bar".into())],
            env_remove: vec!["HALYARD_TEST_REMOVED".into()],
            ..SessionConfig::default()
        };
        let cmd = build_command(&config, "abc123").unwrap().cmd;
        let get = |k: &str| cmd.get_env(k).and_then(|v| v.to_str()).map(str::to_owned);
        assert_eq!(get("TERM").as_deref(), Some("xterm-256color"));
        assert_eq!(get("COLORTERM").as_deref(), Some("truecolor"));
        assert_eq!(get("TERM_PROGRAM").as_deref(), Some("Halyard"));
        assert_eq!(get("TERM_PROGRAM_VERSION").as_deref(), Some(env!("CARGO_PKG_VERSION")));
        assert_eq!(get("HALYARD_SESSION_ID").as_deref(), Some("abc123"));
        assert_eq!(get("FOO").as_deref(), Some("bar"));
        assert!(cmd.is_default_prog());
        assert_eq!(get("SHELL").map(PathBuf::from), Some(default_shell()));
    }

    fn argv(cmd: &CommandBuilder) -> Vec<String> {
        cmd.get_argv().iter().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    /// The env file's `KEY=VALUE` entries, read before the `Built` (and its
    /// file) is dropped.
    fn env_entries(built: &Built) -> Vec<String> {
        let file = built.env_file.as_ref().expect("an env file");
        let bytes = std::fs::read(file.path()).unwrap();
        let mode = std::fs::metadata(file.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the env file must be owner-only");
        bytes
            .split(|b| *b == 0)
            .filter(|e| !e.is_empty())
            .map(|e| String::from_utf8_lossy(e).into_owned())
            .collect()
    }

    #[test]
    fn inside_flatpak_the_login_shell_runs_on_the_host() {
        let secret = "token-value-that-must-stay-out-of-argv";
        let config = SessionConfig {
            cwd: Some(PathBuf::from("/srv/project")),
            env: vec![
                ("FOO".into(), "bar".into()),
                ("TERM".into(), "dumb".into()),
                ("HALYARD_SESSION_TOKEN".into(), secret.into()),
            ],
            env_remove: vec!["OLDVAR".into()],
            ..SessionConfig::default()
        };
        let built = build_command_for(&config, "abc123", Placement::FlatpakHost).unwrap();
        let cmd = &built.cmd;
        assert!(!cmd.is_default_prog());
        let argv = argv(cmd);
        assert_eq!(argv[..2], ["/bin/sh", "-c"], "{argv:?}");
        let spawn = argv.iter().position(|a| a == "flatpak-spawn").unwrap();
        assert_eq!(
            argv[spawn..spawn + 4],
            ["flatpak-spawn", "--host", "--watch-bus", "--env-fd=3"]
        );
        assert!(argv.contains(&"--directory=/srv/project".to_owned()), "{argv:?}");
        assert!(argv.contains(&"--unset-env=OLDVAR".to_owned()), "{argv:?}");
        for arg in &argv {
            assert!(!arg.contains(secret) && !arg.contains("abc123"), "value in argv: {arg}");
        }

        // Exactly the session's variables, plus SHELL and LANG when they
        // apply, and nothing else from this process's environment.
        let entries = env_entries(&built);
        let mut names: Vec<&str> =
            entries.iter().map(|e| e.split_once('=').map_or(e.as_str(), |(k, _)| k)).collect();
        names.retain(|n| *n != "SHELL" && *n != "LANG");
        names.sort_unstable();
        assert_eq!(
            names,
            [
                "COLORTERM",
                "FOO",
                "HALYARD_SESSION_ID",
                "HALYARD_SESSION_TOKEN",
                "TERM",
                "TERM_PROGRAM",
                "TERM_PROGRAM_VERSION"
            ]
        );
        for want in ["TERM=dumb", "HALYARD_SESSION_ID=abc123", "FOO=bar"] {
            assert!(entries.contains(&want.to_owned()), "{want} missing from {entries:?}");
        }
        assert!(entries.contains(&format!("HALYARD_SESSION_TOKEN={secret}")));
        // The program comes last: the login shell from $SHELL, or the host's own.
        let tail = &argv[argv.len() - 2..];
        assert!(tail[1] == "-l" || tail[1] == crate::flatpak::HOST_SHELL_FALLBACK, "{argv:?}");
        // The sandbox side does not chdir to a host path that may not exist in it.
        assert_eq!(cmd.get_cwd(), None);
        // The wrapper never ran, so dropping the command removes its env file.
        let path = built.env_file.as_ref().unwrap().path().to_owned();
        drop(built);
        assert!(!path.exists());
    }

    #[test]
    fn inside_flatpak_the_locale_default_applies_too() {
        let config = SessionConfig {
            env_remove: vec!["LANG".into(), "LC_ALL".into(), "LC_CTYPE".into()],
            ..SessionConfig::default()
        };
        let built = build_command_for(&config, "id", Placement::FlatpakHost).unwrap();
        let entries = env_entries(&built);
        assert!(
            entries.iter().any(|e| e.starts_with("LANG=") && e.ends_with("UTF-8")),
            "{entries:?}"
        );
        let config = SessionConfig {
            env: vec![("LANG".into(), "de_DE.UTF-8".into())],
            ..SessionConfig::default()
        };
        let built = build_command_for(&config, "id", Placement::FlatpakHost).unwrap();
        let langs: Vec<_> =
            env_entries(&built).into_iter().filter(|e| e.starts_with("LANG=")).collect();
        assert_eq!(langs, ["LANG=de_DE.UTF-8"]);
    }

    #[test]
    fn inside_flatpak_an_explicit_program_runs_on_the_host() {
        let config = SessionConfig {
            program: Some(PathBuf::from("/usr/bin/htop")),
            args: vec!["-d".into(), "10".into()],
            ..SessionConfig::default()
        };
        let built = build_command_for(&config, "id", Placement::FlatpakHost).unwrap();
        let argv = argv(&built.cmd);
        assert!(argv.contains(&"flatpak-spawn".to_owned()), "{argv:?}");
        assert_eq!(argv[argv.len() - 3..], ["/usr/bin/htop", "-d", "10"], "{argv:?}");
    }

    #[test]
    fn outside_flatpak_the_program_runs_directly() {
        let config = SessionConfig {
            program: Some(PathBuf::from("/usr/bin/htop")),
            args: vec!["-d".into(), "10".into()],
            cwd: Some(PathBuf::from("/tmp")),
            ..SessionConfig::default()
        };
        let built = build_command_for(&config, "id", Placement::Local).unwrap();
        assert!(built.env_file.is_none());
        let cmd = built.cmd;
        assert_eq!(argv(&cmd), ["/usr/bin/htop", "-d", "10"]);
        assert_eq!(cmd.get_cwd().map(PathBuf::from), Some(PathBuf::from("/tmp")));
    }

    #[test]
    fn path_append_goes_last_and_only_once() {
        let dir = Path::new("/Applications/Halyard.app/Contents/MacOS");
        let appended = |p: &str| path_with_appended(Some(OsStr::new(p)), dir);
        assert_eq!(
            appended("/usr/bin:/bin"),
            Some(OsString::from("/usr/bin:/bin:/Applications/Halyard.app/Contents/MacOS"))
        );
        // Already there, in any position: unchanged.
        assert_eq!(appended("/Applications/Halyard.app/Contents/MacOS:/usr/bin"), None);
        assert_eq!(appended("/usr/bin:/Applications/Halyard.app/Contents/MacOS"), None);
        // An unset or empty PATH stays as it is: setting it would drop the default search path.
        assert_eq!(path_with_appended(None, dir), None);
        assert_eq!(appended(""), None);
        // A relative directory, or one PATH cannot express, is never added.
        assert_eq!(path_with_appended(Some(OsStr::new("/usr/bin")), Path::new("bin")), None);
        assert_eq!(path_with_appended(Some(OsStr::new("/usr/bin")), Path::new("/a:b")), None);
    }

    #[test]
    fn a_local_program_gets_path_append_at_the_end() {
        let dir = PathBuf::from("/opt/halyard-test-helpers/bin");
        let config = SessionConfig {
            env: vec![("PATH".into(), "/usr/bin:/bin".into())],
            path_append: Some(dir.clone()),
            ..SessionConfig::default()
        };
        let cmd = build_command_for(&config, "id", Placement::Local).unwrap().cmd;
        assert_eq!(
            cmd.get_env("PATH").and_then(|v| v.to_str()),
            Some("/usr/bin:/bin:/opt/halyard-test-helpers/bin")
        );

        // Control: without the field, PATH is exactly what the session set.
        let config = SessionConfig {
            env: vec![("PATH".into(), "/usr/bin:/bin".into())],
            ..SessionConfig::default()
        };
        let cmd = build_command_for(&config, "id", Placement::Local).unwrap().cmd;
        assert_eq!(cmd.get_env("PATH").and_then(|v| v.to_str()), Some("/usr/bin:/bin"));
    }

    #[test]
    fn a_host_program_never_gets_path_append() {
        // The directory is inside the sandbox; the host's PATH must not name it, and the
        // sandbox's own PATH must not reach the host either.
        let config = SessionConfig {
            path_append: Some(PathBuf::from("/app/bin")),
            ..SessionConfig::default()
        };
        let built = build_command_for(&config, "id", Placement::FlatpakHost).unwrap();
        let entries = env_entries(&built);
        assert!(!entries.iter().any(|e| e.starts_with("PATH=")), "{entries:?}");
        assert!(!argv(&built.cmd).iter().any(|a| a.contains("/app/bin")), "{:?}", argv(&built.cmd));
    }

    #[test]
    fn foreign_terminal_variables_are_scrubbed() {
        for name in [
            "TMUX",
            "TMUX_PANE",
            "ITERM_SESSION_ID",
            "ITERM_PROFILE",
            "GHOSTTY_RESOURCES_DIR",
            "KITTY_WINDOW_ID",
            "WEZTERM_PANE",
            "LC_TERMINAL",
            "LC_TERMINAL_VERSION",
            "TERM_SESSION_ID",
            "COLUMNS",
            "LINES",
            "VTE_VERSION",
            "WT_SESSION",
            "ALACRITTY_WINDOW_ID",
            "STY",
            "ZELLIJ",
            "ZELLIJ_SESSION_NAME",
            "ZELLIJ_PANE_ID",
            "TERM_FEATURES",
            "TERMINAL_EMULATOR",
            "KONSOLE_DBUS_SESSION",
        ] {
            assert!(is_foreign_terminal_var(name), "{name}");
        }
        for name in ["HOME", "PATH", "TERM", "LANG", "TMUXINATOR_CONFIG", "KITTYX"] {
            assert!(!is_foreign_terminal_var(name), "{name}");
        }
    }

    #[test]
    fn a_reaped_child_is_never_signalled() {
        // Its pid and process group may already belong to another process.
        let quiet = ShutdownPlan {
            hangup_group: false,
            hangup_child: false,
            force_kill_after_grace: false,
        };
        // Exit already received by the run loop, or reaped and queued but not yet received.
        assert_eq!(ShutdownPlan::at_close(false, false), quiet);
        assert_eq!(ShutdownPlan::at_close(false, true), quiet);
        assert_eq!(ShutdownPlan::at_close(true, true), quiet);
        let plan = ShutdownPlan::at_close(true, false);
        assert!(plan.hangup_group && plan.hangup_child && plan.force_kill_after_grace);
    }

    #[test]
    fn kill_guard_kills_unless_defused() {
        let mut armed = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        drop(KillOnDrop { pid: Some(armed.id()) });
        let status = armed.wait().unwrap();
        assert!(!status.success(), "the guard did not kill the child");

        let mut defused = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        let mut guard = KillOnDrop { pid: Some(defused.id()) };
        guard.pid = None;
        drop(guard);
        assert!(defused.try_wait().unwrap().is_none(), "a defused guard killed the child");
        defused.kill().unwrap();
        defused.wait().unwrap();
    }

    #[test]
    fn session_ids_are_unique_hex() {
        let a = new_session_id();
        let b = new_session_id();
        assert_ne!(a, b);
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
