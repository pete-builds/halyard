//! Session scope: process ancestry with real processes, and the halyard-msg client.

mod common;

use std::io::Write;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use common::{Harness, code_of, start};
use halyard_ipc::{AgentState, code};
use serde_json::json;

const MSG: &str = env!("CARGO_BIN_EXE_halyard-msg");

/// A stand-in for a pane's shell: `sh` waits for a line on stdin, then runs halyard-msg as
/// a CHILD (not exec'd: the trailing commands prevent the exec optimization) with `args`.
/// Waiting lets the test register the shell's pid before the client connects.
fn pane_shell(h: &Harness, session_id: &str, token: &str, args: &[&str]) -> Child {
    Command::new("/bin/sh")
        .arg("-c")
        .arg(r#"read x; "$HALYARD_MSG" "$@"; status=$?; exit $status"#)
        .arg("sh")
        .args(args)
        .env("HALYARD_MSG", MSG)
        .env("HALYARD_SOCKET", &h.socket)
        .env("HALYARD_SESSION_ID", session_id)
        .env("HALYARD_SESSION_TOKEN", token)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn sh")
}

fn release(mut child: Child) -> Output {
    child.stdin.take().expect("stdin").write_all(b"go\n").expect("release");
    child.wait_with_output().expect("wait")
}

const WAITING: &[&str] =
    &["state", "waiting", "--agent", "claude-code", "--msg", "Needs approval", "--strict"];

#[test]
fn child_of_the_pane_process_is_accepted() {
    let h = start();
    // As the app does: generate the token, spawn the pane with it in the environment,
    // register the child's pid, then let the shell run the client as its child.
    let token = halyard_ipc::SessionToken::generate();
    let sh = pane_shell(&h, "s1", token.as_str(), WAITING);
    register(&h, "s1", sh.id(), &token);
    let out = release(sh);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let reports = h.dir.reports_for("s1");
    assert_eq!(reports.len(), 1, "{reports:?}");
    assert_eq!(reports[0].state, AgentState::Waiting);
    assert_eq!(reports[0].agent.as_deref(), Some("claude-code"));
    assert_eq!(reports[0].msg.as_deref(), Some("Needs approval"));
}

/// Registers a session with a token the test generated before spawning the pane.
fn register(h: &Harness, id: &str, pid: u32, token: &halyard_ipc::SessionToken) {
    let info = halyard_ipc::SessionInfo {
        id: id.into(),
        title: format!("pane {id}"),
        cwd: None,
        agent: None,
        state: None,
    };
    let auth = halyard_ipc::SessionAuth { child_pid: pid, token_hash: token.hash() };
    h.dir.sessions.lock().unwrap().insert(id.into(), (info, auth));
}

#[test]
fn a_process_in_another_pane_cannot_spoof_this_one() {
    let h = start();
    let token_a = halyard_ipc::SessionToken::generate();
    let token_b = halyard_ipc::SessionToken::generate();
    let pane_a = pane_shell(&h, "a", token_a.as_str(), &["--version"]);
    register(&h, "a", pane_a.id(), &token_a);
    // Pane B's process claims to be pane A, with pane A's real (stolen) token.
    let pane_b = pane_shell(&h, "a", token_a.as_str(), WAITING);
    register(&h, "b", pane_b.id(), &token_b);
    let out = release(pane_b);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "spoof must fail; stderr: {stderr}");
    assert!(stderr.contains(&code::NOT_IN_SESSION.to_string()), "{stderr}");
    assert!(h.dir.reports_for("a").is_empty(), "no state was recorded for pane A");
    drop(release(pane_a));
}

#[test]
fn an_unrelated_process_is_rejected() {
    let h = start();
    let token = halyard_ipc::SessionToken::generate();
    let pane = pane_shell(&h, "s1", token.as_str(), &["--version"]);
    register(&h, "s1", pane.id(), &token);
    // The test process is the pane's PARENT, not a descendant.
    let mut c = h.hello();
    let r = c.call("session.attach", json!({ "session_id": "s1", "token": token.as_str() }));
    assert_eq!(code_of(r), code::NOT_IN_SESSION);
    assert_eq!(
        code_of(c.call("session.report_state", json!({ "state": "done" }))),
        code::UNAUTHENTICATED
    );
    drop(release(pane));
}

