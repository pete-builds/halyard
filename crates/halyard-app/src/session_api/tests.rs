//! The session API wired to real panes: a real server on a real socket, real
//! shells in real PTYs, and the app's own directory and approver. Only the UI
//! thread is faked: a thread answers [`UiRequest`]s from a script.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use futures::StreamExt;
use halyard_ipc::{
    Client, ClientError, GrantStore, Scope, SendTextReason, ServerConfig, code,
    paths::MAX_SOCKET_PATH_BYTES,
};
use halyard_term::SessionConfig;
use serde_json::json;

use super::*;

/// The UI thread's stand-in: answers dialogs from a script and records them.
#[derive(Default)]
struct FakeUi {
    pair: Mutex<VecDeque<PairingDecision>>,
    send: Mutex<VecDeque<bool>>,
    send_seen: Mutex<Vec<SendTextRequest>>,
    pair_seen: AtomicUsize,
    changed: AtomicUsize,
}

impl FakeUi {
    fn run(self: &Arc<Self>, mut rx: futures::channel::mpsc::UnboundedReceiver<UiRequest>) {
        let ui = Arc::clone(self);
        std::thread::spawn(move || {
            futures::executor::block_on(async move {
                while let Some(request) = rx.next().await {
                    match request {
                        UiRequest::Notice(_) => {}
                        UiRequest::Changed { .. } => {
                            ui.changed.fetch_add(1, Ordering::SeqCst);
                        }
                        UiRequest::Focus { reply, .. } => {
                            reply.try_send(Ok(())).ok();
                        }
                        UiRequest::Pairing { reply, .. } => {
                            ui.pair_seen.fetch_add(1, Ordering::SeqCst);
                            if let Some(d) = lock(&ui.pair).pop_front() {
                                reply.try_send(d).ok();
                            } // else: dropping the reply denies
                        }
                        UiRequest::SendText { request, reply } => {
                            lock(&ui.send_seen).push(*request);
                            let yes = lock(&ui.send).pop_front().unwrap_or(false);
                            reply.try_send(yes).ok();
                        }
                    }
                }
            });
        });
    }

    fn approve_pair(&self, scopes: &[Scope], always: bool) {
        lock(&self.pair).push_back(PairingDecision::Approve {
            scopes: scopes.iter().copied().collect(),
            always_allow_input: always,
        });
    }
}

struct Harness {
    _tmp: tempfile::TempDir,
    api: PaneApi,
    ui: Arc<FakeUi>,
    socket: PathBuf,
    panes: Vec<(Arc<Session>, SharedMonitor)>,
    /// Each pane's token, as its environment holds it.
    tokens: Vec<String>,
}

fn start() -> Harness {
    let tmp = tempfile::tempdir().expect("tempdir");
    let socket = tmp.path().join("run").join("t.sock");
    assert!(socket.as_os_str().len() <= MAX_SOCKET_PATH_BYTES, "{}", socket.display());
    let (tx, rx) = futures::channel::mpsc::unbounded();
    let registry = Registry::default();
    let publisher = Arc::new(Mutex::new(EventPublisher::default()));
    let events = EventHub::new();
    let slot: ServerSlot = Arc::default();
    let directory = Arc::new(AppDirectory::new(
        registry.clone(),
        tx.clone(),
        events.clone(),
        Arc::clone(&publisher),
    ));
    let approver = Arc::new(DialogApprover::new(tx, Duration::from_secs(10)));
    let mut config = ServerConfig::new(&socket, GrantStore::in_memory());
    config.events = events.clone();
    let server = IpcServer::bind(config, directory, approver).expect("bind");
    *lock(&slot) = Some(Arc::new(server));
    let ui = Arc::new(FakeUi::default());
    ui.run(rx);
    let api = PaneApi::new(socket.clone(), registry, slot, publisher, events);
    Harness { _tmp: tmp, api, ui, socket, panes: Vec::new(), tokens: Vec::new() }
}

