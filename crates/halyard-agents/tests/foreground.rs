//! Finds the foreground process of a real PTY and identifies it. Runs on macOS and
//! Linux (the same test exercises `proc_pidpath`/`KERN_PROCARGS2` on macOS and
//! `/proc` on Linux).
#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::cell::RefCell;
use std::fmt;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use halyard_agents::platform::{foreground_process, foreground_process_group};
use halyard_agents::{AgentKind, detect_agent};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

struct Pty {
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    trace: Trace,
}

/// Every foreground read a test made, in order, printed when an assertion fails
/// so a flaky failure explains itself. Repeats of the same reading are counted
/// rather than listed.
struct Trace {
    start: Instant,
    /// (milliseconds since the PTY opened, what was read, how many times in a row)
    lines: RefCell<Vec<(u128, String, usize)>>,
}

impl Trace {
    fn new() -> Trace {
        Trace { start: Instant::now(), lines: RefCell::new(Vec::new()) }
    }

    fn note(&self, what: String) {
        let mut lines = self.lines.borrow_mut();
        match lines.last_mut() {
            Some((_, last, n)) if *last == what => *n += 1,
            _ => lines.push((self.start.elapsed().as_millis(), what, 1)),
        }
    }
}

impl fmt::Display for Trace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "foreground reads, oldest first:")?;
        for (ms, what, n) in self.lines.borrow().iter() {
            let repeat = if *n > 1 { format!("  (x{n})") } else { String::new() };
            writeln!(f, "  {ms:>6} ms  {what}{repeat}")?;
        }
        Ok(())
    }
}

impl Pty {
    fn spawn(cmd: CommandBuilder) -> Pty {
        let pair = native_pty_system().openpty(PtySize::default()).expect("openpty");
        let child = pair.slave.spawn_command(cmd).expect("spawn");
        drop(pair.slave);
        // Drain output so the child never blocks on a full PTY buffer.
        let mut reader = pair.master.try_clone_reader().expect("reader");
        std::thread::spawn(move || {
            let mut buf = [0u8; 1024];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
            }
        });
        Pty { master: pair.master, child, trace: Trace::new() }
    }

    fn fd(&self) -> i32 {
        self.master.as_raw_fd().expect("master fd")
    }

    /// Polls until the foreground group satisfies `pred`.
    fn wait_foreground(&self, pred: impl Fn(i32) -> bool) -> i32 {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let read = foreground_process_group(self.fd());
            self.trace.note(format!("tcgetpgrp: {read:?}"));
            if let Ok(pg) = read
                && pred(pg)
            {
                return pg;
            }
            assert!(Instant::now() < deadline, "foreground never matched\n{}", self.trace);
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Reads the foreground process once, recording what was seen.
    fn read_foreground(&self) -> std::io::Result<halyard_agents::ProcessInfo> {
        let read = foreground_process(self.fd());
        self.trace.note(format!("foreground_process: {read:?}"));
        read
    }
}

impl Pty {
    /// Reads the foreground process once it has finished exec'ing: right after
    /// fork it is still a copy of this test binary.
    fn exec_done_info(&self) -> halyard_agents::ProcessInfo {
        self.exec_done_info_from(&std::env::current_exe().unwrap())
    }

