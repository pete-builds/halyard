//! One pane's signals put together: the scanner's events, command blocks, the
//! attention machine and the foreground process.
//!
//! The app splits the work across two threads:
//!
//! * the pane's session thread runs a [`SignalScanner`] over every PTY output
//!   chunk. The scanner finds the escape sequences, the session feeds the engine up
//!   to each one and hands back the cursor row there, and
//!   [`SignalScanner::finish`] applies the chunk to the pane's [`PaneMonitor`]
//!   (one lock per chunk).
//! * the UI thread feeds user input, focus and the foreground process (polled
//!   about every 500 ms while a command runs) to the same [`PaneMonitor`], and
//!   reads it to draw indicators and the dashboard.
//!
//! Nothing here reads a clock: every call takes the time it happened, so tests run
//! on a fake clock.

use std::time::SystemTime;

use halyard_shell::{AgentReport, BlockTracker, Clock, ReportedCwd, Scanner, ShellEvent};

use crate::attention::{AttentionConfig, AttentionEvent, AttentionMachine, AttentionState};
use crate::dashboard::SessionSummary;
use crate::detect::{AgentKind, ProcessInfo, detect_agent};

/// What holds a pane's terminal, from the caller's poll of the operating system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Foreground {
    /// The shell itself (no command running).
    Shell,
    /// A command, which may be an agent.
    Command {
        /// Its process group.
        pgid: i32,
        /// The agent it was recognized as, if any.
        agent: Option<AgentKind>,
    },
}

/// What the UI shows about a pane, compared to tell whether a redraw is needed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Visible {
    state: AttentionState,
    unread: bool,
    agent: Option<AgentKind>,
    note: Option<String>,
}

/// The signals of one pane.
pub struct PaneMonitor<C: Clock> {
    blocks: BlockTracker<C>,
    attention: AttentionMachine,
    cwd: Option<ReportedCwd>,
    /// The foreground group last seen running a detected agent.
    detected: Option<(i32, AgentKind)>,
    /// Set when what the UI shows may have changed; cleared by
    /// [`PaneMonitor::take_changed`].
    changed: bool,
    /// Blocks changed (a block opened or closed) since the last
    /// [`PaneMonitor::take_blocks_changed`].
    blocks_changed: bool,
    /// Scrollback rows of the primary screen after the last chunk, plus the
    /// rows dropped from its top by then: everything the screen ever kept.
    history_total: Option<u64>,
    /// Rows dropped from the top of scrollback so far. Block rows are counted
    /// from the first row the session ever had; subtract this for screen rows.
    row_base: u64,
}

impl<C: Clock> std::fmt::Debug for PaneMonitor<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PaneMonitor")
            .field("attention", &self.attention)
            .field("blocks", &self.blocks)
            .field("detected", &self.detected)
            .finish_non_exhaustive()
    }
}

impl<C: Clock> PaneMonitor<C> {
    /// A monitor for the pane whose `HALYARD_SESSION_ID` is `session_id`, keeping
    /// at most `block_history` closed command blocks.
    pub fn new(
        session_id: impl Into<String>,
        config: AttentionConfig,
        clock: C,
        block_history: usize,
    ) -> Self {
        Self {
            blocks: BlockTracker::new(clock, block_history),
            attention: AttentionMachine::new(config, Some(session_id.into())),
            cwd: None,
            detected: None,
            changed: false,
            blocks_changed: false,
            history_total: None,
            row_base: 0,
        }
    }

    fn visible(&self) -> Visible {
        Visible {
            state: self.attention.state(),
            unread: self.attention.unread(),
            agent: self.attention.agent().cloned(),
            note: self.attention.last_notification().map(str::to_owned),
        }
    }

    fn apply(&mut self, event: AttentionEvent, at: SystemTime) {
        let before = self.visible();
        self.attention.handle(event, at);
        if self.visible() != before {
            self.changed = true;
        }
    }

    /// The PTY produced output (any chunk, before its events).
    pub fn on_output(&mut self, at: SystemTime) {
        self.apply(AttentionEvent::Output, at);
    }

