//! The attention state machine.
//!
//! One [`AttentionMachine`] per pane. It is driven by timestamped
//! [`AttentionEvent`]s and by [`AttentionMachine::tick`], and never reads a clock
//! itself. Every state records the [`Cause`] that produced it. The rules, in the
//! order they are checked, are documented in `docs/agents.md`; in short:
//!
//! 1. A Halyard protocol report with `state=` always wins, and holds until the next
//!    report, user input, the agent exiting, or the shell's end-of-command mark.
//! 2. A notification (OSC 9, OSC 777) or bell while an agent is active means
//!    WaitingForInput, latched until user input.
//! 3. User input releases either latch; the state then follows output activity.
//! 4. Output while an agent is active (and nothing is latched) means Working.
//! 5. No output for `idle_after` (default 8 s) while Working means Idle, never
//!    "waiting": silence alone does not prove the agent wants the user.
//! 6. The agent process leaving the foreground, or the shell's OSC 133 D, means
//!    Finished, with the exit status when known.
//! 7. Error only ever comes from a protocol report.
//!
//! Unread is set when the state becomes WaitingForInput or Finished while the pane is
//! not focused, and cleared when it gains focus. Nothing unobservable (token counts,
//! cost, model) is tracked.

use std::time::{Duration, SystemTime};

use halyard_shell::sanitize::{limits, sanitize};
use halyard_shell::scanner::{AgentReport, ReportedState, ShellEvent};

use crate::detect::AgentKind;

/// Default quiet period before Working becomes Idle.
pub const DEFAULT_IDLE_AFTER: Duration = Duration::from_secs(8);

/// Longest notification text kept, in characters.
pub const MAX_NOTIFICATION_LEN: usize = 256;

/// Tunables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttentionConfig {
    /// Quiet period after which Working becomes Idle.
    pub idle_after: Duration,
}

impl Default for AttentionConfig {
    fn default() -> Self {
        Self { idle_after: DEFAULT_IDLE_AFTER }
    }
}

/// What the pane's agent is doing, as far as Halyard can observe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttentionState {
    /// No agent has been observed in this pane.
    Unknown,
    /// The agent is producing output, or said it is working.
    Working,
    /// The agent asked for the user (notification, bell, or protocol report).
    WaitingForInput,
    /// The agent exited or reported it is done.
    Finished {
        /// Exit status, when the agent exit or the shell's D mark supplied one.
        exit_code: Option<i32>,
    },
    /// The agent is running but has been quiet, or said it is idle.
    Idle,
    /// The agent reported an error through the protocol.
    Error,
}

/// The signal that put the machine in its current state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    /// Nothing observed yet.
    Initial,
    /// A Halyard protocol report with `state=`.
    ProtocolReport,
    /// An OSC 9 or OSC 777 notification.
    Notification,
    /// A bell.
    Bell,
    /// Output from the PTY.
    OutputActivity,
    /// No output for the configured idle period.
    Quiet,
    /// An agent became the foreground process.
    AgentStarted,
    /// The agent left the foreground.
    AgentExited,
    /// The shell's OSC 133 D (end of command).
    CommandFinished,
    /// The user typed into the pane.
    UserInput,
}

/// An observation about one pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttentionEvent {
    /// The PTY produced output.
    Output,
    /// OSC 9 or OSC 777 notification text (title and body joined by the caller).
    Notification(String),
    /// A bell.
    Bell,
    /// The shell's end-of-command mark, with its exit status.
    CommandFinished {
        /// Exit status, when the mark had one.
        exit_code: Option<i32>,
    },
    /// A Halyard protocol report.
    Report(AgentReport),
    /// The foreground process became an agent of this kind.
    AgentStarted(AgentKind),
    /// The agent left the foreground. The exit status is known only when Halyard
    /// spawned the agent itself; otherwise the shell's D mark supplies it.
    AgentExited {
        /// Exit status, if known.
        exit_code: Option<i32>,
    },
    /// The user typed into the pane (keys or paste, not focus or mouse moves).
    UserInput,
    /// The pane gained (`true`) or lost focus.
    Focus(bool),
}

