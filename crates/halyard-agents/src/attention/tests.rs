use std::time::{Duration, SystemTime};

use halyard_shell::scanner::{NotificationSource, ShellEvent, TitleKind};

use super::*;
use AttentionState::*;

fn t(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + secs)
}

#[derive(Debug, Clone)]
enum Step {
    E(AttentionEvent),
    Tick,
}

use Step::{E, Tick};

fn start() -> Step {
    E(AttentionEvent::AgentStarted(AgentKind::ClaudeCode))
}
fn out() -> Step {
    E(AttentionEvent::Output)
}
fn bell() -> Step {
    E(AttentionEvent::Bell)
}
fn notify(s: &str) -> Step {
    E(AttentionEvent::Notification(s.into()))
}
fn input() -> Step {
    E(AttentionEvent::UserInput)
}
fn exited(code: Option<i32>) -> Step {
    E(AttentionEvent::AgentExited { exit_code: code })
}
fn d(code: Option<i32>) -> Step {
    E(AttentionEvent::CommandFinished { exit_code: code })
}
fn focus(f: bool) -> Step {
    E(AttentionEvent::Focus(f))
}
fn report(state: Option<ReportedState>) -> Step {
    E(AttentionEvent::Report(AgentReport { state, ..Default::default() }))
}
fn rep(state: ReportedState) -> Step {
    report(Some(state))
}