impl Harness {
    /// Starts `/bin/sh -i` in a real PTY, registered like an app pane.
    fn pane(&mut self) -> (String, Arc<Session>, SharedMonitor) {
        let id = halyard_term::new_session_id();
        let creds = pane_credentials(&self.socket, &id).expect("utf-8 socket path");
        let monitor = signals::new_monitor(&id);
        let session = Arc::new(
            Session::spawn(SessionConfig {
                program: Some("/bin/sh".into()),
                args: vec!["-i".into()],
                session_id: Some(id.clone()),
                env: creds.env.clone(),
                observer: Some(Box::new(signals::SignalObserver::new(Arc::clone(&monitor)))),
                cols: 100,
                rows: 30,
                ..SessionConfig::default()
            })
            .expect("spawn"),
        );
        assert!(self.api.register(&session, &monitor, &creds.token));
        self.panes.push((Arc::clone(&session), Arc::clone(&monitor)));
        self.tokens.push(creds.token.as_str().to_owned());
        (id, session, monitor)
    }

    fn client(&self) -> Client {
        let mut c = Client::connect(&self.socket, Some(Duration::from_secs(20))).expect("connect");
        c.hello("test-tool").expect("hello");
        c
    }

    /// A controller paired with these scopes (the fake UI approves).
    fn paired(&self, scopes: &[Scope], always: bool) -> Client {
        self.ui.approve_pair(scopes, always);
        let mut c = self.client();
        let names: Vec<&str> = scopes.iter().map(|s| s.as_str()).collect();
        c.call("halyard.pair", json!({ "client_name": "test-tool", "scopes": names }))
            .expect("pair");
        c
    }
}

fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn rpc_code(r: Result<serde_json::Value, ClientError>) -> i64 {
    r.expect_err("expected an error").rpc_code().expect("an RPC error")
}

/// The body of the in-pane client: what `halyard-msg state waiting` does, from
/// a process the pane's shell started. Runs only inside the PTY, when the
/// variable is set (the normal test run skips it).
#[test]
fn api_client_main() {
    if std::env::var_os("HALYARD_API_TEST_CLIENT").is_none() {
        return;
    }
    let var = |k: &str| std::env::var(k).expect(k);
    let mut c = Client::connect(Path::new(&var("HALYARD_SOCKET")), Some(Duration::from_secs(3)))
        .expect("connect");
    c.hello("halyard-msg").expect("hello");
    let attach = json!({
        "session_id": var("HALYARD_SESSION_ID"),
        "token": var("HALYARD_SESSION_TOKEN"),
    });
    c.call("session.attach", attach).expect("attach");
    c.call(
        "session.report_state",
        json!({ "state": "waiting", "agent": "claude-code", "msg": "Needs approval" }),
    )
    .expect("report");
    std::io::Write::write_all(&mut std::io::stdout(), b"API-CLIENT-OK\n").expect("stdout");
}

/// A process inside the pane reports `waiting` over the socket: the pane's
/// monitor (what the attention marks draw from) changes, and the UI is asked to
/// redraw at once.
#[test]
fn a_report_from_inside_a_pane_updates_its_state() {
    let mut h = start();
    let (_, session, monitor) = h.pane();
    let exe = std::env::current_exe().expect("test exe");
    session.write(format!(
        "HALYARD_API_TEST_CLIENT=1 '{}' --exact session_api::tests::api_client_main --nocapture\n",
        exe.display()
    ));
    wait_for("the waiting state", || {
        signals::lock(&monitor).attention().state() == AttentionState::WaitingForInput
    });
    let m = signals::lock(&monitor);
    assert_eq!(m.attention().cause(), Cause::ProtocolReport);
    assert_eq!(m.attention().agent(), Some(&AgentKind::ClaudeCode));
    assert_eq!(m.attention().last_notification(), Some("Needs approval"));
    drop(m);
    wait_for("a redraw request", || h.ui.changed.load(Ordering::SeqCst) >= 1);
    wait_for("the client's success line", || {
        session.latest_frame().text().contains("API-CLIENT-OK")
    });
}