impl AttentionEvent {
    /// Maps a scanner event to an attention event, for the signals the attention
    /// model uses. Others (prompt marks, cwd, titles, progress) return `None`.
    pub fn from_shell_event(event: &ShellEvent) -> Option<Self> {
        match event {
            ShellEvent::Bell => Some(Self::Bell),
            ShellEvent::Notification { title, body, .. } => Some(Self::Notification(match title {
                Some(t) if !body.is_empty() => format!("{t}: {body}"),
                Some(t) => t.clone(),
                None => body.clone(),
            })),
            ShellEvent::CommandFinished { exit_code, .. } => {
                Some(Self::CommandFinished { exit_code: *exit_code })
            }
            ShellEvent::Agent(report) => Some(Self::Report(report.clone())),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Latch {
    None,
    /// A notification or bell: WaitingForInput until user input.
    Waiting,
    /// A protocol report: its state until the next report or user input.
    Protocol,
}

/// Attention state for one pane.
#[derive(Debug, Clone)]
pub struct AttentionMachine {
    config: AttentionConfig,
    session_id: Option<String>,
    state: AttentionState,
    cause: Cause,
    since: Option<SystemTime>,
    agent: Option<AgentKind>,
    agent_active: bool,
    latch: Latch,
    focused: bool,
    unread: bool,
    last_output: Option<SystemTime>,
    last_notification: Option<String>,
}

impl AttentionMachine {
    /// A machine for a pane whose `HALYARD_SESSION_ID` is `session_id`. Reports
    /// naming a different session are ignored.
    pub fn new(config: AttentionConfig, session_id: Option<String>) -> Self {
        Self {
            config,
            session_id,
            state: AttentionState::Unknown,
            cause: Cause::Initial,
            since: None,
            agent: None,
            agent_active: false,
            latch: Latch::None,
            focused: false,
            unread: false,
            last_output: None,
            last_notification: None,
        }
    }

    /// Current state.
    pub fn state(&self) -> AttentionState {
        self.state
    }

    /// The signal behind the current state.
    pub fn cause(&self) -> Cause {
        self.cause
    }

    /// When the current state began.
    pub fn since(&self) -> Option<SystemTime> {
        self.since
    }

    /// The agent detected or reported most recently, kept after it exits.
    pub fn agent(&self) -> Option<&AgentKind> {
        self.agent.as_ref()
    }

    /// True while an agent is running in the foreground (detected, or reporting
    /// through the protocol).
    pub fn agent_active(&self) -> bool {
        self.agent_active
    }

    /// True when the pane has news the user has not looked at.
    pub fn unread(&self) -> bool {
        self.unread
    }

    /// True when the pane has focus.
    pub fn focused(&self) -> bool {
        self.focused
    }

    /// Time of the last output.
    pub fn last_output(&self) -> Option<SystemTime> {
        self.last_output
    }

    /// The last notification or protocol `msg=`, sanitized and capped.
    pub fn last_notification(&self) -> Option<&str> {
        self.last_notification.as_deref()
    }

    /// The configured session id.
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    fn set(&mut self, state: AttentionState, cause: Cause, at: SystemTime) {
        if state != self.state {
            self.state = state;
            self.cause = cause;
            self.since = Some(at);
            if !self.focused
                && matches!(
                    state,
                    AttentionState::WaitingForInput | AttentionState::Finished { .. }
                )
            {
                self.unread = true;
            }
        }
    }

    fn note(&mut self, text: &str) {
        let t = sanitize(text, MAX_NOTIFICATION_LEN.min(limits::NOTIFICATION));
        if !t.is_empty() {
            self.last_notification = Some(t);
        }
    }

    /// What the state is with no latch: Working when output arrived within the idle
    /// period, Idle otherwise.
    fn follow_activity(&mut self, cause: Cause, at: SystemTime) {
        let recent = self
            .last_output
            .map(|t| at.duration_since(t).unwrap_or(Duration::ZERO))
            .is_some_and(|d| d < self.config.idle_after);
        let state = if recent { AttentionState::Working } else { AttentionState::Idle };
        self.set(state, cause, at);
    }

    /// Applies one event observed at `at`.
    pub fn handle(&mut self, event: AttentionEvent, at: SystemTime) {
        match event {
            AttentionEvent::Output => {
                self.last_output = Some(at);
                if self.agent_active && self.latch == Latch::None {
                    self.set(AttentionState::Working, Cause::OutputActivity, at);
                }
            }
            AttentionEvent::Notification(text) => {
                self.note(&text);
                self.attention_request(Cause::Notification, at);
            }
            AttentionEvent::Bell => self.attention_request(Cause::Bell, at),
            AttentionEvent::CommandFinished { exit_code } => {
                if self.agent_active {
                    self.agent_active = false;
                    self.latch = Latch::None;
                    self.set(AttentionState::Finished { exit_code }, Cause::CommandFinished, at);
                } else if self.state == (AttentionState::Finished { exit_code: None })
                    && exit_code.is_some()
                {
                    // The agent exit was seen first; the shell now supplies the status.
                    self.state = AttentionState::Finished { exit_code };
                }
            }
            AttentionEvent::Report(report) => self.report(report, at),
            AttentionEvent::AgentStarted(kind) => {
                self.agent = Some(kind);
                if !self.agent_active {
                    // A new run: Working from now, whatever the previous state was.
                    // An agent already active through protocol reports keeps its state.
                    self.agent_active = true;
                    self.latch = Latch::None;
                    self.state = AttentionState::Working;
                    self.cause = Cause::AgentStarted;
                    self.since = Some(at);
                }
            }
            AttentionEvent::AgentExited { exit_code } => {
                if self.agent_active {
                    self.agent_active = false;
                    self.latch = Latch::None;
                    self.set(AttentionState::Finished { exit_code }, Cause::AgentExited, at);
                }
            }
            AttentionEvent::UserInput => {
                if self.latch != Latch::None {
                    self.latch = Latch::None;
                    if self.agent_active {
                        self.follow_activity(Cause::UserInput, at);
                    }
                }
            }
            AttentionEvent::Focus(focused) => {
                self.focused = focused;
                if focused {
                    self.unread = false;
                }
            }
        }
    }

    fn attention_request(&mut self, cause: Cause, at: SystemTime) {
        if self.agent_active && self.latch != Latch::Protocol {
            self.latch = Latch::Waiting;
            self.set(AttentionState::WaitingForInput, cause, at);
        }
    }

    fn report(&mut self, report: AgentReport, at: SystemTime) {
        if let (Some(theirs), Some(ours)) = (&report.session, &self.session_id)
            && theirs != ours
        {
            return;
        }
        if let Some(name) = &report.agent {
            self.agent = Some(AgentKind::from_reported_name(name));
        }
        if let Some(msg) = &report.msg {
            self.note(msg);
        }
        let Some(state) = report.state else {
            return;
        };
        self.agent_active = true;
        self.latch = Latch::Protocol;
        let state = match state {
            ReportedState::Working => AttentionState::Working,
            ReportedState::Waiting => AttentionState::WaitingForInput,
            ReportedState::Idle => AttentionState::Idle,
            ReportedState::Done => AttentionState::Finished { exit_code: report.exit_code },
            ReportedState::Error => AttentionState::Error,
        };
        self.set(state, Cause::ProtocolReport, at);
    }

    /// Advances time to `now`: Working becomes Idle after the quiet period.
    pub fn tick(&mut self, now: SystemTime) {
        if !self.agent_active || self.latch != Latch::None || self.state != AttentionState::Working
        {
            return;
        }
        let last = self.last_output.max(self.since);
        let quiet =
            last.map_or(Duration::ZERO, |t| now.duration_since(t).unwrap_or(Duration::ZERO));
        if quiet >= self.config.idle_after {
            self.set(AttentionState::Idle, Cause::Quiet, now);
        }
    }
}

#[cfg(test)]
mod tests;
