//! How agent attention looks and when it notifies: pure decisions, tested here,
//! drawn by the window.
//!
//! * [`indicator`]: the mark a tab and a pane show for an attention state.
//! * [`most_urgent`]: which pane of a tab its tab mark stands for.
//! * [`Notifier`]: whether a state change sends a desktop notification, with
//!   rate limits, and the sanitized text it carries.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use halyard_agents::AttentionState;
use halyard_config::UiColors;
use halyard_shell::sanitize::sanitize;
use halyard_term::Rgb;

/// Which theme token colors an indicator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// `ui.attention`: the agent waits for the user.
    Attention,
    /// `ui.accent`: the agent is working.
    Accent,
    /// `ui.text_muted`: idle.
    Muted,
    /// `ui.success`: finished with status 0 (or no status).
    Success,
    /// `ui.danger`: finished non-zero, or reported an error.
    Danger,
}

impl Tone {
    /// The token's color in `ui`.
    pub fn color(self, ui: &UiColors) -> Rgb {
        match self {
            Tone::Attention => ui.attention,
            Tone::Accent => ui.accent,
            Tone::Muted => ui.text_muted,
            Tone::Success => ui.success,
            Tone::Danger => ui.danger,
        }
    }
}

/// The mark for one pane or tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Indicator {
    /// Glyph drawn in the tab title: `●` for states, `✓` or `✗N` when finished.
    pub glyph: String,
    /// Short word for the pane badge and the dashboard.
    pub label: String,
    /// Color token.
    pub tone: Tone,
    /// The user has not looked at this news yet.
    pub unread: bool,
}

/// The indicator for a state, or `None` when no agent was ever seen (nothing to
/// show, nothing invented).
pub fn indicator(state: AttentionState, unread: bool) -> Option<Indicator> {
    let (glyph, label, tone) = match state {
        AttentionState::Unknown => return None,
        AttentionState::Working => ("●".to_owned(), "working".to_owned(), Tone::Accent),
        AttentionState::WaitingForInput => ("●".to_owned(), "waiting".to_owned(), Tone::Attention),
        AttentionState::Idle => ("●".to_owned(), "idle".to_owned(), Tone::Muted),
        AttentionState::Error => ("!".to_owned(), "error".to_owned(), Tone::Danger),
        AttentionState::Finished { exit_code: None | Some(0) } => {
            ("✓".to_owned(), "finished".to_owned(), Tone::Success)
        }
        AttentionState::Finished { exit_code: Some(c) } => {
            (format!("✗{c}"), format!("exited {c}"), Tone::Danger)
        }
    };
    Some(Indicator { glyph, label, tone, unread })
}

/// Sort rank for a pane: lower needs the user sooner. Matches
/// `halyard_agents::SessionSummary::rank`.
pub fn rank(state: AttentionState, unread: bool) -> u8 {
    match state {
        AttentionState::WaitingForInput => 0,
        AttentionState::Error => 1,
        _ if unread => 2,
        AttentionState::Working => 3,
        AttentionState::Idle => 4,
        AttentionState::Finished { .. } => 5,
        AttentionState::Unknown => 6,
    }
}

/// The pane a tab's mark stands for: the one that needs the user soonest. The
/// tab is unread when any of its panes is.
pub fn most_urgent(panes: &[(AttentionState, bool)]) -> Option<(AttentionState, bool)> {
    let any_unread = panes.iter().any(|(_, u)| *u);
    panes
        .iter()
        .min_by_key(|(s, u)| rank(*s, *u))
        .filter(|(s, _)| *s != AttentionState::Unknown)
        .map(|(s, _)| (*s, any_unread))
}

/// Longest notification title, in characters.
pub const MAX_TITLE: usize = 80;
/// Longest notification body, in characters.
pub const MAX_BODY: usize = 200;
/// At most one notification per pane in this period.
pub const PER_PANE_INTERVAL: Duration = Duration::from_secs(10);
/// At most [`BURST`] notifications in this period across the window.
pub const BURST_WINDOW: Duration = Duration::from_secs(30);
/// See [`BURST_WINDOW`].
pub const BURST: usize = 3;

/// A desktop notification to send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    /// Title line.
    pub title: String,
    /// Body text.
    pub body: String,
}

/// One state change, as the notification decision sees it.
#[derive(Clone, Debug)]
pub struct Change<'a> {
    /// The pane, by `HALYARD_SESSION_ID`.
    pub pane: &'a str,
    /// State before.
    pub before: AttentionState,
    /// State now.
    pub after: AttentionState,
    /// The pane's window is the active window.
    pub window_active: bool,
    /// `[agents] notify_on_waiting`.
    pub enabled: bool,
    /// Agent name, if known.
    pub agent: Option<&'a str>,
    /// The tab's title (from the program: untrusted).
    pub tab_title: &'a str,
    /// The last notification text (from the program: untrusted).
    pub message: Option<&'a str>,
}

