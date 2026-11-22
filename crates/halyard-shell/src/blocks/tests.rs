use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use super::*;

const T0: u64 = 1_700_000_000;

struct H {
    t: BlockTracker<Box<dyn Fn() -> SystemTime>>,
    now: Rc<Cell<SystemTime>>,
}

fn at(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(T0 + secs)
}

impl H {
    fn new() -> Self {
        Self::with_capacity(DEFAULT_HISTORY)
    }
    fn with_capacity(cap: usize) -> Self {
        let now = Rc::new(Cell::new(at(0)));
        let n = now.clone();
        let clock: Box<dyn Fn() -> SystemTime> = Box::new(move || n.get());
        Self { t: BlockTracker::new(clock, cap), now }
    }
    fn tick(&mut self, secs: u64) -> &mut Self {
        self.now.set(self.now.get() + Duration::from_secs(secs));
        self
    }
    fn ev(&mut self, e: ShellEvent, row: u64) -> &mut Self {
        self.t.on_event(&e, row);
        self
    }
    fn a(&mut self, row: u64) -> &mut Self {
        self.ev(ShellEvent::PromptStart { aid: None }, row)
    }
    fn a_aid(&mut self, aid: &str, row: u64) -> &mut Self {
        self.ev(ShellEvent::PromptStart { aid: Some(aid.into()) }, row)
    }
    fn b(&mut self, row: u64) -> &mut Self {
        self.ev(ShellEvent::CommandInputStart, row)
    }
    fn c(&mut self, row: u64) -> &mut Self {
        self.ev(ShellEvent::CommandExecuted, row)
    }
    fn d(&mut self, code: Option<i32>, row: u64) -> &mut Self {
        self.ev(ShellEvent::CommandFinished { exit_code: code, aid: None }, row)
    }
    fn d_aid(&mut self, code: Option<i32>, aid: &str, row: u64) -> &mut Self {
        self.ev(ShellEvent::CommandFinished { exit_code: code, aid: Some(aid.into()) }, row)
    }
    fn e(&mut self, cmd: &str) -> &mut Self {
        self.ev(ShellEvent::CommandLine(cmd.into()), 0)
    }
    fn cwd(&mut self, p: &str) -> &mut Self {
        self.ev(ShellEvent::Cwd(crate::scanner::ReportedCwd::local(p)), 0)
    }
    fn alt(&mut self, on: bool) -> &mut Self {
        self.ev(ShellEvent::AltScreen { active: on }, 0)
    }
    /// A full well-formed command: prompt at `row`, output from row+1, ends at end.
    fn cmd(&mut self, text: &str, row: u64, end: u64, code: i32) -> &mut Self {
        self.a(row).b(row).e(text).c(row + 1).tick(1).d(Some(code), end)
    }
    fn hist(&self) -> Vec<&Block> {
        self.t.history().collect()
    }
}

#[test]
fn full_cycle_records_everything() {
    let mut h = H::new();
    h.cwd("/home/p").a(0).b(0).tick(2).e("make test").c(1).tick(5).d(Some(0), 9);
    let b = h.hist()[0].clone();
    assert_eq!(b.id, BlockId(1));
    assert_eq!(b.command.as_deref(), Some("make test"));
    assert_eq!(b.cwd.as_deref(), Some("/home/p"));
    assert_eq!(b.started_at, Some(at(2)));
    assert_eq!(b.finished_at, Some(at(7)));
    assert_eq!(b.exit_code, Some(0));
    assert_eq!(
        (b.prompt_row, b.input_row, b.output_start_row, b.output_end_row),
        (Some(0), Some(0), Some(1), Some(9))
    );
    assert_eq!(b.state, BlockState::Finished);
    assert!(!b.failed());
    assert!(h.t.running().is_none());
    assert_eq!(h.t.stats(), TrackerStats::default());
}

#[test]
fn running_block_is_visible_before_d() {
    let mut h = H::new();
    h.a(0).b(0).c(1);
    let r = h.t.running().unwrap();
    assert_eq!(r.state, BlockState::Running);
    assert_eq!(r.output_end_row, None);
    assert_eq!(h.t.blocks().count(), 1);
    assert_eq!(h.t.block_at_row(500).map(|b| b.id), Some(r.id));
}

#[test]
fn cwd_is_taken_at_prompt_and_updates_while_typing() {
    let mut h = H::new();
    h.cwd("/a").a(0).cwd("/b").b(0).c(1).cwd("/c").d(Some(0), 2);
    assert_eq!(h.hist()[0].cwd.as_deref(), Some("/b"));
    assert_eq!(h.t.cwd(), Some("/c"));
}

