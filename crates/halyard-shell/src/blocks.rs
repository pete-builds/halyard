//! Command blocks built from shell integration marks.
//!
//! A [`BlockTracker`] is fed [`ShellEvent`]s together with the absolute row (scrollback
//! included) at which each one occurred. The caller gets that row from the VT engine
//! after feeding it the bytes up to the event's offset (see [`crate::scanner`]). The
//! tracker never touches the engine itself.
//!
//! # Lifecycle of a block
//!
//! ```text
//! A (prompt start) -> B (input start) -> [E command line] -> C (output start) -> D (finished)
//! ```
//!
//! A..B..C is the *prompt phase* and is not a block yet. C turns it into a
//! [`BlockState::Running`] block, D makes it [`BlockState::Finished`].
//!
//! # Missing and odd marks
//!
//! Real shells and scripts drop marks. The rules, each covered by a test:
//!
//! * **D after a prompt without C** (Enter on an empty line, Ctrl+C at the prompt):
//!   an aborted prompt, recorded nowhere. This matches the FinalTerm semantics.
//! * **D with no prompt and nothing running** (integration loaded mid-command): a
//!   Finished block with only an exit status and end row, kept only when the status
//!   is known.
//! * **C without A or B**: a Running block with no prompt row.
//! * **C while a block is already running**: the earlier block lost its D. It is
//!   closed as [`BlockState::Unknown`].
//! * **A while a block is running**: if both marks carry different `aid=` values, a
//!   nested shell started (for example `bash` typed at a zsh prompt). The outer block
//!   stays running underneath, and the outer shell's own D (matched by `aid`) closes
//!   it later, discarding whatever the inner shell left open. Without `aid` on both,
//!   the running block lost its D and is closed as Unknown at the new prompt's row.
//! * **Alternate screen**: marks that arrive while the alternate screen is active are
//!   ignored and counted, because rows there do not belong to the main screen's
//!   scrollback. The running block is flagged [`Block::used_alt_screen`].
//! * **A twice** (prompt redraw): the prompt row moves to the newest A.
//!
//! * **Nesting is bounded** at [`MAX_NESTING`] running blocks. Output is untrusted
//!   (`cat` of a file can carry any marks), so a stream of `A;aid=<new>` and `C`
//!   pairs must not grow memory or time without bound: when a new block would exceed
//!   the limit, the outermost running block is closed as Unknown.
//!
//! History is bounded: the oldest closed blocks are evicted first.

use std::collections::VecDeque;
use std::fmt;
use std::time::SystemTime;

use crate::scanner::ShellEvent;

/// Default number of closed blocks kept.
pub const DEFAULT_HISTORY: usize = 1000;

/// Most running blocks kept at once (nested shells). Real nesting is a handful deep.
pub const MAX_NESTING: usize = 32;

/// A source of wall clock time. Injected so tests can control it.
pub trait Clock {
    /// The current time.
    fn now(&self) -> SystemTime;
}

/// [`Clock`] backed by [`SystemTime::now`].
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

impl<F: Fn() -> SystemTime> Clock for F {
    fn now(&self) -> SystemTime {
        self()
    }
}

/// Identifies a block within one tracker. Ids increase in creation order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockId(pub u64);

/// Where a block is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockState {
    /// Output start (C) seen, no finish (D) yet.
    Running,
    /// Finish (D) seen.
    Finished,
    /// Closed without a finish mark: the shell's D was lost.
    Unknown,
}

/// One command and its output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// Identifier, unique within the tracker.
    pub id: BlockId,
    /// Command text, from OSC 633;E, kitty's `cmdline_url`, or [`BlockTracker::set_command`].
    pub command: Option<String>,
    /// Working directory reported by OSC 7 when the command started.
    pub cwd: Option<String>,
    /// When output started (C).
    pub started_at: Option<SystemTime>,
    /// When the block closed (D, or the mark that implied a lost D).
    pub finished_at: Option<SystemTime>,
    /// Exit status from D, when the mark carried one.
    pub exit_code: Option<i32>,
    /// Row of the prompt start (A).
    pub prompt_row: Option<u64>,
    /// Row where input started (B).
    pub input_row: Option<u64>,
    /// Row where output started (C).
    pub output_start_row: Option<u64>,
    /// Row of the mark that closed the block. Output occupies the rows from
    /// `output_start_row` through this one; the last row is empty when the output
    /// ended with a newline.
    pub output_end_row: Option<u64>,
    /// Running, Finished or Unknown.
    pub state: BlockState,
    /// The `aid=` of the shell that produced the block, if it sent one.
    pub aid: Option<String>,
    /// True when the alternate screen was entered while this block ran.
    pub used_alt_screen: bool,
}

