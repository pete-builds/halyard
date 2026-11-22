//! Agent awareness for Halyard.
//!
//! Coding agents run as ordinary programs in a pane. This crate derives what they
//! are doing only from signals Halyard can observe (ADR 0004):
//!
//! * [`detect`]: which agent, from the foreground process's name and argv.
//! * [`platform`]: finding that foreground process for a PTY on macOS and Linux.
//! * [`attention`]: the attention state machine. Every state names the signal that
//!   caused it; nothing is guessed. Rules are listed in `docs/agents.md`.
//! * [`dashboard`]: the cross-pane list with git branch, sorted by who needs the
//!   user first.
//! * [`git`]: reading the current branch from `.git/HEAD` without running git.
//!
//! UI wiring (polling the foreground process, focus and input events, the sidebar) is
//! not part of this crate.

pub mod attention;
pub mod dashboard;
pub mod detect;
pub mod git;
pub mod platform;

pub use attention::{AttentionConfig, AttentionEvent, AttentionMachine, AttentionState, Cause};
pub use dashboard::{Dashboard, SessionSummary};
pub use detect::{AgentKind, ProcessInfo, detect_agent};
pub use git::{GitHead, git_head};
