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
    /// Bytes of `raw` already searched for terminal queries.
    answered_upto: usize,
    /// Terminal queries answered.
    answers: usize,
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
            answered_upto: 0,
            answers: 0,
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
                self.answer_queries();
                if self.ready_at.is_none() && String::from_utf8_lossy(&self.raw).contains("READY1")
                {
                    self.ready_at = Some(before);
                }
                true
            }
            Err(_) => false,
        }
    }

    /// Answers the terminal queries shells send, the way a terminal does: primary
    /// device attributes, cursor position (DSR 6) and the OSC 11 background color.
    /// fish 4 sends all three at every prompt, waits for DA1 before its first prompt,
    /// and repaints when answers are missing. Queries split across chunks are found.
    fn answer_queries(&mut self) {
        const QUERIES: [(&[u8], &[u8]); 4] = [
            (b"\x1b[c", b"\x1b[?62;22c"),
            (b"\x1b[0c", b"\x1b[?62;22c"),
            (b"\x1b[6n", b"\x1b[1;1R"),
            (b"\x1b]11;?", b"\x1b]11;rgb:0000/0000/0000\x1b\\"),
        ];
        const LONGEST: usize = 6;
        let mut pos = self.answered_upto;
        loop {
            let hay = &self.raw[pos..];
            let next = QUERIES
                .iter()
                .filter_map(|(q, a)| {
                    hay.windows(q.len()).position(|w| w == *q).map(|i| (pos + i, q.len(), *a))
                })
                .min_by_key(|(i, _, _)| *i);
            let Some((at, len, answer)) = next else {
                break;
            };
            let _ = self.writer.write_all(answer);
            let _ = self.writer.flush();
            self.answers += 1;
            pos = at + len;
        }
        // Leave a tail unsearched in case a query is split across chunks.
        self.answered_upto = pos.max(self.raw.len().saturating_sub(LONGEST - 1));
    }

    /// Events since the script was ready.
    fn events(&self) -> &[ShellEvent] {
        &self.events[self.base..]
    }

    /// Prompts drawn since the script was ready, counting a redrawn prompt (A and B
    /// again with no command in between) once: the B marks of the collapsed marks.
    fn prompts(&self) -> usize {
        collapse_redraws(&mark_list(self.events())).iter().filter(|m| **m == Mark::B).count()
    }

    /// Raw B marks since the script was ready, redraws included.
    fn raw_input_starts(&self) -> usize {
        self.events().iter().filter(|e| matches!(e, ShellEvent::CommandInputStart)).count()
    }

    /// Sends an empty line and waits for the prompt to be drawn again. An empty line
    /// is not a command, so in the collapsed count it is the same prompt.
    fn send_empty_line(&mut self) {
        let before = self.raw_input_starts();
        self.send("");
        let deadline = Instant::now() + STEP_TIMEOUT;
        while self.raw_input_starts() <= before {
            self.pump(deadline);
            if self.raw_input_starts() <= before && Instant::now() >= deadline {
                self.timeout("the prompt after an empty line");
            }
        }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mark {
    A,
    B,
    C,
    D(Option<i32>),
}

fn mark_list(events: &[ShellEvent]) -> Vec<Mark> {
    events
        .iter()
        .filter_map(|e| match e {
            ShellEvent::PromptStart { .. } => Some(Mark::A),
            ShellEvent::CommandInputStart => Some(Mark::B),
            ShellEvent::CommandExecuted => Some(Mark::C),
            ShellEvent::CommandFinished { exit_code, .. } => Some(Mark::D(*exit_code)),
            _ => None,
        })
        .collect()
}

/// Drops prompt redraws: an A that follows a B with nothing executed in between
/// (and the B after it), and a repeated B. The block model treats these the same
/// way (`blocks::tests::repeated_prompt_marks_are_one_redrawn_prompt`). An empty
/// line also collapses, since it is not a command either.
fn collapse_redraws(marks: &[Mark]) -> Vec<Mark> {
    let mut out: Vec<Mark> = Vec::new();
    let mut in_redraw = false;
    for &m in marks {
        match m {
            Mark::A if out.last() == Some(&Mark::B) => in_redraw = true,
            Mark::B if in_redraw => in_redraw = false,
            Mark::B if out.last() == Some(&Mark::B) => {}
            _ => {
                in_redraw = false;
                out.push(m);
            }
        }
    }
    out
}

fn marks_text(marks: &[Mark]) -> String {
    marks
        .iter()
        .map(|m| match m {
            Mark::A => "A".to_string(),
            Mark::B => "B".to_string(),
            Mark::C => "C".to_string(),
            Mark::D(Some(c)) => format!("D{c}"),
            Mark::D(None) => "D".to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The marks exactly as emitted, with D's exit status.
fn marks(events: &[ShellEvent]) -> String {
    marks_text(&mark_list(events))
}

/// The marks with prompt redraws collapsed.
fn collapsed_marks(events: &[ShellEvent]) -> String {
    marks_text(&collapse_redraws(&mark_list(events)))
}

#[test]
fn collapsing_redraws() {
    use Mark::*;
    let fish = [A, B, C, D(Some(0)), A, B, A, B, C, D(Some(1)), A, B, B, A, B];
    assert_eq!(marks_text(&collapse_redraws(&fish)), "A B C D0 A B C D1 A B");
    // Nothing to collapse.
    let plain = [A, B, C, D(Some(0)), A, B];
    assert_eq!(collapse_redraws(&plain), plain.to_vec());
    // A prompt without B (some scripts) followed by a command is kept.
    assert_eq!(marks_text(&collapse_redraws(&[A, C, D(None), A, B])), "A C D A B");
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
///
/// With `exact`, the raw marks must match too (zsh and bash never redraw); without
/// it only the collapsed marks are checked (fish 4 repaints its prompt).
fn exercise(
    shell: Shell,
    program: &Path,
    args: &[&str],
    setup: &str,
    exact: bool,
    env: &[(&str, &str)],
) -> Vec<ShellEvent> {
    let name = format!("{shell:?}-{}", env.len());
    let (mut s, dir) = start(shell, &name, program, args, setup, &[], env);
    let odd = dir.join("dir with space é");
    std::fs::create_dir_all(&odd).unwrap();
    let odd_str = odd.to_str().unwrap().to_string();

    s.wait_prompts(1, "ready");
    s.send("true");
    s.wait_prompts(2, "true");
    s.send_empty_line();
    s.send("false");
    s.wait_prompts(3, "false");
    s.send("cd /tmp");
    s.wait_prompts(4, "cd /tmp");
    s.send(&format!("cd '{odd_str}'"));
    s.wait_prompts(5, "cd odd dir");
    s.send("echo 'a;b\\c' | cat");
    s.wait_prompts(6, "pipeline");
    s.send("echo HOOKS-$__user_count");
    s.wait_prompts(7, "hook count");
    let events = s.events().to_vec();
    let raw = String::from_utf8_lossy(&s.raw).into_owned();
    let answers = s.answers;
    s.finish();
    eprintln!("{shell:?}: answered {answers} terminal queries; raw marks {}", marks(&events));

    assert_eq!(
        collapsed_marks(&events),
        "A B C D0 A B C D1 A B C D0 A B C D0 A B C D0 A B C D0 A B",
        "{shell:?} marks; raw marks {}; raw output: {raw:?}",
        marks(&events)
    );
    if exact {
        assert_eq!(
            marks(&events),
            "A B C D0 A B A B C D1 A B C D0 A B C D0 A B C D0 A B C D0 A B",
            "{shell:?} raw marks; raw output: {raw:?}"
        );
    }

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

    let mut dirs = cwds(&events);
    if exact {
        assert_eq!(dirs.len(), 8, "{shell:?} one OSC 7 per prompt: {dirs:?}");
    }
    dirs.dedup();
    assert_eq!(dirs.len(), 3, "{shell:?} start, /tmp, odd dir: {dirs:?}");
    assert!(same_dir(&dirs[0], &dir), "{shell:?} start dir {dirs:?}");
    assert!(same_dir(&dirs[1], Path::new("/tmp")), "{shell:?} /tmp: {dirs:?}");
    assert!(same_dir(&dirs[2], &odd), "{shell:?} odd dir: {dirs:?}");

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
    events
}

#[test]
fn zsh_integration() {
    let Some(zsh) = require_shell("zsh", "zsh_integration") else {
        return;
    };
    let _ = exercise(
        Shell::Zsh,
        &zsh,
        &["-f", "-i"],
        "typeset -gi __user_count=0; __user_pre() { __user_count+=1 }; \
         __user_precmd() { print -n USERHOOK; return 7 }; \
         preexec_functions+=(__user_pre); precmd_functions+=(__user_precmd)",
        true,
        &[],
    );
}

#[test]
fn bash_integration() {
    let Some(bash) = require_shell("bash", "bash_integration") else {
        return;
    };
    let _ = exercise(
        Shell::Bash,
        &bash,
        BASH_ARGS,
        "__user_count=0; trap '__user_count=$((__user_count+1))' DEBUG; \
         PROMPT_COMMAND='printf USERHOOK; (exit 7)'",
        true,
        &[],
    );
}

#[test]
fn fish_integration() {
    // fish 4 marks prompts itself (no `aid=`); fish 3 gets the script's marks.
    if let Some((major, _, events)) = fish_exercise("fish_integration", &[]) {
        assert_eq!(prompt_aids(&events), major < 4, "fish {major}: who emitted the marks");
    }
}

/// fish 4 with its own marking turned off: the script must emit the marks itself.
/// On fish 3 the setting is unknown and changes nothing.
#[test]
fn fish_integration_without_native_marks() {
    let env = [("fish_features", "no-mark-prompt")];
    if let Some((major, minor, events)) =
        fish_exercise("fish_integration_without_native_marks", &env)
    {
        // fish 4.0 has no `mark-prompt` feature flag (`status test-feature` returns 2), so
        // `no-mark-prompt` cannot turn its marking off and fish's own aid-less marks stay.
        let cannot_disable = major == 4 && minor == 0;
        assert_eq!(
            prompt_aids(&events),
            !cannot_disable,
            "fish {major}.{minor}: who emitted the marks"
        );
    }
}

/// True when every prompt mark carries `aid=` (the script's marks), false when none
/// do (fish's own). A mix fails the test.
fn prompt_aids(events: &[ShellEvent]) -> bool {
    let aids: Vec<bool> = events
        .iter()
        .filter_map(|e| match e {
            ShellEvent::PromptStart { aid } => Some(aid.is_some()),
            _ => None,
        })
        .collect();
    assert!(!aids.is_empty());
    assert!(aids.iter().all(|a| *a == aids[0]), "mixed prompt marks: {aids:?}");
    aids[0]
}

const FISH_SETUP: &str = "set -g __user_count 0; \
     function __user_pre --on-event fish_preexec; set -g __user_count (math $__user_count + 1); end; \
     function __user_prompt --on-event fish_prompt; printf USERHOOK; end";

/// Runs the standard sequence in fish; returns fish's major and minor version and the
/// events. The standard sequence waits for a B mark, so on fish 4.0 to 4.2 (which send
/// none) it only passes because the script appends one.
fn fish_exercise(test: &str, env: &[(&str, &str)]) -> Option<(u32, u32, Vec<ShellEvent>)> {
    let fish = require_shell("fish", test)?;
    let out = std::process::Command::new(&fish).arg("--version").output().unwrap();
    let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
    eprintln!("{test}: {version}");
    let mut parts = version.rsplit(' ').next().expect("fish version").split('.');
    let mut next =
        || -> u32 { parts.next().and_then(|m| m.parse().ok()).expect("fish version number") };
    let (major, minor) = (next(), next());
    let events = exercise(Shell::Fish, &fish, &["--no-config", "-i"], FISH_SETUP, false, env);
    Some((major, minor, events))
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
    s.send_empty_line();
    let events = s.events().to_vec();
    s.finish();
    assert_eq!(marks(&events), "A B C D0 A B C D0 A B C D0 A B A B");
    // Only the first `make` reached history, so only it has a command line.
    assert_eq!(command_lines(&events), vec!["make"]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// bash: a compound command another tool puts in front of PROMPT_COMMAND after the
/// script was sourced is never taken for the user's command, so empty lines do not
/// become blocks.
#[test]
fn bash_prepended_prompt_command_is_not_a_command() {
    let Some(bash) = require_shell("bash", "bash_prepended_prompt_command_is_not_a_command") else {
        return;
    };
    let (mut s, dir) = start(Shell::Bash, "bash-prepend", &bash, BASH_ARGS, "", &[], &[]);
    s.send("PROMPT_COMMAND='[[ -n $HOME ]] && __pc_seen=1;'\"$PROMPT_COMMAND\"");
    s.wait_prompts(2, "prepend");
    s.send_empty_line();
    s.send_empty_line();
    s.send("echo \"pc=$__pc_seen\"");
    s.wait_prompts(3, "echo pc");
    let events = s.events().to_vec();
    let raw = String::from_utf8_lossy(&s.raw).into_owned();
    s.finish();
    assert_eq!(marks(&events), "A B C D0 A B A B A B C D0 A B", "raw {raw:?}");
    assert!(raw.contains("pc=1\r\n"), "the prepended command still runs; raw {raw:?}");
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
