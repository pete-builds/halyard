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
