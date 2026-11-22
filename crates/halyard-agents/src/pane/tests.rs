use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use super::*;
use crate::attention::Cause;

/// A fake clock the test moves by hand; the block tracker reads it too.
#[derive(Clone, Debug)]
struct FakeClock(Rc<Cell<u64>>);

impl FakeClock {
    fn new() -> Self {
        Self(Rc::new(Cell::new(0)))
    }
    fn at(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + self.0.get())
    }
    fn advance(&self, secs: u64) {
        self.0.set(self.0.get() + secs);
    }
}

impl Clock for FakeClock {
    fn now(&self) -> SystemTime {
        self.at()
    }
}

fn monitor(clock: &FakeClock) -> PaneMonitor<FakeClock> {
    PaneMonitor::new("pane-1", AttentionConfig::default(), clock.clone(), 100)
}

/// Runs a chunk through the scanner the way the session thread does, with the
/// cursor on `row` at every mark.
fn feed(
    scan: &mut SignalScanner,
    m: &mut PaneMonitor<FakeClock>,
    chunk: &[u8],
    row: u64,
    at: SystemTime,
) {
    let mut marks = Vec::new();
    scan.scan(chunk, &mut marks);
    for _ in &marks {
        scan.at_mark(row);
    }
    scan.finish(m, at);
}

fn claude() -> Foreground {
    Foreground::Command { pgid: 4242, agent: Some(AgentKind::ClaudeCode) }
}

#[test]
fn plain_output_makes_no_marks() {
    let mut scan = SignalScanner::new();
    let mut marks = Vec::new();
    scan.scan(b"y\ny\ny\n", &mut marks);
    assert!(marks.is_empty());
    assert!(!scan.has_events());
    scan.scan(b"a\x1b]9;hi\x07b", &mut marks);
    assert_eq!(marks, vec![8]);
    assert!(scan.has_events());
}

#[test]
fn agent_working_then_waiting_then_typing_then_exit() {
    let clock = FakeClock::new();
    let mut scan = SignalScanner::new();
    let mut m = monitor(&clock);
    m.set_focused(false, clock.at());

    // No agent: output and a notification leave the state alone.
    feed(&mut scan, &mut m, b"hello\x1b]9;not an agent\x07", 0, clock.at());
    assert_eq!(m.attention().state(), AttentionState::Unknown);
    assert!(!m.take_changed() || m.attention().last_notification().is_some());

    // The poll sees `claude` in the foreground.
    m.on_foreground(&claude(), clock.at());
    assert_eq!(m.attention().state(), AttentionState::Working);
    assert_eq!(m.attention().cause(), Cause::AgentStarted);
    assert!(m.take_changed());
    assert!(m.agent_detected());

    // Its OSC 9 notification: waiting, unread, the text recorded.
    clock.advance(2);
    feed(&mut scan, &mut m, b"\x1b]9;Claude needs your permission\x1b\\", 3, clock.at());
    assert_eq!(m.attention().state(), AttentionState::WaitingForInput);
    assert_eq!(m.attention().cause(), Cause::Notification);
    assert!(m.attention().unread());
    assert_eq!(m.attention().last_notification(), Some("Claude needs your permission"));
    assert!(m.take_changed());
    assert!(!m.take_changed(), "changed is reported once");

    // Redrawing its prompt does not clear waiting; focus clears unread.
    clock.advance(1);
    feed(&mut scan, &mut m, b"\x1b[2K> ", 3, clock.at());
    assert_eq!(m.attention().state(), AttentionState::WaitingForInput);
    m.set_focused(true, clock.at());
    assert!(!m.attention().unread());

    // Typing releases the latch; recent output means Working.
    m.on_user_input(clock.at());
    assert_eq!(m.attention().state(), AttentionState::Working);

    // Quiet for the idle period: Idle.
    clock.advance(9);
    m.tick(clock.at());
    assert_eq!(m.attention().state(), AttentionState::Idle);

    // The foreground goes back to the shell, then the shell's D mark brings the status.
    m.set_focused(false, clock.at());
    m.on_foreground(&Foreground::Shell, clock.at());
    assert_eq!(m.attention().state(), AttentionState::Finished { exit_code: None });
    assert!(m.attention().unread());
    feed(&mut scan, &mut m, b"\x1b]133;D;3\x07", 9, clock.at());
    assert_eq!(m.attention().state(), AttentionState::Finished { exit_code: Some(3) });
    assert!(!m.agent_detected());
}

