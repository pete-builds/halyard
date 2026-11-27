//! Renaming a tab in place, and the right-click menus for tabs and panes.
//!
//! Both hold their target by pane id ([`TabRef`], a pane), never by tab
//! index, so a tab that opens, closes or moves while a field or menu is
//! open cannot redirect it to another tab.
//!
//! Every mouse-down handled here calls `window.prevent_default()` and stops
//! propagation: the window root tracks focus, and a click that bubbled up to
//! it would take keyboard focus away from the field, menu or pane the click
//! just focused.

use gpui::{
    AnyElement, Context, FocusHandle, KeyDownEvent, MouseButton, MouseDownEvent, Pixels, Point,
    SharedString, Subscription, Window, div, prelude::*, px,
};
use halyard_config::{Action, Theme};
use halyard_workspace::{PaneId, Workspace};

use super::{WorkspaceView, rgba};
use crate::context_menu::{self, Entry, Item, MENU_W, Menu, PAD_Y, ROW_H, SEP_H};
use crate::keybind;
use crate::line_edit::{Edit, LineEdit};
use crate::view::to_hsla;

/// A tab, named by its panes so that it stays the same tab while others
/// open, close or move. Panes never move between tabs, so any of them still
/// open finds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TabRef {
    panes: Vec<PaneId>,
}

impl TabRef {
    /// The tab at `index` now.
    pub(super) fn of(model: &Workspace, index: usize) -> Option<Self> {
        model.tabs().get(index).map(|t| Self { panes: t.root.panes() })
    }

    /// Where that tab is now, or `None` once it has closed.
    pub(super) fn index(&self, model: &Workspace) -> Option<usize> {
        self.panes.iter().find_map(|p| model.tab_of(*p))
    }

    /// Take up the tab's current panes (after a split or a close), so the
    /// reference survives its original panes closing one at a time. False
    /// once the tab has closed.
    pub(super) fn refresh(&mut self, model: &Workspace) -> bool {
        let Some(i) = self.index(model) else { return false };
        self.panes = model.tabs()[i].root.panes();
        true
    }
}

/// Whether a confirmed close may go ahead: true unless it must keep a tab
/// (Close Other Tabs) and that tab has closed in the meantime.
pub(super) fn still_kept(keep: Option<&TabRef>, model: &Workspace) -> bool {
    keep.is_none_or(|t| t.index(model).is_some())
}

/// Save a typed tab name on `target`. An empty name clears it, so the tab
/// shows its automatic title again. `auto` is the automatic title the field
/// started with when the tab had no name: confirming it unchanged leaves the
/// tab unnamed, so its title keeps following the program. False when the
/// tab has closed.
pub(super) fn commit_name(
    model: &mut Workspace,
    target: &TabRef,
    text: &str,
    auto: Option<&str>,
) -> bool {
    let Some(i) = target.index(model) else { return false };
    if auto.is_some_and(|a| a.trim() == text.trim()) {
        return true;
    }
    model.rename_tab(i, Some(text))
}

/// The in-place tab name field.
pub(super) struct RenameUi {
    target: TabRef,
    edit: LineEdit,
    /// The automatic title, when the tab had no name.
    auto: Option<String>,
    focus: FocusHandle,
    _blur: Subscription,
}

/// What a menu acts on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum MenuTarget {
    /// A tab (its right-click menu).
    Tab(TabRef),
    /// A pane (its right-click menu).
    Pane(PaneId),
}

/// An open right-click menu.
pub(super) struct MenuUi {
    menu: Menu,
    target: MenuTarget,
    /// Where it was opened, in window coordinates.
    at: (f32, f32),
    focus: FocusHandle,
    _blur: Subscription,
}

impl WorkspaceView {
    /// Whether tab `index` shows the name field.
    pub(super) fn renaming(&self, index: usize) -> bool {
        self.rename.as_ref().is_some_and(|r| r.target.index(&self.model) == Some(index))
    }

