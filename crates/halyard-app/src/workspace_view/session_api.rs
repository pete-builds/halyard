//! The session API in a window: approval dialogs, focusing a pane for a tool,
//! redrawing after an API report, and the "Paired Tools" palette view. The
//! dialog rules are in [`crate::approval`]; the server half is
//! [`crate::session_api`].

use std::sync::mpsc::SyncSender;
use std::time::{Instant, SystemTime};

use gpui::{
    AnyElement, Context, FocusHandle, KeyDownEvent, MouseButton, ScrollHandle, SharedString, Task,
    Window, div, prelude::*, px,
};
use halyard_config::Theme;
use halyard_ipc::PairingDecision;
use halyard_workspace::PaneId;

use super::{WorkspaceView, rgba};
use crate::approval::{self, Ask, Dialog, Mods, Outcome};
use crate::palette::{self, Mode};
use crate::session_api::{DIALOG_TIMEOUT, PairedTools};
use crate::view::to_hsla;

/// Where a dialog's answer goes. Dropping it unanswered is a denial.
#[derive(Debug)]
pub enum Reply {
    /// A pairing dialog.
    Pairing(SyncSender<PairingDecision>),
    /// A typing dialog.
    Text(SyncSender<bool>),
}

/// An open approval dialog.
pub(super) struct ApprovalUi {
    dialog: Dialog,
    reply: Reply,
    focus: FocusHandle,
    /// The text box's scroll position, for the scroll gate.
    scroll: ScrollHandle,
    serial: u64,
    _timers: [Task<()>; 2],
}

/// How far one scroll step moves the text box.
const SCROLL_LINE: f32 = 18.0;

impl ApprovalUi {
    /// Reads the text box's layout into the scroll gate. Returns true when the
    /// gate changed (redraw, so the Approve button follows).
    fn observe_scroll(&mut self) -> bool {
        let max = f32::from(self.scroll.max_offset().height);
        let scrolled = -f32::from(self.scroll.offset().y);
        let laid_out = f32::from(self.scroll.bounds().size.height) > 0.0;
        let before = self.dialog.view;
        self.dialog.view = self.dialog.view.observe(laid_out, max, scrolled);
        before != self.dialog.view
    }

    fn scroll_by(&mut self, lines: i32) {
        let max = f32::from(self.scroll.max_offset().height);
        let y = match lines {
            i32::MAX => -max,
            i32::MIN => 0.0,
            n => (f32::from(self.scroll.offset().y) - n as f32 * SCROLL_LINE).clamp(-max, 0.0),
        };
        self.scroll.set_offset(gpui::point(px(0.0), px(y)));
    }
}

impl WorkspaceView {
    /// Adds a notice to this window's banner and shows it.
    pub fn push_notice(&mut self, text: String, cx: &mut Context<Self>) {
        if !self.local_notices.contains(&text) {
            self.local_notices.push(text);
        }
        self.banner_hidden = false;
        cx.notify();
    }

    /// The pane with this `HALYARD_SESSION_ID`, if it is in this window.
    pub fn pane_for_session(&self, session_id: &str, cx: &gpui::App) -> Option<PaneId> {
        self.panes
            .iter()
            .find(|(_, p)| p.view.read(cx).session_id() == session_id)
            .map(|(id, _)| *id)
    }

    /// A pane's attention changed through the API: redraw and notify now.
    pub fn api_changed(&mut self, session_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.pane_for_session(session_id, cx) {
            self.attention_changed(id, window, cx);
        }
    }

    /// Shows the pane with this id (switching tabs). Returns whether it is here.
    pub fn focus_session(
        &mut self,
        session_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(id) = self.pane_for_session(session_id, cx) else { return false };
        if self.model.focus_pane(id) {
            self.after_change(window, cx);
        }
        true
    }