/// The right id and token are not enough from outside the pane's process tree:
/// the test process is not a descendant of the pane's shell.
#[test]
fn attach_from_outside_the_pane_is_rejected_by_ancestry() {
    let mut h = start();
    let (id, _session, monitor) = h.pane();
    assert!(h.api.registry.get(&id).is_some(), "registered");
    // The test holds the pane's real id and token (what its environment has), so
    // only the ancestry check stands between it and the pane.
    let token = h.tokens[0].clone();
    assert!(token.starts_with("hlys_"));
    let mut c = h.client();
    let r = c.call("session.attach", json!({ "session_id": id, "token": token }));
    assert_eq!(rpc_code(r), code::NOT_IN_SESSION);
    // Control: a wrong token fails earlier, on the token check.
    let mut c = h.client();
    let r = c.call("session.attach", json!({ "session_id": id, "token": "hlys_wrong" }));
    assert_eq!(rpc_code(r), code::UNAUTHENTICATED);
    assert_eq!(signals::lock(&monitor).attention().state(), AttentionState::Unknown);
}

/// Typing a command needs the user: the dialog sees the escaped text, a denial
/// types nothing, an approval types it through the pane's paste path.
#[test]
fn send_text_with_a_newline_needs_approval() {
    let mut h = start();
    let (id, session, _) = h.pane();
    let mut c = h.paired(&[Scope::InputWrite, Scope::ScreenRead], false);
    lock(&h.ui.send).push_back(false);
    let r = c.call("session.send_text", json!({ "session_id": id, "text": "echo den''ied\n" }));
    assert_eq!(rpc_code(r), code::DENIED);
    lock(&h.ui.send).push_back(true);
    let r = c
        .call("session.send_text", json!({ "session_id": id, "text": "echo app''roved\n" }))
        .expect("approved");
    assert_eq!(r["approval"], "user");
    {
        let seen = lock(&h.ui.send_seen);
        assert_eq!(seen.len(), 2, "one dialog per request");
        assert_eq!(seen[1].reason, SendTextReason::ControlCharacters);
        assert_eq!(seen[1].display_text, "echo app''roved\\n");
        assert_eq!(seen[1].session.id, id);
        assert_eq!(seen[1].client_name, "test-tool");
    }
    // Only the approved text reached the pane. Whether its newline runs it
    // depends on the shell: with bracketed paste on (bash 5 as `sh` on Arch)
    // it waits in the line editor for the user's Enter; without (macOS `sh`,
    // dash) it runs. Either way the typed text is on screen, the denied never.
    wait_for("the approved text", || session.latest_frame().text().contains("echo app''roved"));
    let screen = c.call("session.read_screen", json!({ "session_id": id })).expect("read");
    let text = screen["text"].as_str().unwrap_or_default();
    assert!(text.contains("echo app''roved"), "{text}");
    assert!(!text.contains("den''ied"), "{text}");
}

/// The rule from `docs/session-api.md`: only the user's own keys in the pane
/// clear `typed_since_approval`. Text typed through the app's `send_text` path
/// must never clear it, or "approve Enter" would hide what Enter runs.
#[test]
fn send_text_never_resets_typed_since_approval() {
    let mut h = start();
    let (id, _session, _) = h.pane();
    let mut c = h.paired(&[Scope::InputWrite], true);
    let r = c.call("session.send_text", json!({ "session_id": id, "text": "abc" })).expect("send");
    assert_eq!(r["approval"], "standing");
    let r = c.call("session.send_text", json!({ "session_id": id, "text": "def" })).expect("send");
    assert_eq!(r["approval"], "standing");
    // The Enter needs a dialog, which must show everything typed since.
    lock(&h.ui.send).push_back(false);
    let r = c.call("session.send_text", json!({ "session_id": id, "text": "\r" }));
    assert_eq!(rpc_code(r), code::DENIED);
    assert_eq!(lock(&h.ui.send_seen)[0].typed_since_approval, "abcdef");
    // Control: a real key event in the pane (what `TerminalView` calls on
    // Enter, Ctrl-C or Ctrl-U) does clear it, so the check above can fail.
    h.api.user_line_key(&id);
    lock(&h.ui.send).push_back(false);
    let r = c.call("session.send_text", json!({ "session_id": id, "text": "\r" }));
    assert_eq!(rpc_code(r), code::DENIED);
    assert_eq!(lock(&h.ui.send_seen)[1].typed_since_approval, "");
}

