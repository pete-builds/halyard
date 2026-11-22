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

    fn chunk_done(&mut self, history_rows: Option<u64>) {
        let now = SystemTime::now();
        let mut m = lock(&self.monitor);
        self.scan.finish(&mut m, now);
        if let Some(rows) = history_rows {
            m.on_history(rows);
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
        let (shell, program) = ["/bin/zsh", "/usr/bin/zsh", "/bin/bash", "/usr/bin/bash"]
            .iter()
            .find(|p| std::path::Path::new(p).exists())
            .map(|p| (Shell::from_program(p).unwrap(), PathBuf::from(p)))
            .expect("zsh or bash");
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
        // Interactive, so the shell gives the job its own process group.
        let session = spawn(sh, vec!["-i".into()], &monitor, "fake");
        let pid = session.pid().expect("pid");
        session.write(format!(
            "HALYARD_FAKE_AGENT=1 '{}' --exact signals::tests::fake_agent_main --nocapture\n",
            claude.display()
        ));
        wait_for("claude in the foreground", || {
            let fg = poll_foreground(pid, &[]);
            if let Some(fg) = &fg {
                lock(&monitor).on_foreground(fg, SystemTime::now());
            }
            lock(&monitor).agent_detected()
        });
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
