//! End-to-end tests: real PTYs, real programs, the real VT engine. No window.
//!
//! The shell is `/bin/sh` with a fixed prompt so output is deterministic.
//! Tests wait by polling frames with a deadline, woken by session events,
//! never by fixed sleeps. Tests that need an optional program (vim, less,
//! htop) print why they skipped when it is missing; CI installs all three.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use halyard_term::{
    Frame, KeyCode, KeyInput, Modifiers, MouseAction, MouseButton, MouseInput, Palette,
    ScrollRequest, Session, SessionConfig, SessionEvent,
};

const PROMPT: &str = "hy$ ";
const TIMEOUT: Duration = Duration::from_secs(15);

fn config(cols: u16, rows: u16) -> SessionConfig {
    let env = |k: &str, v: &str| (k.to_owned(), v.to_owned());
    SessionConfig {
        program: Some(PathBuf::from("/bin/sh")),
        cols,
        rows,
        env: vec![
            env("PS1", PROMPT),
            env("PS2", "> "),
            env("HISTFILE", "/dev/null"),
            env("LESS", ""),
            env("LANG", "C"),
            env("LC_ALL", "C"),
            env("BASH_SILENCE_DEPRECATION_WARNING", "1"),
        ],
        env_remove: ["ENV", "BASH_ENV", "PROMPT_COMMAND", "LESSOPEN", "LESSCLOSE", "VIMINIT"]
            .map(str::to_owned)
            .to_vec(),
        ..SessionConfig::default()
    }
}

fn spawn(cols: u16, rows: u16) -> Session {
    let s = Session::spawn(config(cols, rows)).expect("spawn /bin/sh");
    wait_for(&s, "the first prompt", |f| prompt_count(f) >= 1);
    s
}

/// Poll the latest frame until `pred` holds, waking on session events.
fn wait_for(s: &Session, what: &str, pred: impl Fn(&Frame) -> bool) -> Arc<Frame> {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let f = s.latest_frame();
        if pred(&f) {
            return f;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}; screen:\n{}", f.text());
        // Any event (or 100 ms) means it is worth looking again.
        let _woken = s.events().recv_timeout(Duration::from_millis(100));
    }
}

fn wait_for_exit(s: &Session) -> Option<i32> {
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        if let Ok(SessionEvent::Exited(code)) = s.events().recv_timeout(Duration::from_millis(100))
        {
            return code;
        }
    }
    panic!("no exit event; screen:\n{}", s.latest_frame().text());
}

fn prompt_count(f: &Frame) -> usize {
    (0..f.rows).filter(|&r| f.row_text(r).starts_with(PROMPT.trim_end())).count()
}

fn row_equal(f: &Frame, text: &str) -> Option<u16> {
    (0..f.rows).find(|&r| f.row_text(r) == text)
}

fn run(s: &Session, line: &str) {
    s.write(format!("{line}\r"));
}

/// Returns false (and says why) when `program` is not on PATH. In CI (the
/// `CI` variable is set) a missing program is a failure, not a skip: CI
/// installs vim, less and htop, and libtest hides the skip message.
fn have(program: &str) -> bool {
    let found = std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(program).is_file()));
    assert!(found || std::env::var_os("CI").is_none(), "`{program}` must be installed in CI");
    if !found {
        eprintln!("SKIPPED: `{program}` is not installed on this machine");
    }
    found
}

#[test]
fn shell_echo_reaches_the_screen() {
    let s = spawn(80, 24);
    run(&s, "echo hello-halyard");
    wait_for(&s, "echo output", |f| row_equal(f, "hello-halyard").is_some());
}

#[test]
fn default_program_is_a_login_shell() {
    // No program: the user's shell from $SHELL or passwd, run as a login
    // shell (argv[0] starts with '-'). Its prompt is whatever the user has.
    let s = Session::spawn(SessionConfig::default()).expect("spawn login shell");
    s.write("echo login-check:$0\r");
    wait_for(&s, "login shell argv[0]", |f| {
        (0..f.rows).any(|r| f.row_text(r).starts_with("login-check:-"))
    });
}

