//! Shell integration for Halyard.
//!
//! This crate is pure logic with no dependency on the VT engine:
//!
//! * [`scanner`]: an incremental scanner that pulls terminal signals (OSC 133 and 633
//!   prompt marks, OSC 7 working directory, OSC 9 and 777 notifications, OSC 9;4
//!   progress, titles, the bell, the alternate screen, and Halyard's own session
//!   protocol) out of raw PTY output chunks split at arbitrary boundaries.
//! * [`blocks`]: a command block model fed by those signals plus the row on which
//!   each one occurred.
//! * [`integration`]: the zsh, bash and fish scripts that make a shell emit the marks.
//!
//! Everything read from the PTY is untrusted. Strings are length capped and stripped
//! of control characters before they leave the scanner.

pub mod blocks;
pub mod integration;
pub mod sanitize;
pub mod scanner;

pub use blocks::{Block, BlockId, BlockState, BlockTracker, Clock, SystemClock};
pub use integration::{Shell, install_line, integration_script};
pub use scanner::{
    AgentReport, NotificationSource, ProgressState, ReportedCwd, ReportedState, Scanner,
    ShellEvent, TitleKind,
};
