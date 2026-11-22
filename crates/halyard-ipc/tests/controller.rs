//! Controller scope: pairing, tokens, revocation, scopes, typing approval, events.

mod common;

use std::time::Duration;

use common::{code_of, start};
use halyard_ipc::{AgentState, Scope, SendTextReason, SessionEvent, code};
use serde_json::json;

#[test]
fn pairing_approved_grants_only_the_approved_scopes() {
    let h = start();
    h.dir.add("s1", 999_999);
    // The user narrows the request to sessions:read.
    h.approver.approve_pair(&[Scope::SessionsRead], false);
    let mut c = h.hello();
    let result = c
        .call(
            "halyard.pair",
            json!({ "client_name": "dash", "scopes": ["sessions:read", "input:write"] }),
        )
        .expect("pair");
    assert_eq!(result["scopes"], json!(["sessions:read"]));
    let token = result["token"].as_str().expect("token");
    assert!(token.starts_with("hlyg_") && token.len() == 5 + 64);

    // The dialog was shown the kernel-reported peer, not a client claim.
    let seen = h.approver.pair_seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].client_name, "dash");
    assert_eq!(seen[0].peer.pid, std::process::id());

    // Pairing authenticates the connection.
    let list = c.call("sessions.list", json!({})).expect("list");
    assert_eq!(list["sessions"][0]["id"], json!("s1"));
    assert_eq!(list["sessions"][0]["state"], json!(null));
    let r = c.call("session.send_text", json!({ "session_id": "s1", "text": "x" }));
    assert_eq!(code_of(r), code::FORBIDDEN, "input:write was not approved");

    // A new connection authenticates with the token.
    let mut c2 = h.hello();
    let auth = c2.call("halyard.authenticate", json!({ "token": token })).expect("auth");
    assert_eq!(auth["client_name"], json!("dash"));
    c2.call("sessions.list", json!({})).expect("list via token");
}

#[test]
fn pairing_denied_creates_no_grant() {
    let h = start();
    let mut c = h.hello(); // empty script: deny
    let r = c.call("halyard.pair", json!({ "client_name": "dash", "scopes": ["sessions:read"] }));
    assert_eq!(code_of(r), code::DENIED);
    assert!(h.server.grants().list().is_empty());
    assert_eq!(code_of(c.call("sessions.list", json!({}))), code::UNAUTHENTICATED);
}

#[test]
fn pairing_attempts_are_capped_per_connection() {
    let h = start();
    let mut c = h.hello();
    for _ in 0..3 {
        let r = c.call("halyard.pair", json!({ "client_name": "d", "scopes": ["ui:focus"] }));
        assert_eq!(code_of(r), code::DENIED);
    }
    let r = c.call("halyard.pair", json!({ "client_name": "d", "scopes": ["ui:focus"] }));
    assert_eq!(code_of(r), code::RATE_LIMITED);
    assert_eq!(h.approver.pair_seen.lock().unwrap().len(), 3, "no fourth dialog");
}