impl Block {
    /// The first row that belongs to the block, from the earliest mark it has.
    pub fn start_row(&self) -> Option<u64> {
        self.prompt_row.or(self.input_row).or(self.output_start_row).or(self.output_end_row)
    }

    /// True when the block finished with a known non-zero exit status.
    pub fn failed(&self) -> bool {
        self.state == BlockState::Finished && matches!(self.exit_code, Some(c) if c != 0)
    }
}

/// Counters for marks the tracker could not use. Useful in diagnostics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TrackerStats {
    /// Prompts ended by D without C.
    pub aborted_prompts: u64,
    /// Marks ignored because the alternate screen was active.
    pub ignored_alt_screen_marks: u64,
    /// Blocks closed as Unknown.
    pub lost_finish_marks: u64,
    /// Closed blocks evicted from the bounded history.
    pub evicted: u64,
    /// Outermost running blocks closed because nesting reached [`MAX_NESTING`].
    pub nesting_overflows: u64,
}

#[derive(Debug, Default)]
struct Pending {
    prompt_row: Option<u64>,
    input_row: Option<u64>,
    aid: Option<String>,
    cwd: Option<String>,
    command: Option<String>,
}

/// Builds [`Block`]s from marks. One per pane.
pub struct BlockTracker<C: Clock = SystemClock> {
    clock: C,
    history: VecDeque<Block>,
    /// Running blocks, innermost last. More than one only with nested shells.
    running: Vec<Block>,
    pending: Option<Pending>,
    /// A command line that arrived with no prompt open.
    next_command: Option<String>,
    cwd: Option<String>,
    alt_screen: bool,
    next_id: u64,
    capacity: usize,
    stats: TrackerStats,
}

impl<C: Clock> fmt::Debug for BlockTracker<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BlockTracker")
            .field("history", &self.history.len())
            .field("running", &self.running.len())
            .field("pending", &self.pending.is_some())
            .field("alt_screen", &self.alt_screen)
            .field("capacity", &self.capacity)
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

impl Default for BlockTracker<SystemClock> {
    fn default() -> Self {
        Self::new(SystemClock, DEFAULT_HISTORY)
    }
}

impl<C: Clock> BlockTracker<C> {
    /// A tracker keeping at most `capacity` closed blocks (at least one).
    pub fn new(clock: C, capacity: usize) -> Self {
        Self {
            clock,
            history: VecDeque::new(),
            running: Vec::new(),
            pending: None,
            next_command: None,
            cwd: None,
            alt_screen: false,
            next_id: 1,
            capacity: capacity.max(1),
            stats: TrackerStats::default(),
        }
    }

    /// Feeds one event that occurred with the cursor on absolute row `row`.
    /// Events that do not concern blocks are ignored.
    pub fn on_event(&mut self, event: &ShellEvent, row: u64) {
        match event {
            ShellEvent::AltScreen { active } => {
                self.alt_screen = *active;
                if *active && let Some(top) = self.running.last_mut() {
                    top.used_alt_screen = true;
                }
            }
            ShellEvent::Cwd(cwd) => {
                // Blocks keep the display form; see `ReportedCwd` for local use.
                self.cwd = Some(cwd.path.clone());
                if let Some(p) = self.pending.as_mut() {
                    p.cwd = Some(cwd.path.clone());
                }
            }
            ShellEvent::CommandLine(line) => self.set_command(line.clone()),
            ShellEvent::PromptStart { .. }
            | ShellEvent::CommandInputStart
            | ShellEvent::CommandExecuted
            | ShellEvent::CommandFinished { .. }
                if self.alt_screen =>
            {
                self.stats.ignored_alt_screen_marks += 1;
            }
            ShellEvent::PromptStart { aid } => self.prompt_start(aid.clone(), row),
            ShellEvent::CommandInputStart => {
                let fresh = self.new_pending();
                self.pending.get_or_insert(fresh).input_row = Some(row);
            }
            ShellEvent::CommandExecuted => self.executed(row),
            ShellEvent::CommandFinished { exit_code, aid } => {
                self.finished(*exit_code, aid.as_deref(), row);
            }
            _ => {}
        }
    }