    /// One scanner event that occurred with the cursor on absolute row `row`.
    pub fn on_shell_event(&mut self, event: &ShellEvent, row: u64, at: SystemTime) {
        let before = (self.blocks.running().map(|b| b.id), self.blocks.history().len());
        // A mark above the start of the newest block means the screen was erased
        // and written again from there (`clear` homes the cursor and erases):
        // blocks reaching those rows now point at other text.
        let newest_start = self
            .blocks
            .running()
            .or_else(|| self.blocks.history().last())
            .and_then(halyard_shell::Block::start_row);
        self.blocks.on_event(event, row);
        if newest_start.is_some_and(|start| row < start) {
            self.blocks.forget_from(row);
        }
        let after = (self.blocks.running().map(|b| b.id), self.blocks.history().len());
        if before != after {
            self.blocks_changed = true;
        }
        if let ShellEvent::Cwd(cwd) = event {
            self.cwd = Some(cwd.clone());
        }
        let Some(e) = AttentionEvent::from_shell_event(event) else { return };
        let counts = match &e {
            // A protocol report counts only when it names this pane's session:
            // anything printed in the pane (a `cat` of a file) can carry a report,
            // but only a program started in this pane can know the id, from
            // `HALYARD_SESSION_ID`. Without it the report is plain output.
            AttentionEvent::Report(r) => {
                r.session.is_some() && r.session.as_deref() == self.attention.session_id()
            }
            // A notification or bell matters only while an agent runs, and its
            // text is kept only then, so a `cat` before the agent starts cannot
            // plant the message a later notification would show.
            AttentionEvent::Notification(_) | AttentionEvent::Bell => self.attention.agent_active(),
            _ => true,
        };
        if !counts {
            return;
        }
        self.apply(e, at);
    }

    /// A state report through the session API (`session.report_state`). The server
    /// already checked that the reporter runs inside this pane (process ancestry plus the
    /// pane's token), so unlike an escape-sequence report it needs no `session=` match:
    /// it is applied as this pane's own report. `msg` is untrusted text and is
    /// sanitized like a notification.
    pub fn on_api_report(&mut self, mut report: AgentReport, at: SystemTime) {
        report.session = self.attention.session_id().map(str::to_owned);
        self.apply(AttentionEvent::Report(report), at);
    }

    /// A notification through the session API (`session.notify`), already verified
    /// to come from inside this pane. The text is untrusted and sanitized.
    pub fn on_api_notification(&mut self, text: &str, at: SystemTime) {
        self.apply(AttentionEvent::Notification(text.to_owned()), at);
    }

    /// The primary screen's scrollback after a chunk: `rows` now, `dropped`
    /// from its top since the session started (the engine counts them, see
    /// `halyard_term::History`), and `lost` when more went than could be
    /// counted. Block rows count from the session's first row, so dropping old
    /// rows moves the base ([`PaneMonitor::row_base`]) instead of the blocks;
    /// blocks wholly above the base are forgotten.
    ///
    /// When the scrollback kept less than before in total (`rows + dropped`
    /// shrank), it was cleared (`clear` sends `CSI 3 J`; a reset does too), and
    /// when the count was lost the base is unreliable: either way closed blocks
    /// point at other text now and are forgotten. Running blocks and the prompt
    /// being typed are kept.
    pub fn on_history(&mut self, rows: u64, dropped: u64, lost: bool) {
        let total = rows.saturating_add(dropped);
        if lost || self.history_total.is_some_and(|before| total < before) {
            self.blocks.prune_before(u64::MAX);
            self.blocks_changed = true;
        } else if dropped > self.row_base {
            let before = self.blocks.history().len();
            self.blocks.prune_before(dropped);
            self.blocks_changed |= before != self.blocks.history().len();
        }
        self.history_total = Some(total);
        self.row_base = dropped;
    }

    /// Rows dropped from the top of scrollback so far: block rows minus this are
    /// screen rows (as `halyard_term::Session::screen_rows_text` counts them).
    pub fn row_base(&self) -> u64 {
        self.row_base
    }

    /// The user typed or pasted into the pane.
    pub fn on_user_input(&mut self, at: SystemTime) {
        self.apply(AttentionEvent::UserInput, at);
    }

    /// The pane gained or lost focus. Focus means the user can see the pane and
    /// type into it: its tab is shown and its window is active. Returns true when
    /// unread changed. It leaves [`PaneMonitor::take_changed`] alone: focus is
    /// applied by the window, which redraws anyway, and consuming the flag here
    /// would swallow a state change the session thread made at the same time.
    pub fn set_focused(&mut self, focused: bool, at: SystemTime) -> bool {
        if self.attention.focused() == focused {
            return false;
        }
        let before = self.attention.unread();
        self.attention.handle(AttentionEvent::Focus(focused), at);
        before != self.attention.unread()
    }