#[test]
fn d_after_prompt_without_c_is_an_aborted_prompt() {
    let mut h = H::new();
    h.a(0).b(0).d(Some(130), 1).a(1).b(1).d(None, 2);
    assert!(h.hist().is_empty());
    assert_eq!(h.t.stats().aborted_prompts, 2);
}

#[test]
fn d_without_any_prompt_keeps_only_known_status() {
    let mut h = H::new();
    h.d(Some(2), 4).d(None, 5);
    let hist = h.hist();
    assert_eq!(hist.len(), 1);
    assert_eq!(hist[0].state, BlockState::Finished);
    assert_eq!(hist[0].exit_code, Some(2));
    assert_eq!(hist[0].prompt_row, None);
    assert_eq!(hist[0].started_at, None);
    assert_eq!(hist[0].start_row(), Some(4));
    assert!(hist[0].failed());
}

#[test]
fn c_without_a_or_b() {
    let mut h = H::new();
    h.e("ls").c(3).d(Some(0), 6);
    let b = h.hist()[0];
    assert_eq!(b.prompt_row, None);
    assert_eq!(b.input_row, None);
    assert_eq!(b.output_start_row, Some(3));
    assert_eq!(b.command.as_deref(), Some("ls"));
    assert_eq!(b.state, BlockState::Finished);
}

#[test]
fn b_without_a() {
    let mut h = H::new();
    h.b(2).c(3).d(Some(0), 4);
    let b = h.hist()[0];
    assert_eq!((b.prompt_row, b.input_row), (None, Some(2)));
    assert_eq!(b.start_row(), Some(2));
}

#[test]
fn c_twice_closes_the_first_as_unknown() {
    let mut h = H::new();
    h.a(0).c(1).c(5).d(Some(0), 8);
    let hist = h.hist();
    assert_eq!(hist.len(), 2);
    assert_eq!(hist[0].state, BlockState::Unknown);
    assert_eq!(hist[0].exit_code, None);
    assert_eq!(hist[0].output_end_row, Some(5));
    assert_eq!(hist[1].state, BlockState::Finished);
    assert_eq!(hist[1].prompt_row, None);
    assert_eq!(h.t.stats().lost_finish_marks, 1);
}

#[test]
fn a_while_running_without_aid_means_lost_d() {
    let mut h = H::new();
    h.a(0).c(1).a(7).b(7).c(8).d(Some(1), 9);
    let hist = h.hist();
    assert_eq!(hist.len(), 2);
    assert_eq!(hist[0].state, BlockState::Unknown);
    assert_eq!(hist[0].output_end_row, Some(7));
    assert_eq!(hist[1].prompt_row, Some(7));
    assert_eq!(hist[1].exit_code, Some(1));
}

#[test]
fn a_with_same_aid_while_running_means_lost_d() {
    let mut h = H::new();
    h.a_aid("1", 0).c(1).a_aid("1", 4);
    assert_eq!(h.hist()[0].state, BlockState::Unknown);
    assert!(h.t.running().is_none());
}

#[test]
fn nested_shell_with_aids() {
    let mut h = H::new();
    // zsh (aid 1) runs `bash`.
    h.a_aid("1", 0).b(0).e("bash").c(1);
    // bash (aid 2) prompts and runs two commands.
    h.a_aid("2", 1).b(1).e("true").c(2).d_aid(Some(0), "2", 2);
    h.a_aid("2", 2).b(2).e("false").c(3).d_aid(Some(1), "2", 3);
    // The outer block is still running underneath.
    assert_eq!(h.t.running().unwrap().command.as_deref(), Some("bash"));
    // bash prompts once more, then the user types `exit`: bash never sends D for
    // `exit`, zsh sends its own D for `bash`.
    h.a_aid("2", 3).b(3).d_aid(Some(0), "1", 4);
    let hist = h.hist();
    let cmds: Vec<_> =
        hist.iter().map(|b| (b.command.as_deref().unwrap(), b.state, b.exit_code)).collect();
    assert_eq!(
        cmds,
        vec![
            ("true", BlockState::Finished, Some(0)),
            ("false", BlockState::Finished, Some(1)),
            ("bash", BlockState::Finished, Some(0)),
        ]
    );
    assert_eq!(hist[2].prompt_row, Some(0));
    assert_eq!(hist[2].output_end_row, Some(4));
    assert!(h.t.running().is_none());
    // The inner prompt left open was discarded, not counted as aborted.
    assert_eq!(h.t.stats().aborted_prompts, 0);
    // Navigation follows rows, not closing order.
    assert_eq!(h.t.previous_block(1).unwrap().command.as_deref(), Some("bash"));
    assert_eq!(h.t.previous_block(2).unwrap().command.as_deref(), Some("true"));
    assert_eq!(h.t.next_block(0).unwrap().command.as_deref(), Some("true"));
}