/// The "Paired Tools" view lists a grant and revoking it there cuts the tool
/// off on its next request.
#[test]
fn revoking_in_the_paired_tools_view_cuts_the_tool_off() {
    let mut h = start();
    let (id, _session, _) = h.pane();
    let mut c = h.paired(&[Scope::SessionsRead], false);
    let list = c.call("sessions.list", json!({})).expect("list");
    assert_eq!(list["sessions"][0]["id"], json!(id));
    let server = h.api.server().expect("server");
    let tools = PairedTools::load(&server.grants());
    assert_eq!(tools.grants.len(), 1);
    let labels = tools.labels(SystemTime::now());
    assert!(labels[0].starts_with("test-tool · sessions:read · "), "{labels:?}");
    assert_eq!(tools.revoke(&server.grants(), 0).expect("revoke"), Some("test-tool".into()));
    assert_eq!(tools.revoke(&server.grants(), 0).expect("again"), None, "already gone");
    assert_eq!(rpc_code(c.call("sessions.list", json!({}))), code::UNAUTHENTICATED);
}

/// A pane that closes leaves the registry, so the API stops listing it.
#[test]
fn a_closed_pane_leaves_the_api() {
    let mut h = start();
    let (id, _session, _) = h.pane();
    let mut c = h.paired(&[Scope::SessionsRead, Scope::UiFocus], false);
    c.call("session.focus", json!({ "session_id": id })).expect("focus answered by the UI");
    h.api.closed(&id, Some(0));
    let list = c.call("sessions.list", json!({})).expect("list");
    assert_eq!(list["sessions"], json!([]));
    assert_eq!(
        rpc_code(c.call("session.focus", json!({ "session_id": id }))),
        code::SESSION_NOT_FOUND
    );
}

#[test]
fn pairing_prompts_are_limited_across_connections() {
    let mut l = PairingLimiter::default();
    let t0 = Instant::now();
    for _ in 0..MAX_PAIRING_PROMPTS {
        assert!(l.allow(t0));
    }
    assert!(!l.allow(t0), "a fourth prompt within the window is refused");
    assert!(l.allow(t0 + PAIRING_WINDOW), "the window slides");
    let mut l = PairingLimiter::default();
    l.denied(t0);
    assert!(!l.allow(t0 + PAIRING_COOLDOWN - Duration::from_millis(1)));
    assert!(l.allow(t0 + PAIRING_COOLDOWN));
}

#[test]
fn a_denied_pairing_cools_down_without_a_second_dialog() {
    let h = start();
    let mut c = h.client();
    // No scripted answer: the fake UI drops the reply, which is a denial.
    let r = c.call("halyard.pair", json!({ "client_name": "x", "scopes": ["sessions:read"] }));
    assert_eq!(rpc_code(r), code::DENIED);
    let mut c = h.client();
    let r = c.call("halyard.pair", json!({ "client_name": "x", "scopes": ["sessions:read"] }));
    assert_eq!(rpc_code(r), code::DENIED);
    assert_eq!(h.ui.pair_seen.load(Ordering::SeqCst), 1, "the second was refused without asking");
}

#[test]
fn events_are_published_once_per_change() {
    let mut p = EventPublisher::default();
    let obs = |state, cause, note| Observed {
        session_id: "p1",
        state,
        cause,
        agent: Some(&AgentKind::ClaudeCode),
        note,
    };
    assert!(p.observe(obs(AttentionState::Unknown, Cause::Initial, None)).is_empty());
    let ev = p.observe(obs(AttentionState::WaitingForInput, Cause::ProtocolReport, Some("hi")));
    assert_eq!(
        ev,
        vec![SessionEvent::StateChanged {
            session_id: "p1".into(),
            state: AgentState::Waiting,
            agent: Some("claude-code".into()),
            msg: Some("hi".into()),
        }]
    );
    assert!(
        p.observe(obs(AttentionState::WaitingForInput, Cause::ProtocolReport, Some("hi")))
            .is_empty()
    );
    // An OSC notification while waiting: one notification event, no state event.
    let ev = p.observe(obs(AttentionState::WaitingForInput, Cause::Notification, Some("Tests")));
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0].method(), "session.notification");
    // A note the API already published is not sent again.
    p.mark_note("p1", Some("api: body"));
    assert!(
        p.observe(obs(AttentionState::WaitingForInput, Cause::Notification, Some("api: body")))
            .is_empty()
    );
    assert_eq!(
        p.exited("p1", Some(3)),
        SessionEvent::Exited { session_id: "p1".into(), exit_code: Some(3) }
    );
}

