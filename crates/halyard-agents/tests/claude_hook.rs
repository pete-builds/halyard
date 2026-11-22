//! Runs the shipped Claude Code hook script and feeds what Claude Code would print
//! through the scanner and the attention machine.

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::SystemTime;

use halyard_agents::{
    AgentKind, AttentionConfig, AttentionEvent, AttentionMachine, AttentionState,
};
use halyard_shell::{ReportedState, Scanner, ShellEvent};

const HOOK: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/hooks/claude-code-hook.sh");

fn run_hook(arg: &str, session: Option<&str>) -> String {
    let mut cmd = Command::new("sh");
    cmd.arg(HOOK).arg(arg).env_remove("HALYARD_SESSION_ID");
    if let Some(s) = session {
        cmd.env("HALYARD_SESSION_ID", s);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run sh");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"hook_event_name":"Notification","message":"Claude needs your permission","notification_type":"permission_prompt"}"#)
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{out:?}");
    assert!(out.stderr.is_empty(), "{out:?}");
    String::from_utf8(out.stdout).unwrap()
}

/// Extracts and JSON-unescapes the `terminalSequence` string (only the escapes the
/// script can produce).
fn terminal_sequence(json: &str) -> String {
    let start = json.find(r#""terminalSequence":""#).expect("field") + 20;
    let end = start + json[start..].find('"').expect("closing quote");
    json[start..end].replace(r"\u001b", "\x1b").replace(r"\u0007", "\x07")
}

/// The acceptance rules Claude Code 2.1.325 applies to `terminalSequence`, as read
/// from its shipped validator: at most 4096 bytes, only BEL or OSC sequences, OSC
/// numbers 0, 1, 2, 9, 99, 777, no ESC inside a payload.
fn claude_would_accept(seq: &str) -> bool {
    if seq.is_empty() || seq.len() > 4096 {
        return false;
    }
    let mut rest = seq;
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix('\x07') {
            rest = r;
            continue;
        }
        let Some(body) = rest.strip_prefix("\x1b]") else {
            return false;
        };
        let (end, term_len) = match (body.find('\x07'), body.find("\x1b\\")) {
            (Some(b), Some(s)) if s < b => (s, 2),
            (Some(b), _) => (b, 1),
            (None, Some(s)) => (s, 2),
            (None, None) => return false,
        };
        let payload = &body[..end];
        if payload.contains('\x1b') {
            return false;
        }
        let num = payload.split(';').next().unwrap();
        if !matches!(num, "0" | "1" | "2" | "9" | "99" | "777") {
            return false;
        }
        rest = &body[end + term_len..];
    }
    true
}

#[test]
fn hook_reports_each_state_through_osc_777() {
    for (arg, want) in [
        ("working", ReportedState::Working),
        ("waiting", ReportedState::Waiting),
        ("idle", ReportedState::Idle),
        ("done", ReportedState::Done),
        ("error", ReportedState::Error),
    ] {
        let json = run_hook(arg, Some("pane-7.a_b"));
        let seq = terminal_sequence(&json);
        assert!(claude_would_accept(&seq), "{seq:?}");
        let events = Scanner::new().feed_collect(seq.as_bytes());
        let [ShellEvent::Agent(report)] = events.as_slice() else {
            panic!("{arg}: {events:?}");
        };
        assert_eq!(report.state, Some(want));
        assert_eq!(report.agent.as_deref(), Some("claude"));
        assert_eq!(report.session.as_deref(), Some("pane-7.a_b"));
    }
}

#[test]
fn hook_output_drives_the_attention_machine() {
    let seq = terminal_sequence(&run_hook("waiting", Some("s1")));
    let mut m = AttentionMachine::new(AttentionConfig::default(), Some("s1".into()));
    let mut other = AttentionMachine::new(AttentionConfig::default(), Some("s2".into()));
    for ev in Scanner::new().feed_collect(seq.as_bytes()) {
        let ev = AttentionEvent::from_shell_event(&ev).expect("attention event");
        m.handle(ev.clone(), SystemTime::now());
        other.handle(ev, SystemTime::now());
    }
    assert_eq!(m.state(), AttentionState::WaitingForInput);
    assert_eq!(m.agent(), Some(&AgentKind::ClaudeCode));
    assert_eq!(other.state(), AttentionState::Unknown, "wrong pane ignores it");
}

#[test]
fn hook_is_silent_outside_halyard_and_on_bad_input() {
    assert_eq!(run_hook("waiting", None), "");
    assert_eq!(run_hook("waiting", Some("")), "");
    assert_eq!(run_hook("pwned", Some("s1")), "");
    assert_eq!(run_hook("", Some("s1")), "");
    assert_eq!(run_hook("waiting", Some("a\"b")), "");
    assert_eq!(run_hook("waiting", Some("a;b")), "");
    assert_eq!(run_hook("waiting", Some("a b")), "");
}

#[test]
fn validator_model_rejects_what_claude_rejects() {
    // Controls for `claude_would_accept`, so a pass above means something.
    assert!(claude_would_accept("\x1b]777;notify;t;b\x07"));
    assert!(claude_would_accept("\x07"));
    assert!(claude_would_accept("\x1b]9;hi\x1b\\\x07"));
    assert!(!claude_would_accept("\x1b]7717;state=waiting\x07"));
    assert!(!claude_would_accept("\x1b]1337;x\x07"));
    assert!(!claude_would_accept("\x1b[31m"));
    assert!(!claude_would_accept("\x1b]777;a\x1bb\x07"));
    assert!(!claude_would_accept("\x1b]777;unterminated"));
    assert!(!claude_would_accept(""));
}