#[test]
fn outer_d_closes_inner_running_blocks_as_unknown() {
    let mut h = H::new();
    h.a_aid("1", 0).c(1);
    h.a_aid("2", 1).c(2); // inner command running, inner shell killed
    h.d_aid(Some(137), "1", 5);
    let hist = h.hist();
    assert_eq!(hist.len(), 2);
    assert_eq!(hist[0].state, BlockState::Unknown);
    assert_eq!(hist[0].output_end_row, Some(5));
    assert_eq!(hist[1].exit_code, Some(137));
    assert_eq!(hist[1].state, BlockState::Finished);
}

#[test]
fn outer_prompt_returning_closes_inner_blocks() {
    let mut h = H::new();
    h.a_aid("1", 0).c(1);
    h.a_aid("2", 1).c(2);
    // The outer shell prompts again without having sent D at all.
    h.a_aid("1", 6);
    let states: Vec<_> = h.hist().iter().map(|b| b.state).collect();
    assert_eq!(states, vec![BlockState::Unknown, BlockState::Unknown]);
    assert!(h.t.running().is_none());
    assert_eq!(h.t.pending_prompt_row(), Some(6));
}

#[test]
fn d_without_aid_finishes_innermost() {
    let mut h = H::new();
    h.a_aid("1", 0).c(1).a_aid("2", 1).c(2).d(Some(0), 3);
    assert_eq!(h.hist().len(), 1);
    assert_eq!(h.hist()[0].output_start_row, Some(2));
    assert_eq!(h.t.running().unwrap().output_start_row, Some(1));
}

#[test]
fn alternate_screen_marks_are_ignored() {
    let mut h = H::new();
    h.a(0).b(0).e("vim").c(1).alt(true);
    // Something inside the full screen app emits marks (a shell in a :terminal).
    h.a(3).c(4).d(Some(5), 5);
    h.alt(false).d(Some(0), 2);
    let hist = h.hist();
    assert_eq!(hist.len(), 1);
    assert_eq!(hist[0].command.as_deref(), Some("vim"));
    assert_eq!(hist[0].exit_code, Some(0));
    assert!(hist[0].used_alt_screen);
    assert_eq!(h.t.stats().ignored_alt_screen_marks, 3);
    assert!(!h.t.alt_screen());
}

#[test]
fn prompt_redraw_moves_prompt_row() {
    let mut h = H::new();
    h.a(0).b(0).a(3).b(3).c(4).d(Some(0), 5);
    let b = h.hist()[0];
    assert_eq!((b.prompt_row, b.input_row), (Some(3), Some(3)));
}

#[test]
fn command_line_arriving_after_c_or_without_prompt() {
    let mut h = H::new();
    h.a(0).c(1).e("late").d(Some(0), 2);
    assert_eq!(h.hist()[0].command.as_deref(), Some("late"));
    // No prompt open, nothing running: kept for the next block only.
    h.e("orphan").c(3).d(Some(0), 4);
    assert_eq!(h.hist()[1].command.as_deref(), Some("orphan"));
    h.a(5).c(6).d(Some(0), 7);
    assert_eq!(h.hist()[2].command, None);
    // A running block's existing command is not overwritten.
    h.a(8).e("one").c(9).e("two").d(Some(0), 10);
    assert_eq!(h.hist()[3].command.as_deref(), Some("one"));
}

