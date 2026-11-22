//! The agent dashboard (a palette mode listing every pane of every window),
//! jumping to a pane, and the command block actions on the focused pane.

use std::time::SystemTime;

use gpui::{AnyElement, App, ClipboardItem, Context, Window, div, prelude::*};
use halyard_config::{Theme, UiColors};

use super::{PaletteUi, WorkspaceView, rgba};
use crate::app_state::AppState;
use crate::blocks_ui::{self, Jump};
use crate::dashboard_ui::{self, Row, Target};
use crate::palette::{self, Mode};
use crate::signals;
use crate::view::to_hsla;

impl WorkspaceView {
    /// This window's panes as dashboard entries.
    fn dashboard_entries(
        &self,
        window_id: u64,
        cx: &App,
    ) -> Vec<(Target, halyard_agents::SessionSummary, String)> {
        let host = crate::app_state::hostname().unwrap_or("");
        let mut out = Vec::new();
        for (index, tab) in self.model.tabs().iter().enumerate() {
            let title = self.tab_title(index, cx);
            for id in tab.root.panes() {
                let Some(p) = self.panes.get(&id) else { continue };
                let v = p.view.read(cx);
                let summary = signals::lock(v.monitor()).summary(v.session_id(), host);
                out.push((Target { window: window_id, pane: id }, summary, title.clone()));
            }
        }
        out
    }

    /// Opens the dashboard: every pane of every window, sorted by who needs
    /// the user first.
    pub(super) fn open_dashboard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let me = window.window_handle();
        let mut entries = self.dashboard_entries(me.window_id().as_u64(), cx);
        for handle in cx.windows() {
            if handle == me {
                continue;
            }
            let Some(h) = handle.downcast::<WorkspaceView>() else { continue };
            let id = handle.window_id().as_u64();
            if let Ok(view) = h.read(cx) {
                entries.extend(view.dashboard_entries(id, cx));
            }
        }
        let rows = dashboard_ui::rows(entries, SystemTime::now());
        let labels = rows.iter().map(Row::filter_text).collect();
        let model = palette::Model::new(Mode::Agents, labels, String::new(), None);
        self.show_palette(model, Vec::new(), window, cx);
        if let Some(p) = &mut self.palette {
            p.agents = rows;
        }
    }

    /// Focuses a pane in this window (its tab too).
    fn focus_pane_here(
        &mut self,
        pane: halyard_workspace::PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.model.focus_pane(pane) {
            self.after_change(window, cx);
        }
    }

    /// Jumps to a dashboard row's pane: focuses its tab and pane, and its
    /// window when that is another one.
    pub(super) fn jump_to(&mut self, target: Target, window: &mut Window, cx: &mut Context<Self>) {
        if window.window_handle().window_id().as_u64() == target.window {
            self.focus_pane_here(target.pane, window, cx);
            return;
        }
        let other = cx
            .windows()
            .into_iter()
            .filter(|h| h.window_id().as_u64() == target.window)
            .find_map(|h| h.downcast::<WorkspaceView>());
        if let Some(h) = other {
            h.update(cx, |view, window, cx| {
                window.activate_window();
                view.focus_pane_here(target.pane, window, cx);
            })
            .ok();
        }
    }

    /// One dashboard row: state mark, agent and title, then cwd, branch and the
    /// last notification on a second line.
    pub(super) fn agent_row(
        p: &PaletteUi,
        entry: usize,
        selected: bool,
        theme: &Theme,
    ) -> Option<AnyElement> {
        let r = p.agents.get(entry)?;
        let ui: &UiColors = &theme.ui;
        let (bg, fg, sub) = if selected {
            (to_hsla(ui.accent), to_hsla(ui.accent_text), to_hsla(ui.accent_text))
        } else {
            (rgba(ui.surface_raised, 0.0), to_hsla(ui.text), to_hsla(ui.text_muted))
        };
        let tone = match (selected, r.tone) {
            (false, Some(t)) => to_hsla(t.color(ui)),
            _ => sub,
        };
        let mut detail = Vec::new();
        if let Some(c) = &r.cwd {
            detail.push(c.clone());
        }
        if let Some(b) = &r.branch {
            detail.push(format!("⎇ {b}"));
        }
        if let Some(n) = &r.note {
            detail.push(format!("“{n}”"));
        }
        let head = div()
            .flex()
            .flex_row()
            .gap_2()
            .child(div().flex_none().text_color(tone).child(r.state.clone()))
            .when(r.unread, |d| {
                d.child(
                    div()
                        .flex_none()
                        .px_1()
                        .rounded_sm()
                        .bg(to_hsla(ui.attention))
                        .text_color(to_hsla(ui.surface))
                        .text_xs()
                        .child("new"),
                )
            })
            .child(div().flex_none().child(r.agent.clone()))
            .child(
                div().overflow_hidden().whitespace_nowrap().text_color(sub).child(r.title.clone()),
            );
        Some(
            div()
                .flex()
                .flex_col()
                .px_3()
                .py_1()
                .mx_1()
                .rounded_sm()
                .bg(bg)
                .text_color(fg)
                .text_sm()
                .child(head)
                .when(!detail.is_empty(), |d| {
                    d.child(
                        div()
                            .text_xs()
                            .text_color(sub)
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(detail.join("  ·  ")),
                    )
                })
                .into_any_element(),
        )
    }

    /// Scrolls the focused pane to the previous or next prompt.
    pub(super) fn jump_prompt(&mut self, jump: Jump, cx: &mut Context<Self>) {
        if let Some(p) = self.model.focused().and_then(|id| self.panes.get(&id)) {
            p.view.read(cx).jump_prompt(jump);
        }
    }

    /// Copies the focused pane's last command output, or says why not.
    pub(super) fn copy_last_output(&mut self, cx: &mut Context<Self>) {
        let text = self
            .model
            .focused()
            .and_then(|id| self.panes.get(&id))
            .and_then(|p| p.view.read(cx).last_output());
        match text {
            Some(t) => cx.write_to_clipboard(ClipboardItem::new_string(t)),
            None => self.notice(
                "no finished command to copy (it needs shell integration: see Copy Shell \
                 Integration Line)"
                    .into(),
                cx,
            ),
        }
    }

    /// Writes the integration script for the configured shell next to the
    /// config file and copies the line that loads it. Nothing is added to any
    /// rc file: the user pastes the line where they want it.
    pub(super) fn copy_shell_integration(&mut self, cx: &mut Context<Self>) {
        let s = &cx.global::<AppState>().settings;
        let program = crate::settings::shell_command(&s.config).map(|(p, _)| p);
        let dir = s.path.as_deref().and_then(|p| p.parent()).map(|d| d.join("shell-integration"));
        let Some(dir) = dir else {
            self.notice("could not find the config folder for the integration script".into(), cx);
            return;
        };
        match blocks_ui::integration_line(program.as_deref(), &dir) {
            Ok((shell, line)) => {
                cx.write_to_clipboard(ClipboardItem::new_string(line));
                self.notice(format!("copied: add the line to {} to load it", shell.rc_file()), cx);
            }
            Err(e) => self.notice(e, cx),
        }
    }

    fn notice(&mut self, text: String, cx: &mut Context<Self>) {
        self.local_notices.retain(|n| *n != text);
        self.local_notices.push(text);
        self.banner_hidden = false;
        cx.notify();
    }
}