#[test]
fn bell_inside_a_chunk_counts_after_the_output() {
    // Output and a bell in one chunk: Output applies first, so the bell wins.
    let clock = FakeClock::new();
    let mut scan = SignalScanner::new();
    let mut m = monitor(&clock);
    m.on_foreground(&claude(), clock.at());
    feed(&mut scan, &mut m, b"thinking...\x07", 0, clock.at());
    assert_eq!(m.attention().state(), AttentionState::WaitingForInput);
    assert_eq!(m.attention().cause(), Cause::Bell);
}

#[test]
fn protocol_report_for_another_session_is_ignored() {
    let clock = FakeClock::new();
    let mut scan = SignalScanner::new();
    let mut m = monitor(&clock);
    feed(&mut scan, &mut m, b"\x1b]7717;state=waiting;session=other\x1b\\", 0, clock.at());
    assert_eq!(m.attention().state(), AttentionState::Unknown);
    feed(
        &mut scan,
        &mut m,
        b"\x1b]7717;state=waiting;agent=mybot;session=pane-1\x1b\\",
        0,
        clock.at(),
    );
    assert_eq!(m.attention().state(), AttentionState::WaitingForInput);
    assert_eq!(m.attention().agent(), Some(&AgentKind::Other("mybot".into())));
}

#[test]
fn blocks_get_the_rows_of_their_marks() {
    let clock = FakeClock::new();
    let mut scan = SignalScanner::new();
    let mut m = monitor(&clock);
    let mut marks = Vec::new();
    // One chunk holding a whole command: A at row 4, B at 4, C at 5, D at 7.
    let chunk = b"\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n\x1b]133;C\x07a\r\nb\r\n\x1b]133;D;0\x07";
    scan.scan(chunk, &mut marks);
    assert_eq!(marks.len(), 4);
    for row in [4, 4, 5, 7] {
        scan.at_mark(row);
    }
    scan.finish(&mut m, clock.at());
    assert!(m.take_blocks_changed());
    let b = m.blocks().history().last().expect("a closed block");
    assert_eq!(b.prompt_row, Some(4));
    assert_eq!(b.output_start_row, Some(5));
    assert_eq!(b.output_end_row, Some(7));
    assert_eq!(b.exit_code, Some(0));
    // No agent ran, so the D mark does not invent a state.
    assert_eq!(m.attention().state(), AttentionState::Unknown);
}

#[test]
fn cwd_is_kept_for_the_dashboard() {
    let clock = FakeClock::new();
    let mut scan = SignalScanner::new();
    let mut m = monitor(&clock);
    feed(&mut scan, &mut m, b"\x1b]7;file://elsewhere/srv/app\x07", 0, clock.at());
    let s = m.summary("pane-1", "thishost");
    assert_eq!(s.cwd.as_deref(), Some("/srv/app"));
    assert_eq!(s.local_cwd, None, "a cwd from another host is display only");
}

#[test]
fn a_different_agent_replacing_the_first_restarts_the_run() {
    let clock = FakeClock::new();
    let mut m = monitor(&clock);
    m.on_foreground(&claude(), clock.at());
    clock.advance(1);
    let codex = Foreground::Command { pgid: 99, agent: Some(AgentKind::Codex) };
    m.on_foreground(&codex, clock.at());
    assert_eq!(m.attention().state(), AttentionState::Working);
    assert_eq!(m.attention().agent(), Some(&AgentKind::Codex));
    // A non-agent command after it ends the run.
    m.on_foreground(&Foreground::Command { pgid: 100, agent: None }, clock.at());
    assert_eq!(m.attention().state(), AttentionState::Finished { exit_code: None });
}

#[test]
fn extra_detect_names_are_agents() {
    let info = ProcessInfo {
        pid: 1,
        exe: Some("/usr/local/bin/mybot".into()),
        argv: vec!["mybot".into(), "--fast".into()],
    };
    assert_eq!(detect_with_extra(&info, &[]), None);
    assert_eq!(detect_with_extra(&info, &["MyBot".into()]), Some(AgentKind::Other("MyBot".into())));
    let claude = ProcessInfo { pid: 2, exe: None, argv: vec!["claude".into()] };
    assert_eq!(detect_with_extra(&claude, &["mybot".into()]), Some(AgentKind::ClaudeCode));
    // An empty name never matches.
    let empty = ProcessInfo { pid: 3, exe: None, argv: vec![String::new()] };
    assert_eq!(detect_with_extra(&empty, &[String::new()]), None);
}