#[test]
fn navigation() {
    let mut h = H::new();
    h.cmd("a", 0, 3, 0).cmd("b", 3, 6, 2).cmd("c", 6, 10, 0).cmd("d", 10, 12, 1);
    let cmd = |b: Option<&Block>| b.and_then(|b| b.command.clone());
    assert_eq!(cmd(h.t.previous_block(7)), Some("c".into()));
    assert_eq!(cmd(h.t.previous_block(6)), Some("b".into()));
    assert_eq!(cmd(h.t.previous_block(0)), None);
    assert_eq!(cmd(h.t.next_block(0)), Some("b".into()));
    assert_eq!(cmd(h.t.next_block(4)), Some("c".into()));
    assert_eq!(cmd(h.t.next_block(10)), None);
    assert_eq!(cmd(h.t.block_at_row(4)), Some("b".into()));
    assert_eq!(cmd(h.t.block_at_row(6)), Some("c".into()));
    assert_eq!(cmd(h.t.block_at_row(12)), Some("d".into()));
    assert_eq!(cmd(h.t.block_at_row(13)), None);
    assert_eq!(cmd(h.t.last_failed()), Some("d".into()));
    h.cmd("e", 12, 14, 0);
    assert_eq!(cmd(h.t.last_failed()), Some("d".into()));
    assert_eq!(h.t.get(BlockId(2)).unwrap().command.as_deref(), Some("b"));
    assert!(h.t.get(BlockId(99)).is_none());
}

#[test]
fn unknown_and_unset_exit_do_not_count_as_failed() {
    let mut h = H::new();
    h.a(0).c(1).a(2).c(3).d(None, 4);
    assert!(h.t.last_failed().is_none());
}

#[test]
fn bounded_history_evicts_oldest() {
    let mut h = H::with_capacity(3);
    for i in 0..5u64 {
        h.cmd(&format!("c{i}"), i * 2, i * 2 + 1, 0);
    }
    let cmds: Vec<_> = h.hist().iter().map(|b| b.command.clone().unwrap()).collect();
    assert_eq!(cmds, vec!["c2", "c3", "c4"]);
    assert_eq!(h.t.stats().evicted, 2);
    assert!(h.t.get(BlockId(1)).is_none());
    // A zero capacity still keeps the latest block.
    let mut z = H::with_capacity(0);
    z.cmd("x", 0, 1, 0).cmd("y", 1, 2, 0);
    assert_eq!(z.hist().len(), 1);
}

#[test]
fn prune_before_drops_scrolled_off_blocks() {
    let mut h = H::new();
    h.cmd("a", 0, 3, 0).cmd("b", 3, 6, 0).cmd("c", 6, 10, 0).a(10).c(11);
    h.t.prune_before(5);
    let cmds: Vec<_> = h.hist().iter().map(|b| b.command.clone().unwrap()).collect();
    assert_eq!(cmds, vec!["b", "c"]);
    h.t.prune_before(1000);
    assert!(h.hist().is_empty());
    assert!(h.t.running().is_some());
}

#[test]
fn non_block_events_are_ignored() {
    let mut h = H::new();
    h.ev(ShellEvent::Bell, 0)
        .ev(ShellEvent::Title { kind: crate::scanner::TitleKind::Window, text: "x".into() }, 0);
    assert_eq!(h.t.blocks().count(), 0);
    assert!(format!("{:?}", h.t).contains("BlockTracker"));
}

#[test]
fn default_tracker_uses_system_clock() {
    let mut t = BlockTracker::default();
    let before = SystemTime::now();
    t.on_event(&ShellEvent::CommandExecuted, 0);
    assert!(t.running().unwrap().started_at.unwrap() >= before);
}

#[test]
fn hostile_nesting_is_bounded_and_linear() {
    // `cat` of untrusted output: every mark claims a new nested shell.
    let mut h = H::with_capacity(DEFAULT_HISTORY);
    let start = std::time::Instant::now();
    for i in 0..100_000u64 {
        let aid = i.to_string();
        h.a_aid(&aid, i).c(i);
        assert!(h.t.running_depth() <= MAX_NESTING);
    }
    let elapsed = start.elapsed();
    assert_eq!(h.t.running_depth(), MAX_NESTING);
    assert_eq!(h.t.stats().nesting_overflows, 100_000 - MAX_NESTING as u64);
    assert_eq!(h.t.history().len(), DEFAULT_HISTORY);
    // Same aids closing in reverse still work near the top of the stack.
    let top = h.t.running().unwrap().aid.clone().unwrap();
    h.d_aid(Some(0), &top, 200_000);
    assert_eq!(h.t.running_depth(), MAX_NESTING - 1);
    assert!(elapsed < Duration::from_secs(1), "100k hostile marks took {elapsed:?}");
}

