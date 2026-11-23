//! The per-pane signal pipeline: the session thread's observer and the UI's
//! foreground poll, both feeding one [`PaneMonitor`].
//!
//! * [`SignalObserver`] runs on the session thread. It scans each PTY chunk for
//!   shell integration marks, notifications and protocol reports, gets the cursor
//!   row at each from the engine, and applies the chunk to the monitor with one
//!   lock. A chunk with no escape sequence costs one scan and one lock.
//! * [`poll_foreground`] runs on the UI thread about every 500 ms. It reads the
//!   shell's process groups (one `proc_pidinfo` or `/proc` read) and only when a
//!   command holds the terminal reads that command's argv to recognize an agent.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime};

use halyard_agents::platform::process_info;
use halyard_agents::{Foreground, PaneMonitor, SignalScanner, detect_with_extra};
use halyard_shell::SystemClock;
use halyard_term::OutputObserver;

/// How often the window polls each pane's foreground process.
pub const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Closed command blocks kept per pane.
pub const BLOCK_HISTORY: usize = 1000;

/// A pane's monitor, shared by its session thread and the UI.
pub type SharedMonitor = Arc<Mutex<PaneMonitor<SystemClock>>>;

/// A new monitor for the pane with this `HALYARD_SESSION_ID`.
pub fn new_monitor(session_id: &str) -> SharedMonitor {
    Arc::new(Mutex::new(PaneMonitor::new(
        session_id,
        halyard_agents::AttentionConfig::default(),
        SystemClock,
        BLOCK_HISTORY,
    )))
}

/// Locks a monitor. A panic while it was held leaves plain data behind, which is
/// still the best information there is.
pub fn lock(m: &SharedMonitor) -> MutexGuard<'_, PaneMonitor<SystemClock>> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The session thread's half: scans output and applies it to the monitor.
#[derive(Debug)]
pub struct SignalObserver {
    scan: SignalScanner,
    monitor: SharedMonitor,
}

impl SignalObserver {
    /// An observer feeding `monitor`.
    pub fn new(monitor: SharedMonitor) -> Self {
        Self { scan: SignalScanner::new(), monitor }
    }
}

impl OutputObserver for SignalObserver {
    fn scan(&mut self, chunk: &[u8], marks: &mut Vec<usize>) {
        self.scan.scan(chunk, marks);
    }

    fn at_mark(&mut self, _index: usize, row: u64) {
        self.scan.at_mark(row);
    }

    fn chunk_done(&mut self, history: Option<halyard_term::History>) {
        let now = SystemTime::now();
        let mut m = lock(&self.monitor);
        self.scan.finish(&mut m, now);
        if let Some(h) = history {
            m.on_history(h.rows, h.dropped, h.lost);
        }
    }
}

