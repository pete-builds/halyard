//! Runs the real integration scripts in real shells under a PTY and checks the marks
//! they emit with the scanner. A shell that is not installed is skipped with a
//! message on stderr, except when the `CI` environment variable is set: CI installs
//! zsh, bash and fish, so a missing shell there is a failure, not a silent skip.

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

/// The shell's path, or `None` (with a message) when it is missing outside CI.
fn require_shell(name: &str, test: &str) -> Option<PathBuf> {
    match find_in_path(name) {
        Some(p) => Some(p),
        None if std::env::var_os("CI").is_some() => {
            panic!("{test}: {name} is not installed, and CI must run this test")
        }
        None => {
            eprintln!("SKIPPED {test}: {name} is not installed");
            None
        }
    }
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
    /// Events before this index happened before the script was ready.
    base: usize,
    /// Event count when the `READY1` marker first appeared in the output.
    ready_at: Option<usize>,
}

impl Session {
    fn spawn(
        program: &Path,
        args: &[&str],
        dir: &Path,
        term_program: &str,
        extra_env: &[(&str, &str)],
    ) -> Session {
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
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
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
        Session {
            rx,
            writer,
            child,
            scanner: Scanner::new(),
            events: Vec::new(),
            raw: Vec::new(),
            base: 0,
            ready_at: None,
        }
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
                let before = self.events.len();
                self.raw.extend_from_slice(&chunk);
                let events = &mut self.events;
                self.scanner.feed(&chunk, |_, e| events.push(e));
                // Answer the primary device attributes query, as a terminal does;
                // fish 4 waits for the answer before drawing its first prompt.
                if chunk.windows(3).any(|w| w == b"\x1b[c")
                    || chunk.windows(4).any(|w| w == b"\x1b[0c")
                {
                    let _ = self.writer.write_all(b"\x1b[?62;22c");
                    let _ = self.writer.flush();
                }
                if self.ready_at.is_none() && String::from_utf8_lossy(&self.raw).contains("READY1")
                {
                    self.ready_at = Some(before);
                }
                true
            }
            Err(_) => false,
        }
    }

    /// Events since the script was ready.
    fn events(&self) -> &[ShellEvent] {
        &self.events[self.base..]
    }

    /// Prompts fully drawn since the script was ready: both A and B seen.
    fn prompts(&self) -> usize {
        let count = |f: fn(&ShellEvent) -> bool| self.events().iter().filter(|e| f(e)).count();
        count(|e| matches!(e, ShellEvent::PromptStart { .. }))
            .min(count(|e| matches!(e, ShellEvent::CommandInputStart)))
    }

    fn timeout(&self, what: &str) -> ! {
        panic!(
            "timed out waiting for {what}; events {:?}\nraw: {:?}",
            self.events,
            String::from_utf8_lossy(&self.raw)
        )
    }

    /// Waits until `n` prompts have been drawn since the script was ready.
    fn wait_prompts(&mut self, n: usize, what: &str) {
        let deadline = Instant::now() + STEP_TIMEOUT;
        while self.prompts() < n {
            self.pump(deadline);
            if self.prompts() < n && Instant::now() >= deadline {
                self.timeout(&format!("prompt {n} after {what}"));
            }
        }
    }

    /// Runs `echo READY""1` and waits for the prompt after it. Everything before
    /// that prompt (setup lines, sourcing, fish's own marks) is set aside, so the
    /// shells can be compared from the same starting point.
    fn wait_ready(&mut self) {
        self.send("echo READY\"\"1");
        let deadline = Instant::now() + STEP_TIMEOUT;
        loop {
            if let Some(at) = self.ready_at
                && let Some(d) = self.events[at..]
                    .iter()
                    .position(|e| matches!(e, ShellEvent::CommandFinished { .. }))
            {
                let base = at + d + 1;
                let after = &self.events[base..];
                let a = after.iter().any(|e| matches!(e, ShellEvent::PromptStart { .. }));
                let b = after.iter().any(|e| matches!(e, ShellEvent::CommandInputStart));
                if a && b {
                    self.base = base;
                    return;
                }
            }
            if Instant::now() >= deadline {
                self.timeout("the READY1 marker and the prompt after it");
            }
            self.pump(deadline);
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
            ShellEvent::Cwd(c) => Some(c.path.clone()),
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

const BASH_ARGS: &[&str] = &["--norc", "--noprofile", "-i"];

/// Spawns `program`, runs `setup` (the user's own configuration), sources the
/// script twice (sourcing twice must not double any mark), and waits until it is
/// ready. `history` lines are written to the history file first.
fn start(
    shell: Shell,
    name: &str,
    program: &Path,
    args: &[&str],
    setup: &str,
    history: &[String],
    env: &[(&str, &str)],
) -> (Session, PathBuf) {
    let dir = tmp_dir(name);
    let script = write_script(&dir, shell);
    if !history.is_empty() {
        std::fs::write(dir.join("history"), history.join("\n") + "\n").unwrap();
    }
    let mut s = Session::spawn(program, args, &dir, "Halyard", env);
    if !setup.is_empty() {
        s.send(setup);
    }
    let src = script.to_str().unwrap();
    s.send(&format!("source '{src}'; source '{src}'"));
    s.wait_ready();
    (s, dir)
}

/// Drives a shell through the standard sequence and checks the marks.
///
/// `setup` installs the user's own hooks before the script is sourced; the hooks
/// print `USERHOOK` at each prompt and leave `$?` at 7, which must not leak into the
/// reported exit status.
fn exercise(shell: Shell, program: &Path, args: &[&str], setup: &str) {
    let (mut s, dir) = start(shell, &format!("{shell:?}"), program, args, setup, &[], &[]);
    let odd = dir.join("dir with space é");
    std::fs::create_dir_all(&odd).unwrap();
    let odd_str = odd.to_str().unwrap().to_string();

    s.wait_prompts(1, "ready");
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
    let events = s.events().to_vec();
    let raw = String::from_utf8_lossy(&s.raw).into_owned();
    s.finish();

    assert_eq!(
        marks(&events),
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

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn zsh_integration() {
    let Some(zsh) = require_shell("zsh", "zsh_integration") else {
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
    let Some(bash) = require_shell("bash", "bash_integration") else {
        return;
    };
    exercise(
        Shell::Bash,
        &bash,
        BASH_ARGS,
        "__user_count=0; trap '__user_count=$((__user_count+1))' DEBUG; \
         PROMPT_COMMAND='printf USERHOOK; (exit 7)'",
    );
}

#[test]
fn fish_integration() {
    let Some(fish) = require_shell("fish", "fish_integration") else {
        return;
    };
    let version = std::process::Command::new(&fish).arg("--version").output().unwrap();
    eprintln!("fish_integration: {}", String::from_utf8_lossy(&version.stdout).trim());
    exercise(
        Shell::Fish,
        &fish,
        &["--no-config", "-i"],
        "set -g __user_count 0; \
         function __user_pre --on-event fish_preexec; set -g __user_count (math $__user_count + 1); end; \
         function __user_prompt --on-event fish_prompt; printf USERHOOK; end",
    );
}

/// bash: command lines stay exact past history entry 9, 99 and 999.
#[test]
fn bash_command_lines_with_long_history() {
    let Some(bash) = require_shell("bash", "bash_command_lines_with_long_history") else {
        return;
    };
    for seed in [15usize, 1200] {
        let history: Vec<String> = (0..seed).map(|i| format!("seed_command_{i}")).collect();
        let env = [("HISTSIZE", "5000"), ("HISTFILESIZE", "5000")];
        let name = format!("bash-history-{seed}");
        let (mut s, dir) = start(Shell::Bash, &name, &bash, BASH_ARGS, "", &history, &env);
        s.send("echo 8");
        s.wait_prompts(2, "echo 8");
        s.send("true make");
        s.wait_prompts(3, "true make");
        s.send("echo 'x  y' 42");
        s.wait_prompts(4, "echo x y");
        let events = s.events().to_vec();
        s.finish();
        assert_eq!(
            command_lines(&events),
            vec!["echo 8", "true make", "echo 'x  y' 42"],
            "{seed} history entries"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// bash: the `  123* ` prefix of `history 1` is stripped exactly.
#[test]
fn bash_history_prefix_stripping() {
    let Some(bash) = require_shell("bash", "bash_history_prefix_stripping") else {
        return;
    };
    let dir = tmp_dir("bash-strip");
    let script = write_script(&dir, Shell::Bash);
    let cases = [
        ("    1  a", "a"),
        ("   12  b c", "b c"),
        ("  123* d", "d"),
        (" 1234  echo 8", "echo 8"),
        ("99999  e  f", "e  f"),
        ("    7  42", "42"),
    ];
    let mut program = format!("source '{}'", script.display());
    for (input, _) in cases {
        program +=
            &format!("; __halyard_strip_history_number '{input}'; echo \"[$__halyard_line]\"");
    }
    let out = std::process::Command::new(bash)
        .args(["--norc", "--noprofile", "-i", "-c", &program])
        .env("TERM_PROGRAM", "Halyard")
        .env("HISTFILE", dir.join("h"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let want: String = cases.iter().map(|(_, w)| format!("[{w}]\n")).collect();
    assert_eq!(stdout, want, "{out:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// bash: `$_` survives the DEBUG trap and PROMPT_COMMAND.
#[test]
fn bash_keeps_dollar_underscore() {
    let Some(bash) = require_shell("bash", "bash_keeps_dollar_underscore") else {
        return;
    };
    let (mut s, dir) = start(Shell::Bash, "bash-underscore", &bash, BASH_ARGS, "", &[], &[]);
    s.send("true first_arg");
    s.wait_prompts(2, "true first_arg");
    s.send("echo \"U2=$_\"");
    s.wait_prompts(3, "echo U2");
    s.send("true second_arg && echo \"U1=$_\"");
    s.wait_prompts(4, "echo U1");
    s.send("mkdir -p dir_x && cd \"$_\"");
    s.wait_prompts(5, "mkdir and cd");
    let events = s.events().to_vec();
    let raw = String::from_utf8_lossy(&s.raw).into_owned();
    s.finish();
    assert!(raw.contains("U2=first_arg\r\n"), "across prompts; raw {raw:?}");
    assert!(raw.contains("U1=second_arg\r\n"), "within a line; raw {raw:?}");
    let last = cwds(&events).pop().unwrap();
    assert!(last.ends_with("/dir_x"), "cd \"$_\" went to {last}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// bash: with HISTCONTROL=ignoreboth (Ubuntu's default) a repeated command and a
/// space-prefixed one are not saved in history, and must still be marked.
#[test]
fn bash_ignoreboth_repeats_are_marked() {
    let Some(bash) = require_shell("bash", "bash_ignoreboth_repeats_are_marked") else {
        return;
    };
    let setup = "HISTCONTROL=ignoreboth; make() { echo MADE; }";
    let (mut s, dir) = start(Shell::Bash, "bash-ignoreboth", &bash, BASH_ARGS, setup, &[], &[]);
    s.send("make");
    s.wait_prompts(2, "make");
    s.send("make");
    s.wait_prompts(3, "make again");
    s.send(" echo hidden");
    s.wait_prompts(4, "space-prefixed");
    s.send("");
    s.wait_prompts(5, "empty line");
    let events = s.events().to_vec();
    s.finish();
    assert_eq!(marks(&events), "A B C D0 A B C D0 A B C D0 A B A B");
    // Only the first `make` reached history, so only it has a command line.
    assert_eq!(command_lines(&events), vec!["make"]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Outside Halyard the scripts do nothing at all.
fn inert(program: &Path, args: &[&str], shell: Shell) {
    let dir = tmp_dir(&format!("inert-{shell:?}"));
    let script = write_script(&dir, shell);
    let mut s = Session::spawn(program, args, &dir, "xterm", &[]);
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
                    | ShellEvent::Cwd(_)
            )
        })
        .collect();
    assert!(found.is_empty(), "{shell:?} emitted {found:?} outside Halyard");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn scripts_are_inert_outside_halyard() {
    for (name, args, shell) in
        [("zsh", &["-f", "-i"][..], Shell::Zsh), ("bash", BASH_ARGS, Shell::Bash)]
    {
        if let Some(p) = require_shell(name, "scripts_are_inert_outside_halyard") {
            inert(&p, args, shell);
        }
    }
}

#[test]
fn scripts_are_inert_in_non_interactive_shells() {
    let dir = tmp_dir("noninteractive");
    for (name, shell, probe) in [
        ("zsh", Shell::Zsh, "echo \"[${__halyard_zsh_loaded-}]\""),
        ("bash", Shell::Bash, "echo \"[${__halyard_bash_loaded-}]\""),
    ] {
        let Some(program) = require_shell(name, "scripts_are_inert_in_non_interactive_shells")
        else {
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
