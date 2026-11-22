//! The cross-pane agent dashboard model.
//!
//! A [`Dashboard`] holds one [`SessionSummary`] per pane and lists them in the order
//! the user should look at them:
//!
//! | rank | sessions |
//! |------|----------|
//! | 0 | WaitingForInput |
//! | 1 | Error |
//! | 2 | Finished and unread |
//! | 3 | Working |
//! | 4 | Idle |
//! | 5 | Finished and read |
//! | 6 | Unknown (no agent seen) |
//!
//! Within a rank, the session that entered its state earliest comes first (it has
//! waited longest), then by session id for a stable order.

use std::path::PathBuf;
use std::time::SystemTime;

use halyard_shell::sanitize::sanitize;

use crate::attention::{AttentionMachine, AttentionState, Cause, MAX_NOTIFICATION_LEN};
use crate::detect::AgentKind;
use crate::git::{GitHead, git_head};

/// One pane as the dashboard shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    /// The pane's `HALYARD_SESSION_ID`.
    pub session_id: String,
    /// Detected or reported agent.
    pub agent: Option<AgentKind>,
    /// Working directory from OSC 7.
    pub cwd: Option<PathBuf>,
    /// Git `HEAD` of the working directory.
    pub git: Option<GitHead>,
    /// Attention state.
    pub state: AttentionState,
    /// The signal behind `state`.
    pub cause: Cause,
    /// When `state` began.
    pub since: Option<SystemTime>,
    /// Last output from the pane.
    pub last_activity: Option<SystemTime>,
    /// True when the pane has news the user has not seen.
    pub unread: bool,
    /// Last notification text, sanitized and capped.
    pub last_notification: Option<String>,
}

impl SessionSummary {
    /// Builds a summary from a pane's machine, reading the git branch of `cwd`.
    pub fn from_machine(
        session_id: impl Into<String>,
        machine: &AttentionMachine,
        cwd: Option<PathBuf>,
    ) -> Self {
        let git = cwd.as_deref().and_then(git_head);
        Self {
            session_id: session_id.into(),
            agent: machine.agent().cloned(),
            git,
            cwd,
            state: machine.state(),
            cause: machine.cause(),
            since: machine.since(),
            last_activity: machine.last_output(),
            unread: machine.unread(),
            last_notification: machine
                .last_notification()
                .map(|n| sanitize(n, MAX_NOTIFICATION_LEN)),
        }
    }

    /// Sort rank: lower needs the user sooner. See the module documentation.
    pub fn rank(&self) -> u8 {
        match self.state {
            AttentionState::WaitingForInput => 0,
            AttentionState::Error => 1,
            AttentionState::Finished { .. } if self.unread => 2,
            AttentionState::Working => 3,
            AttentionState::Idle => 4,
            AttentionState::Finished { .. } => 5,
            AttentionState::Unknown => 6,
        }
    }

    /// The branch name, if `HEAD` is on one.
    pub fn branch(&self) -> Option<&str> {
        match &self.git {
            Some(GitHead::Branch(b)) => Some(b),
            _ => None,
        }
    }
}

/// All panes, in attention order.
#[derive(Debug, Clone, Default)]
pub struct Dashboard {
    sessions: Vec<SessionSummary>,
}

impl Dashboard {
    /// An empty dashboard.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a session or replaces the one with the same id.
    pub fn upsert(&mut self, summary: SessionSummary) {
        match self.sessions.iter_mut().find(|s| s.session_id == summary.session_id) {
            Some(slot) => *slot = summary,
            None => self.sessions.push(summary),
        }
        self.sort();
    }

    /// Removes a session. Returns true if it was present.
    pub fn remove(&mut self, session_id: &str) -> bool {
        let before = self.sessions.len();
        self.sessions.retain(|s| s.session_id != session_id);
        before != self.sessions.len()
    }

    /// Sessions in attention order.
    pub fn sessions(&self) -> &[SessionSummary] {
        &self.sessions
    }

    /// The first session that needs the user: waiting, error, or finished unread.
    pub fn next_needing_attention(&self) -> Option<&SessionSummary> {
        self.sessions.first().filter(|s| s.rank() <= 2)
    }

    /// Number of sessions with unread news.
    pub fn unread_count(&self) -> usize {
        self.sessions.iter().filter(|s| s.unread).count()
    }

