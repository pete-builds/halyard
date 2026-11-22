//! The agent dashboard's rows: every pane of every window, in the order
//! `halyard_agents::Dashboard` sorts them (who needs the user first).

use std::time::{Duration, SystemTime};

use halyard_agents::{AttentionState, Dashboard, SessionSummary};

use crate::attention_ui::{self, Tone, clean};

/// Where a row's pane lives: its window and its pane id there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    /// The window, as `gpui::WindowId` turned into a number.
    pub window: u64,
    /// The pane in that window's workspace.
    pub pane: halyard_workspace::PaneId,
}

/// A pane as the dashboard lists it. Every string is display text from
/// untrusted output, already cleaned and capped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// Where to jump.
    pub target: Target,
    /// Agent name, empty without one (the state then reads "no agent").
    pub agent: String,
    /// The pane's tab title.
    pub title: String,
    /// Working directory from OSC 7, for display.
    pub cwd: Option<String>,
    /// Git branch of the working directory, when it is local.
    pub branch: Option<String>,
    /// State word with the time in it, for example "waiting 2m".
    pub state: String,
    /// The state's color token, `None` without an agent.
    pub tone: Option<Tone>,
    /// The last notification text.
    pub note: Option<String>,
    /// News the user has not seen.
    pub unread: bool,
}

impl Row {
    /// The text the palette filter matches against.
    pub fn filter_text(&self) -> String {
        let mut s = format!("{} {} {}", self.agent, self.title, self.state);
        for extra in [&self.cwd, &self.branch, &self.note].into_iter().flatten() {
            s.push(' ');
            s.push_str(extra);
        }
        s
    }
}

/// "12s", "3m", "5h", "2d": how long ago `since` was.
pub fn ago(now: SystemTime, since: SystemTime) -> String {
    let d = now.duration_since(since).unwrap_or(Duration::ZERO).as_secs();
    match d {
        0..=59 => format!("{d}s"),
        60..=3599 => format!("{}m", d / 60),
        3600..=86_399 => format!("{}h", d / 3600),
        _ => format!("{}d", d / 86_400),
    }
}

/// The dashboard rows for `panes` (target, summary, tab title), sorted as
/// [`Dashboard`] sorts them.
pub fn rows(panes: Vec<(Target, SessionSummary, String)>, now: SystemTime) -> Vec<Row> {
    let mut dash = Dashboard::new();
    let mut extra = Vec::with_capacity(panes.len());
    for (target, summary, title) in panes {
        extra.push((summary.session_id.clone(), target, title));
        dash.upsert(summary);
    }
    dash.sessions()
        .iter()
        .filter_map(|s| {
            let (_, target, title) = extra.iter().find(|(id, _, _)| *id == s.session_id)?;
            let ind = attention_ui::indicator(s.state, s.unread);
            let word = ind.as_ref().map_or("no agent".to_owned(), |i| i.label.clone());
            let state = match (s.state, s.since) {
                (AttentionState::Unknown, _) | (_, None) => word,
                (_, Some(t)) => format!("{word} {}", ago(now, t)),
            };
            Some(Row {
                target: *target,
                agent: s.agent.as_ref().map_or_else(String::new, |a| clean(a.display_name(), 40)),
                title: clean(title, 60),
                cwd: s.cwd.as_deref().map(|c| clean(c, 80)),
                branch: s.branch().map(|b| clean(b, 40)),
                state,
                tone: ind.map(|i| i.tone),
                note: s
                    .last_notification
                    .as_deref()
                    .map(|n| clean(n, 120))
                    .filter(|n| !n.is_empty()),
                unread: s.unread,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use halyard_agents::{AgentKind, Cause, GitHead};
    use halyard_workspace::PaneId;

    use super::*;

    fn t(s: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + s)
    }

    fn summary(
        id: &str,
        state: AttentionState,
        unread: bool,
        since: Option<u64>,
    ) -> SessionSummary {
        SessionSummary {
            session_id: id.into(),
            agent: (state != AttentionState::Unknown).then_some(AgentKind::ClaudeCode),
            cwd: Some("/home/me/src/app".into()),
            local_cwd: None,
            git: Some(GitHead::Branch("main".into())),
            state,
            cause: Cause::Initial,
            since: since.map(t),
            last_activity: None,
            unread,
            last_notification: (state == AttentionState::WaitingForInput)
                .then(|| "Claude needs\nyour permission".into()),
        }
    }

    fn target(n: u64) -> Target {
        Target { window: 1, pane: PaneId(n) }
    }

    #[test]
    fn rows_follow_the_dashboard_order_and_carry_their_targets() {
        let panes = vec![
            (target(1), summary("a", AttentionState::Unknown, false, None), "zsh".into()),
            (target(2), summary("b", AttentionState::Working, false, Some(10)), "build".into()),
            (
                target(3),
                summary("c", AttentionState::WaitingForInput, true, Some(40)),
                "api".into(),
            ),
            (
                target(4),
                summary("d", AttentionState::Finished { exit_code: Some(1) }, true, Some(5)),
                "x".into(),
            ),
        ];
        let r = rows(panes, t(100));
        let order: Vec<u64> = r.iter().map(|r| r.target.pane.0).collect();
        assert_eq!(order, vec![3, 4, 2, 1]);
        let w = &r[0];
        assert_eq!(w.agent, "Claude Code");
        assert_eq!(w.title, "api");
        assert_eq!(w.state, "waiting 1m");
        assert_eq!(w.tone, Some(Tone::Attention));
        assert_eq!(w.note.as_deref(), Some("Claude needs your permission"));
        assert_eq!(w.branch.as_deref(), Some("main"));
        assert_eq!(w.cwd.as_deref(), Some("/home/me/src/app"));
        assert!(w.unread);
        assert_eq!(r[1].state, "exited 1 1m");
        assert_eq!(r[2].state, "working 1m");
        assert_eq!(r[3].state, "no agent");
        assert_eq!(r[3].agent, "");
        assert_eq!(r[3].tone, None);
        assert!(r[0].filter_text().contains("api"));
        assert!(r[0].filter_text().contains("main"));
    }

    #[test]
    fn ago_reads_naturally() {
        assert_eq!(ago(t(10), t(10)), "0s");
        assert_eq!(ago(t(69), t(10)), "59s");
        assert_eq!(ago(t(70), t(10)), "1m");
        assert_eq!(ago(t(3610), t(10)), "1h");
        assert_eq!(ago(t(86_410), t(10)), "1d");
        assert_eq!(ago(t(0), t(10)), "0s", "a clock that went back is now");
    }
}