/// (name, steps as (second, step, expected state, expected cause)).
type Case = (&'static str, Vec<(u64, Step, AttentionState, Cause)>);

fn cases() -> Vec<Case> {
    use Cause as C;
    use ReportedState as R;
    vec![
        (
            "no agent: nothing changes the state",
            vec![
                (0, out(), Unknown, C::Initial),
                (1, bell(), Unknown, C::Initial),
                (2, notify("hi"), Unknown, C::Initial),
                (3, d(Some(1)), Unknown, C::Initial),
                (30, Tick, Unknown, C::Initial),
                (31, input(), Unknown, C::Initial),
                (31, focus(true), Unknown, C::Initial),
                (32, exited(Some(0)), Unknown, C::Initial),
            ],
        ),
        (
            "agent start, output, quiet, output again",
            vec![
                (0, start(), Working, C::AgentStarted),
                (1, out(), Working, C::AgentStarted),
                (8, Tick, Working, C::AgentStarted),
                (9, Tick, Idle, C::Quiet),
                (10, out(), Working, C::OutputActivity),
                (17, Tick, Working, C::OutputActivity),
                (18, Tick, Idle, C::Quiet),
            ],
        ),
        (
            "agent start with no output goes idle after the window",
            vec![
                (0, start(), Working, C::AgentStarted),
                (7, Tick, Working, C::AgentStarted),
                (8, Tick, Idle, C::Quiet),
            ],
        ),
        (
            "bell latches waiting until user input; output does not clear it",
            vec![
                (0, start(), Working, C::AgentStarted),
                (1, bell(), WaitingForInput, C::Bell),
                (2, out(), WaitingForInput, C::Bell),
                (3, focus(true), WaitingForInput, C::Bell),
                (4, focus(false), WaitingForInput, C::Bell),
                (60, Tick, WaitingForInput, C::Bell),
                (61, input(), Idle, C::UserInput),
                (62, out(), Working, C::OutputActivity),
            ],
        ),
        (
            "notification then input with recent output resumes working",
            vec![
                (0, start(), Working, C::AgentStarted),
                (1, notify("Claude needs your permission"), WaitingForInput, C::Notification),
                (5, out(), WaitingForInput, C::Notification),
                (6, input(), Working, C::UserInput),
            ],
        ),
        (
            "user input without a latch changes nothing",
            vec![
                (0, start(), Working, C::AgentStarted),
                (9, Tick, Idle, C::Quiet),
                (10, input(), Idle, C::Quiet),
            ],
        ),
        (
            "agent exit means finished with the status if known",
            vec![
                (0, start(), Working, C::AgentStarted),
                (1, exited(Some(2)), Finished { exit_code: Some(2) }, C::AgentExited),
                (2, out(), Finished { exit_code: Some(2) }, C::AgentExited),
                (20, Tick, Finished { exit_code: Some(2) }, C::AgentExited),
                (21, bell(), Finished { exit_code: Some(2) }, C::AgentExited),
            ],
        ),
        (
            "agent exit without status, then the shell's D supplies it",
            vec![
                (0, start(), Working, C::AgentStarted),
                (1, exited(None), Finished { exit_code: None }, C::AgentExited),
                (1, d(Some(130)), Finished { exit_code: Some(130) }, C::AgentExited),
                (2, d(Some(0)), Finished { exit_code: Some(130) }, C::AgentExited),
            ],
        ),
        (
            "D arriving before the foreground poll notices the exit",
            vec![
                (0, start(), Working, C::AgentStarted),
                (3, d(Some(1)), Finished { exit_code: Some(1) }, C::CommandFinished),
                (3, exited(None), Finished { exit_code: Some(1) }, C::CommandFinished),
            ],
        ),
        (
            "D clears a waiting latch",
            vec![
                (0, start(), Working, C::AgentStarted),
                (1, bell(), WaitingForInput, C::Bell),
                (2, d(Some(0)), Finished { exit_code: Some(0) }, C::CommandFinished),
            ],
        ),
        (
            "protocol state always wins over heuristics",
            vec![
                (0, start(), Working, C::AgentStarted),
                (1, rep(R::Working), Working, C::AgentStarted),
                (60, Tick, Working, C::AgentStarted),
                (61, rep(R::Idle), Idle, C::ProtocolReport),
                (62, out(), Idle, C::ProtocolReport),
                (63, bell(), Idle, C::ProtocolReport),
                (64, notify("x"), Idle, C::ProtocolReport),
                (65, rep(R::Waiting), WaitingForInput, C::ProtocolReport),
                (66, rep(R::Error), Error, C::ProtocolReport),
                (67, rep(R::Done), Finished { exit_code: None }, C::ProtocolReport),
            ],
        ),
        (
            "protocol report without a detected agent activates it",
            vec![
                (0, rep(R::Working), Working, C::ProtocolReport),
                (100, Tick, Working, C::ProtocolReport),
                (101, rep(R::Waiting), WaitingForInput, C::ProtocolReport),
                (102, input(), Idle, C::UserInput),
                (103, out(), Working, C::OutputActivity),
                (120, Tick, Idle, C::Quiet),
                (121, d(Some(0)), Finished { exit_code: Some(0) }, C::CommandFinished),
            ],
        ),
        (
            "user input releases a protocol done; the next turn shows as working",
            vec![
                (0, start(), Working, C::AgentStarted),
                (5, rep(R::Done), Finished { exit_code: None }, C::ProtocolReport),
                (6, out(), Finished { exit_code: None }, C::ProtocolReport),
                (7, input(), Working, C::UserInput),
            ],
        ),
        (
            "report without state changes nothing",
            vec![
                (0, start(), Working, C::AgentStarted),
                (1, report(None), Working, C::AgentStarted),
            ],
        ),
        (
            "agent started again after finishing restarts the cycle",
            vec![
                (0, start(), Working, C::AgentStarted),
                (1, exited(Some(0)), Finished { exit_code: Some(0) }, C::AgentExited),
                (5, start(), Working, C::AgentStarted),
                (6, bell(), WaitingForInput, C::Bell),
            ],
        ),
        (
            "a detection after protocol reports keeps the reported state",
            vec![
                (0, rep(R::Waiting), WaitingForInput, C::ProtocolReport),
                (1, start(), WaitingForInput, C::ProtocolReport),
            ],
        ),
        (
            "done with exit code through the protocol",
            vec![(
                0,
                E(AttentionEvent::Report(AgentReport {
                    state: Some(R::Done),
                    exit_code: Some(4),
                    ..Default::default()
                })),
                Finished { exit_code: Some(4) },
                C::ProtocolReport,
            )],
        ),
    ]
}

fn run(steps: &[(u64, Step, AttentionState, Cause)], name: &str) -> AttentionMachine {
    let mut m = AttentionMachine::new(AttentionConfig::default(), Some("s1".into()));
    for (i, (sec, step, want, cause)) in steps.iter().enumerate() {
        match step {
            E(e) => m.handle(e.clone(), t(*sec)),
            Tick => m.tick(t(*sec)),
        }
        assert_eq!((m.state(), m.cause()), (*want, *cause), "case {name:?}, step {i} ({step:?})");
    }
    m
}

#[test]
fn state_machine_table() {
    for (name, steps) in cases() {
        run(&steps, name);
    }
}

#[test]
fn since_records_state_changes_only() {
    let m = run(
        &[
            (0, start(), Working, Cause::AgentStarted),
            (3, out(), Working, Cause::AgentStarted),
            (4, bell(), WaitingForInput, Cause::Bell),
            (6, bell(), WaitingForInput, Cause::Bell),
        ],
        "since",
    );
    assert_eq!(m.since(), Some(t(4)));
    assert_eq!(m.last_output(), Some(t(3)));
}

#[test]
fn configurable_idle_period() {
    let mut m = AttentionMachine::new(AttentionConfig { idle_after: Duration::from_secs(2) }, None);
    m.handle(AttentionEvent::AgentStarted(AgentKind::Codex), t(0));
    m.tick(t(1));
    assert_eq!(m.state(), Working);
    m.tick(t(2));
    assert_eq!(m.state(), Idle);
    assert_eq!(AttentionConfig::default().idle_after, Duration::from_secs(8));
}

#[test]
fn clock_going_backwards_is_harmless() {
    let mut m = AttentionMachine::new(AttentionConfig::default(), None);
    m.handle(AttentionEvent::AgentStarted(AgentKind::Codex), t(100));
    m.handle(AttentionEvent::Output, t(100));
    m.tick(t(50));
    assert_eq!(m.state(), Working);
    m.handle(AttentionEvent::Bell, t(101));
    m.handle(AttentionEvent::UserInput, t(90));
    assert_eq!(m.state(), Working);
}

#[test]
fn unread_rules() {
    // Unfocused: waiting and finished set unread; focus clears it.
    let mut m = AttentionMachine::new(AttentionConfig::default(), None);
    m.handle(AttentionEvent::AgentStarted(AgentKind::ClaudeCode), t(0));
    assert!(!m.unread(), "working does not set unread");
    m.handle(AttentionEvent::Bell, t(1));
    assert!(m.unread());
    m.handle(AttentionEvent::Focus(true), t(2));
    assert!(!m.unread());
    assert!(m.focused());
    m.handle(AttentionEvent::Focus(false), t(3));
    assert!(!m.unread(), "losing focus does not set unread");
    m.handle(AttentionEvent::UserInput, t(4));
    m.handle(AttentionEvent::AgentExited { exit_code: Some(0) }, t(5));
    assert!(m.unread());

    // Focused: nothing becomes unread.
    let mut f = AttentionMachine::new(AttentionConfig::default(), None);
    f.handle(AttentionEvent::Focus(true), t(0));
    f.handle(AttentionEvent::AgentStarted(AgentKind::ClaudeCode), t(0));
    f.handle(AttentionEvent::Notification("x".into()), t(1));
    assert_eq!(f.state(), WaitingForInput);
    assert!(!f.unread());
    f.handle(AttentionEvent::AgentExited { exit_code: None }, t(2));
    assert!(!f.unread());

    // Idle and error never set unread; protocol waiting and done do.
    let mut p = AttentionMachine::new(AttentionConfig::default(), None);
    p.handle(AttentionEvent::AgentStarted(AgentKind::ClaudeCode), t(0));
    p.tick(t(100));
    assert_eq!(p.state(), Idle);
    assert!(!p.unread());
    for (s, unread) in [(ReportedState::Error, false), (ReportedState::Waiting, true)] {
        let mut q = p.clone();
        q.handle(
            AttentionEvent::Report(AgentReport { state: Some(s), ..Default::default() }),
            t(101),
        );
        assert_eq!(q.unread(), unread, "{s:?}");
    }
    p.handle(
        AttentionEvent::Report(AgentReport {
            state: Some(ReportedState::Done),
            ..Default::default()
        }),
        t(101),
    );
    assert!(p.unread());
}

#[test]
fn reports_for_another_session_are_ignored() {
    let mut m = AttentionMachine::new(AttentionConfig::default(), Some("mine".into()));
    let other = AgentReport {
        state: Some(ReportedState::Waiting),
        session: Some("theirs".into()),
        msg: Some("not for you".into()),
        agent: Some("codex".into()),
        ..Default::default()
    };
    m.handle(AttentionEvent::Report(other.clone()), t(0));
    assert_eq!(m.state(), Unknown);
    assert_eq!(m.last_notification(), None);
    assert_eq!(m.agent(), None);
    let mine = AgentReport { session: Some("mine".into()), ..other.clone() };
    m.handle(AttentionEvent::Report(mine), t(1));
    assert_eq!(m.state(), WaitingForInput);
    assert_eq!(m.agent(), Some(&AgentKind::Codex));
    assert_eq!(m.last_notification(), Some("not for you"));
    assert_eq!(m.session_id(), Some("mine"));
    // A report without a session id is accepted; a machine without one accepts all.
    let mut any = AttentionMachine::new(AttentionConfig::default(), None);
    any.handle(AttentionEvent::Report(other), t(0));
    assert_eq!(any.state(), WaitingForInput);
}

#[test]
fn agent_kind_is_kept_after_exit_and_reported_names_map() {
    let mut m = AttentionMachine::new(AttentionConfig::default(), None);
    m.handle(AttentionEvent::AgentStarted(AgentKind::Aider), t(0));
    assert!(m.agent_active());
    m.handle(AttentionEvent::AgentExited { exit_code: None }, t(1));
    assert!(!m.agent_active());
    assert_eq!(m.agent(), Some(&AgentKind::Aider));
    m.handle(
        AttentionEvent::Report(AgentReport { agent: Some("my-tool".into()), ..Default::default() }),
        t(2),
    );
    assert_eq!(m.agent(), Some(&AgentKind::Other("my-tool".into())));
    assert!(!m.agent_active(), "a report without state does not activate");
}

#[test]
fn notification_text_is_sanitized_and_capped() {
    let mut m = AttentionMachine::new(AttentionConfig::default(), None);
    m.handle(AttentionEvent::Notification("a\x1b[31mb\u{202e}".into()), t(0));
    assert_eq!(m.last_notification(), Some("a[31mb"));
    m.handle(AttentionEvent::Notification("x".repeat(1000)), t(1));
    assert_eq!(m.last_notification().unwrap().chars().count(), MAX_NOTIFICATION_LEN);
    // Empty text keeps the previous one.
    m.handle(AttentionEvent::Notification("\x07".into()), t(2));
    assert_eq!(m.last_notification().unwrap().len(), MAX_NOTIFICATION_LEN);
}

#[test]
fn from_shell_events() {
    assert_eq!(AttentionEvent::from_shell_event(&ShellEvent::Bell), Some(AttentionEvent::Bell));
    let n = |title: Option<&str>, body: &str| ShellEvent::Notification {
        source: NotificationSource::Osc777,
        title: title.map(str::to_string),
        body: body.into(),
    };
    assert_eq!(
        AttentionEvent::from_shell_event(&n(Some("Claude"), "needs you")),
        Some(AttentionEvent::Notification("Claude: needs you".into()))
    );
    assert_eq!(
        AttentionEvent::from_shell_event(&n(Some("Only title"), "")),
        Some(AttentionEvent::Notification("Only title".into()))
    );
    assert_eq!(
        AttentionEvent::from_shell_event(&n(None, "body")),
        Some(AttentionEvent::Notification("body".into()))
    );
    assert_eq!(
        AttentionEvent::from_shell_event(&ShellEvent::CommandFinished {
            exit_code: Some(3),
            aid: None
        }),
        Some(AttentionEvent::CommandFinished { exit_code: Some(3) })
    );
    let r = AgentReport { state: Some(ReportedState::Idle), ..Default::default() };
    assert_eq!(
        AttentionEvent::from_shell_event(&ShellEvent::Agent(r.clone())),
        Some(AttentionEvent::Report(r))
    );
    for ev in [
        ShellEvent::PromptStart { aid: None },
        ShellEvent::CommandExecuted,
        ShellEvent::Title { kind: TitleKind::Both, text: "x".into() },
        ShellEvent::AltScreen { active: true },
    ] {
        assert_eq!(AttentionEvent::from_shell_event(&ev), None, "{ev:?}");
    }
}

#[test]
fn protocol_state_cannot_hide_a_real_notification() {
    for (name, ev) in [
        ("notification", AttentionEvent::Notification("Claude needs your permission".into())),
        ("bell", AttentionEvent::Bell),
    ] {
        // Something in the pane printed a fake `state=working` report.
        let mut m = AttentionMachine::new(AttentionConfig::default(), None);
        m.handle(AttentionEvent::AgentStarted(AgentKind::ClaudeCode), t(0));
        m.handle(
            AttentionEvent::Report(AgentReport {
                state: Some(ReportedState::Working),
                ..Default::default()
            }),
            t(1),
        );
        assert!(!m.unread());
        m.handle(ev.clone(), t(2));
        assert_eq!(m.state(), Working, "{name}: the protocol state stands");
        assert!(m.unread(), "{name}: but the pane is unread");
        // Focused panes are not marked.
        let mut f = AttentionMachine::new(AttentionConfig::default(), None);
        f.handle(AttentionEvent::Focus(true), t(0));
        f.handle(AttentionEvent::AgentStarted(AgentKind::ClaudeCode), t(0));
        f.handle(
            AttentionEvent::Report(AgentReport {
                state: Some(ReportedState::Idle),
                ..Default::default()
            }),
            t(1),
        );
        f.handle(ev, t(2));
        assert!(!f.unread(), "{name}");
    }
    // Without an active agent nothing changes.
    let mut n = AttentionMachine::new(AttentionConfig::default(), None);
    n.handle(AttentionEvent::Bell, t(0));
    assert!(!n.unread());
}