    /// Follow the rename and menu targets through a layout change; drop
    /// them when their tab or pane has gone.
    pub(super) fn refresh_targets(&mut self, window: &mut Window) {
        if self.rename.as_mut().is_some_and(|r| !r.target.refresh(&self.model)) {
            self.drop_rename(false, window);
        }
        let gone = self.menu.as_mut().is_some_and(|m| match &mut m.target {
            MenuTarget::Tab(t) => !t.refresh(&self.model),
            MenuTarget::Pane(id) => self.model.tab_of(*id).is_none(),
        });
        if gone {
            self.menu = None;
        }
    }

    /// The focus handle an open menu or name field keeps through
    /// `focus_active_pane`. A layout change while a name is being typed (a
    /// pane exiting in another tab, a tool focusing a session) must not hand
    /// the rest of the name to a shell; a field that lost the keyboard while
    /// the window was inactive (a dialog) gets it back when that closes.
    pub(super) fn overlay_focus(&self) -> Option<FocusHandle> {
        if let Some(m) = &self.menu {
            return Some(m.focus.clone());
        }
        self.rename.as_ref().map(|r| r.focus.clone())
    }

    /// Close the name field. Every way it closes comes through here.
    ///
    /// `user_chose` is Enter, Escape or a click elsewhere, after which the
    /// caller gives the keyboard to the pane (or the click takes it). Any
    /// other end while the field holds the keyboard (its tab closing, another
    /// field or menu opening) parks the keyboard on the window root, where
    /// typing goes nowhere, until the user clicks anywhere or runs an action
    /// (`unpark`): the rest of a name must never run in a shell.
    fn drop_rename(&mut self, user_chose: bool, window: &mut Window) -> Option<RenameUi> {
        let r = self.rename.take()?;
        if !user_chose && r.focus.is_focused(window) {
            self.parked = true;
            window.focus(&self.focus);
        }
        Some(r)
    }

    /// The user picked where the keyboard goes (an action, or any click
    /// through `unpark_on_click`): a parked keyboard follows the active pane
    /// again.
    pub(super) fn unpark(&mut self) {
        self.parked = false;
    }

    /// Any mouse-down in the window, before anything handles it (the root's
    /// capture phase, so a control that stops propagation, such as a tab's
    /// close button or the banner's, still counts): a parked keyboard goes
    /// to the active pane, and the click may then move it elsewhere.
    pub(super) fn unpark_on_click(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.parked {
            self.unpark();
            self.focus_active_pane(window, cx);
        }
    }

    // ------------------------------------------------------------- rename

    /// Turn tab `index`'s title into a text field holding its name (or its
    /// automatic title, all selected).
    pub(super) fn start_rename(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.approval_open() {
            return;
        }
        let Some(target) = TabRef::of(&self.model, index) else { return };
        self.menu = None;
        if self.palette.is_some() {
            self.close_palette(window, cx);
        }
        // Another tab's field keeps what was typed.
        self.commit_rename(false, window, cx);
        self.unpark();
        let name = self.model.tabs()[index].name.clone();
        let auto = name.is_none().then(|| self.tab_title(index, cx));
        let edit = LineEdit::new(name.as_deref().or(auto.as_deref()).unwrap_or_default());
        let focus = cx.focus_handle();
        // Focus moving anywhere else (the palette, a dialog) keeps the typed
        // name, like a text field does; clicks are handled by the field's
        // `on_mouse_down_out`.
        // GPUI also reports a blur when the window goes inactive (Cmd-Tab)
        // while the field keeps focus: then it stays open with the keyboard
        // for when the user comes back.
        let held = focus.clone();
        let blur = cx.on_blur(&focus, window, move |this, window, cx| {
            if !held.is_focused(window) {
                this.commit_rename(false, window, cx);
            }
        });
        window.focus(&focus);
        self.rename = Some(RenameUi { target, edit, auto, focus, _blur: blur });
        cx.notify();
    }

    /// Save the field's text and close it. `refocus` gives the keyboard back
    /// to the active pane (Enter); a blur leaves focus where it went.
    pub(super) fn commit_rename(
        &mut self,
        refocus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(r) = self.drop_rename(refocus, window) else { return };
        commit_name(&mut self.model, &r.target, r.edit.text(), r.auto.as_deref());
        if refocus {
            self.after_change(window, cx);
        } else {
            self.update_title(window, cx);
            crate::app_state::request_save(cx);
            cx.notify();
        }
    }