#[test]
fn agent_ids_use_the_protocol_names() {
    assert_eq!(agent_id(&AgentKind::ClaudeCode).as_deref(), Some("claude-code"));
    assert_eq!(agent_id(&AgentKind::Other("my-bot".into())).as_deref(), Some("my-bot"));
    assert_eq!(agent_id(&AgentKind::Other("has space".into())), None);
    assert_eq!(wire_state(AttentionState::Unknown), None);
    assert_eq!(wire_state(AttentionState::Finished { exit_code: Some(1) }), Some(AgentState::Done));
}

#[test]
fn screen_text_is_capped_on_a_char_boundary() {
    let s = format!("{}é", "a".repeat(9));
    assert_eq!(cap(s.clone(), 10), "a".repeat(9));
    assert_eq!(cap(s.clone(), 11), s);
}

#[test]
fn dialogs_name_a_pane_by_title_else_folder() {
    let mut info = SessionInfo {
        id: "p7".into(),
        title: " vim ".into(),
        cwd: Some("/src".into()),
        agent: None,
        state: None,
    };
    assert_eq!(pane_label(&info), "\u{201c}vim\u{201d} (id p7)");
    info.title.clear();
    assert_eq!(pane_label(&info), "the pane in /src (id p7)");
    info.cwd = None;
    assert_eq!(pane_label(&info), "a pane (id p7)");
    // A hostile title: a bidi override that would reverse the line, and enough
    // text to push the buttons off screen.
    info.title = format!("evil\u{202e}txt.exe{}", "A".repeat(1000));
    let label = pane_label(&info);
    assert!(label.contains("evil\\u{202e}txt.exe"), "{label}");
    assert!(!label.contains('\u{202e}'));
    assert!(label.chars().count() < MAX_DIALOG_NAME + 20, "{label}");
    assert!(label.contains('\u{2026}'));
}

#[test]
fn dialog_names_are_escaped_and_capped_at_the_limit() {
    let exactly = "a".repeat(MAX_DIALOG_NAME);
    assert_eq!(dialog_text(&exactly, MAX_DIALOG_NAME), exactly, "at the cap: whole");
    let over = "a".repeat(MAX_DIALOG_NAME + 1);
    let cut = dialog_text(&over, MAX_DIALOG_NAME);
    assert_eq!(cut.chars().count(), MAX_DIALOG_NAME);
    assert!(cut.ends_with('\u{2026}'));
    assert_eq!(dialog_text("a\nb\u{1b}", 80), "a\\nb\\e");
    let path = format!("/{}/python3", "d".repeat(300));
    let tail = dialog_tail(&path, MAX_DIALOG_PATH);
    assert_eq!(tail.chars().count(), MAX_DIALOG_PATH);
    assert!(tail.starts_with('\u{2026}') && tail.ends_with("/python3"), "{tail}");
    assert_eq!(dialog_tail("/bin/sh", MAX_DIALOG_PATH), "/bin/sh");
}

#[test]
fn panes_never_inherit_another_instances_socket_or_token() {
    let (set, remove) = pane_env(None);
    assert!(set.is_empty());
    assert_eq!(remove, ["HALYARD_SOCKET", "HALYARD_SESSION_TOKEN"]);
    let creds = pane_credentials(Path::new("/run/h/1.sock"), "p1").expect("creds");
    let (set, remove) = pane_env(Some(&creds));
    let names: Vec<&str> = set.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(names, ["HALYARD_SOCKET", "HALYARD_SESSION_ID", "HALYARD_SESSION_TOKEN"]);
    assert_eq!(set[0].1, "/run/h/1.sock");
    assert!(set[2].1.starts_with("hlys_"));
    // Removed first, then set (`SessionConfig::env` applies after `env_remove`).
    assert_eq!(remove.len(), 2);
}

