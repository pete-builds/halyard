//! Runs the real integration scripts in real shells under a PTY and checks the marks
//! they emit with the scanner. A shell that is not installed is skipped with a
//! message on stderr.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use halyard_shell::{Scanner, Shell, ShellEvent, integration_script};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

const STEP_TIMEOUT: Duration = Duration::from_secs(20);

fn find_in_path(program: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths).map(|d| d.join(program)).find(|p| p.is_file())
}

fn tmp_dir(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("halyard-shell-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write_script(dir: &Path, shell: Shell) -> PathBuf {
    let p = dir.join(shell.script_file_name());
    std::fs::write(&p, integration_script(shell)).unwrap();
    p
}

struct Session {
    rx: Receiver<Vec<u8>>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    scanner: Scanner,
    events: Vec<ShellEvent>,
    raw: Vec<u8>,
}

impl Session {
    fn spawn(program: &Path, args: &[&str], dir: &Path, term_program: &str) -> Session {
        let pair = native_pty_system()
            .openpty(PtySize { rows: 40, cols: 250, pixel_width: 0, pixel_height: 0 })
            .expect("openpty");
        let mut cmd = CommandBuilder::new(program);
        cmd.args(args);
        cmd.env_clear();
        for key in ["PATH", "HOME", "USER", "LANG", "LC_ALL", "LC_CTYPE"] {
            if let Some(v) = std::env::var_os(key) {
                cmd.env(key, v);
            }
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("TERM_PROGRAM", term_program);
        cmd.env("HALYARD_SESSION_ID", "test-session");
        cmd.env("HISTFILE", dir.join("history"));
        cmd.env("BASH_SILENCE_DEPRECATION_WARNING", "1");
        cmd.env("fish_history", "");
        cmd.env("XDG_CONFIG_HOME", dir.join("config"));
        cmd.env("XDG_DATA_HOME", dir.join("data"));
        cmd.cwd(dir);
        let child = pair.slave.spawn_command(cmd).expect("spawn shell");
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().expect("reader");
        let writer = pair.master.take_writer().expect("writer");
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            // Keep the master alive as long as the reader runs.
            let _master = pair.master;
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Session { rx, writer, child, scanner: Scanner::new(), events: Vec::new(), raw: Vec::new() }
    }

    fn send(&mut self, line: &str) {
        self.writer.write_all(line.as_bytes()).unwrap();
        self.writer.write_all(b"\r").unwrap();
        self.writer.flush().unwrap();
    }

    fn pump(&mut self, deadline: Instant) -> bool {
        let left = deadline.saturating_duration_since(Instant::now());
        match self.rx.recv_timeout(left.max(Duration::from_millis(1))) {
            Ok(chunk) => {
                self.raw.extend_from_slice(&chunk);
                let events = &mut self.events;
                self.scanner.feed(&chunk, |_, e| events.push(e));
                true
            }
            Err(_) => false,
        }
    }

    /// Prompts fully drawn: both A and B seen.
    fn prompts(&self) -> usize {
        let count = |f: fn(&ShellEvent) -> bool| self.events.iter().filter(|e| f(e)).count();
        count(|e| matches!(e, ShellEvent::PromptStart { .. }))
            .min(count(|e| matches!(e, ShellEvent::CommandInputStart)))
    }

    /// Waits until `n` prompt starts have been seen.
    fn wait_prompts(&mut self, n: usize, what: &str) {
        let deadline = Instant::now() + STEP_TIMEOUT;
        while self.prompts() < n {
            self.pump(deadline);
            if self.prompts() < n && Instant::now() >= deadline {
                panic!(
                    "timed out waiting for prompt {n} after {what}; events {:?}\nraw: {:?}",
                    self.events,
                    String::from_utf8_lossy(&self.raw)
                );
            }
        }
    }

    /// Waits until `needle` appears in the raw output.
    fn wait_text(&mut self, needle: &str) {
        let deadline = Instant::now() + STEP_TIMEOUT;
        while !String::from_utf8_lossy(&self.raw).contains(needle) {
            if Instant::now() >= deadline {
                panic!(
                    "timed out waiting for {needle:?}; raw: {:?}",
                    String::from_utf8_lossy(&self.raw)
                );
            }
            self.pump(deadline);
        }
    }

    fn finish(mut self) {
        let _ = self.writer.write_all(b"exit\r");
        let _ = self.writer.flush();
        let deadline = Instant::now() + STEP_TIMEOUT;
        loop {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                return;
            }
            self.pump(Instant::now() + Duration::from_millis(50));
        }
    }
}

/// The marks only, with D's exit status.
fn marks(events: &[ShellEvent]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            ShellEvent::PromptStart { .. } => Some("A".to_string()),
            ShellEvent::CommandInputStart => Some("B".to_string()),
            ShellEvent::CommandExecuted => Some("C".to_string()),
            ShellEvent::CommandFinished { exit_code, .. } => Some(match exit_code {
                Some(c) => format!("D{c}"),
                None => "D".to_string(),
            }),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn cwds(events: &[ShellEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            ShellEvent::Cwd { path, .. } => Some(path.clone()),
            _ => None,
        })
        .collect()
}

fn command_lines(events: &[ShellEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            ShellEvent::CommandLine(c) => Some(c.clone()),
            _ => None,
        })
        .collect()
}