    fn cancel_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.drop_rename(true, window).is_some() {
            self.focus_active_pane(window, cx);
            cx.notify();
        }
    }

    /// Paste into the name field (first line only). False when it is not
    /// open and focused.
    pub(super) fn paste_into_rename(
        &mut self,
        text: &str,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(r) = &mut self.rename else { return false };
        if !r.focus.is_focused(window) {
            return false;
        }
        r.edit.insert(text.lines().next().unwrap_or_default());
        cx.notify();
        true
    }

    fn rename_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(r) = &mut self.rename else { return };
        let ks = &ev.keystroke;
        let m = ks.modifiers;
        // Cmd on macOS, Ctrl elsewhere, as text fields do.
        let word = m.platform || m.control;
        let edit = match ks.key.as_str() {
            "enter" => {
                cx.stop_propagation();
                self.commit_rename(true, window, cx);
                return;
            }
            "escape" => {
                cx.stop_propagation();
                self.cancel_rename(window, cx);
                return;
            }
            "backspace" => Some(Edit::Backspace),
            "delete" => Some(Edit::Delete),
            "left" if m.platform => Some(Edit::Home),
            "right" if m.platform => Some(Edit::End),
            "left" => Some(Edit::Left),
            "right" => Some(Edit::Right),
            "home" | "up" => Some(Edit::Home),
            "end" | "down" => Some(Edit::End),
            "a" if word => Some(Edit::SelectAll),
            _ if word => return,
            _ => None,
        };
        match edit {
            Some(e) => r.edit.apply(e),
            None => match ks.key_char.as_deref() {
                Some(text) => r.edit.insert(text),
                None => return,
            },
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// The name field drawn in place of tab `index`'s title.
    pub(super) fn render_rename_field(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let ui = &theme.ui;
        let Some(r) = &self.rename else { return div().into_any_element() };
        let caret = || div().w(px(1.5)).h(px(14.0)).bg(to_hsla(ui.text));
        let content = if r.edit.all_selected() {
            div().child(
                div()
                    .rounded_sm()
                    .bg(to_hsla(ui.accent))
                    .text_color(to_hsla(ui.accent_text))
                    .child(r.edit.text().to_owned()),
            )
        } else if r.edit.text().is_empty() {
            // Empty clears the name: show the title it would go back to.
            let hint: SharedString =
                r.auto.clone().unwrap_or_else(|| "Automatic title".into()).into();
            div().child(caret()).child(div().text_color(to_hsla(ui.text_muted)).child(hint))
        } else {
            let (before, after) = r.edit.split();
            div().child(before.to_owned()).child(caret()).child(after.to_owned())
        };
        div()
            .id("tab-rename")
            .key_context("TabRename")
            .track_focus(&r.focus)
            .on_key_down(cx.listener(Self::rename_key))
            // Clicks inside the field stay there: no tab switch, no drag, and
            // the window root must not take focus.
            .on_any_mouse_down(|_, window, cx| {
                window.prevent_default();
                cx.stop_propagation();
            })
            // A click anywhere else saves the name before it lands and gives
            // the keyboard to the active pane (the click may then move it).
            // Not left to the blur: GPUI reports none while the window is
            // inactive.
            .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                this.commit_rename(true, window, cx);
            }))
            .flex_1()
            .min_w(px(0.0))
            .overflow_hidden()
            .whitespace_nowrap()
            .px_1()
            .rounded_sm()
            .border_1()
            .border_color(to_hsla(ui.border_focused))
            .bg(to_hsla(ui.surface_sunken))
            .text_color(to_hsla(ui.text))
            .child(content.flex().flex_row().items_center())
            .into_any_element()
    }

    // -------------------------------------------------------------- menus

    /// Right-click on tab `index`.
    pub(super) fn open_tab_menu(
        &mut self,
        index: usize,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = TabRef::of(&self.model, index) else { return };
        let entries = context_menu::tab_entries(index, self.model.tabs().len());
        self.open_menu(MenuTarget::Tab(target), entries, at, window, cx);
    }

    /// Right-click in pane `id` (which the pane has already decided opens a
    /// menu rather than going to its program).
    pub(super) fn open_pane_menu(
        &mut self,
        id: PaneId,
        at: Point<Pixels>,
        has_selection: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.model.tab_of(id) != Some(self.model.active_index()) {
            return;
        }
        // The pane under the pointer becomes the focused pane, so Split and
        // Close act on it; the menu holds the keyboard until it closes.
        if self.model.focused() != Some(id) && self.model.focus_pane(id) {
            self.refresh_chrome(window, cx);
            self.update_title(window, cx);
        }
        let entries =
            context_menu::pane_entries(has_selection, self.model.split_refusal().is_none());
        self.open_menu(MenuTarget::Pane(id), entries, at, window, cx);
    }

    fn open_menu(
        &mut self,
        target: MenuTarget,
        entries: Vec<Entry>,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.approval_open() {
            return;
        }
        if self.palette.is_some() {
            self.close_palette(window, cx);
        }
        self.commit_rename(false, window, cx);
        self.unpark();
        let focus = cx.focus_handle();
        // Focus moving elsewhere closes the menu. GPUI also reports a blur
        // when the window goes inactive while the menu keeps focus: it closes
        // then too, as native menus do, and hands the keyboard to the pane,
        // or the window would come back focused on a handle that no longer
        // draws.
        let held = focus.clone();
        let blur = cx.on_blur(&focus, window, move |this, window, cx| {
            let deactivated = held.is_focused(window);
            this.close_menu(deactivated, window, cx);
        });
        window.focus(&focus);
        self.menu = Some(MenuUi {
            menu: Menu::new(entries),
            target,
            at: (f32::from(at.x), f32::from(at.y)),
            focus,
            _blur: blur,
        });
        cx.notify();
    }

    /// Close the menu. `refocus` gives the keyboard back to the active pane.
    pub(super) fn close_menu(
        &mut self,
        refocus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.menu.take().is_some() {
            if refocus {
                self.focus_active_pane(window, cx);
            }
            cx.notify();
        }
    }

    fn menu_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(m) = &mut self.menu else { return };
        match ev.keystroke.key.as_str() {
            "up" => m.menu.step(false),
            "down" | "tab" => m.menu.step(true),
            "enter" | "space" => {
                if let Some(item) = m.menu.chosen() {
                    self.run_menu_item(item, window, cx);
                }
            }
            "escape" => self.close_menu(true, window, cx),
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// Run a menu item on the menu's target. Each item takes the same path as
    /// its keyboard action, so the same confirmations apply.
    fn run_menu_item(&mut self, item: Item, window: &mut Window, cx: &mut Context<Self>) {
        let Some(m) = self.menu.take() else { return };
        self.focus_active_pane(window, cx);
        match m.target {
            MenuTarget::Tab(t) => {
                let Some(i) = t.index(&self.model) else { return };
                match item {
                    Item::RenameTab => self.start_rename(i, window, cx),
                    Item::CloseTab => self.close_tab(i, window, cx),
                    Item::CloseOtherTabs => {
                        let ids = (0..self.model.tabs().len())
                            .filter(|j| *j != i)
                            .flat_map(|j| self.model.tabs()[j].root.panes())
                            .collect();
                        self.close_panes_confirmed(ids, "other tabs", Some(t), window, cx);
                    }
                    Item::MoveTabLeft | Item::MoveTabRight => {
                        let to =
                            if item == Item::MoveTabLeft { i.checked_sub(1) } else { Some(i + 1) };
                        if let Some(to) = to
                            && self.model.move_tab(i, to)
                        {
                            self.after_change(window, cx);
                        }
                    }
                    Item::NewTab => self.run(Action::NewTab, window, cx),
                    _ => {}
                }
            }
            MenuTarget::Pane(id) => {
                if !self.model.focus_pane(id) {
                    return;
                }
                self.after_change(window, cx);
                match item {
                    Item::Copy | Item::Paste => {
                        // The pane's own copy and paste (paste asks before
                        // multi-line text); it has focus again, so dispatch
                        // once this update is over.
                        let a = if item == Item::Copy { Action::Copy } else { Action::Paste };
                        if let Some(action) = keybind::gpui_action(a) {
                            window.defer(cx, move |window, cx| window.dispatch_action(action, cx));
                        }
                    }
                    Item::SelectAll => {
                        if let Some(p) = self.panes.get(&id) {
                            p.view.update(cx, |v, cx| {
                                v.select_all();
                                cx.notify();
                            });
                        }
                    }
                    Item::SplitRight => self.run(Action::SplitRight, window, cx),
                    Item::SplitDown => self.run(Action::SplitDown, window, cx),
                    Item::ClosePane => self.run(Action::ClosePane, window, cx),
                    _ => {}
                }
            }
        }
        cx.notify();
    }

    /// The open menu, if any, drawn over everything at the pointer and kept
    /// inside the window.
    pub(super) fn render_menu(
        &self,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let m = self.menu.as_ref()?;
        let ui = &theme.ui;
        let keymap = &self.settings(cx).keymap;
        let vp = window.viewport_size();
        // The 1px border on each side.
        let h = context_menu::height(&m.menu.entries) + 2.0;
        let (x, y) = context_menu::place(
            m.at.0,
            m.at.1,
            MENU_W,
            h,
            f32::from(vp.width),
            f32::from(vp.height),
        );
        let mut list = div()
            .id("context-menu")
            .key_context("ContextMenu")
            .track_focus(&m.focus)
            .on_key_down(cx.listener(Self::menu_key))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close_menu(true, window, cx)))
            .on_any_mouse_down(|_, window, cx| {
                window.prevent_default();
                cx.stop_propagation();
            })
            .occlude()
            .absolute()
            .left(px(x))
            .top(px(y))
            .w(px(MENU_W))
            .py(px(PAD_Y))
            .flex()
            .flex_col()
            .rounded_md()
            .border_1()
            .border_color(to_hsla(ui.border))
            .bg(to_hsla(ui.surface_raised))
            .shadow_lg()
            .text_sm();
        for (i, e) in m.menu.entries.iter().enumerate() {
            if e.separator {
                list = list.child(
                    div()
                        .h(px(SEP_H))
                        .flex()
                        .items_center()
                        .px_2()
                        .child(div().h(px(1.0)).w_full().bg(to_hsla(ui.border))),
                );
            }
            let selected = m.menu.selected() == Some(i);
            let shortcut = e
                .item
                .action()
                .and_then(|a| keymap.chords_for(a).first().map(keybind::label))
                .unwrap_or_default();
            let (bg, fg, sub) = if selected {
                (to_hsla(ui.accent), to_hsla(ui.accent_text), to_hsla(ui.accent_text))
            } else if e.enabled {
                (rgba(ui.surface_raised, 0.0), to_hsla(ui.text), to_hsla(ui.text_muted))
            } else {
                let muted = rgba(ui.text_muted, 0.55);
                (rgba(ui.surface_raised, 0.0), muted, muted)
            };
            list = list.child(
                div()
                    .id(("menu-row", i))
                    .h(px(ROW_H))
                    .mx_1()
                    .px_2()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .rounded_sm()
                    .bg(bg)
                    .text_color(fg)
                    .child(e.item.label())
                    .child(div().text_xs().text_color(sub).child(shortcut))
                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        if *hovered && let Some(m) = &mut this.menu {
                            m.menu.hover(i);
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if let Some(item) = this.menu.as_ref().and_then(|m| m.menu.clicked(i)) {
                            this.run_menu_item(item, window, cx);
                        }
                    })),
            );
        }
        Some(list.into_any_element())
    }

    /// A mouse-down on a tab: left selects it (a double click renames it),
    /// right opens its menu.
    pub(super) fn tab_mouse_down(
        &mut self,
        index: usize,
        ev: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The window root tracks focus: without this the click bubbles up to
        // it and takes the keyboard from the pane or field just focused.
        window.prevent_default();
        cx.stop_propagation();
        match ev.button {
            MouseButton::Left => {
                // An open name field has already saved (its click-outside).
                if self.model.select_tab(index) {
                    self.after_change(window, cx);
                    if ev.click_count >= 2 {
                        self.start_rename(index, window, cx);
                    }
                }
            }
            MouseButton::Right => self.open_tab_menu(index, ev.position, window, cx),
            _ => {}
        }
    }

    /// A short description of the open field and menu, for `dump`.
    #[cfg(feature = "debug-script")]
    pub(super) fn overlay_dump(&self, window: &Window) -> String {
        let rename = self.rename.as_ref().map(|r| {
            (r.target.index(&self.model), r.edit.text().to_owned(), r.focus.is_focused(window))
        });
        let menu = self.menu.as_ref().map(|m| {
            let items: Vec<String> = m
                .menu
                .entries
                .iter()
                .map(|e| format!("{}{}", e.item.label(), if e.enabled { "" } else { " (off)" }))
                .collect();
            (items, m.focus.is_focused(window))
        });
        format!(
            "rename (tab, text, focused) {rename:?}; menu (items, focused) {menu:?}; parked {}; window active {}",
            self.parked,
            window.is_window_active()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(ws: &Workspace) -> Vec<Option<&str>> {
        ws.tabs().iter().map(|t| t.name.as_deref()).collect()
    }

    #[test]
    fn a_rename_lands_on_its_tab_after_an_earlier_tab_closes() {
        // Tabs A, B, C, D; rename opens on C; A closes; the name is saved.
        let (mut ws, a) = Workspace::new();
        for _ in 0..3 {
            ws.new_tab();
        }
        let c = TabRef::of(&ws, 2).unwrap();
        ws.close_pane(a);
        assert!(commit_name(&mut ws, &c, "Build", None));
        assert_eq!(names(&ws), [None, Some("Build"), None], "the name is on C, now second");
    }

    #[test]
    fn close_other_tabs_does_nothing_once_the_kept_tab_is_gone() {
        let (mut ws, _) = Workspace::new();
        let b = ws.new_tab();
        let keep = TabRef::of(&ws, 1).unwrap();
        assert!(still_kept(None, &ws), "plain closes always go ahead");
        assert!(still_kept(Some(&keep), &ws));
        ws.close_pane(b);
        assert!(!still_kept(Some(&keep), &ws), "the kept tab exited while the dialog was up");
    }

    #[test]
    fn a_tab_ref_follows_moves_and_ends_with_its_tab() {
        let (mut ws, _) = Workspace::new();
        ws.new_tab();
        let c = ws.new_tab();
        let r = TabRef::of(&ws, 2).unwrap();
        assert!(ws.move_tab(2, 0));
        assert_eq!(r.index(&ws), Some(0), "moved to the front");
        ws.close_pane(c);
        assert_eq!(r.index(&ws), None, "closed");
        assert!(!commit_name(&mut ws, &r, "Gone", None));
        assert_eq!(names(&ws), [None, None]);
    }

    #[test]
    fn a_refreshed_tab_ref_survives_its_first_pane_closing() {
        // Split the tab, refresh, close the original pane: still the tab.
        let (mut ws, first) = Workspace::new();
        ws.new_tab();
        assert!(ws.focus_pane(first));
        let mut r = TabRef::of(&ws, 0).unwrap();
        ws.split(halyard_workspace::Axis::Horizontal).unwrap();
        assert!(r.refresh(&ws));
        ws.close_pane(first);
        assert!(r.refresh(&ws));
        assert_eq!(r.index(&ws), Some(0));
    }

    #[test]
    fn empty_clears_and_an_unchanged_automatic_title_stays_automatic() {
        let (mut ws, _) = Workspace::new();
        let t = TabRef::of(&ws, 0).unwrap();
        assert!(commit_name(&mut ws, &t, "zsh", Some("zsh")));
        assert_eq!(names(&ws), [None], "confirming the automatic title does not pin it");
        assert!(commit_name(&mut ws, &t, "logs", Some("zsh")));
        assert_eq!(names(&ws), [Some("logs")]);
        assert!(commit_name(&mut ws, &t, "  ", None));
        assert_eq!(names(&ws), [None], "an empty name clears it");
    }
}