#[test]
fn grants_file_holds_a_hash_not_the_token() {
    let h = start();
    let (_c, token) = h.controller(&[Scope::SessionsRead], false);
    let text = std::fs::read_to_string(&h.grants_path).expect("grants file");
    assert!(!text.contains(&token));
    assert!(!text.contains(&token["hlyg_".len()..]));
    let grants = h.server.grants().list();
    assert_eq!(grants.len(), 1);
    assert!(text.contains(&grants[0].token_sha256));
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&h.grants_path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn bad_tokens_are_refused() {
    let h = start();
    let (_c, token) = h.controller(&[Scope::SessionsRead], false);
    let mut c = h.hello();
    let mut wrong = token.clone();
    wrong.pop();
    wrong.push(if token.ends_with('0') { '1' } else { '0' });
    assert_eq!(
        code_of(c.call("halyard.authenticate", json!({ "token": wrong }))),
        code::UNAUTHENTICATED
    );
    assert_eq!(
        code_of(c.call("halyard.authenticate", json!({ "token": "" }))),
        code::UNAUTHENTICATED
    );
}

#[test]
fn revocation_cuts_off_live_connections_and_the_token() {
    let h = start();
    let (mut c, token) = h.controller(&[Scope::SessionsRead], false);
    c.call("sessions.list", json!({})).expect("works before revoke");
    let id = h.server.grants().list()[0].id.clone();
    assert!(h.server.grants().revoke(&id).expect("revoke"));
    assert_eq!(code_of(c.call("sessions.list", json!({}))), code::UNAUTHENTICATED);
    let mut c2 = h.hello();
    assert_eq!(
        code_of(c2.call("halyard.authenticate", json!({ "token": token }))),
        code::UNAUTHENTICATED
    );
    assert!(!std::fs::read_to_string(&h.grants_path).unwrap().contains(&id));
}

#[test]
fn a_client_can_unpair_itself() {
    let h = start();
    let (mut c, token) = h.controller(&[Scope::SessionsRead], false);
    c.call("halyard.unpair", json!({})).expect("unpair");
    assert!(h.server.grants().list().is_empty());
    let mut c2 = h.hello();
    assert_eq!(
        code_of(c2.call("halyard.authenticate", json!({ "token": token }))),
        code::UNAUTHENTICATED
    );
}

#[test]
fn scopes_gate_each_method() {
    let h = start();
    h.dir.add("s1", 999_999);
    let (mut c, _) = h.controller(&[Scope::UiFocus], false);
    c.call("session.focus", json!({ "session_id": "s1" })).expect("focus allowed");
    assert_eq!(h.dir.focused.lock().unwrap().as_slice(), ["s1"]);
    assert_eq!(
        code_of(c.call("session.focus", json!({ "session_id": "nope" }))),
        code::SESSION_NOT_FOUND
    );
    assert_eq!(code_of(c.call("sessions.list", json!({}))), code::FORBIDDEN);
    assert_eq!(
        code_of(c.call("session.read_screen", json!({ "session_id": "s1" }))),
        code::FORBIDDEN
    );
    assert_eq!(code_of(c.call("events.subscribe", json!({}))), code::FORBIDDEN);
    // Controllers cannot impersonate an agent.
    let r = c.call("session.report_state", json!({ "state": "done" }));
    assert_eq!(code_of(r), code::FORBIDDEN);

    let (mut r, _) = h.controller(&[Scope::ScreenRead], false);
    let screen = r.call("session.read_screen", json!({ "session_id": "s1" })).expect("read");
    assert_eq!(screen["text"], json!("$ screen of s1"));
}

#[test]
fn send_text_without_standing_permission_asks_every_time() {
    let h = start();
    h.dir.add("s1", 999_999);
    let (mut c, _) = h.controller(&[Scope::InputWrite], false);
    // Denied: nothing typed.
    let r = c.call("session.send_text", json!({ "session_id": "s1", "text": "git status" }));
    assert_eq!(code_of(r), code::DENIED);
    assert!(h.dir.sent.lock().unwrap().is_empty());
    // Approved: typed exactly once.
    h.approver.answer_send(true);
    let ok =
        c.call("session.send_text", json!({ "session_id": "s1", "text": "git status" })).unwrap();
    assert_eq!(ok["approval"], json!("user"));
    let seen = h.approver.send_seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[1].reason, SendTextReason::NoStandingPermission);
    assert_eq!(seen[1].text, "git status");
    assert_eq!(h.dir.sent.lock().unwrap().as_slice(), [("s1".to_owned(), "git status".to_owned())]);
}

#[test]
fn always_allow_still_asks_for_newlines_and_controls() {
    let h = start();
    h.dir.add("s1", 999_999);
    let (mut c, _) = h.controller(&[Scope::InputWrite], true);

    // Plain text with "always allow": no dialog.
    let ok =
        c.call("session.send_text", json!({ "session_id": "s1", "text": "git status" })).unwrap();
    assert_eq!(ok["approval"], json!("standing"));
    assert!(h.approver.send_seen.lock().unwrap().is_empty());

    // A newline would execute the line: fresh approval, here denied.
    for text in ["rm -rf ~\n", "ls\r", "x\u{f}", "\u{1b}[200~", "a\u{202e}b"] {
        let r = c.call("session.send_text", json!({ "session_id": "s1", "text": text }));
        assert_eq!(code_of(r), code::DENIED, "{text:?} must need approval");
    }
    let seen = h.approver.send_seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 5);
    assert!(seen.iter().all(|r| r.reason == SendTextReason::ControlCharacters));
    assert_eq!(seen[0].display_text, "rm -rf ~\\n", "the dialog shows the newline");
    assert_eq!(h.dir.sent.lock().unwrap().len(), 1, "only the plain text was typed");

    // Approved per request: typed.
    h.approver.answer_send(true);
    let ok = c.call("session.send_text", json!({ "session_id": "s1", "text": "make\n" })).unwrap();
    assert_eq!(ok["approval"], json!("user"));
    assert_eq!(h.dir.sent.lock().unwrap().last().unwrap().1, "make\n");
}