    /// Reads the foreground process once it is no longer a copy of `forked_from`.
    /// Between fork and exec the new group leader already holds the terminal but
    /// still runs its parent's image: this test binary for a direct spawn, the
    /// shell for a job the shell started. A read in that window is accurate, it
    /// just describes the program before the exec.
    fn exec_done_info_from(&self, forked_from: &Path) -> halyard_agents::ProcessInfo {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(info) = self.read_foreground()
                && !info.argv.is_empty()
                && info.exe.as_deref().and_then(Path::file_name) != forked_from.file_name()
            {
                return info;
            }
            assert!(
                Instant::now() < deadline,
                "foreground never stopped being a copy of {forked_from:?}\n{}",
                self.trace
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn find_in_path(program: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths).map(|d| d.join(program)).find(|p| p.is_file())
}

fn scratch(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("halyard-agents-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn identifies_sleep_as_the_foreground_process() {
    let sleep = find_in_path("sleep").expect("sleep is on PATH");
    let mut cmd = CommandBuilder::new(&sleep);
    cmd.arg("5");
    let pty = Pty::spawn(cmd);
    let pid = pty.child.process_id().expect("child pid") as i32;

    let pg = pty.wait_foreground(|pg| pg == pid);
    assert_eq!(pg, pid);
    let info = pty.exec_done_info();
    assert_eq!(info.pid, pid);
    assert_eq!(info.argv, vec![sleep.to_str().unwrap().to_string(), "5".into()]);
    assert_eq!(detect_agent(&info), None);
    let exe = info.exe.expect("exe path readable for our own child");
    // The executable is the resolved file: `sleep` itself on macOS, but a multicall
    // binary (`coreutils`, `busybox`) on NixOS and some Linux systems.
    assert_eq!(exe, std::fs::canonicalize(&sleep).unwrap(), "argv[0] {:?}", info.argv[0]);
}

#[test]
fn follows_the_foreground_job_of_an_interactive_shell() {
    let bash = find_in_path("bash").expect("bash is on PATH");
    let mut cmd = CommandBuilder::new(&bash);
    cmd.args(["--norc", "--noprofile", "-i"]);
    cmd.env("BASH_SILENCE_DEPRECATION_WARNING", "1");
    cmd.env("HISTFILE", "/dev/null");
    let pty = Pty::spawn(cmd);
    let shell_pid = pty.child.process_id().expect("pid") as i32;
    pty.wait_foreground(|pg| pg == shell_pid);
    let shell = pty.exec_done_info();
    let t = &pty.trace;
    assert_eq!(shell.pid, shell_pid, "{t}");
    let shell_exe = shell.exe.expect("exe path readable for our own child");
    assert_eq!(shell_exe, std::fs::canonicalize(&bash).unwrap(), "{t}");

    let mut w = pty.master.take_writer().expect("writer");
    w.write_all(b"sleep 7\r").unwrap();
    w.flush().unwrap();
    let job = pty.wait_foreground(|pg| pg != shell_pid);
    // The shell hands the terminal to the job before the job execs `sleep`
    // (seen on Ubuntu CI and on macOS under load): wait for that exec rather than
    // taking the first read, which can still be the shell's fork.
    let info = pty.exec_done_info_from(&shell_exe);
    let t = &pty.trace;
    assert_eq!(info.pid, job, "{t}");
    assert_eq!(info.argv, vec!["sleep".to_string(), "7".into()], "{t}");
    // The shell ran the `sleep` this test finds on the same PATH.
    let sleep = find_in_path("sleep").expect("sleep is on PATH");
    assert_eq!(info.exe, Some(std::fs::canonicalize(&sleep).unwrap()), "{t}");
    assert_eq!(detect_agent(&info), None, "{t}");

    // Ctrl+C ends the job and the shell is foreground again.
    w.write_all(b"\x03").unwrap();
    w.flush().unwrap();
    pty.wait_foreground(|pg| pg == shell_pid);
    let back = pty.read_foreground().expect("shell info");
    let t = &pty.trace;
    assert_eq!(back.pid, shell_pid, "{t}");
    assert_eq!(detect_agent(&back), None, "{t}");
}

#[test]
fn detects_an_agent_by_its_program_name() {
    // A program invoked as `claude` is Claude Code by argv[0], even though the
    // resolved executable is something else. The program is the crate's own sleeper:
    // `sleep` is a multicall binary on NixOS and busybox and would exit at once
    // when called `claude`.
    let dir = scratch("symlink");
    let sleeper = Path::new(env!("CARGO_BIN_EXE_halyard-test-sleeper"));
    let claude = dir.join("claude");
    std::os::unix::fs::symlink(sleeper, &claude).unwrap();
    let mut cmd = CommandBuilder::new(&claude);
    cmd.arg("30");
    let mut pty = Pty::spawn(cmd);
    let pid = pty.child.process_id().unwrap() as i32;
    pty.wait_foreground(|pg| pg == pid);
    let info = pty.exec_done_info();
    assert_eq!(info.argv, vec![claude.to_str().unwrap().to_string(), "30".into()]);
    assert_eq!(info.exe, Some(std::fs::canonicalize(sleeper).unwrap()));
    assert_eq!(detect_agent(&info), Some(AgentKind::ClaudeCode), "{info:?}");
    // Still running: the check did not race a process that had already exited.
    assert!(pty.child.try_wait().unwrap().is_none());
    drop(pty);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn detects_a_node_wrapped_agent() {
    let Some(node) = find_in_path("node") else {
        eprintln!("SKIPPED detects_a_node_wrapped_agent: node is not installed");
        return;
    };
    let dir = scratch("node");
    let script = dir.join("codex");
    std::fs::write(&script, "setTimeout(() => {}, 5000);\n").unwrap();
    let mut cmd = CommandBuilder::new(&node);
    cmd.arg(&script);
    let pty = Pty::spawn(cmd);
    let pid = pty.child.process_id().unwrap() as i32;
    pty.wait_foreground(|pg| pg == pid);
    let info = pty.exec_done_info();
    assert_eq!(detect_agent(&info), Some(AgentKind::Codex), "{info:?}");
    drop(pty);
    let _ = std::fs::remove_dir_all(&dir);
}