#[test]
fn hostile_marks_through_the_scanner() {
    let mut data = Vec::new();
    for i in 0..20_000u32 {
        data.extend_from_slice(format!("\x1b]133;A;aid={i}\x07\x1b]133;C\x07").as_bytes());
    }
    let mut scanner = crate::scanner::Scanner::new();
    let mut t = BlockTracker::new(SystemClock, 100);
    let mut row = 0;
    scanner.feed(&data, |_, ev| {
        row += 1;
        t.on_event(&ev, row);
    });
    assert_eq!(t.running_depth(), MAX_NESTING);
    assert_eq!(t.history().len(), 100);
}

#[test]
fn repeated_prompt_marks_are_one_redrawn_prompt() {
    // fish 4 repaints its prompt (after `cd`, or while waiting for terminal query
    // answers): A and B arrive again with no C in between. That is the same prompt,
    // never a new block, an empty block, or an aborted prompt.
    let mut h = H::new();
    h.a(0).b(0).e("true").c(1).d(Some(0), 2);
    h.a(2).b(2).a(2).b(2).a(3).b(3).b(3).e("false").c(4).d(Some(1), 5);
    let hist = h.hist();
    assert_eq!(hist.len(), 2, "{hist:?}");
    assert_eq!(hist[1].command.as_deref(), Some("false"));
    assert_eq!((hist[1].prompt_row, hist[1].input_row), (Some(3), Some(3)));
    assert_eq!(h.t.stats(), TrackerStats::default());
    // The same through the scanner, with fish 4's own marks.
    let mut scanner = crate::scanner::Scanner::new();
    let mut t = BlockTracker::new(SystemClock, 10);
    let stream = b"\x1b]133;A;click_events=1\x1b\\> \x1b]133;B\x1b\\\
        \x1b]133;C;cmdline_url=cd%20/tmp\x07\x1b]133;D;0\x07\
        \x1b]133;A;click_events=1\x1b\\> \x1b]133;B\x1b\\\
        \x1b]133;A;click_events=1\x1b\\> \x1b]133;B\x1b\\\
        \x1b]133;C;cmdline_url=false\x07\x1b]133;D;1\x07\
        \x1b]133;A;click_events=1\x1b\\> \x1b]133;B\x1b\\";
    let mut row = 0;
    scanner.feed(stream, |_, ev| {
        t.on_event(&ev, row);
        row += 1;
    });
    let cmds: Vec<_> = t.history().map(|b| (b.command.clone(), b.exit_code)).collect();
    assert_eq!(cmds, vec![(Some("cd /tmp".into()), Some(0)), (Some("false".into()), Some(1))]);
    assert_eq!(t.stats(), TrackerStats::default());
    assert!(t.pending_prompt_row().is_some());
}

#[test]
fn aidless_outer_shell_with_aid_shell_nested() {
    // fish 4 (own marks, no aid) runs bash with Halyard's script (aid "b").
    let mut h = H::new();
    h.a(0).b(0).e("bash").c(1);
    h.a_aid("b", 1).b(1).e("true").c(2).d_aid(Some(0), "b", 2);
    h.a_aid("b", 2).b(2);
    // `exit` in bash sends no D; fish's D (no aid) closes its own `bash` block.
    h.d(Some(0), 3);
    let got: Vec<_> =
        h.hist().iter().map(|b| (b.command.clone().unwrap(), b.state, b.exit_code)).collect();
    assert_eq!(
        got,
        vec![
            ("true".to_string(), BlockState::Finished, Some(0)),
            ("bash".to_string(), BlockState::Finished, Some(0)),
        ]
    );
    assert!(h.t.running().is_none());
    assert_eq!(h.t.stats(), TrackerStats::default());
}

#[test]
fn aidless_shell_nested_in_aid_shell() {
    // zsh (aid "z") runs fish 4 (no aid).
    let mut h = H::new();
    h.a_aid("z", 0).b(0).e("fish").c(1);
    h.a(1).b(1).e("ls").c(2).d(Some(0), 3);
    h.a(3).b(3);
    h.d_aid(Some(0), "z", 4);
    let got: Vec<_> =
        h.hist().iter().map(|b| (b.command.clone().unwrap(), b.state, b.exit_code)).collect();
    assert_eq!(
        got,
        vec![
            ("ls".to_string(), BlockState::Finished, Some(0)),
            ("fish".to_string(), BlockState::Finished, Some(0)),
        ]
    );
    assert_eq!(h.t.stats(), TrackerStats::default());
    // A D without aid that follows an aid-less prompt is still an aborted prompt.
    h.a(5).b(5).d(Some(130), 6);
    assert_eq!(h.t.stats().aborted_prompts, 1);
    assert_eq!(h.hist().len(), 2);
}