#[test]
fn the_right_process_with_the_wrong_token_is_rejected() {
    let h = start();
    let token = halyard_ipc::SessionToken::generate();
    let pane = pane_shell(&h, "s1", "hlys_not_the_token", WAITING);
    register(&h, "s1", pane.id(), &token);
    let out = release(pane);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains(&code::UNAUTHENTICATED.to_string()), "{stderr}");
    assert!(h.dir.reports_for("s1").is_empty());
}

#[test]
fn notify_from_a_pane_reaches_the_directory() {
    let h = start();
    let token = halyard_ipc::SessionToken::generate();
    let pane =
        pane_shell(&h, "s1", token.as_str(), &["notify", "Build", "line1\nline2", "--strict"]);
    register(&h, "s1", pane.id(), &token);
    let out = release(pane);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let notes = h.dir.notes.lock().unwrap().clone();
    // halyard-msg replaces control characters so multi-line hook text still arrives.
    assert_eq!(notes, vec![("s1".to_owned(), "Build".to_owned(), "line1 line2".to_owned())]);
}

#[test]
fn session_connections_only_touch_their_own_session() {
    let h = start();
    // Register the test process itself as the pane child, so this process is "inside".
    let token = h.dir.add("me", std::process::id());
    h.dir.add("other", 999_999);
    let mut c = h.hello();
    let attached = c.call("session.attach", json!({ "session_id": "me", "token": token.as_str() }));
    assert_eq!(attached.expect("attach")["session"]["id"], json!("me"));
    c.call("session.report_state", json!({ "state": "working", "agent": "codex" }))
        .expect("report");
    assert_eq!(h.dir.reports_for("me")[0].state, AgentState::Working);
    c.call("session.notify", json!({ "title": "t", "body": "b" })).expect("notify");
    assert_eq!(c.call("session.info", json!({})).expect("info")["session"]["id"], json!("me"));

    // Strict vocabulary and display text.
    assert_eq!(
        code_of(c.call("session.report_state", json!({ "state": "busy" }))),
        code::INVALID_PARAMS
    );
    let r = c.call("session.report_state", json!({ "state": "done", "msg": "a\u{1b}]0;x\u{7}" }));
    assert_eq!(code_of(r), code::INVALID_PARAMS);
    let r = c.call("session.report_state", json!({ "state": "done", "session_id": "other" }));
    assert_eq!(code_of(r), code::INVALID_PARAMS, "no field to name another session");
    // No controller powers, no second identity.
    assert_eq!(code_of(c.call("sessions.list", json!({}))), code::FORBIDDEN);
    let r = c.call("session.attach", json!({ "session_id": "other", "token": token.as_str() }));
    assert_eq!(code_of(r), code::INVALID_REQUEST);
    assert!(h.dir.reports_for("other").is_empty());

    // Once the session is gone, its connection can no longer report.
    h.dir.sessions.lock().unwrap().remove("me");
    assert_eq!(
        code_of(c.call("session.report_state", json!({ "state": "done" }))),
        code::SESSION_NOT_FOUND
    );
}

fn run_msg(args: &[&str], envs: &[(&str, &str)]) -> (Output, Duration) {
    let started = Instant::now();
    let mut cmd = Command::new(MSG);
    cmd.args(args)
        .env_remove("HALYARD_SOCKET")
        .env_remove("HALYARD_SESSION_ID")
        .env_remove("HALYARD_SESSION_TOKEN");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    // Detach from any controlling terminal so the fallback cannot write to the developer's.
    // SAFETY: setsid is async-signal-safe and touches no Rust state.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let out = cmd.stdin(Stdio::null()).output().expect("run halyard-msg");
    (out, started.elapsed())
}