fn request_for(text: &str) -> SendTextRequest {
    SendTextRequest {
        grant_id: "g".into(),
        client_name: "tool".into(),
        session: SessionInfo {
            id: "p".into(),
            title: String::new(),
            cwd: None,
            agent: None,
            state: None,
        },
        text: text.into(),
        display_text: halyard_ipc::text::escape_for_display(text),
        reason: SendTextReason::ControlCharacters,
        peer: halyard_ipc::PeerInfo { pid: 1, uid: 1, exe: None },
        typed_since_approval: String::new(),
        typed_since_approval_display: String::new(),
        typed_since_approval_truncated: false,
    }
}

/// A dialog that never gets an answer (the window is busy, or the user walked
/// away) is a denial once the timeout passes, for typing and for pairing.
#[test]
fn a_dialog_nobody_answers_is_a_denial() {
    let (tx, mut rx) = futures::channel::mpsc::unbounded();
    let approver = DialogApprover::new(tx, Duration::from_millis(150));
    let started = Instant::now();
    // The request (and its reply sender) sits unanswered in the channel.
    assert!(!approver.approve_send_text(&request_for("ls\n")));
    assert!(started.elapsed() >= Duration::from_millis(150));
    let pairing = PairingRequest {
        client_name: "tool".into(),
        scopes: [Scope::SessionsRead].into_iter().collect(),
        peer: halyard_ipc::PeerInfo { pid: 1, uid: 1, exe: None },
    };
    assert_eq!(approver.approve_pairing(&pairing), PairingDecision::Deny);
    assert!(rx.try_recv().is_ok(), "the dialogs were asked for");
}

/// Text the dialog could not show whole is refused without asking.
#[test]
fn the_approver_never_asks_about_text_over_the_dialog_cap() {
    let (tx, mut rx) = futures::channel::mpsc::unbounded();
    let approver = DialogApprover::new(tx, Duration::from_secs(5));
    let max = crate::approval::MAX_DIALOG_TEXT_BYTES;
    assert_eq!(max, 4096);
    assert!(!approver.approve_send_text(&request_for(&"x".repeat(max + 1))));
    assert!(rx.try_recv().is_err(), "no dialog was requested for 4097 bytes");
    // Exactly the cap is asked about (and, unanswered, times out to a denial).
    let approver = DialogApprover::new(approver.ui.clone(), Duration::from_millis(50));
    assert!(!approver.approve_send_text(&request_for(&"x".repeat(max))));
    assert!(rx.try_recv().is_ok(), "a dialog was requested for 4096 bytes");
}

/// The real key path from the pane (`view::send_user_key`, what
/// `TerminalView::handle_key` calls): Enter clears the record of a tool's standing
/// text, Ctrl-U does not (readline deletes only left of the cursor).
#[test]
fn the_pane_key_path_resets_on_enter_but_not_on_ctrl_u() {
    use halyard_term::{KeyAction, KeyCode, KeyInput, Modifiers};
    let mut h = start();
    let (id, session, _) = h.pane();
    let api = h.api.clone();
    let mut c = h.paired(&[Scope::InputWrite], true);
    c.call("session.send_text", json!({ "session_id": id, "text": "abc" })).expect("standing");
    let ctrl_u = KeyInput {
        key: KeyCode::Char('u'),
        text: None,
        mods: Modifiers { ctrl: true, ..Modifiers::NONE },
        action: KeyAction::Press,
    };
    crate::view::send_user_key(&session, Some(&api), ctrl_u);
    lock(&h.ui.send).push_back(false);
    let r = c.call("session.send_text", json!({ "session_id": id, "text": "\r" }));
    assert_eq!(rpc_code(r), code::DENIED);
    assert_eq!(lock(&h.ui.send_seen)[0].typed_since_approval, "abc", "Ctrl-U kept it");
    c.call("session.send_text", json!({ "session_id": id, "text": "def" })).expect("standing");
    crate::view::send_user_key(
        &session,
        Some(&api),
        KeyInput::new(KeyCode::Enter, Modifiers::NONE),
    );
    lock(&h.ui.send).push_back(false);
    let r = c.call("session.send_text", json!({ "session_id": id, "text": "\r" }));
    assert_eq!(rpc_code(r), code::DENIED);
    assert_eq!(lock(&h.ui.send_seen)[1].typed_since_approval, "", "Enter cleared it");
}