    /// Opens an approval dialog, replacing (and so denying) any open one. It
    /// takes keyboard focus and is modal: the window's own actions are blocked
    /// while it is open (`WorkspaceView::run`).
    pub fn show_approval(
        &mut self,
        ask: Ask,
        reply: Reply,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.next_approval += 1;
        let serial = self.next_approval;
        let dialog = Dialog::new(ask, Instant::now());
        // No answer in time is a denial; close the dialog with it.
        let expire = cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(DIALOG_TIMEOUT).await;
            this.update_in(cx, |this, window, cx| {
                if this.approval.as_ref().is_some_and(|a| a.serial == serial) {
                    this.finish_approval(Outcome::Deny, window, cx);
                }
            })
            .ok();
        });
        let focus = cx.focus_handle();
        window.focus(&focus);
        self.approval = Some(ApprovalUi {
            dialog,
            reply,
            focus,
            scroll: ScrollHandle::new(),
            serial,
            _timers: [Self::redraw_when_armed(window, cx), expire],
        });
        window.activate_window();
        cx.notify();
    }

    /// A task that redraws once the dialog arms, so the Approve button lights up.
    fn redraw_when_armed(window: &mut Window, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn_in(window, async move |this, cx| {
            loop {
                let wait = this
                    .update(cx, |this, _| {
                        this.approval
                            .as_ref()
                            .map(|a| a.dialog.arms_at().saturating_duration_since(Instant::now()))
                    })
                    .ok()
                    .flatten();
                let Some(wait) = wait else { return };
                if wait.is_zero() {
                    this.update(cx, |_, cx| cx.notify()).ok();
                    return;
                }
                cx.background_executor().timer(wait).await;
            }
        })
    }

    /// The window became active again: start the arming delay over, since the user
    /// may be mid-keystroke from another window.
    pub(super) fn approval_window_activated(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !window.is_window_active() {
            return;
        }
        let Some(a) = &mut self.approval else { return };
        a.dialog.rearm(Instant::now());
        a._timers[0] = Self::redraw_when_armed(window, cx);
        cx.notify();
    }

    fn finish_approval(&mut self, outcome: Outcome, window: &mut Window, cx: &mut Context<Self>) {
        match outcome {
            Outcome::Approve | Outcome::Deny => {}
            Outcome::Changed => {
                cx.notify();
                return;
            }
            Outcome::Scroll(n) => {
                if let Some(a) = &mut self.approval {
                    a.scroll_by(n);
                }
                cx.notify();
                return;
            }
            Outcome::Ignored => {
                // A stray key re-armed the dialog; redraw now and again once armed.
                if let Some(a) = &mut self.approval {
                    a._timers[0] = Self::redraw_when_armed(window, cx);
                }
                cx.notify();
                return;
            }
        }
        let Some(a) = self.approval.take() else { return };
        let yes = outcome == Outcome::Approve;
        // The server may have stopped waiting (timeout); then nobody listens.
        match a.reply {
            Reply::Pairing(tx) => {
                let d = if yes { a.dialog.pairing_decision() } else { PairingDecision::Deny };
                tx.try_send(d).ok();
            }
            Reply::Text(tx) => {
                tx.try_send(yes).ok();
            }
        }
        // Back to where the keyboard was: the palette if one is open, else the pane.
        match &self.palette {
            Some(p) => window.focus(&p.focus),
            None => self.focus_active_pane(window, cx),
        }
        cx.notify();
    }

    /// A key no binding took. Every such key stops here, so none reaches the pane.
    pub(super) fn approval_key(
        &mut self,
        ev: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(a) = &mut self.approval else { return };
        let m = ev.keystroke.modifiers;
        let mods = Mods { control: m.control, alt: m.alt, shift: m.shift, platform: m.platform };
        let outcome = a.dialog.key(&ev.keystroke.key, mods, Instant::now());
        cx.stop_propagation();
        self.finish_approval(outcome, window, cx);
    }

    /// The approve chord (the dialog's own action) or a click on Approve.
    pub(super) fn approval_approve(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(a) = &mut self.approval else { return };
        a.observe_scroll();
        let outcome = a.dialog.approve_action(Instant::now());
        self.finish_approval(outcome, window, cx);
    }

    /// Escape (the dialog's own action) or a click on Deny.
    pub(super) fn approval_deny(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.finish_approval(Outcome::Deny, window, cx);
    }

    fn approval_toggle(&mut self, index: Option<usize>, cx: &mut Context<Self>) {
        let Some(a) = &mut self.approval else { return };
        let now = Instant::now();
        let outcome = match index {
            Some(i) if a.dialog.armed(now) => match a.dialog.requested().get(i) {
                Some(scope) => a.dialog.toggle_scope(*scope),
                None => Outcome::Ignored,
            },
            Some(_) => Outcome::Ignored,
            None => a.dialog.click_always(now),
        };
        if outcome == Outcome::Changed {
            cx.notify();
        }
    }

    /// Updates the scroll gate from the last layout (called each render).
    pub(super) fn approval_prepaint(&mut self, cx: &mut Context<Self>) {
        if let Some(a) = &mut self.approval
            && a.observe_scroll()
        {
            cx.notify();
        }
    }

    /// Whether a dialog holds the keyboard (panes must not take focus back).
    pub(super) fn approval_open(&self) -> bool {
        self.approval.is_some()
    }

    pub(super) fn render_approval(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let a = self.approval.as_ref()?;
        let ui = &theme.ui;
        let blocked = a.dialog.blocked(Instant::now());
        let mono: SharedString = self.settings(cx).font().family.clone().into();
        let muted = to_hsla(ui.text_muted);
        let line = |text: String| div().text_sm().whitespace_normal().child(text);
        let code = |text: String| {
            div().font_family(mono.clone()).text_sm().whitespace_normal().child(text)
        };
        let mut body = div().flex().flex_col().gap_2();
        let title;
        match &a.dialog.ask {
            Ask::Pairing(r) => {
                title = "Pair a tool with Halyard?";
                body = body
                    .child(line(format!(
                        "A program calling itself \u{201c}{}\u{201d} asks for access.",
                        r.client_name
                    )))
                    .child(div().text_xs().text_color(muted).whitespace_normal().child(format!(
                        "The name is the tool's own claim. Process {}: {}",
                        r.peer.pid,
                        r.peer.exe.as_ref().map_or_else(
                            || "executable unknown".to_owned(),
                            |p| p.display().to_string()
                        )
                    )));
                for (i, scope) in a.dialog.requested().into_iter().enumerate() {
                    let on = a.dialog.scopes.contains(&scope);
                    let tone = if approval::scope_is_sensitive(scope) {
                        to_hsla(ui.danger)
                    } else {
                        to_hsla(ui.text)
                    };
                    body = body.child(
                        div()
                            .id(("approval-scope", i))
                            .flex()
                            .flex_row()
                            .gap_2()
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.approval_toggle(Some(i), cx);
                            }))
                            .child(div().font_family(mono.clone()).child(if on {
                                "[x]"
                            } else {
                                "[ ]"
                            }))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .child(div().text_sm().text_color(tone).child(format!(
                                        "{}  {}",
                                        i + 1,
                                        scope.as_str()
                                    )))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(muted)
                                            .whitespace_normal()
                                            .child(approval::scope_help(scope)),
                                    ),
                            ),
                    );
                }
                if a.dialog.offers_always() {
                    let on = a.dialog.always_allow_input;
                    body = body.child(
                        div()
                            .id("approval-always")
                            .flex()
                            .flex_row()
                            .gap_2()
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| this.approval_toggle(None, cx)))
                            .child(div().font_family(mono.clone()).child(if on {
                                "[x]"
                            } else {
                                "[ ]"
                            }))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .text_sm()
                                    .whitespace_normal()
                                    .text_color(to_hsla(ui.warning))
                                    .child(
                                        "Always allow typing from this tool without asking (click \
                                     to change). Text with Enter or control characters still \
                                     asks every time; a program that acts on single keys could \
                                     still act on it.",
                                    ),
                            ),
                    );
                }
            }
            Ask::SendText(r) => {
                title = "Type into a pane?";
                let pane = crate::session_api::pane_label(&r.session);
                body = body
                    .child(line(format!(
                        "\u{201c}{}\u{201d} wants to type into {pane}.",
                        r.client_name
                    )))
                    .child(div().text_xs().text_color(muted).child(match r.reason {
                        halyard_ipc::SendTextReason::ControlCharacters => {
                            "The text holds Enter, a control or an invisible character, so it always asks."
                        }
                        halyard_ipc::SendTextReason::NoStandingPermission => {
                            "This tool asks every time it types."
                        }
                    }))
                    .child(div().text_sm().child(format!(
                        "Exact text, every control character shown ({}):",
                        approval::size_label(&r.text)
                    )));
                // One scrolling box holds everything the approval covers; Approve
                // waits until its end has been on screen (`TextView`).
                let mut shown = div().flex().flex_col().gap_2().child(code(r.display_text.clone()));
                if !r.typed_since_approval.is_empty() {
                    shown = shown
                        .child(
                            div()
                                .text_xs()
                                .text_color(to_hsla(ui.warning))
                                .whitespace_normal()
                                .child(format!(
                                    "Already typed on this line by a tool since your last \
                                     approval ({}). Enter runs it together with the text above:",
                                    approval::size_label(&r.typed_since_approval)
                                )),
                        )
                        .child(code(r.typed_since_approval_display.clone()));
                }
                let needs_scroll = a.dialog.view == approval::TextView::NeedsScroll;
                body = body.child(
                    div()
                        .id("approval-text")
                        .px_2()
                        .py_1()
                        .rounded_sm()
                        .bg(to_hsla(ui.surface_sunken))
                        .border_1()
                        .border_color(if needs_scroll {
                            to_hsla(ui.warning)
                        } else {
                            to_hsla(ui.border)
                        })
                        .max_h(px(200.0))
                        .overflow_y_scroll()
                        .track_scroll(&a.scroll)
                        .on_scroll_wheel(cx.listener(|_, _, _, cx| cx.notify()))
                        .child(shown),
                );
                if needs_scroll {
                    body = body.child(div().text_xs().text_color(to_hsla(ui.warning)).child(
                        "More below: scroll to the end (wheel, \u{2193}, Page Down or End) \
                         before you can approve.",
                    ));
                }
            }
        }
        let (_, chord) = approval::approve_chord(cfg!(target_os = "macos"));
        let button = |id: &'static str, label: String, bg, fg| {
            div()
                .id(id)
                .px_3()
                .py_1()
                .rounded_sm()
                .border_1()
                .border_color(to_hsla(ui.border_focused))
                .bg(bg)
                .text_color(fg)
                .text_sm()
                .cursor_pointer()
                .child(label)
        };
        let deny = button(
            "approval-deny",
            "Deny (Esc)".into(),
            to_hsla(ui.surface_sunken),
            to_hsla(ui.text),
        )
        .on_click(cx.listener(|this, _, window, cx| this.approval_deny(window, cx)));
        let (abg, afg) = if blocked.is_none() {
            (to_hsla(ui.accent), to_hsla(ui.accent_text))
        } else {
            (rgba(ui.surface_sunken, 1.0), muted)
        };
        let label = match blocked {
            None => format!("Approve ({chord})"),
            Some(why) => format!("Approve: {why}"),
        };
        let approve = button("approval-approve", label, abg, afg)
            .on_click(cx.listener(|this, _, window, cx| this.approval_approve(window, cx)));
        Some(
            div()
                .id("approval-backdrop")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .occlude()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .bg(rgba(ui.surface, 0.55))
                .flex()
                .justify_center()
                .items_center()
                .child(
                    div()
                        .id("approval")
                        .key_context(crate::keybind::APPROVAL_CONTEXT)
                        .track_focus(&a.focus)
                        .on_action(cx.listener(
                            |this, _: &crate::keybind::ApproveDialog, window, cx| {
                                this.approval_approve(window, cx);
                            },
                        ))
                        .on_action(cx.listener(
                            |this, _: &crate::keybind::DenyDialog, window, cx| {
                                this.approval_deny(window, cx);
                            },
                        ))
                        .on_key_down(cx.listener(Self::approval_key))
                        .w(px(520.0))
                        .max_w_full()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .p_4()
                        .rounded_md()
                        .border_2()
                        .border_color(to_hsla(ui.attention))
                        .bg(to_hsla(ui.surface_raised))
                        .text_color(to_hsla(ui.text))
                        .shadow_lg()
                        .child(div().text_color(to_hsla(ui.attention)).child(title))
                        .child(body)
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .justify_end()
                                .gap_2()
                                .child(deny)
                                .child(approve),
                        ),
                )
                .into_any_element(),
        )
    }

    // ------------------------------------------------------------ paired tools

    /// Opens the "Paired Tools" view: every grant, Enter revokes the selected one.
    /// The grants file is read on a background thread (its lock can wait up to 5 s
    /// for another instance); the view says "loading" until then.
    pub(super) fn open_paired_tools(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(handle) =
            self.settings_api(cx).and_then(|api| api.server()).map(|server| server.grants())
        else {
            self.push_notice(
                "the session API is not running, so there are no paired tools".into(),
                cx,
            );
            return;
        };
        let model = palette::Model::new(Mode::PairedTools, Vec::new(), String::new(), None);
        self.show_palette(model, Vec::new(), window, cx);
        if let Some(p) = &mut self.palette {
            p.paired_loading = true;
        }
        let load = handle.clone();
        let task = cx.background_spawn(async move { PairedTools::load(&load) });
        cx.spawn_in(window, async move |this, cx| {
            let tools = task.await;
            this.update(cx, |this, cx| {
                let Some(p) = &mut this.palette else { return };
                if !matches!(p.model.mode, Mode::PairedTools) {
                    return;
                }
                let labels = tools.labels(SystemTime::now());
                let query = p.model.query.clone();
                p.model = palette::Model::new(Mode::PairedTools, labels, query, None);
                p.paired = Some((tools, handle));
                p.paired_loading = false;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Revokes the grant picked in the "Paired Tools" view, on a background
    /// thread, and says how it went.
    pub(super) fn revoke_picked(
        &mut self,
        picked: Option<usize>,
        paired: Option<(PairedTools, halyard_ipc::GrantsHandle)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(i), Some((tools, handle))) = (picked, paired) else { return };
        let task = cx.background_spawn(async move {
            match tools.revoke(&handle, i) {
                Ok(Some(name)) => format!("revoked the pairing of \u{201c}{name}\u{201d}"),
                Ok(None) => "that pairing was already gone".to_owned(),
                Err(e) => format!("could not revoke the pairing: {e}"),
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            let text = task.await;
            this.update(cx, |this, cx| this.push_notice(text, cx)).ok();
        })
        .detach();
    }

    fn settings_api<'a>(&self, cx: &'a gpui::App) -> Option<&'a crate::session_api::PaneApi> {
        cx.global::<crate::app_state::AppState>().session_api.as_ref()
    }
}