#[test]
fn events_stream_to_subscribers_until_revoked() {
    let h = start();
    let (mut c, _) = h.controller(&[Scope::EventsSubscribe], false);
    assert_eq!(c.call("events.subscribe", json!({})).unwrap()["subscribed"], json!(true));
    let events = h.server.events();
    // The subscription is registered before the response is sent.
    assert_eq!(events.subscriber_count(), 1);
    events.publish(SessionEvent::StateChanged {
        session_id: "s1".into(),
        state: AgentState::Waiting,
        agent: Some("claude-code".into()),
        msg: Some("Needs approval".into()),
    });
    events.publish(SessionEvent::Notification {
        session_id: "s1".into(),
        title: "Build".into(),
        body: "done".into(),
    });
    events.publish(SessionEvent::Exited { session_id: "s1".into(), exit_code: Some(0) });
    let n1 = c.next_notification().expect("state");
    assert_eq!(n1["method"], json!("session.state_changed"));
    assert_eq!(n1["params"]["state"], json!("waiting"));
    assert_eq!(n1["params"]["agent"], json!("claude-code"));
    let n2 = c.next_notification().expect("notification");
    assert_eq!(n2["method"], json!("session.notification"));
    assert_eq!(n2["params"]["title"], json!("Build"));
    let n3 = c.next_notification().expect("exited");
    assert_eq!(n3["method"], json!("session.exited"));
    assert_eq!(n3["params"]["exit_code"], json!(0));

    // Revoking the grant ends the stream at the next event.
    let id = h.server.grants().list()[0].id.clone();
    h.server.grants().revoke(&id).unwrap();
    events.publish(SessionEvent::Exited { session_id: "s2".into(), exit_code: None });
    assert!(c.next_notification().is_err(), "stream closed after revocation");
}

#[test]
fn closing_a_subscriber_unsubscribes_it() {
    let h = start();
    let (mut c, _) = h.controller(&[Scope::EventsSubscribe], false);
    c.call("events.subscribe", json!({})).unwrap();
    drop(c);
    let events = h.server.events();
    for _ in 0..100 {
        if events.subscriber_count() == 0 {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("subscriber was not removed after the client disconnected");
}

#[test]
fn python_example_client_lists_sessions() {
    let Ok(status) = std::process::Command::new("python3").arg("--version").output() else {
        eprintln!("python3 not found; skipping the Python example test");
        return;
    };
    assert!(status.status.success());
    let h = start();
    h.dir.add("s1", 999_999);
    let (_c, token) = h.controller(&[Scope::SessionsRead], false);
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/list_sessions.py");
    let out = std::process::Command::new("python3")
        .arg("-I")
        .arg(script)
        .env("HALYARD_SOCKET", &h.socket)
        .env("HALYARD_TOKEN", &token)
        .output()
        .expect("run python");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "s1 unknown pane s1\n");

    // The documentation shows this exact file.
    let doc =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/session-api.md"))
            .expect("docs/session-api.md");
    let source = std::fs::read_to_string(script).unwrap();
    assert!(
        doc.contains(source.trim_end()),
        "docs/session-api.md must embed examples/list_sessions.py verbatim"
    );
}

#[test]
fn a_session_connection_cannot_become_a_controller() {
    let h = start();
    let token = h.dir.add("me", std::process::id());
    let mut c = h.hello();
    c.call("session.attach", json!({ "session_id": "me", "token": token.as_str() })).unwrap();
    let r = c.call("halyard.pair", json!({ "client_name": "x", "scopes": ["sessions:read"] }));
    assert_eq!(code_of(r), code::INVALID_REQUEST);
    assert_eq!(code_of(c.call("sessions.list", json!({}))), code::FORBIDDEN);
}
