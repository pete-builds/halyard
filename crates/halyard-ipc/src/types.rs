//! Shared data types: agent states, session descriptions, events and scopes.

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The attention state an agent reports for its session.
///
/// This is the same vocabulary as the escape-sequence protocol (`docs/protocol.md`):
/// `working`, `waiting`, `idle`, `done`, `error`. Nothing else is accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentState {
    /// The agent is busy producing output or running tools.
    Working,
    /// The agent is blocked on the user (a permission prompt, a question).
    Waiting,
    /// The agent is running but has nothing to do.
    Idle,
    /// The agent finished its turn or task.
    Done,
    /// The agent stopped with an error.
    Error,
}

impl AgentState {
    /// Every state, in documentation order.
    pub const ALL: [AgentState; 5] =
        [Self::Working, Self::Waiting, Self::Idle, Self::Done, Self::Error];

    /// The wire name of the state.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Waiting => "waiting",
            Self::Idle => "idle",
            Self::Done => "done",
            Self::Error => "error",
        }
    }

    /// Parses a wire name. Returns `None` for anything outside the vocabulary.
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|state| state.as_str() == s)
    }
}

impl fmt::Display for AgentState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a controller is told about one session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionInfo {
    /// The session id, the same value the pane exports as `HALYARD_SESSION_ID`.
    pub id: String,
    /// The pane title.
    pub title: String,
    /// The working directory, when the shell reported one (OSC 7).
    pub cwd: Option<String>,
    /// The agent kind detected or reported in the pane (`claude-code`, `codex`, ...).
    pub agent: Option<String>,
    /// The last known agent state, or `None` when unknown.
    pub state: Option<AgentState>,
}

/// A state report from an agent inside a session (`session.report_state`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateReport {
    /// The reported state.
    pub state: AgentState,
    /// The agent name, when given (`claude-code`).
    pub agent: Option<String>,
    /// A short human-readable message, when given. Untrusted text.
    pub msg: Option<String>,
}

/// An event the app publishes to subscribed controllers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    /// A session's agent state changed.
    StateChanged {
        /// The session.
        session_id: String,
        /// The new state.
        state: AgentState,
        /// The agent, when known.
        agent: Option<String>,
        /// The message attached to the change, when any.
        msg: Option<String>,
    },
    /// An agent or program in a session posted a notification.
    Notification {
        /// The session.
        session_id: String,
        /// Notification title. Untrusted text.
        title: String,
        /// Notification body. Untrusted text.
        body: String,
    },
    /// A session's child process exited and the session is gone.
    Exited {
        /// The session.
        session_id: String,
        /// The exit code, when the process exited normally.
        exit_code: Option<i32>,
    },
}

impl SessionEvent {
    /// The JSON-RPC notification method name for this event.
    pub fn method(&self) -> &'static str {
        match self {
            Self::StateChanged { .. } => "session.state_changed",
            Self::Notification { .. } => "session.notification",
            Self::Exited { .. } => "session.exited",
        }
    }

    /// The JSON-RPC notification params for this event.
    pub fn params(&self) -> Value {
        match self {
            Self::StateChanged { session_id, state, agent, msg } => {
                json!({ "session_id": session_id, "state": state, "agent": agent, "msg": msg })
            }
            Self::Notification { session_id, title, body } => {
                json!({ "session_id": session_id, "title": title, "body": body })
            }
            Self::Exited { session_id, exit_code } => {
                json!({ "session_id": session_id, "exit_code": exit_code })
            }
        }
    }
}

/// A named capability a controller can be granted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Scope {
    /// List sessions with title, cwd, agent and state (`sessions.list`).
    #[serde(rename = "sessions:read")]
    SessionsRead,
    /// Focus a pane (`session.focus`).
    #[serde(rename = "ui:focus")]
    UiFocus,
    /// Read a pane's visible screen text (`session.read_screen`).
    #[serde(rename = "screen:read")]
    ScreenRead,
    /// Type text into a pane (`session.send_text`). Always gated by the user.
    #[serde(rename = "input:write")]
    InputWrite,
    /// Receive session events (`events.subscribe`).
    #[serde(rename = "events:subscribe")]
    EventsSubscribe,
}

impl Scope {
    /// Every scope.
    pub const ALL: [Scope; 5] = [
        Self::SessionsRead,
        Self::UiFocus,
        Self::ScreenRead,
        Self::InputWrite,
        Self::EventsSubscribe,
    ];

    /// The wire name of the scope.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SessionsRead => "sessions:read",
            Self::UiFocus => "ui:focus",
            Self::ScreenRead => "screen:read",
            Self::InputWrite => "input:write",
            Self::EventsSubscribe => "events:subscribe",
        }
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What the server knows about the process on the other end of a connection.
///
/// Taken from the kernel (`SO_PEERCRED` on Linux, `getpeereid` and `LOCAL_PEERPID` on
/// macOS) when the connection is accepted, so a client cannot lie about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerInfo {
    /// Process id of the connecting process, as recorded at `connect()` time.
    pub pid: u32,
    /// Effective user id of the connecting process.
    pub uid: u32,
    /// Executable path, when the platform reports it. Shown to the user in dialogs.
    pub exe: Option<PathBuf>,
}