#[test]
fn session_environment_is_set() {
    let s = spawn(120, 24);
    run(&s, r#"echo "env:$TERM:$COLORTERM:$TERM_PROGRAM:$HALYARD_SESSION_ID""#);
    let want = format!("env:xterm-256color:truecolor:Halyard:{}", s.id());
    wait_for(&s, "environment line", |f| row_equal(f, &want).is_some());
}

#[test]
fn sgr_red_is_rendered_red() {
    let s = spawn(80, 24);
    run(&s, r"printf '\033[31mred\033[0m\n'");
    let f = wait_for(&s, "red output", |f| row_equal(f, "red").is_some());
    let row = row_equal(&f, "red").unwrap();
    let cell = f.cell(0, row).unwrap();
    assert_eq!(cell.fg, Palette::default().ansi[1], "cell {cell:?}");
    // Text after the reset is back to the default color.
    assert_eq!(f.cell(0, row + 1).unwrap().fg, Palette::default().foreground);
}

#[test]
fn resize_reaches_the_program() {
    let s = spawn(80, 24);
    run(&s, "stty size");
    wait_for(&s, "initial size", |f| row_equal(f, "24 80").is_some());
    s.resize(100, 40, 8, 16);
    wait_for(&s, "resized frame", |f| f.cols == 100 && f.rows == 40);
    run(&s, "stty size");
    wait_for(&s, "new size", |f| row_equal(f, "40 100").is_some());
}

#[test]
fn ctrl_c_byte_interrupts_sleep() {
    let s = spawn(80, 24);
    let start = Instant::now();
    run(&s, "sleep 30");
    wait_for(&s, "sleep echoed", |f| prompt_count(f) == 1 && f.text().contains("sleep 30"));
    s.write(vec![0x03]);
    wait_for(&s, "prompt after ^C", |f| prompt_count(f) == 2);
    assert!(start.elapsed() < Duration::from_secs(20), "sleep was not interrupted");
}

#[test]
fn ctrl_c_key_event_interrupts_sleep() {
    let s = spawn(80, 24);
    let start = Instant::now();
    run(&s, "sleep 30");
    wait_for(&s, "sleep echoed", |f| f.text().contains("sleep 30"));
    s.send_key(KeyInput::new(KeyCode::Char('c'), Modifiers::CTRL));
    wait_for(&s, "prompt after Ctrl+C", |f| prompt_count(f) == 2);
    assert!(start.elapsed() < Duration::from_secs(20), "sleep was not interrupted");
}

#[test]
fn output_flood_keeps_up() {
    let s = spawn(80, 24);
    run(&s, "yes halyard-flood | head -n 200000; echo flood-done");
    wait_for(&s, "end of flood", |f| row_equal(f, "flood-done").is_some());
    // Coalescing: far fewer frames than output chunks were published.
    let f = s.latest_frame();
    assert!(f.generation < 20_000, "published {} frames", f.generation);
}

#[test]
fn trickling_output_is_read_in_batches() {
    // `head` writing to a terminal is line buffered: one 2-byte `write` per
    // line. Read the moment it arrived, that reached the session as about 8
    // bytes per chunk on macOS, each with its own poll, read, channel send and
    // wakeup (`docs/PERFORMANCE.md`). The reader now pauses briefly after a
    // small read so the output collects in the PTY first.
    let bytes = Arc::new(AtomicUsize::new(0));
    let chunks = Arc::new(AtomicUsize::new(0));
    let (b, c) = (Arc::clone(&bytes), Arc::clone(&chunks));
    let s = Session::spawn(SessionConfig {
        program: Some(PathBuf::from("/bin/sh")),
        args: vec!["-c".into(), "yes | head -c 400000; echo trickle-done".into()],
        output_tap: Some(Box::new(move |chunk: &[u8]| {
            b.fetch_add(chunk.len(), Ordering::Relaxed);
            c.fetch_add(1, Ordering::Relaxed);
        })),
        ..config(80, 24)
    })
    .expect("spawn /bin/sh");
    wait_for(&s, "end of output", |f| row_equal(f, "trickle-done").is_some());
    let (bytes, chunks) = (bytes.load(Ordering::Relaxed), chunks.load(Ordering::Relaxed));
    assert!(bytes > 400_000, "only {bytes} bytes arrived");
    let average = bytes / chunks.max(1);
    assert!(average >= 32, "{average} bytes per chunk ({chunks} chunks)");
}

#[test]
fn typed_keys_run_a_command() {
    let s = spawn(80, 24);
    for c in "echo typed".chars() {
        let key = if c == ' ' { KeyCode::Space } else { KeyCode::Char(c) };
        s.send_key(KeyInput::with_text(key, c.to_string(), Modifiers::NONE));
    }
    s.send_key(KeyInput::new(KeyCode::Enter, Modifiers::NONE));
    wait_for(&s, "typed command output", |f| row_equal(f, "typed").is_some());
}

#[test]
fn shell_exit_reports_status() {
    let s = spawn(80, 24);
    run(&s, "exit 3");
    assert_eq!(wait_for_exit(&s), Some(3));
}

#[test]
fn title_report_cannot_inject_a_command() {
    // CVE-2024-38396 class: output sets a title holding a command and asks
    // the terminal to report it back as input. Nothing may come back.
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("pwned");
    let payload = dir.path().join("payload");
    std::fs::write(
        &payload,
        format!(
            "\x1b]2;;touch {};\x07\x1b[21t\x1b]1;;touch {};\x07\x1b[20t",
            marker.display(),
            marker.display()
        ),
    )
    .unwrap();
    let s = spawn(120, 24);
    run(&s, &format!("cat {}; echo cat-done", payload.display()));
    wait_for(&s, "cat finished", |f| row_equal(f, "cat-done").is_some() && prompt_count(f) == 2);
    // Anything the terminal typed back would now sit on the prompt line;
    // Enter would run it.
    s.write("\r");
    wait_for(&s, "prompt after Enter", |f| prompt_count(f) == 3);
    run(&s, "echo after-enter");
    let f = wait_for(&s, "shell still responsive", |f| row_equal(f, "after-enter").is_some());
    assert!(!f.text().contains("touch"), "a reply reached the input line:\n{}", f.text());
    assert!(!marker.exists(), "the injected command ran");
}

#[test]
fn device_query_replies_reach_the_program() {
    // A raw-mode program asks for the cursor position and prints what it
    // read back, with ESC shown as E.
    let s = spawn(80, 24);
    run(
        &s,
        r"stty raw -echo min 0 time 20; printf '\033[6n'; r=$(dd bs=1 count=16 2>/dev/null | tr '\033' E); stty sane; echo; echo cpr:$r",
    );
    let f = wait_for(&s, "cursor position report", |f| {
        (0..f.rows).any(|r| f.row_text(r).starts_with("cpr:E["))
    });
    let row = (0..f.rows).find(|&r| f.row_text(r).starts_with("cpr:E[")).unwrap();
    let text = f.row_text(row);
    let body = text.strip_prefix("cpr:E[").and_then(|t| t.strip_suffix('R'));
    let ok = body.is_some_and(|b| {
        b.split(';').count() == 2 && b.split(';').all(|n| n.parse::<u16>().is_ok_and(|n| n > 0))
    });
    assert!(ok, "not a CPR reply: {text:?}");
}

#[test]
fn dropping_the_session_kills_the_program() {
    let mut c = config(80, 24);
    c.program = Some(PathBuf::from("sleep"));
    c.args = vec!["30".into()];
    let s = Session::spawn(c).unwrap();
    let pid = libc::pid_t::try_from(s.pid().expect("child pid")).unwrap();
    // SAFETY: signal 0 only checks that the process exists.
    assert_eq!(unsafe { libc::kill(pid, 0) }, 0, "sleep is not running");
    drop(s);
    let deadline = Instant::now() + TIMEOUT;
    // SAFETY: as above.
    while unsafe { libc::kill(pid, 0) } == 0 {
        assert!(Instant::now() < deadline, "sleep {pid} still running after drop");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn dropping_the_session_kills_a_program_that_ignores_hangup() {
    // Closing the PTY sends SIGHUP; a program that ignores it must still be
    // stopped (the session escalates to SIGKILL).
    let mut c = config(80, 24);
    c.args = vec!["-c".into(), "trap '' HUP; while :; do sleep 1; done".into()];
    let s = Session::spawn(c).unwrap();
    let pid = libc::pid_t::try_from(s.pid().expect("child pid")).unwrap();
    // Let the shell install its trap.
    std::thread::sleep(Duration::from_millis(300));
    drop(s);
    let deadline = Instant::now() + TIMEOUT;
    // SAFETY: signal 0 only checks that the process exists.
    while unsafe { libc::kill(pid, 0) } == 0 {
        assert!(Instant::now() < deadline, "shell {pid} still running after drop");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn program_exit_status_and_signal() {
    let mut c = config(80, 24);
    c.args = vec!["-c".into(), "echo bye; exit 7".into()];
    let s = Session::spawn(c).unwrap();
    assert_eq!(wait_for_exit(&s), Some(7));
    // The final output is on screen when the exit is reported.
    assert!(row_equal(&s.latest_frame(), "bye").is_some());

    let mut c = config(80, 24);
    c.args = vec!["-c".into(), "kill -9 $$".into()];
    let s = Session::spawn(c).unwrap();
    assert_eq!(wait_for_exit(&s), None);
}

#[test]
fn bell_and_title_events() {
    let s = spawn(80, 24);
    run(&s, r"printf '\033]2;hello-title\007'; printf '\007'");
    let deadline = Instant::now() + TIMEOUT;
    let (mut bell, mut title) = (false, None);
    while Instant::now() < deadline && !(bell && title.is_some()) {
        match s.events().recv_timeout(Duration::from_millis(100)) {
            Ok(SessionEvent::Bell) => bell = true,
            Ok(SessionEvent::TitleChanged(t)) => title = Some(t),
            _ => {}
        }
    }
    assert!(bell, "no bell event");
    assert_eq!(title.as_deref(), Some("hello-title"));
    wait_for(&s, "title in frame", |f| f.title == "hello-title");
}

#[test]
fn output_tap_sees_raw_bytes() {
    let seen = Arc::new(Mutex::new(Vec::<u8>::new()));
    let mut c = config(80, 24);
    let sink = Arc::clone(&seen);
    c.output_tap = Some(Box::new(move |chunk| sink.lock().unwrap().extend_from_slice(chunk)));
    let s = Session::spawn(c).unwrap();
    run(&s, r"printf '\033]133;A\007tap-marker\n'");
    wait_for(&s, "marker", |f| row_equal(f, "tap-marker").is_some());
    let bytes = seen.lock().unwrap().clone();
    let needle = b"\x1b]133;A\x07tap-marker";
    assert!(bytes.windows(needle.len()).any(|w| w == needle), "tap missed the raw OSC bytes");
}

#[test]
fn notify_callback_fires() {
    let count = Arc::new(Mutex::new(0usize));
    let mut c = config(80, 24);
    let n = Arc::clone(&count);
    c.notify = Some(Arc::new(move || *n.lock().unwrap() += 1));
    let s = Session::spawn(c).unwrap();
    wait_for(&s, "prompt", |f| prompt_count(f) >= 1);
    assert!(*count.lock().unwrap() > 0);
}

#[test]
fn scrollback_scrolls_and_typing_returns_to_bottom() {
    let s = spawn(80, 10);
    run(&s, "i=0; while [ $i -lt 60 ]; do i=$((i+1)); echo scroll-$i; done");
    wait_for(&s, "loop output", |f| row_equal(f, "scroll-60").is_some());
    s.scroll(ScrollRequest::Top);
    let f = wait_for(&s, "top of scrollback", |f| f.scroll.offset == 0 && !f.scroll.at_bottom());
    assert!(row_equal(&f, "scroll-60").is_none());
    s.scroll(ScrollRequest::Pages(1));
    wait_for(&s, "one page down", |f| f.scroll.offset == 10);
    s.send_key(KeyInput::with_text(KeyCode::Char('x'), "x", Modifiers::NONE));
    wait_for(&s, "back at bottom", |f| f.scroll.at_bottom());
}

#[test]
fn selection_copies_text() {
    let s = spawn(80, 24);
    run(&s, "echo select-me-123");
    let f = wait_for(&s, "output", |f| row_equal(f, "select-me-123").is_some());
    let row = row_equal(&f, "select-me-123").unwrap();
    s.selection_start(0, row);
    s.selection_extend(12, row);
    assert_eq!(s.selection_text().as_deref(), Some("select-me-123"));
    wait_for(&s, "highlight", |f| {
        f.cell(3, row).is_some_and(|c| c.flags.has(halyard_term::CellFlags::SELECTED))
    });
    s.selection_clear();
    assert_eq!(s.selection_text(), None);
}

#[test]
fn bracketed_paste_and_mouse_reach_the_program() {
    let s = spawn(80, 24);
    // Turn on bracketed paste and SGR mouse reporting, then echo input back
    // visibly with `cat -v`.
    run(&s, r"printf '\033[?2004h\033[?1000h\033[?1006h'; cat -v");
    wait_for(&s, "modes on", |f| f.mouse_tracking);
    s.paste("pa\nste");
    s.write("\r");
    wait_for(&s, "bracketed paste", |f| f.text().contains("^[[200~pa"));
    s.send_mouse(MouseInput {
        action: MouseAction::Press,
        button: Some(MouseButton::Left),
        x: 8.0 * 2.0 + 1.0,
        y: 16.0 * 3.0 + 1.0,
        mods: Modifiers::NONE,
        any_button_pressed: true,
    });
    s.write("\r");
    wait_for(&s, "mouse report", |f| f.text().contains("^[[<0;3;4M"));
    s.write(vec![0x03]);
}

#[test]
fn vim_uses_the_alternate_screen() {
    if !have("vim") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("note.txt");
    std::fs::write(&file, "halyard vim line one\nsecond line\n").unwrap();
    let s = spawn(80, 24);
    run(&s, &format!("vim -u NONE -i NONE -N {}", file.display()));
    wait_for(&s, "vim showing the file", |f| {
        f.alt_screen && row_equal(f, "halyard vim line one").is_some()
    });
    s.send_key(KeyInput::new(KeyCode::Escape, Modifiers::NONE));
    run(&s, ":q!");
    wait_for(&s, "back at the shell", |f| !f.alt_screen && prompt_count(f) >= 2);
}

#[test]
fn less_pages_and_quits() {
    if !have("less") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("long.txt");
    let body: String = (1..=300).map(|i| format!("row-{i:04}\n")).collect();
    std::fs::write(&file, body).unwrap();
    let s = spawn(80, 24);
    run(&s, &format!("less {}", file.display()));
    wait_for(&s, "first page", |f| f.alt_screen && row_equal(f, "row-0001").is_some());
    s.write(" ");
    wait_for(&s, "second page", |f| {
        row_equal(f, "row-0030").is_some() && row_equal(f, "row-0001").is_none()
    });
    s.write("q");
    wait_for(&s, "back at the shell", |f| !f.alt_screen && prompt_count(f) >= 2);
}

#[test]
fn htop_starts_and_quits() {
    if !have("htop") {
        return;
    }
    // A fresh HOME so a personal htoprc cannot change the layout.
    let home = tempfile::tempdir().unwrap();
    let mut c = config(120, 30);
    c.env.push(("HOME".into(), home.path().display().to_string()));
    c.env.push(("XDG_CONFIG_HOME".into(), home.path().display().to_string()));
    let s = Session::spawn(c).unwrap();
    wait_for(&s, "prompt", |f| prompt_count(f) >= 1);
    run(&s, "htop");
    wait_for(&s, "htop header", |f| f.alt_screen && f.text().contains("PID"));
    s.write("q");
    wait_for(&s, "back at the shell", |f| !f.alt_screen && prompt_count(f) >= 2);
}