/// Decides which state changes become desktop notifications.
#[derive(Debug, Default)]
pub struct Notifier {
    last_by_pane: HashMap<String, Instant>,
    recent: VecDeque<Instant>,
}

/// Sanitizes untrusted text for a notification: control and bidi characters
/// removed, runs of whitespace collapsed to one space, capped at `max`
/// characters (an ellipsis marks a cut).
pub fn clean(text: &str, max: usize) -> String {
    let spaced: String = text.chars().map(|c| if c.is_whitespace() { ' ' } else { c }).collect();
    let s = sanitize(&spaced, usize::MAX);
    let words: Vec<&str> = s.split_whitespace().collect();
    let joined = words.join(" ");
    if joined.chars().count() <= max {
        return joined;
    }
    let mut cut: String = joined.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

impl Notifier {
    /// A notifier with no history.
    pub fn new() -> Self {
        Self::default()
    }

    /// The notification for `change`, if one should be sent at `now`: the pane
    /// just became waiting or finished, its window is not active, notifications
    /// are on, and the rate limits allow it. Sending is recorded.
    pub fn decide(&mut self, change: &Change<'_>, now: Instant) -> Option<Note> {
        let became = |s: AttentionState| {
            change.after == s
                || matches!(
                    (s, change.after),
                    (AttentionState::Finished { .. }, AttentionState::Finished { .. })
                )
        };
        let notable = became(AttentionState::WaitingForInput)
            || became(AttentionState::Finished { exit_code: None });
        if !change.enabled
            || change.window_active
            || !notable
            || std::mem::discriminant(&change.before) == std::mem::discriminant(&change.after)
        {
            return None;
        }
        if self
            .last_by_pane
            .get(change.pane)
            .is_some_and(|t| now.duration_since(*t) < PER_PANE_INTERVAL)
        {
            return None;
        }
        while self.recent.front().is_some_and(|t| now.duration_since(*t) >= BURST_WINDOW) {
            self.recent.pop_front();
        }
        if self.recent.len() >= BURST {
            return None;
        }
        self.recent.push_back(now);
        self.last_by_pane.insert(change.pane.to_owned(), now);
        let agent = clean(change.agent.unwrap_or("Agent"), 40);
        let agent = if agent.is_empty() { "Agent".to_owned() } else { agent };
        let title = match change.after {
            AttentionState::WaitingForInput => format!("{agent} needs your input"),
            AttentionState::Finished { exit_code: Some(c) } if c != 0 => {
                format!("{agent} exited with status {c}")
            }
            _ => format!("{agent} finished"),
        };
        let body = match change.message.map(|m| clean(m, MAX_BODY)).filter(|m| !m.is_empty()) {
            Some(m) => m,
            None => clean(change.tab_title, MAX_BODY),
        };
        Some(Note { title: clean(&title, MAX_TITLE), body })
    }

    /// Forgets a closed pane.
    pub fn forget(&mut self, pane: &str) {
        self.last_by_pane.remove(pane);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: AttentionState = AttentionState::WaitingForInput;
    const WORK: AttentionState = AttentionState::Working;

    fn change(pane: &str, before: AttentionState, after: AttentionState) -> Change<'_> {
        Change {
            pane,
            before,
            after,
            window_active: false,
            enabled: true,
            agent: Some("Claude Code"),
            tab_title: "~/src/app",
            message: Some("Claude needs your permission to use Bash"),
        }
    }

    #[test]
    fn indicators_follow_the_theme_tokens() {
        assert_eq!(indicator(AttentionState::Unknown, true), None);
        let w = indicator(W, true).unwrap();
        assert_eq!((w.glyph.as_str(), w.tone, w.unread), ("●", Tone::Attention, true));
        assert_eq!(indicator(WORK, false).unwrap().tone, Tone::Accent);
        assert_eq!(indicator(AttentionState::Idle, false).unwrap().tone, Tone::Muted);
        let ok = indicator(AttentionState::Finished { exit_code: Some(0) }, false).unwrap();
        assert_eq!((ok.glyph.as_str(), ok.tone), ("✓", Tone::Success));
        let none = indicator(AttentionState::Finished { exit_code: None }, false).unwrap();
        assert_eq!(none.tone, Tone::Success);
        let bad = indicator(AttentionState::Finished { exit_code: Some(2) }, true).unwrap();
        assert_eq!(
            (bad.glyph.as_str(), bad.label.as_str(), bad.tone),
            ("✗2", "exited 2", Tone::Danger)
        );
        assert_eq!(indicator(AttentionState::Error, false).unwrap().tone, Tone::Danger);
        let ui = halyard_config::ThemeRegistry::bundled()
            .default_for(halyard_config::Appearance::Dark)
            .ui
            .clone();
        assert_eq!(Tone::Attention.color(&ui), ui.attention);
        assert_eq!(Tone::Muted.color(&ui), ui.text_muted);
    }

    #[test]
    fn tab_mark_is_the_most_urgent_pane_and_unread_if_any_is() {
        assert_eq!(most_urgent(&[]), None);
        assert_eq!(most_urgent(&[(AttentionState::Unknown, false)]), None);
        assert_eq!(
            most_urgent(&[(WORK, false), (W, false), (AttentionState::Idle, true)]),
            Some((W, true))
        );
        let fin = AttentionState::Finished { exit_code: Some(0) };
        assert_eq!(most_urgent(&[(WORK, false), (fin, true)]), Some((fin, true)));
        assert_eq!(most_urgent(&[(WORK, false), (fin, false)]), Some((WORK, false)));
    }

    #[test]
    fn one_notification_when_waiting_in_an_inactive_window() {
        let mut n = Notifier::new();
        let t = Instant::now();
        let note = n.decide(&change("a", WORK, W), t).expect("a notification");
        assert_eq!(note.title, "Claude Code needs your input");
        assert_eq!(note.body, "Claude needs your permission to use Bash");
        // Active window, disabled, or no transition: nothing.
        let mut n = Notifier::new();
        assert_eq!(n.decide(&Change { window_active: true, ..change("a", WORK, W) }, t), None);
        assert_eq!(n.decide(&Change { enabled: false, ..change("a", WORK, W) }, t), None);
        assert_eq!(n.decide(&change("a", W, W), t), None);
        assert_eq!(n.decide(&change("a", W, WORK), t), None);
        assert_eq!(n.decide(&change("a", WORK, AttentionState::Idle), t), None);
        // Finished, with and without a failure.
        let done = n
            .decide(&change("b", WORK, AttentionState::Finished { exit_code: Some(0) }), t)
            .unwrap();
        assert_eq!(done.title, "Claude Code finished");
        let failed =
            n.decide(&change("c", W, AttentionState::Finished { exit_code: Some(3) }), t).unwrap();
        assert_eq!(failed.title, "Claude Code exited with status 3");
    }

    #[test]
    fn rate_limits_per_pane_and_per_burst() {
        let mut n = Notifier::new();
        let t = Instant::now();
        assert!(n.decide(&change("a", WORK, W), t).is_some());
        // The same pane flapping within the interval: dropped.
        assert!(n.decide(&change("a", WORK, W), t + Duration::from_secs(3)).is_none());
        assert!(n.decide(&change("a", WORK, W), t + PER_PANE_INTERVAL).is_some());
        // Many panes at once: at most BURST in the burst window.
        let mut n = Notifier::new();
        let names: Vec<String> = (0..6).map(|i| format!("p{i}")).collect();
        let sent = names.iter().filter(|p| n.decide(&change(p, WORK, W), t).is_some()).count();
        assert_eq!(sent, BURST);
        let later = t + BURST_WINDOW;
        assert!(n.decide(&change("p5", WORK, W), later).is_some());
        // Forgetting a pane lets it notify again.
        n.forget("p5");
        assert!(n.decide(&change("p5", WORK, W), later + Duration::from_secs(1)).is_some());
    }

    #[test]
    fn notification_text_is_sanitized_and_capped() {
        let mut n = Notifier::new();
        let hostile = format!("rm\x1b[31m -rf\u{202e}\n\n{}", "x".repeat(500));
        let c = Change {
            agent: Some("evil\u{7}\u{202e}bot"),
            message: Some(&hostile),
            ..change("a", WORK, W)
        };
        let note = n.decide(&c, Instant::now()).unwrap();
        assert_eq!(note.title, "evilbot needs your input");
        assert!(note.body.starts_with("rm[31m -rf x"), "{}", note.body);
        assert_eq!(note.body.chars().count(), MAX_BODY);
        assert!(note.body.ends_with('…'));
        assert!(!note.body.chars().any(char::is_control));
        // No message: the tab title, also cleaned. No agent: a generic name.
        let c =
            Change { agent: None, message: None, tab_title: "a\tb\u{0}c", ..change("b", WORK, W) };
        let note = n.decide(&c, Instant::now()).unwrap();
        assert_eq!((note.title.as_str(), note.body.as_str()), ("Agent needs your input", "a bc"));
    }
}
