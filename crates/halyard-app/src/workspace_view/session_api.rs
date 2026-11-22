//! The session API in a window: approval dialogs, focusing a pane for a tool,
//! redrawing after an API report, and the "Paired Tools" palette view. The
//! dialog rules are in [`crate::approval`]; the server half is
//! [`crate::session_api`].

use std::sync::mpsc::SyncSender;
use std::time::{Instant, SystemTime};

use gpui::{
    AnyElement, Context, FocusHandle, KeyDownEvent, MouseButton, SharedString, Task, Window, div,
    prelude::*, px,
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
    serial: u64,
    _timers: [Task<()>; 2],
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
    /// takes keyboard focus, so keys meant for the terminal land on it, where
    /// only Escape and the deliberate approve chord do anything.
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
        let arm_in = dialog.arms_at().saturating_duration_since(Instant::now());
        // Redraw once armed, so the Approve button lights up.
        let arm = cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(arm_in).await;
            this.update(cx, |_, cx| cx.notify()).ok();
        });
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
        self.approval = Some(ApprovalUi { dialog, reply, focus, serial, _timers: [arm, expire] });
        window.activate_window();
        cx.notify();
    }

    fn finish_approval(&mut self, outcome: Outcome, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(outcome, Outcome::Approve | Outcome::Deny) {
            if outcome == Outcome::Changed {
                cx.notify();
            }
            return;
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

    pub(super) fn approval_key(
        &mut self,
        ev: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(a) = &mut self.approval else { return };
        let m = ev.keystroke.modifiers;
        let mods = Mods { control: m.control, alt: m.alt, shift: m.shift, platform: m.platform };
        let outcome =
            a.dialog.key(&ev.keystroke.key, mods, cfg!(target_os = "macos"), Instant::now());
        // Every key stops here: none reaches the pane behind the dialog.
        cx.stop_propagation();
        self.finish_approval(outcome, window, cx);
    }

    fn approval_click(&mut self, approve: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(a) = &mut self.approval else { return };
        let outcome = if approve { a.dialog.click_approve(Instant::now()) } else { Outcome::Deny };
        self.finish_approval(outcome, window, cx);
    }

    fn approval_toggle(&mut self, index: Option<usize>, cx: &mut Context<Self>) {
        let Some(a) = &mut self.approval else { return };
        if !a.dialog.armed(Instant::now()) {
            return;
        }
        let outcome = match index {
            Some(i) => match a.dialog.requested().get(i) {
                Some(scope) => a.dialog.toggle_scope(*scope),
                None => Outcome::Ignored,
            },
            None => a.dialog.toggle_always(),
        };
        if outcome == Outcome::Changed {
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
        let armed = a.dialog.armed(Instant::now());
        let mono: SharedString = self.settings(cx).font().family.clone().into();
        let muted = to_hsla(ui.text_muted);
        let line = |text: String| div().text_sm().whitespace_normal().child(text);
        let code_box = |text: String| {
            div()
                .px_2()
                .py_1()
                .rounded_sm()
                .bg(to_hsla(ui.surface_sunken))
                .border_1()
                .border_color(to_hsla(ui.border))
                .font_family(mono.clone())
                .text_sm()
                .whitespace_normal()
                .max_h(px(160.0))
                .overflow_hidden()
                .child(text)
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
                            .child(div().font_family(mono.clone()).child(if on { "[x]" } else { "[ ]" }))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .text_sm()
                                    .whitespace_normal()
                                    .text_color(to_hsla(ui.warning))
                                    .child(
                                    "T  Always allow typing from this tool without asking. Text \
                                     with Enter or control characters still asks every time; a \
                                     program that acts on single keys could still act on it.",
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
                    .child(div().text_xs().text_color(muted).child("Exact text, every control character shown:"))
                    .child(code_box(r.display_text.clone()));
                if !r.typed_since_approval.is_empty() {
                    body = body
                        .child(div().text_xs().text_color(to_hsla(ui.warning)).whitespace_normal().child(
                            "Already typed on this line by a tool since your last approval. \
                             Enter runs it together with the text above:",
                        ))
                        .child(code_box(r.typed_since_approval_display.clone()));
                }
            }
        }
        let chord = approval::approve_chord_label(cfg!(target_os = "macos"));
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
        .on_click(cx.listener(|this, _, window, cx| this.approval_click(false, window, cx)));
        let (abg, afg) = if armed {
            (to_hsla(ui.accent), to_hsla(ui.accent_text))
        } else {
            (rgba(ui.surface_sunken, 1.0), muted)
        };
        let approve = button("approval-approve", format!("Approve ({chord})"), abg, afg)
            .on_click(cx.listener(|this, _, window, cx| this.approval_click(true, window, cx)));
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
                        .key_context("Approval")
                        .track_focus(&a.focus)
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
        let tools = PairedTools::load(&handle);
        let labels = tools.labels(SystemTime::now());
        let model = palette::Model::new(Mode::PairedTools, labels, String::new(), None);
        self.show_palette(model, Vec::new(), window, cx);
        if let Some(p) = &mut self.palette {
            p.paired = Some((tools, handle));
        }
    }

    /// Revokes the grant picked in the "Paired Tools" view.
    pub(super) fn revoke_picked(
        &mut self,
        picked: Option<usize>,
        paired: Option<(PairedTools, halyard_ipc::GrantsHandle)>,
        cx: &mut Context<Self>,
    ) {
        let (Some(i), Some((tools, handle))) = (picked, paired) else { return };
        let text = match tools.revoke(&handle, i) {
            Ok(Some(name)) => format!("revoked the pairing of \u{201c}{name}\u{201d}"),
            Ok(None) => "that pairing was already gone".to_owned(),
            Err(e) => format!("could not revoke the pairing: {e}"),
        };
        self.push_notice(text, cx);
    }

    fn settings_api<'a>(&self, cx: &'a gpui::App) -> Option<&'a crate::session_api::PaneApi> {
        cx.global::<crate::app_state::AppState>().session_api.as_ref()
    }
}