    /// Sets the command text of the prompt being typed at or, failing that, of the
    /// innermost running block if it has none yet. With neither, the text is kept for
    /// the next block. Callers that capture the command line themselves use this.
    pub fn set_command(&mut self, command: String) {
        if let Some(p) = self.pending.as_mut() {
            p.command = Some(command);
        } else if let Some(top) = self.running.last_mut() {
            if top.command.is_none() {
                top.command = Some(command);
            }
        } else {
            self.next_command = Some(command);
        }
    }

    fn new_pending(&self) -> Pending {
        Pending { cwd: self.cwd.clone(), ..Pending::default() }
    }

    fn prompt_start(&mut self, aid: Option<String>, row: u64) {
        if let Some(top_aid) = self.running.last().map(|b| b.aid.clone()) {
            let matching = aid
                .as_ref()
                .and_then(|a| self.running.iter().position(|b| b.aid.as_ref() == Some(a)));
            match matching {
                // The same shell prompts again: everything from its block up lost D.
                Some(i) => self.close_unknown_from(i, row),
                // A different shell prompts while ours runs: nested shell.
                None if aid.is_some() && top_aid.is_some() => {}
                // No way to tell: assume the running block lost its D.
                None => self.close_unknown_from(self.running.len() - 1, row),
            }
        }
        let mut p = match self.pending.take() {
            // A redraw keeps what was already known about this prompt.
            Some(p) if p.aid == aid => p,
            _ => self.new_pending(),
        };
        p.prompt_row = Some(row);
        p.input_row = None;
        p.aid = aid;
        p.cwd = self.cwd.clone();
        self.pending = Some(p);
    }

    fn executed(&mut self, row: u64) {
        let pending = self.pending.take();
        if pending.is_none() && !self.running.is_empty() {
            // C twice without D in between.
            self.close_unknown_from(self.running.len() - 1, row);
        }
        let p = pending.unwrap_or_else(|| self.new_pending());
        let block = Block {
            id: self.alloc_id(),
            command: p.command.or_else(|| self.next_command.take()),
            cwd: p.cwd.or_else(|| self.cwd.clone()),
            started_at: Some(self.clock.now()),
            finished_at: None,
            exit_code: None,
            prompt_row: p.prompt_row,
            input_row: p.input_row,
            output_start_row: Some(row),
            output_end_row: None,
            state: BlockState::Running,
            aid: p.aid,
            used_alt_screen: false,
        };
        self.next_command = None;
        if self.running.len() >= MAX_NESTING {
            let mut outer = self.running.remove(0);
            outer.state = BlockState::Unknown;
            outer.finished_at = Some(self.clock.now());
            outer.output_end_row = Some(row);
            self.stats.nesting_overflows += 1;
            self.push_history(outer);
        }
        self.running.push(block);
    }

    fn finished(&mut self, exit_code: Option<i32>, aid: Option<&str>, row: u64) {
        let target =
            aid.and_then(|a| self.running.iter().position(|b| b.aid.as_deref() == Some(a)));
        if let Some(i) = target {
            // The shell that ran block i is back. Anything an inner shell left open
            // went away with it.
            self.pending = None;
            if i + 1 < self.running.len() {
                self.close_unknown_from(i + 1, row);
            }
            self.finish_top(exit_code, row);
        } else if self.pending.take().is_some() {
            self.stats.aborted_prompts += 1;
        } else if !self.running.is_empty() {
            self.finish_top(exit_code, row);
        } else if exit_code.is_some() {
            let now = self.clock.now();
            let block = Block {
                id: self.alloc_id(),
                command: self.next_command.take(),
                cwd: self.cwd.clone(),
                started_at: None,
                finished_at: Some(now),
                exit_code,
                prompt_row: None,
                input_row: None,
                output_start_row: None,
                output_end_row: Some(row),
                state: BlockState::Finished,
                aid: aid.map(str::to_string),
                used_alt_screen: false,
            };
            self.push_history(block);
        }
    }