#[test]
fn outside_halyard_it_exits_0_silently_and_fast() {
    let (out, elapsed) =
        run_msg(&["state", "waiting", "--agent", "claude-code", "--msg", "hi"], &[]);
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty() && out.stderr.is_empty(), "{out:?}");
    assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
    let (out, _) = run_msg(&["notify", "t", "--strict"], &[]);
    assert_eq!(out.status.code(), Some(1), "--strict reports not being inside Halyard");
}

#[test]
fn usage_errors_exit_64_never_2() {
    for args in [
        &["state", "busy"][..],
        &["state"],
        &[],
        &["frobnicate"],
        &["state", "done", "--bogus"],
        &["notify", "t", "--agent", "x"],
    ] {
        let (out, _) = run_msg(args, &[]);
        assert_eq!(out.status.code(), Some(64), "{args:?}");
    }
    let (out, _) = run_msg(&["--help"], &[]);
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).contains("usage:"));
}

#[test]
fn unreachable_socket_without_a_terminal_still_exits_0() {
    let envs = [("HALYARD_SESSION_ID", "s1"), ("HALYARD_SOCKET", "/nonexistent/halyard.sock")];
    let (out, elapsed) = run_msg(&["state", "done"], &envs);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(elapsed < Duration::from_secs(2));
    let (out, _) = run_msg(&["state", "done", "--strict"], &envs);
    assert_eq!(out.status.code(), Some(1), "no socket and no tty is a failure under --strict");
}

#[test]
fn unreachable_socket_falls_back_to_the_escape_sequence_on_the_tty() {
    // Give halyard-msg a pseudo-terminal as its controlling terminal and read what it
    // writes to /dev/tty from the master side.
    let mut master: libc::c_int = -1;
    let mut slave: libc::c_int = -1;
    // SAFETY: out-pointers are valid; name, termios and winsize may be null.
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(rc, 0, "openpty failed");
    // Close-on-exec, so children spawned concurrently by other tests do not inherit the
    // pty. Our child gets the terminal through TIOCSCTTY before exec, not through the fd.
    for fd in [master, slave] {
        // SAFETY: fd is a valid descriptor we own.
        assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) }, 0);
    }
    let mut cmd = Command::new(MSG);
    cmd.args([
        "state",
        "waiting",
        "--agent",
        "claude-code",
        "--msg",
        "Needs approval; now",
        "--strict",
    ])
    .env("HALYARD_SESSION_ID", "s1")
    .env("HALYARD_SOCKET", "/nonexistent/halyard.sock")
    .env_remove("HALYARD_SESSION_TOKEN")
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::piped());
    // SAFETY: setsid and ioctl are async-signal-safe; `slave` is a valid fd in the child.
    unsafe {
        cmd.pre_exec(move || {
            if libc::setsid() < 0 || libc::ioctl(slave, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = cmd.spawn().expect("spawn halyard-msg");
    // Keep our slave fd open until the end: with no slave open, the master reads EOF
    // before the child has opened /dev/tty. Read BEFORE waiting: on macOS a tty close
    // blocks until its output is drained, so waiting first would deadlock.
    let mut got = Vec::new();
    let mut buf = [0u8; 256];
    while !got.ends_with(b"\x1b\\") {
        let mut pfd = libc::pollfd { fd: master, events: libc::POLLIN, revents: 0 };
        // SAFETY: one valid pollfd.
        let ready = unsafe { libc::poll(&mut pfd, 1, 5000) };
        assert!(ready > 0, "no output from halyard-msg within 5 s: {got:?}");
        // SAFETY: `buf` is writable for its length.
        let n = unsafe { libc::read(master, buf.as_mut_ptr().cast(), buf.len()) };
        assert!(n > 0, "pty closed before the sequence arrived: {got:?}");
        got.extend_from_slice(&buf[..n as usize]);
    }
    let out = child.wait_with_output().expect("wait");
    // SAFETY: both fds are ours and still open.
    unsafe {
        libc::close(slave);
        libc::close(master);
    }
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        String::from_utf8_lossy(&got),
        "\u{1b}]7717;state=waiting;agent=claude-code;msg=Needs%20approval%3B%20now;session=s1\u{1b}\\"
    );
}