/// What holds the terminal of the shell `pid`: the shell, or a command (read
/// once more to recognize an agent, with `extra` names from `[agents] detect`).
/// `None` when the process groups cannot be read (the shell is gone).
pub fn poll_foreground(pid: u32, extra: &[String]) -> Option<Foreground> {
    let groups = crate::procinfo::groups(pid)?;
    if !groups.running_command() {
        return Some(Foreground::Shell);
    }
    let pgid = i32::try_from(groups.tpgid).ok()?;
    // The group leader is the job the shell started. Right after a fork it can
    // still be a copy of the shell; the next poll sees the exec'd program.
    let agent = process_info(pgid).ok().and_then(|info| detect_with_extra(&info, extra));
    Some(Foreground::Command { pgid, agent })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Instant;

    use halyard_agents::AttentionState;
    use halyard_shell::Shell;
    use halyard_term::{Session, SessionConfig};

    use super::*;

    fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !f() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Every foreground read a test made, oldest first, printed when it fails
    /// so a flaky failure explains itself. Repeats are counted, not listed.
    struct Trace {
        start: Instant,
        lines: Mutex<Vec<(u128, String, usize)>>,
    }

    impl Trace {
        fn new() -> Self {
            Self { start: Instant::now(), lines: Mutex::new(Vec::new()) }
        }

        fn note(&self, what: String) {
            let mut lines = self.lines.lock().unwrap_or_else(PoisonError::into_inner);
            match lines.last_mut() {
                Some((_, last, n)) if *last == what => *n += 1,
                _ => lines.push((self.start.elapsed().as_millis(), what, 1)),
            }
        }
    }

    impl std::fmt::Display for Trace {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            writeln!(f, "foreground reads, oldest first:")?;
            for (ms, what, n) in self.lines.lock().unwrap_or_else(PoisonError::into_inner).iter() {
                let repeat = if *n > 1 { format!("  (x{n})") } else { String::new() };
                writeln!(f, "  {ms:>6} ms  {what}{repeat}")?;
            }
            Ok(())
        }
    }

    /// Watches a child process for its exit and reads how it ended (exit code
    /// or signal) without reaping it: the session's waiter thread reaps it and
    /// reports only `None` for a signal.
    #[cfg(target_os = "macos")]
    struct ExitWatch {
        kq: Option<std::os::fd::OwnedFd>,
        error: Option<String>,
    }

    #[cfg(target_os = "macos")]
    impl ExitWatch {
        fn new(pid: u32) -> Self {
            use std::os::fd::FromRawFd;
            // SAFETY: kqueue takes no arguments and returns a new fd or -1.
            let fd = unsafe { libc::kqueue() };
            if fd < 0 {
                return Self { kq: None, error: Some(std::io::Error::last_os_error().to_string()) };
            }
            // SAFETY: `fd` was just created and nothing else owns it.
            let kq = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
            // SAFETY: kevent is plain data; zeroed is a valid value.
            let mut ev: libc::kevent = unsafe { std::mem::zeroed() };
            ev.ident = pid as usize;
            ev.filter = libc::EVFILT_PROC;
            ev.flags = libc::EV_ADD;
            ev.fflags = libc::NOTE_EXIT | libc::NOTE_EXITSTATUS;
            // SAFETY: one valid change, no events out, no timeout pointer needed.
            let rc = unsafe {
                libc::kevent(
                    std::os::fd::AsRawFd::as_raw_fd(&kq),
                    &raw const ev,
                    1,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null(),
                )
            };
            if rc < 0 {
                let e = std::io::Error::last_os_error();
                return Self { kq: None, error: Some(format!("not watched: {e}")) };
            }
            Self { kq: Some(kq), error: None }
        }

        /// Why the exit cannot be watched, if it cannot.
        fn error(&self) -> Option<String> {
            self.error.clone()
        }

        /// How the process ended, or `None` while it runs.
        fn ended(&self) -> Option<String> {
            let kq = self.kq.as_ref()?;
            // SAFETY: kevent is plain data; zeroed is a valid value.
            let mut out: libc::kevent = unsafe { std::mem::zeroed() };
            let zero = libc::timespec { tv_sec: 0, tv_nsec: 0 };
            // SAFETY: room for one event out, a valid timeout, no changes.
            let n = unsafe {
                libc::kevent(
                    std::os::fd::AsRawFd::as_raw_fd(kq),
                    std::ptr::null(),
                    0,
                    &raw mut out,
                    1,
                    &raw const zero,
                )
            };
            if n != 1 {
                return None;
            }
            let status = i32::try_from(out.data).unwrap_or(-1);
            Some(if libc::WIFSIGNALED(status) {
                format!("shell killed by signal {}", libc::WTERMSIG(status))
            } else {
                format!("shell exited with code {}", libc::WEXITSTATUS(status))
            })
        }
    }

    /// Exits are only watched on macOS, where this test has failed.
    #[cfg(not(target_os = "macos"))]
    struct ExitWatch;

    #[cfg(not(target_os = "macos"))]
    impl ExitWatch {
        fn new(_pid: u32) -> Self {
            Self
        }

        fn error(&self) -> Option<String> {
            Some("exits are watched only on macOS".into())
        }

        fn ended(&self) -> Option<String> {
            None
        }
    }

    /// One poll as text: the shell's process groups, whether the shell is
    /// alive, what `poll_foreground` returned and what the job leader reads as.
    fn describe_poll(pid: u32, fg: Option<&Foreground>) -> String {
        let groups = crate::procinfo::groups(pid);
        // SAFETY: kill with signal 0 only checks that the process exists.
        let alive = unsafe { libc::kill(libc::pid_t::try_from(pid).unwrap_or(0), 0) } == 0;
        let leader = match fg {
            Some(Foreground::Command { pgid, .. }) => match process_info(*pgid) {
                Ok(info) => format!("leader exe {:?} argv {:?}", info.exe, info.argv),
                Err(e) => format!("leader unreadable: {e}"),
            },
            _ => String::new(),
        };
        format!("groups {groups:?} alive {alive} poll {fg:?} {leader}")
    }

    fn spawn(program: PathBuf, args: Vec<String>, monitor: &SharedMonitor, id: &str) -> Session {
        Session::spawn(SessionConfig {
            program: Some(program),
            args,
            session_id: Some(id.into()),
            observer: Some(Box::new(SignalObserver::new(Arc::clone(monitor)))),
            cols: 80,
            rows: 24,
            ..SessionConfig::default()
        })
        .expect("spawn")
    }

    /// A real zsh (or bash) with Halyard's integration script sourced, through a
    /// real `Session`: blocks get rows, exit statuses and command lines.
    #[test]
    fn integration_script_in_a_real_shell_makes_blocks() {
        let dir = tempfile::tempdir().unwrap();
        // Fixed FHS paths first, then $PATH: Nix build sandboxes have no /bin/zsh
        // or /bin/bash, only the shells their derivation puts on PATH.
        let on_path = |name: &str| {
            std::env::var_os("PATH").and_then(|paths| {
                std::env::split_paths(&paths).map(|d| d.join(name)).find(|p| p.is_file())
            })
        };
        let program = ["/bin/zsh", "/usr/bin/zsh", "/bin/bash", "/usr/bin/bash"]
            .iter()
            .map(PathBuf::from)
            .find(|p| p.exists())
            .or_else(|| on_path("zsh"))
            .or_else(|| on_path("bash"))
            .expect("zsh or bash");
        let shell = Shell::from_program(program.to_str().expect("utf-8 path")).unwrap();
        let script = dir.path().join(shell.script_file_name());
        std::fs::write(&script, halyard_shell::integration_script(shell)).unwrap();
        let rc = dir.path().join("rc");
        let line = halyard_shell::install_line(shell, &script);
        std::fs::write(&rc, format!("PS1='$ '\n{line}\n")).unwrap();
        let args = match shell {
            Shell::Zsh => vec!["-f".into(), "-i".into()],
            _ => vec!["--norc".into(), "--noprofile".into(), "-i".into()],
        };
        let monitor = new_monitor("itest");
        let session = spawn(program, args, &monitor, "itest");
        // Source the rc by hand: zsh -f and bash --norc read none.
        session.write(format!(". '{}'\n", rc.display()));
        wait_for("a prompt mark", || lock(&monitor).blocks().pending_prompt_row().is_some());
        session.write("echo one; echo two\n");
        wait_for("the first block", || lock(&monitor).blocks().history().len() >= 1);
        // Copy last output: the rows between C and D, read back from the engine.
        let rows = crate::blocks_ui::last_output_rows(lock(&monitor).blocks()).expect("rows");
        let text = session.screen_rows_text(rows.0, rows.1).expect("text");
        assert_eq!(crate::blocks_ui::tidy_output(&text), "one\ntwo");
        session.write("false\n");
        wait_for("the failed block", || lock(&monitor).blocks().last_failed().is_some());
        let m = lock(&monitor);
        let blocks: Vec<_> = m.blocks().history().cloned().collect();
        let first = &blocks[0];
        assert_eq!(first.exit_code, Some(0));
        assert_eq!(first.command.as_deref(), Some("echo one; echo two"));
        let (out, end) = (first.output_start_row.unwrap(), first.output_end_row.unwrap());
        assert!(end >= out + 2, "two lines of output between C and D: {first:?}");
        assert!(first.prompt_row.unwrap() <= out);
        let failed = m.blocks().last_failed().unwrap();
        assert_eq!(failed.exit_code, Some(1));
        assert!(failed.prompt_row.unwrap() > first.prompt_row.unwrap());
        // A shell alone is not an agent: no state is invented.
        assert_eq!(m.attention().state(), AttentionState::Unknown);
        let cwd = m.cwd().expect("OSC 7 from the script");
        assert!(cwd.verbatim);
        drop(m);
        // Previous prompt from below the last block, once there is scrollback:
        // the view's top row becomes that command's prompt.
        session.write("seq 1 60\n");
        wait_for("the seq block", || lock(&monitor).blocks().history().len() >= 3);
        let m = lock(&monitor);
        let below = m.blocks().history().last().unwrap().output_end_row.unwrap() + 1;
        let jump = crate::blocks_ui::Jump::Previous;
        let target = crate::blocks_ui::prompt_target(m.blocks(), below, jump).expect("a prompt");
        drop(m);
        session.scroll(halyard_term::ScrollRequest::Row(target));
        wait_for("the view on the prompt", || {
            let f = session.latest_frame();
            f.scroll.offset == target && f.row_text(0).contains("seq 1 60")
        });
        // Clearing scrollback (what `clear` sends) forgets the old blocks, whose
        // rows would now point at other text.
        session.scroll(halyard_term::ScrollRequest::Bottom);
        session.write("printf '\\033[H\\033[2J\\033[3J'\n");
        wait_for("old blocks forgotten", || lock(&monitor).blocks().history().len() <= 1);
    }

    /// A program named `claude` that prints an OSC 9 notification moves its pane
    /// to WaitingForInput: detection through the real foreground poll, the
    /// notification through the session thread's observer.
    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn fake_claude_notification_means_waiting() {
        let dir = tempfile::tempdir().unwrap();
        let claude = dir.path().join("claude");
        // A tiny helper named `claude`: waits for a line on stdin, prints OSC 9,
        // then sleeps so it stays in the foreground.
        let helper = std::env::current_exe().unwrap();
        std::fs::copy(&helper, &claude).unwrap();
        let monitor = new_monitor("fake");
        let sh = PathBuf::from("/bin/sh");
        let raw = Arc::new(Mutex::new(Vec::<u8>::new()));
        let tap_raw = Arc::clone(&raw);
        // Interactive, so the shell gives the job its own process group.
        let session = Session::spawn(SessionConfig {
            program: Some(sh),
            args: vec!["-i".into()],
            session_id: Some("fake".into()),
            observer: Some(Box::new(SignalObserver::new(Arc::clone(&monitor)))),
            output_tap: Some(Box::new(move |b| {
                tap_raw.lock().unwrap_or_else(PoisonError::into_inner).extend_from_slice(b);
            })),
            cols: 80,
            rows: 24,
            ..SessionConfig::default()
        })
        .expect("spawn");
        let pid = session.pid().expect("pid");
        let exit_watch = ExitWatch::new(pid);
        let trace = Trace::new();
        trace.note(format!("shell pid {pid}"));
        if let Some(e) = exit_watch.error() {
            trace.note(e);
        }
        session.write(format!(
            "HALYARD_FAKE_AGENT=1 '{}' --exact signals::tests::fake_agent_main --nocapture\n",
            claude.display()
        ));
        // Every read goes into the trace, and a failure prints it with the
        // shell's output, so a flaky failure explains itself.
        let fail = |why: &str| -> ! {
            let out = raw.lock().unwrap_or_else(PoisonError::into_inner).clone();
            panic!(
                "{why}\n{trace}screen:\n{}\nraw output: {:?}",
                session.latest_frame().text(),
                String::from_utf8_lossy(&out)
            );
        };
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let fg = poll_foreground(pid, &[]);
            trace.note(describe_poll(pid, fg.as_ref()));
            let mut shell_ended = exit_watch.ended();
            while let Ok(ev) = session.events().try_recv() {
                if let halyard_term::SessionEvent::Exited(code) = ev {
                    shell_ended = Some(format!("session event: shell exited, code {code:?}"));
                }
            }
            if let Some(how) = shell_ended {
                trace.note(how);
                fail("the shell ended before claude reached the foreground");
            }
            if let Some(fg) = &fg {
                lock(&monitor).on_foreground(fg, SystemTime::now());
            }
            if lock(&monitor).agent_detected() {
                break;
            }
            if Instant::now() >= deadline {
                fail("timed out waiting for claude in the foreground");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(lock(&monitor).attention().state(), AttentionState::Working);
        session.write("go\n");
        wait_for("waiting", || {
            lock(&monitor).attention().state() == AttentionState::WaitingForInput
        });
        let m = lock(&monitor);
        assert_eq!(m.attention().last_notification(), Some("Claude needs your permission"));
        assert!(m.attention().unread(), "the pane was never focused");
        drop(m);
        drop(session);
    }

    /// TEMPORARY, removed before merge: runs this whole test binary again and
    /// again, three test threads like a CI runner, and prints the output of
    /// every run where the fake claude test failed.
    #[test]
    fn tmp_fake_claude_stress() {
        if std::env::var_os("HALYARD_TMP_STRESS_CHILD").is_some() {
            return;
        }
        let me = std::env::current_exe().unwrap();
        let runs = 250;
        let mut failures = Vec::new();
        for i in 0..runs {
            let out = std::process::Command::new(&me)
                .args(["--test-threads=3", "--skip", "tmp_fake_claude_stress"])
                .env("HALYARD_TMP_STRESS_CHILD", "1")
                .output()
                .unwrap();
            if !out.status.success() {
                let text = format!(
                    "{}{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                );
                eprintln!("==== run {i} failed ====\n{text}");
                failures.push(i);
            }
        }
        if !failures.is_empty() {
            // Crash reports name the crashed program: a copy of this test
            // binary means a child crashed between fork and exec.
            std::thread::sleep(Duration::from_secs(10));
            let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
            for d in [
                home.join("Library/Logs/DiagnosticReports"),
                "/Library/Logs/DiagnosticReports".into(),
            ] {
                let Ok(entries) = std::fs::read_dir(&d) else {
                    eprintln!("no {}", d.display());
                    continue;
                };
                for e in entries.flatten() {
                    let fresh = e
                        .metadata()
                        .and_then(|m| m.modified())
                        .is_ok_and(|t| t.elapsed().is_ok_and(|a| a < Duration::from_secs(900)));
                    if fresh {
                        let text = std::fs::read_to_string(e.path()).unwrap_or_default();
                        let head: String = text.chars().take(3000).collect();
                        eprintln!("==== crash report {} ====\n{head}", e.path().display());
                    }
                }
            }
        }
        assert!(failures.is_empty(), "{} of {runs} runs failed: {failures:?}", failures.len());
    }

    /// The body of the fake agent: runs only inside the PTY, when the
    /// environment variable is set (the normal test run skips it).
    #[test]
    fn fake_agent_main() {
        if std::env::var_os("HALYARD_FAKE_AGENT").is_none() {
            return;
        }
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).ok();
        let mut out = std::io::stdout();
        std::io::Write::write_all(&mut out, b"\x1b]9;Claude needs your permission\x1b\\").ok();
        std::io::Write::flush(&mut out).ok();
        std::thread::sleep(Duration::from_secs(10));
    }
}