fn same_dir(reported: &str, expected: &Path) -> bool {
    let canon = std::fs::canonicalize(expected).unwrap();
    Path::new(reported) == expected || Path::new(reported) == canon
}

/// Drives a shell through the standard sequence and checks the marks.
///
/// `setup` installs the user's own hooks before the script is sourced; the hooks
/// print `USERHOOK` at each prompt and leave `$?` at 7, which must not leak into the
/// reported exit status.
fn exercise(shell: Shell, program: &Path, args: &[&str], setup: &str) {
    let dir = tmp_dir(&format!("{shell:?}"));
    let script = write_script(&dir, shell);
    let odd = dir.join("dir with space é");
    std::fs::create_dir_all(&odd).unwrap();
    let odd_str = odd.to_str().unwrap().to_string();

    let mut s = Session::spawn(program, args, &dir, "Halyard");
    s.send(setup);
    // Sourcing twice must not double any mark.
    let src = script.to_str().unwrap();
    s.send(&format!("source '{src}'; source '{src}'"));
    s.wait_prompts(1, "source");
    s.send("true");
    s.wait_prompts(2, "true");
    s.send("");
    s.wait_prompts(3, "empty line");
    s.send("false");
    s.wait_prompts(4, "false");
    s.send("cd /tmp");
    s.wait_prompts(5, "cd /tmp");
    s.send(&format!("cd '{odd_str}'"));
    s.wait_prompts(6, "cd odd dir");
    s.send("echo 'a;b\\c' | cat");
    s.wait_prompts(7, "pipeline");
    s.send("echo HOOKS-$__user_count");
    s.wait_prompts(8, "hook count");
    let events = s.events.clone();
    let raw = String::from_utf8_lossy(&s.raw).into_owned();
    s.finish();

    let after_source: Vec<ShellEvent> = events.clone();
    assert_eq!(
        marks(&after_source),
        "A B C D0 A B A B C D1 A B C D0 A B C D0 A B C D0 A B C D0 A B",
        "{shell:?} marks; raw output: {raw:?}"
    );

    let lines = command_lines(&events);
    assert_eq!(
        lines,
        vec![
            "true".to_string(),
            "false".to_string(),
            "cd /tmp".to_string(),
            format!("cd '{odd_str}'"),
            "echo 'a;b\\c' | cat".to_string(),
            "echo HOOKS-$__user_count".to_string(),
        ],
        "{shell:?} command lines"
    );

    let dirs = cwds(&events);
    assert_eq!(dirs.len(), 8, "{shell:?} one OSC 7 per prompt: {dirs:?}");
    assert!(same_dir(&dirs[0], &dir), "{shell:?} start dir {dirs:?}");
    assert!(same_dir(&dirs[4], Path::new("/tmp")), "{shell:?} /tmp: {dirs:?}");
    assert!(same_dir(&dirs[5], &odd), "{shell:?} odd dir: {dirs:?}");

    // The user's hooks still ran at every prompt and saw their own commands.
    assert!(raw.matches("USERHOOK").count() >= 7, "{shell:?} user prompt hook kept; raw: {raw:?}");
    let count: usize = raw
        .split("HOOKS-")
        .filter_map(|rest| {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok()
        })
        .max()
        .unwrap_or(0);
    assert!(count >= 6, "{shell:?} user preexec hook kept, count {count}; raw: {raw:?}");

    // Session id reached the shell.
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn zsh_integration() {
    let Some(zsh) = find_in_path("zsh") else {
        eprintln!("SKIPPED zsh_integration: zsh is not installed");
        return;
    };
    exercise(
        Shell::Zsh,
        &zsh,
        &["-f", "-i"],
        "typeset -gi __user_count=0; __user_pre() { __user_count+=1 }; \
         __user_precmd() { print -n USERHOOK; return 7 }; \
         preexec_functions+=(__user_pre); precmd_functions+=(__user_precmd)",
    );
}

#[test]
fn bash_integration() {
    let Some(bash) = find_in_path("bash") else {
        eprintln!("SKIPPED bash_integration: bash is not installed");
        return;
    };
    exercise(
        Shell::Bash,
        &bash,
        &["--norc", "--noprofile", "-i"],
        "__user_count=0; trap '__user_count=$((__user_count+1))' DEBUG; \
         PROMPT_COMMAND='printf USERHOOK; (exit 7)'",
    );
}

#[test]
fn fish_integration() {
    let Some(fish) = find_in_path("fish") else {
        eprintln!("SKIPPED fish_integration: fish is not installed");
        return;
    };
    exercise(
        Shell::Fish,
        &fish,
        &["--no-config", "-i"],
        "set -g __user_count 0; \
         function __user_pre --on-event fish_preexec; set -g __user_count (math $__user_count + 1); end; \
         function __user_prompt --on-event fish_prompt; printf USERHOOK; end",
    );
}

/// Outside Halyard the scripts do nothing at all.
fn inert(program: &Path, args: &[&str], shell: Shell) {
    let dir = tmp_dir(&format!("inert-{shell:?}"));
    let script = write_script(&dir, shell);
    let mut s = Session::spawn(program, args, &dir, "xterm");
    s.send(&format!("source '{}'", script.to_str().unwrap()));
    s.send("true");
    s.send("cd /tmp");
    s.send("echo INERT-$((40+2))");
    s.wait_text("INERT-42");
    let events = s.events.clone();
    s.finish();
    let found: Vec<_> = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                ShellEvent::PromptStart { .. }
                    | ShellEvent::CommandInputStart
                    | ShellEvent::CommandExecuted
                    | ShellEvent::CommandFinished { .. }
                    | ShellEvent::CommandLine(_)
                    | ShellEvent::Cwd { .. }
            )
        })
        .collect();
    assert!(found.is_empty(), "{shell:?} emitted {found:?} outside Halyard");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn scripts_are_inert_outside_halyard() {
    let mut ran = 0;
    for (name, args, shell) in [
        ("zsh", &["-f", "-i"][..], Shell::Zsh),
        ("bash", &["--norc", "--noprofile", "-i"][..], Shell::Bash),
    ] {
        match find_in_path(name) {
            Some(p) => {
                inert(&p, args, shell);
                ran += 1;
            }
            None => eprintln!("SKIPPED inert check for {name}: not installed"),
        }
    }
    eprintln!("inert check ran for {ran} shells");
}

#[test]
fn scripts_are_inert_in_non_interactive_shells() {
    let dir = tmp_dir("noninteractive");
    for (name, shell, probe) in [
        ("zsh", Shell::Zsh, "echo \"[${__halyard_zsh_loaded-}]\""),
        ("bash", Shell::Bash, "echo \"[${__halyard_bash_loaded-}]\""),
    ] {
        let Some(program) = find_in_path(name) else {
            eprintln!("SKIPPED non-interactive check for {name}: not installed");
            continue;
        };
        let script = write_script(&dir, shell);
        let out = std::process::Command::new(program)
            .arg("-c")
            .arg(format!("source '{}'; {probe}", script.display()))
            .env("TERM_PROGRAM", "Halyard")
            .output()
            .unwrap();
        assert!(out.status.success(), "{name}: {out:?}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "[]\n", "{name}");
        assert!(out.stderr.is_empty(), "{name}: {out:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