#[test]
fn reports_without_this_panes_session_are_plain_output() {
    // What `cat hostile.txt` can print: a report with no session, or a wrong one.
    let clock = FakeClock::new();
    let mut scan = SignalScanner::new();
    let mut m = monitor(&clock);
    for chunk in [
        &b"\x1b]7717;state=waiting;agent=Claude Code;msg=<a href=\"https://evil.example\">x</a>\x1b\\"[..],
        b"\x1b]777;halyard;state=done;agent=claude;msg=hi\x07",
        b"\x1b]7717;state=error;session=pane-2\x07",
        b"\x1b]7717;msg=only a message\x07",
    ] {
        feed(&mut scan, &mut m, chunk, 0, clock.at());
        assert_eq!(m.attention().state(), AttentionState::Unknown);
        assert_eq!(m.attention().agent(), None);
        assert_eq!(m.attention().last_notification(), None);
        assert!(!m.take_changed());
    }
    // The same through the OSC 777 carrier with the right session counts.
    feed(
        &mut scan,
        &mut m,
        b"\x1b]777;halyard;state=waiting;agent=claude;session=pane-1\x07",
        0,
        clock.at(),
    );
    assert_eq!(m.attention().state(), AttentionState::WaitingForInput);
}

#[test]
fn notifications_and_bells_count_only_with_an_agent_in_the_foreground() {
    let clock = FakeClock::new();
    let mut scan = SignalScanner::new();
    let mut m = monitor(&clock);
    for chunk in
        [&b"\x1b]9;Re-authenticate now\x07"[..], b"\x1b]777;notify;Title;Body\x07", b"\x07"]
    {
        feed(&mut scan, &mut m, chunk, 0, clock.at());
        assert_eq!(m.attention().state(), AttentionState::Unknown);
        assert_eq!(m.attention().last_notification(), None, "no text is kept without an agent");
    }
    m.on_foreground(&claude(), clock.at());
    feed(&mut scan, &mut m, b"\x1b]777;notify;Claude;Approve?\x07", 0, clock.at());
    assert_eq!(m.attention().state(), AttentionState::WaitingForInput);
    assert_eq!(m.attention().last_notification(), Some("Claude: Approve?"));
    // After the agent leaves, a notification is plain output again.
    m.on_foreground(&Foreground::Shell, clock.at());
    feed(&mut scan, &mut m, b"\x1b]9;later\x07", 0, clock.at());
    assert_eq!(m.attention().last_notification(), Some("Claude: Approve?"));
}

#[test]
fn focus_changes_never_swallow_another_change() {
    // Two panes change in the same tick; the window then syncs focus on every
    // pane before either pane's own event path runs. Both changes must survive.
    let clock = FakeClock::new();
    let mut scan_a = SignalScanner::new();
    let mut scan_b = SignalScanner::new();
    let mut a = monitor(&clock);
    let mut b = monitor(&clock);
    for m in [&mut a, &mut b] {
        m.on_foreground(&claude(), clock.at());
        assert!(m.take_changed());
    }
    feed(&mut scan_a, &mut a, b"\x1b]9;A needs you\x07", 0, clock.at());
    feed(&mut scan_b, &mut b, b"\x1b]9;B needs you\x07", 0, clock.at());
    // The window's focus sync touches both panes (A gains focus, B stays unfocused).
    assert!(a.set_focused(true, clock.at()), "A's unread cleared");
    assert!(!b.set_focused(false, clock.at()), "B already unfocused");
    assert!(a.take_changed(), "A's waiting change is still pending");
    assert!(b.take_changed(), "B's waiting change is still pending");
    assert!(b.attention().unread());
}

#[test]
fn cleared_scrollback_forgets_closed_blocks() {
    let clock = FakeClock::new();
    let mut scan = SignalScanner::new();
    let mut m = monitor(&clock);
    m.on_history(40, 0, false);
    feed(&mut scan, &mut m, b"\x1b]133;A\x07\x1b]133;B\x07\x1b]133;C\x07", 45, clock.at());
    feed(&mut scan, &mut m, b"\x1b]133;D;0\x07\x1b]133;A\x07", 47, clock.at());
    m.on_history(42, 0, false);
    assert_eq!(m.blocks().history().len(), 1, "growing scrollback keeps blocks");
    let _ = m.take_blocks_changed();
    // `clear`: scrollback dropped to 0. The old block's rows mean nothing now.
    m.on_history(0, 0, false);
    assert_eq!(m.blocks().history().len(), 0);
    assert!(m.take_blocks_changed());
    assert!(m.blocks().pending_prompt_row().is_some(), "the prompt being typed stays");
}