    /// The result of polling the pane's foreground process.
    pub fn on_foreground(&mut self, fg: &Foreground, at: SystemTime) {
        match fg {
            Foreground::Command { pgid, agent: Some(kind) } => {
                let same = self.detected.as_ref().is_some_and(|(p, k)| p == pgid && k == kind);
                if !same {
                    if self.detected.is_some() {
                        // A different agent replaced the previous one.
                        self.apply(AttentionEvent::AgentExited { exit_code: None }, at);
                    }
                    self.detected = Some((*pgid, kind.clone()));
                    self.apply(AttentionEvent::AgentStarted(kind.clone()), at);
                }
            }
            Foreground::Command { agent: None, .. } | Foreground::Shell => {
                if self.detected.take().is_some() {
                    self.apply(AttentionEvent::AgentExited { exit_code: None }, at);
                }
            }
        }
    }

    /// Advances time (Working becomes Idle after the quiet period).
    pub fn tick(&mut self, now: SystemTime) {
        let before = self.visible();
        self.attention.tick(now);
        if self.visible() != before {
            self.changed = true;
        }
    }

    /// True once after anything the UI shows changed.
    pub fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }

    /// True once after a command block opened or closed.
    pub fn take_blocks_changed(&mut self) -> bool {
        std::mem::take(&mut self.blocks_changed)
    }

    /// True while a detected agent holds the foreground, so the poll must go on.
    pub fn agent_detected(&self) -> bool {
        self.detected.is_some()
    }

    /// The attention machine.
    pub fn attention(&self) -> &AttentionMachine {
        &self.attention
    }

    /// The command blocks.
    pub fn blocks(&self) -> &BlockTracker<C> {
        &self.blocks
    }

    /// The working directory the shell last reported (OSC 7), untrusted.
    pub fn cwd(&self) -> Option<&ReportedCwd> {
        self.cwd.as_ref()
    }

    /// The pane as the dashboard lists it. Reads `.git/HEAD` when the reported
    /// working directory is local (see [`SessionSummary::from_machine`]).
    pub fn summary(&self, session_id: &str, local_hostname: &str) -> SessionSummary {
        SessionSummary::from_machine(session_id, &self.attention, self.cwd.as_ref(), local_hostname)
    }
}

/// Recognizes an agent from process information: the built-in list first
/// ([`detect_agent`]), then the user's extra program names (`[agents] detect`),
/// compared without case against the program name.
pub fn detect_with_extra(info: &ProcessInfo, extra: &[String]) -> Option<AgentKind> {
    if let Some(kind) = detect_agent(info) {
        return Some(kind);
    }
    let names =
        [info.argv.first().map(String::as_str), info.exe.as_deref().and_then(|p| p.to_str())];
    for prog in names.into_iter().flatten() {
        let base = prog.rsplit('/').next().unwrap_or(prog).trim_start_matches('-');
        if let Some(name) = extra.iter().find(|n| !n.is_empty() && n.eq_ignore_ascii_case(base)) {
            return Some(AgentKind::from_reported_name(name));
        }
    }
    None
}

impl Default for SignalScanner {
    fn default() -> Self {
        Self::new()
    }
}

/// The scanning half of the pipeline, owned by the session thread.
///
/// Per chunk: [`SignalScanner::scan`] finds events and returns the offsets where
/// it needs the cursor row, [`SignalScanner::at_mark`] records each row, and
/// [`SignalScanner::finish`] applies output activity and the events to the
/// monitor. A chunk without escape sequences produces no marks.
#[derive(Debug)]
pub struct SignalScanner {
    scanner: Scanner,
    events: Vec<ShellEvent>,
    rows: Vec<u64>,
}

impl SignalScanner {
    /// A scanner with the default OSC length cap.
    pub fn new() -> Self {
        Self { scanner: Scanner::new(), events: Vec::new(), rows: Vec::new() }
    }

    /// Scans a chunk; pushes one offset per event onto `marks`.
    pub fn scan(&mut self, chunk: &[u8], marks: &mut Vec<usize>) {
        self.events.clear();
        self.rows.clear();
        let events = &mut self.events;
        self.scanner.feed(chunk, |offset, event| {
            marks.push(offset);
            events.push(event);
        });
    }

    /// The cursor row at the next mark.
    pub fn at_mark(&mut self, row: u64) {
        self.rows.push(row);
    }

    /// True when the last chunk produced events.
    pub fn has_events(&self) -> bool {
        !self.events.is_empty()
    }

    /// Applies the chunk to `monitor`: output activity, then each event at its
    /// row. An event whose row was never reported uses the last known row.
    pub fn finish<C: Clock>(&mut self, monitor: &mut PaneMonitor<C>, at: SystemTime) {
        monitor.on_output(at);
        let mut last = 0;
        for (i, event) in self.events.drain(..).enumerate() {
            let row = self.rows.get(i).copied().unwrap_or(last);
            last = row;
            monitor.on_shell_event(&event, row, at);
        }
        self.rows.clear();
    }
}

#[cfg(test)]
mod tests;
