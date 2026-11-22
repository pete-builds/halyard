//! Agent attention in a window: polling each pane, tab and pane marks, focus
//! for unread, and desktop notifications. The decisions are in
//! [`crate::attention_ui`]; this is the wiring.

use std::time::Instant;

use gpui::{AnyElement, Context, Hsla, Task, Window, div, prelude::*};
use halyard_agents::AttentionState;
use halyard_config::Theme;
use halyard_workspace::PaneId;

use super::{WorkspaceView, rgba};
use crate::app_state::AppState;
use crate::attention_ui::{self, Change, Indicator};
use crate::signals::{self, POLL_INTERVAL};
use crate::view::{PaneBadge, to_hsla};

impl WorkspaceView {
    /// The poll loop: every [`POLL_INTERVAL`], each pane's foreground process
    /// and the attention clock. It reads one process entry per pane, and argv
    /// only while a command runs.
    pub(super) fn start_agent_poll(window: &mut Window, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await;
                if this.update_in(cx, |this, window, cx| this.poll_agents(window, cx)).is_err() {
                    break;
                }
            }
        })
    }

    fn poll_agents(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let extra = cx.global::<AppState>().settings.config.agents.detect.clone();
        let mut changed = Vec::new();
        for (id, p) in &self.panes {
            if p.view.update(cx, |v, _| v.poll_agent(&extra)) {
                changed.push(*id);
            }
        }
        for id in changed {
            self.attention_changed(id, window, cx);
        }
    }

    /// A pane's attention changed: notify if it now needs the user and the
    /// window is not active, then redraw the marks.
    pub(super) fn attention_changed(
        &mut self,
        id: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(p) = self.panes.get(&id) else { return };
        let (pane, after, agent, message) = {
            let v = p.view.read(cx);
            let m = signals::lock(v.monitor());
            let a = m.attention();
            (
                v.session_id().to_owned(),
                a.state(),
                a.agent().map(|k| k.display_name().to_owned()),
                a.last_notification().map(str::to_owned),
            )
        };
        let before = self.attention_states.insert(id, after).unwrap_or(AttentionState::Unknown);
        let tab_title = self.model.tab_of(id).map(|t| self.tab_title(t, cx)).unwrap_or_default();
        let change = Change {
            pane: &pane,
            before,
            after,
            window_active: window.is_window_active(),
            enabled: cx.global::<AppState>().settings.config.agents.notify_on_waiting,
            agent: agent.as_deref(),
            tab_title: &tab_title,
            message: message.as_deref(),
        };
        if let Some(note) = self.notifier.decide(&change, Instant::now())
            && let Err(e) = crate::notify::send(&note)
        {
            crate::notify::report_failure(&e);
        }
        self.refresh_chrome(window, cx);
        cx.notify();
    }

    /// Drops a closing pane's notification history.
    pub(super) fn forget_attention(&mut self, id: PaneId, cx: &gpui::App) {
        self.attention_states.remove(&id);
        if let Some(p) = self.panes.get(&id) {
            self.notifier.forget(p.view.read(cx).session_id());
        }
    }

    /// Tells each pane whether the user can see it (focused, in the shown tab,
    /// in the active window). Returns the panes whose marks changed.
    pub(super) fn sync_seen(&mut self, window: &Window, cx: &mut Context<Self>) -> Vec<PaneId> {
        let focused = self.model.focused();
        let active = window.is_window_active();
        let mut changed = Vec::new();
        for (id, p) in &self.panes {
            let seen = active && Some(*id) == focused;
            if p.view.update(cx, |v, _| v.set_seen(seen)) {
                changed.push(*id);
            }
        }
        changed
    }

    /// The attention state and unread flag of a pane.
    fn pane_attention(&self, id: PaneId, cx: &gpui::App) -> Option<(AttentionState, bool, String)> {
        let p = self.panes.get(&id)?;
        let v = p.view.read(cx);
        let m = signals::lock(v.monitor());
        let a = m.attention();
        let agent = a.agent().map_or_else(|| "Agent".to_owned(), |k| k.display_name().to_owned());
        Some((a.state(), a.unread(), agent))
    }

    /// The badge and outline for a pane with an agent; `None` without one.
    pub(super) fn pane_marks(
        &self,
        id: PaneId,
        theme: &Theme,
        cx: &gpui::App,
    ) -> Option<(PaneBadge, Option<Hsla>)> {
        let (state, unread, agent) = self.pane_attention(id, cx)?;
        let ind = attention_ui::indicator(state, unread)?;
        let ui = &theme.ui;
        let tone = to_hsla(ind.tone.color(ui));
        let badge = PaneBadge {
            text: format!("{} {agent} · {}", ind.glyph, ind.label).into(),
            fg: tone,
            bg: rgba(ui.surface_raised, 0.92),
            border: unread.then(|| to_hsla(ui.attention)),
        };
        // Waiting, or news not yet seen, outlines the pane in the attention color.
        let outline =
            (state == AttentionState::WaitingForInput || unread).then(|| to_hsla(ui.attention));
        Some((badge, outline))
    }

    /// The element drawn before tab `index`'s title: the state glyph in its
    /// color, or, when the tab has unread news, the glyph inverted on a pill of
    /// that color so it reads as new at a glance.
    pub(super) fn tab_mark(
        &self,
        index: usize,
        theme: &Theme,
        cx: &gpui::App,
    ) -> Option<AnyElement> {
        let ind = self.tab_indicator(index, cx)?;
        let tone = to_hsla(ind.tone.color(&theme.ui));
        let el = div().id(("tab-mark", index)).flex_none().text_xs().px_1().rounded_sm();
        let el = if ind.unread {
            el.bg(tone).text_color(to_hsla(theme.ui.surface)).child(format!("{} new", ind.glyph))
        } else {
            el.text_color(tone).child(ind.glyph)
        };
        Some(el.into_any_element())
    }

    /// The mark for tab `index`: its most urgent pane.
    pub(super) fn tab_indicator(&self, index: usize, cx: &gpui::App) -> Option<Indicator> {
        let tab = self.model.tabs().get(index)?;
        let states: Vec<(AttentionState, bool)> = tab
            .root
            .panes()
            .into_iter()
            .filter_map(|id| self.pane_attention(id, cx).map(|(s, u, _)| (s, u)))
            .collect();
        let (state, unread) = attention_ui::most_urgent(&states)?;
        attention_ui::indicator(state, unread)
    }
}