    fn sort(&mut self) {
        self.sessions.sort_by(|a, b| {
            a.rank()
                .cmp(&b.rank())
                .then_with(|| match (a.since, b.since) {
                    (Some(x), Some(y)) => x.cmp(&y),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                })
                .then_with(|| a.session_id.cmp(&b.session_id))
        });
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::attention::{AttentionConfig, AttentionEvent};

    fn t(s: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + s)
    }

    fn summary(
        id: &str,
        state: AttentionState,
        unread: bool,
        since: Option<u64>,
    ) -> SessionSummary {
        SessionSummary {
            session_id: id.into(),
            agent: Some(AgentKind::ClaudeCode),
            cwd: None,
            git: None,
            state,
            cause: Cause::Initial,
            since: since.map(t),
            last_activity: None,
            unread,
            last_notification: None,
        }
    }

    fn order(d: &Dashboard) -> Vec<&str> {
        d.sessions().iter().map(|s| s.session_id.as_str()).collect()
    }

    #[test]
    fn sort_order() {
        use AttentionState::*;
        let mut d = Dashboard::new();
        for s in [
            summary("unknown", Unknown, false, None),
            summary("fin-read", Finished { exit_code: Some(0) }, false, Some(1)),
            summary("idle", Idle, false, Some(1)),
            summary("working", Working, false, Some(1)),
            summary("fin-unread", Finished { exit_code: Some(1) }, true, Some(1)),
            summary("error", Error, false, Some(1)),
            summary("waiting-late", WaitingForInput, true, Some(50)),
            summary("waiting-early", WaitingForInput, false, Some(10)),
        ] {
            d.upsert(s);
        }
        assert_eq!(
            order(&d),
            vec![
                "waiting-early",
                "waiting-late",
                "error",
                "fin-unread",
                "working",
                "idle",
                "fin-read",
                "unknown"
            ]
        );
        assert_eq!(d.next_needing_attention().unwrap().session_id, "waiting-early");
        assert_eq!(d.unread_count(), 2);
    }

    #[test]
    fn ties_break_by_since_then_id() {
        let mut d = Dashboard::new();
        d.upsert(summary("b", AttentionState::Working, false, Some(5)));
        d.upsert(summary("a", AttentionState::Working, false, Some(5)));
        d.upsert(summary("c", AttentionState::Working, false, Some(1)));
        d.upsert(summary("z", AttentionState::Working, false, None));
        assert_eq!(order(&d), vec!["c", "a", "b", "z"]);
    }

    #[test]
    fn upsert_replaces_and_resorts_remove_deletes() {
        let mut d = Dashboard::new();
        d.upsert(summary("a", AttentionState::Working, false, Some(1)));
        d.upsert(summary("b", AttentionState::Idle, false, Some(1)));
        assert_eq!(order(&d), vec!["a", "b"]);
        d.upsert(summary("b", AttentionState::WaitingForInput, true, Some(2)));
        assert_eq!(order(&d), vec!["b", "a"]);
        assert_eq!(d.sessions().len(), 2);
        assert!(d.remove("b"));
        assert!(!d.remove("b"));
        assert_eq!(order(&d), vec!["a"]);
        assert!(d.next_needing_attention().is_none());
    }

    #[test]
    fn summary_from_machine_reads_state_and_git() {
        let dir = std::env::temp_dir().join(format!("halyard-dash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join(".git/HEAD"), "ref: refs/heads/agent-work\n").unwrap();

        let mut m = AttentionMachine::new(AttentionConfig::default(), Some("s".into()));
        m.handle(AttentionEvent::AgentStarted(AgentKind::Codex), t(0));
        m.handle(AttentionEvent::Output, t(1));
        m.handle(AttentionEvent::Notification(format!("approve?\x1b[2J{}", "y".repeat(400))), t(2));
        let s = SessionSummary::from_machine("s", &m, Some(dir.clone()));
        std::fs::remove_dir_all(&dir).unwrap();

        assert_eq!(s.agent, Some(AgentKind::Codex));
        assert_eq!(s.branch(), Some("agent-work"));
        assert_eq!(s.state, AttentionState::WaitingForInput);
        assert_eq!(s.cause, Cause::Notification);
        assert_eq!(s.since, Some(t(2)));
        assert_eq!(s.last_activity, Some(t(1)));
        assert!(s.unread);
        assert_eq!(s.rank(), 0);
        let note = s.last_notification.unwrap();
        assert!(note.starts_with("approve?[2J"));
        assert_eq!(note.chars().count(), MAX_NOTIFICATION_LEN);

        let none = SessionSummary::from_machine("x", &m, None);
        assert_eq!(none.git, None);
        assert_eq!(none.branch(), None);
    }
}