#[test]
fn a_mark_above_the_newest_block_forgets_the_erased_blocks() {
    // `clear` at rows 4..5 homes the cursor: its D lands on row 0.
    let clock = FakeClock::new();
    let mut scan = SignalScanner::new();
    let mut m = monitor(&clock);
    feed(&mut scan, &mut m, b"\x1b]133;A\x07\x1b]133;B\x07\x1b]133;C\x07", 2, clock.at());
    feed(&mut scan, &mut m, b"\x1b]133;D;0\x07\x1b]133;A\x07\x1b]133;B\x07", 4, clock.at());
    feed(&mut scan, &mut m, b"\x1b]133;C\x07", 5, clock.at());
    assert_eq!(m.blocks().history().len(), 1);
    feed(&mut scan, &mut m, b"\x1b]133;D;0\x07\x1b]133;A\x07", 0, clock.at());
    assert_eq!(m.blocks().history().len(), 0, "both blocks sat on erased rows");
    assert_eq!(m.blocks().pending_prompt_row(), Some(0));
    // A prompt drawn again one row up (a two-line prompt redraw) keeps blocks.
    feed(&mut scan, &mut m, b"\x1b]133;B\x07\x1b]133;C\x07", 1, clock.at());
    feed(&mut scan, &mut m, b"\x1b]133;D;0\x07\x1b]133;A\x07", 9, clock.at());
    feed(&mut scan, &mut m, b"\x1b]133;A\x07", 8, clock.at());
    assert_eq!(m.blocks().history().len(), 1);
}

#[test]
fn dropped_scrollback_moves_the_base_and_forgets_dropped_blocks() {
    let clock = FakeClock::new();
    let mut scan = SignalScanner::new();
    let mut m = monitor(&clock);
    m.on_history(100, 0, false);
    // Two blocks: rows 10..=20 and 50..=60, counted from the session's first row.
    for (a, c, d) in [(10, 11, 20), (50, 51, 60)] {
        feed(&mut scan, &mut m, b"\x1b]133;A\x07\x1b]133;B\x07", a, clock.at());
        feed(&mut scan, &mut m, b"\x1b]133;C\x07", c, clock.at());
        feed(&mut scan, &mut m, b"\x1b]133;D;0\x07", d, clock.at());
    }
    assert_eq!(m.blocks().history().len(), 2);
    // A full scrollback drops 30 rows (a page) while 30 new ones arrive: the
    // row count stays the same, so only the drop count shows it.
    m.on_history(100, 30, false);
    assert_eq!(m.row_base(), 30);
    let starts: Vec<_> = m.blocks().history().map(|b| b.start_row()).collect();
    assert_eq!(starts, vec![Some(50)], "the block above the base is gone, the other keeps its row");
    // A page dropped with the row count shrinking is still not a clear.
    m.on_history(80, 50, false);
    assert_eq!(m.blocks().history().len(), 1);
    // A lost count forgets everything closed.
    m.on_history(80, 50, true);
    assert_eq!(m.blocks().history().len(), 0);
}

#[test]
fn api_report_applies_without_a_session_echo_and_marks_changed() {
    let clock = FakeClock::new();
    let mut m = monitor(&clock);
    let report = halyard_shell::AgentReport {
        state: Some(halyard_shell::ReportedState::Waiting),
        agent: Some("claude-code".into()),
        msg: Some("Needs approval".into()),
        // Whatever the caller put here, the verified pane is the one that counts.
        session: Some("another-pane".into()),
        exit_code: None,
    };
    m.on_api_report(report, clock.at());
    assert_eq!(m.attention().state(), AttentionState::WaitingForInput);
    assert_eq!(m.attention().cause(), Cause::ProtocolReport);
    assert_eq!(m.attention().last_notification(), Some("Needs approval"));
    assert!(m.take_changed());
    m.on_api_notification("Tests passed: 412 ok", clock.at());
    assert_eq!(m.attention().last_notification(), Some("Tests passed: 412 ok"));
    assert!(m.take_changed());
}