    fn finish_top(&mut self, exit_code: Option<i32>, row: u64) {
        if let Some(mut b) = self.running.pop() {
            b.state = BlockState::Finished;
            b.exit_code = exit_code;
            b.finished_at = Some(self.clock.now());
            b.output_end_row = Some(row);
            self.push_history(b);
        }
    }

    /// Closes running blocks `from..` (innermost first) as Unknown.
    fn close_unknown_from(&mut self, from: usize, row: u64) {
        while self.running.len() > from {
            if let Some(mut b) = self.running.pop() {
                b.state = BlockState::Unknown;
                b.finished_at = Some(self.clock.now());
                b.output_end_row = Some(row);
                self.stats.lost_finish_marks += 1;
                self.push_history(b);
            }
        }
    }

    fn push_history(&mut self, block: Block) {
        self.history.push_back(block);
        while self.history.len() > self.capacity {
            self.history.pop_front();
            self.stats.evicted += 1;
        }
    }

    fn alloc_id(&mut self) -> BlockId {
        let id = BlockId(self.next_id);
        self.next_id += 1;
        id
    }

    /// All known blocks, closed ones first in closing order, then running ones from
    /// outermost to innermost.
    pub fn blocks(&self) -> impl Iterator<Item = &Block> {
        self.history.iter().chain(self.running.iter())
    }

    /// Closed blocks, oldest first.
    pub fn history(&self) -> impl ExactSizeIterator<Item = &Block> {
        self.history.iter()
    }

    /// Number of running blocks (more than one only with nested shells), at most
    /// [`MAX_NESTING`].
    pub fn running_depth(&self) -> usize {
        self.running.len()
    }

    /// The innermost running block.
    pub fn running(&self) -> Option<&Block> {
        self.running.last()
    }

    /// Looks a block up by id.
    pub fn get(&self, id: BlockId) -> Option<&Block> {
        self.blocks().find(|b| b.id == id)
    }

    /// The row of the prompt currently being typed at, if any.
    pub fn pending_prompt_row(&self) -> Option<u64> {
        self.pending.as_ref().and_then(|p| p.prompt_row.or(p.input_row))
    }

    /// True while the alternate screen is active.
    pub fn alt_screen(&self) -> bool {
        self.alt_screen
    }

    /// The working directory last reported by OSC 7.
    pub fn cwd(&self) -> Option<&str> {
        self.cwd.as_deref()
    }

    /// Diagnostic counters.
    pub fn stats(&self) -> TrackerStats {
        self.stats
    }

    /// The block starting closest above `row` (strictly before it), for "jump to
    /// previous prompt".
    pub fn previous_block(&self, row: u64) -> Option<&Block> {
        self.blocks()
            .filter_map(|b| b.start_row().filter(|s| *s < row).map(|s| (s, b)))
            .max_by_key(|(s, b)| (*s, b.id))
            .map(|(_, b)| b)
    }

    /// The block starting closest below `row` (strictly after it), for "jump to next
    /// prompt".
    pub fn next_block(&self, row: u64) -> Option<&Block> {
        self.blocks()
            .filter_map(|b| b.start_row().filter(|s| *s > row).map(|s| (s, b)))
            .min_by_key(|(s, b)| (*s, std::cmp::Reverse(b.id)))
            .map(|(_, b)| b)
    }

    /// The block whose rows contain `row`: the latest block starting at or above it,
    /// provided it has not closed above it.
    pub fn block_at_row(&self, row: u64) -> Option<&Block> {
        let (_, b) = self
            .blocks()
            .filter_map(|b| b.start_row().filter(|s| *s <= row).map(|s| (s, b)))
            .max_by_key(|(s, b)| (*s, b.id))?;
        match b.output_end_row {
            Some(end) if end < row => None,
            _ => Some(b),
        }
    }

    /// The most recently closed block that finished with a non-zero status.
    pub fn last_failed(&self) -> Option<&Block> {
        self.history.iter().rev().find(|b| b.failed())
    }

    /// Forgets closed blocks that ended above `row`, for when the engine drops
    /// scrollback. Running blocks are kept.
    pub fn prune_before(&mut self, row: u64) {
        self.history.retain(|b| b.output_end_row.or(b.start_row()).is_none_or(|end| end >= row));
    }
}

#[cfg(test)]
mod tests;
