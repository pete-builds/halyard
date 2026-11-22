//! Test fixtures: an in-memory session directory and a scripted approver.

#![allow(dead_code)]

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use halyard_ipc::{
    Approver, Client, DirectoryError, GrantStore, IpcServer, Limits, PairingDecision,
    PairingRequest, Scope, SendTextRequest, ServerConfig, SessionAuth, SessionDirectory,
    SessionInfo, SessionToken, StateReport,
};
use serde_json::{Value, json};

#[derive(Default)]
pub struct FakeDirectory {
    pub sessions: Mutex<HashMap<String, (SessionInfo, SessionAuth)>>,
    pub reports: Mutex<Vec<(String, StateReport)>>,
    pub notes: Mutex<Vec<(String, String, String)>>,
    pub sent: Mutex<Vec<(String, String)>>,
    pub focused: Mutex<Vec<String>>,
}

impl FakeDirectory {
    /// Registers a session whose PTY child is `child_pid`, returning its token.
    pub fn add(&self, id: &str, child_pid: u32) -> SessionToken {
        let token = SessionToken::generate();
        let info = SessionInfo {
            id: id.into(),
            title: format!("pane {id}"),
            cwd: Some("/tmp".into()),
            agent: None,
            state: None,
        };
        let auth = SessionAuth { child_pid, token_hash: token.hash() };
        self.sessions.lock().unwrap().insert(id.into(), (info, auth));
        token
    }

    pub fn reports_for(&self, id: &str) -> Vec<StateReport> {
        self.reports
            .lock()
            .unwrap()
            .iter()
            .filter(|(s, _)| s == id)
            .map(|(_, r)| r.clone())
            .collect()
    }
}

impl SessionDirectory for FakeDirectory {
    fn list_sessions(&self) -> Vec<SessionInfo> {
        let mut list: Vec<_> =
            self.sessions.lock().unwrap().values().map(|(i, _)| i.clone()).collect();
        list.sort_by(|a, b| a.id.cmp(&b.id));
        list
    }
    fn session_auth(&self, id: &str) -> Option<SessionAuth> {
        self.sessions.lock().unwrap().get(id).map(|(_, a)| *a)
    }
    fn report_state(&self, id: &str, report: &StateReport) -> Result<(), DirectoryError> {
        self.reports.lock().unwrap().push((id.into(), report.clone()));
        Ok(())
    }
    fn notify(&self, id: &str, title: &str, body: &str) -> Result<(), DirectoryError> {
        self.notes.lock().unwrap().push((id.into(), title.into(), body.into()));
        Ok(())
    }
    fn focus(&self, id: &str) -> Result<(), DirectoryError> {
        if !self.sessions.lock().unwrap().contains_key(id) {
            return Err(DirectoryError::NotFound);
        }
        self.focused.lock().unwrap().push(id.into());
        Ok(())
    }
    fn read_screen(&self, id: &str) -> Result<String, DirectoryError> {
        if !self.sessions.lock().unwrap().contains_key(id) {
            return Err(DirectoryError::NotFound);
        }
        Ok(format!("$ screen of {id}"))
    }
    fn send_text(&self, id: &str, text: &str) -> Result<(), DirectoryError> {
        self.sent.lock().unwrap().push((id.into(), text.into()));
        Ok(())
    }
}

/// Answers from a script; an empty script denies.
#[derive(Default)]
pub struct ScriptedApprover {
    pub pair: Mutex<VecDeque<PairingDecision>>,
    pub send: Mutex<VecDeque<bool>>,
    pub pair_seen: Mutex<Vec<PairingRequest>>,
    pub send_seen: Mutex<Vec<SendTextRequest>>,
}

impl ScriptedApprover {
    pub fn approve_pair(&self, scopes: &[Scope], always_allow_input: bool) {
        self.pair.lock().unwrap().push_back(PairingDecision::Approve {
            scopes: scopes.iter().copied().collect::<BTreeSet<_>>(),
            always_allow_input,
        });
    }
    pub fn answer_send(&self, yes: bool) {
        self.send.lock().unwrap().push_back(yes);
    }
}

impl Approver for ScriptedApprover {
    fn approve_pairing(&self, request: &PairingRequest) -> PairingDecision {
        self.pair_seen.lock().unwrap().push(request.clone());
        self.pair.lock().unwrap().pop_front().unwrap_or(PairingDecision::Deny)
    }
    fn approve_send_text(&self, request: &SendTextRequest) -> bool {
        self.send_seen.lock().unwrap().push(request.clone());
        self.send.lock().unwrap().pop_front().unwrap_or(false)
    }
}

pub struct Harness {
    pub tmp: tempfile::TempDir,
    pub server: IpcServer,
    pub dir: Arc<FakeDirectory>,
    pub approver: Arc<ScriptedApprover>,
    pub socket: PathBuf,
    pub grants_path: PathBuf,
}

pub fn start() -> Harness {
    start_with(Limits::default())
}

pub fn start_with(limits: Limits) -> Harness {
    let tmp = tempfile::tempdir().expect("tempdir");
    let socket = tmp.path().join("run").join("t.sock");
    let grants_path = tmp.path().join("state").join("grants.json");
    let grants = GrantStore::open(&grants_path).expect("grant store");
    let mut config = ServerConfig::new(&socket, grants);
    config.limits = limits;
    let dir = Arc::new(FakeDirectory::default());
    let approver = Arc::new(ScriptedApprover::default());
    let server = IpcServer::bind(config, dir.clone(), approver.clone()).expect("bind");
    Harness { tmp, server, dir, approver, socket, grants_path }
}

impl Harness {
    pub fn connect(&self) -> Client {
        Client::connect(&self.socket, Some(Duration::from_secs(10))).expect("connect")
    }

    pub fn hello(&self) -> Client {
        let mut c = self.connect();
        c.hello("test").expect("hello");
        c
    }

    /// Pairs a controller with the given approved scopes; returns the client and token.
    pub fn controller(&self, scopes: &[Scope], always_allow_input: bool) -> (Client, String) {
        self.approver.approve_pair(scopes, always_allow_input);
        let mut c = self.hello();
        let names: Vec<&str> = scopes.iter().map(|s| s.as_str()).collect();
        let result = c
            .call("halyard.pair", json!({ "client_name": "test-tool", "scopes": names }))
            .expect("pair");
        let token = result["token"].as_str().expect("token").to_owned();
        (c, token)
    }
}

pub fn code_of(result: Result<Value, halyard_ipc::ClientError>) -> i64 {
    match result {
        Ok(v) => panic!("expected an error, got {v}"),
        Err(e) => e.rpc_code().unwrap_or_else(|| panic!("expected an RPC error, got {e}")),
    }
}
