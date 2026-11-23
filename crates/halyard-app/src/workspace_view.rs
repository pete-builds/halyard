//! A window: tab bar, split panes, banner and command palette.
//!
//! The layout lives in `halyard_workspace::Workspace` (pure, tested there);
//! this view draws it, owns one [`TerminalView`] per pane, and turns keymap
//! actions, clicks and drags into model operations.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use gpui::{
    AnyElement, App, BorrowAppContext, Context, Entity, FocusHandle, Focusable, Hsla, KeyDownEvent,
    MouseButton, MouseDownEvent, Render, SharedString, Subscription, Task, Window,
    WindowAppearance, div, prelude::*, px, size,
};
use halyard_config::{Action, Theme};
use halyard_term::Rgb;
use halyard_workspace::{
    Axis, Closed, Direction, MinSize, PaneId, Rect, Restored, SavedWindow, Workspace,
};

mod attention;
mod dashboard;
mod session_api;
mod tab_ui;

pub use session_api::Reply;

use halyard_agents::AttentionState;

use crate::app_state::{self, AppState};
use crate::attention_ui;
use crate::keybind::{self, *};
use crate::palette::{self, Command, Mode};
use crate::settings::{self, Settings};
use crate::view::{PaneChrome, PaneEvent, PaneSpawn, PaneStyle, TerminalView, to_hsla};

/// Height of the tab bar.
pub const TAB_BAR_H: f32 = 32.0;
/// Height of the notice banner.
pub const BANNER_H: f32 = 26.0;
/// Space between panes (the visible divider).
pub const GAP: f32 = 1.0;
/// Width of a divider's grab area, centered on the divider.
pub const DIVIDER_HIT: f32 = 7.0;
/// Smallest pane.
pub const MIN_PANE: MinSize = MinSize { w: 80.0, h: 48.0 };
/// How far a keyboard resize moves a divider.
pub const RESIZE_STEP: f32 = 40.0;
/// Cursor blink half-period.
pub const BLINK: Duration = Duration::from_millis(530);

fn rgba(c: Rgb, alpha: f32) -> Hsla {
    let mut h = to_hsla(c);
    h.a = alpha;
    h
}

struct Pane {
    view: Entity<TerminalView>,
    _events: Subscription,
}

/// A tab being dragged in the tab bar.
#[derive(Clone, Debug)]
struct DraggedTab {
    tab: tab_ui::TabRef,
    label: SharedString,
    bg: Hsla,
    fg: Hsla,
    border: Hsla,
}

impl Render for DraggedTab {
    fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_3()
            .py_1()
            .rounded_sm()
            .border_1()
            .border_color(self.border)
            .bg(self.bg)
            .text_color(self.fg)
            .text_sm()
            .child(self.label.clone())
    }
}

/// A divider being dragged.
#[derive(Clone, Debug)]
struct DraggedDivider {
    path: Vec<bool>,
}

impl Render for DraggedDivider {
    fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

struct PaletteUi {
    model: palette::Model,
    commands: Vec<Command>,
    focus: FocusHandle,
    /// Dashboard rows, in [`Mode::Agents`] (`dashboard.rs`).
    agents: Vec<crate::dashboard_ui::Row>,
    /// The grants listed in [`Mode::PairedTools`] and where to revoke them.
    paired: Option<(crate::session_api::PairedTools, halyard_ipc::GrantsHandle)>,
    /// The grants are still being read.
    paired_loading: bool,
}

/// One window.
pub struct WorkspaceView {
    model: Workspace,
    panes: HashMap<PaneId, Pane>,
    focus: FocusHandle,
    palette: Option<PaletteUi>,
    /// A theme shown while moving through the theme picker.
    preview_theme: Option<String>,
    /// Font size steps from the font size actions, in points.
    font_delta: f32,
    /// Notices from this window (font fallback, restore, spawn failures).
    local_notices: Vec<String>,
    banner_hidden: bool,
    applied_generation: Option<u64>,
    dark: bool,
    blink_on: bool,
    fatal: Option<String>,
    /// Desktop notification decisions (`attention.rs`).
    notifier: attention_ui::Notifier,
    /// Each pane's attention state when last seen, to notice transitions.
    attention_states: HashMap<PaneId, AttentionState>,
    /// The open session API approval dialog (`session_api.rs`).
    approval: Option<session_api::ApprovalUi>,
    /// Numbers dialogs, so a timeout closes only its own.
    next_approval: u64,
    /// The tab name being edited in place (`tab_ui.rs`).
    rename: Option<tab_ui::RenameUi>,
    /// The open right-click menu (`tab_ui.rs`).
    menu: Option<tab_ui::MenuUi>,
    /// The keyboard waits on the window root after a name field closed
    /// under the user (`tab_ui::drop_rename`).
    parked: bool,
    _blink: Task<()>,
    _agent_poll: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl std::fmt::Debug for WorkspaceView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceView").field("model", &self.model).finish_non_exhaustive()
    }
}

/// Split the focused pane, or say why the layout limits refuse it.
fn split_or_notice(model: &mut Workspace, axis: Axis) -> Result<PaneId, String> {
    if let Some(reason) = model.split_refusal() {
        return Err(format!("split not made: {reason}"));
    }
    model.split(axis).ok_or_else(|| "split not made".to_owned())
}

fn is_dark(a: WindowAppearance) -> bool {
    matches!(a, WindowAppearance::Dark | WindowAppearance::VibrantDark)
}

impl WorkspaceView {
    /// Build a window from a restored (or new) layout, starting a shell in
    /// each pane.
    pub fn new(restored: Restored, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut subs = vec![
            cx.observe_global_in::<AppState>(window, |this, window, cx| {
                this.settings_changed(window, cx);
            }),
            cx.observe_window_appearance(window, |this, window, cx| {
                this.dark = is_dark(window.appearance());
                this.apply_settings(window, cx);
            }),
            cx.observe_window_activation(window, |this, window, cx| {
                // Coming back to a window whose focused element went away
                // while it was inactive: give the keyboard to the pane.
                if window.is_window_active() && window.focused(cx).is_none() {
                    this.focus_active_pane(window, cx);
                }
                this.approval_window_activated(window, cx);
                this.refresh_chrome(window, cx);
                for p in this.panes.values() {
                    p.view.update(cx, |_, cx| cx.notify());
                }
            }),
        ];
        subs.push(cx.on_release(|this, cx| {
            // Panes drop with the view; record the window's absence.
            this.panes.clear();
            app_state::request_save(cx);
        }));
        let blink = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(BLINK).await;
                if this.update_in(cx, |this, window, cx| this.blink(window, cx)).is_err() {
                    break;
                }
            }
        });
        let mut view = Self {
            model: restored.workspace,
            panes: HashMap::new(),
            focus: cx.focus_handle(),
            palette: None,
            preview_theme: None,
            font_delta: 0.0,
            local_notices: std::mem::take(&mut cx.global_mut::<AppState>().restore_warnings),
            banner_hidden: false,
            applied_generation: None,
            dark: is_dark(window.appearance()),
            blink_on: true,
            fatal: None,
            notifier: attention_ui::Notifier::new(),
            attention_states: HashMap::new(),
            approval: None,
            next_approval: 0,
            rename: None,
            menu: None,
            parked: false,
            _blink: blink,
            _agent_poll: Self::start_agent_poll(window, cx),
            _subscriptions: subs,
        };
        for (id, cwd) in restored.cwds {
            view.spawn_pane(id, None, cwd, window, cx);
        }
        if view.model.is_empty() {
            view.fatal = view.local_notices.last().cloned();
        }
        view.applied_generation = Some(cx.global::<AppState>().settings.generation);
        view.refresh_chrome(window, cx);
        view.focus_active_pane(window, cx);
        view.update_title(window, cx);
        // The close button asks first when a command is running anywhere in
        // the window, like `close_pane` does for one pane.
        let this = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            this.update(cx, |view, cx| view.should_close(window, cx)).unwrap_or(true)
        });
        #[cfg(feature = "debug-script")]
        if !cx.global::<AppState>().script_started {
            cx.global_mut::<AppState>().script_started = true;
            crate::debug_script::start(window, cx);
        }
        view
    }

    /// Give keyboard focus to the active pane.
    pub fn run_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_active_pane(window, cx);
    }

    /// The saveable layout with each pane's working directory.
    pub fn saved(&self, cx: &App) -> SavedWindow {
        let cwd = |id: PaneId| self.panes.get(&id).and_then(|p| p.view.read(cx).cwd());
        self.model.to_saved(&cwd)
    }

    fn settings<'a>(&self, cx: &'a App) -> &'a Settings {
        &cx.global::<AppState>().settings
    }

    fn theme(&self, cx: &App) -> Theme {
        let s = self.settings(cx);
        if let Some(t) = self.preview_theme.as_deref().and_then(|n| s.registry.get(n)) {
            return t.clone();
        }
        s.theme(settings::appearance(self.dark)).0.clone()
    }

    fn pane_style(&mut self, window: &Window, cx: &App) -> PaneStyle {
        let s = self.settings(cx);
        let font = s.font();
        let installed = window.text_system().all_font_names();
        let (family, notice) = crate::view::resolve_font_family(&font.family, &installed);
        if let Some(n) = notice
            && !self.local_notices.contains(&n)
        {
            self.local_notices.push(n);
            self.banner_hidden = false;
        }
        let t = &s.config.terminal;
        let w = &s.config.window;
        PaneStyle {
            family,
            font_size: settings::step_font_size(font.size, self.font_delta),
            line_height: font.line_height,
            padding_x: w.padding_x,
            padding_y: w.padding_y,
            cursor: settings::cursor_shape(t.cursor_style),
            blink: t.cursor_blink,
            cursor_text: self.theme(cx).terminal.cursor_text,
            copy_on_select: t.copy_on_select,
            confirm_multiline_paste: t.confirm_multiline_paste,
        }
    }

    /// The area panes are laid out in, relative to the content origin.
    fn content_rect(&self, window: &Window, cx: &App) -> Rect {
        let vp = window.viewport_size();
        let h = f32::from(vp.height) - TAB_BAR_H - self.banner_height(cx);
        Rect::new(0.0, 0.0, f32::from(vp.width), h.max(1.0))
    }

    fn notices(&self, cx: &App) -> Vec<String> {
        let mut v = self.settings(cx).notices.clone();
        v.extend(self.local_notices.iter().cloned());
        v
    }

    fn banner_height(&self, cx: &App) -> f32 {
        if !self.banner_hidden && !self.notices(cx).is_empty() { BANNER_H } else { 0.0 }
    }

    fn spawn_pane(
        &mut self,
        id: PaneId,
        inherited: Option<PathBuf>,
        restored: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let style = self.pane_style(window, cx);
        let theme = self.theme(cx);
        let s = self.settings(cx);
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let spawn = PaneSpawn {
            command: settings::shell_command(&s.config),
            cwd: settings::start_directory(
                &s.config.terminal.working_directory,
                inherited.as_deref(),
                restored.as_deref(),
                home.as_deref(),
            ),
            scrollback: usize::try_from(s.config.terminal.scrollback_lines).unwrap_or(usize::MAX),
            palette: settings::engine_palette(&theme.palette()),
            api: cx.global::<AppState>().session_api.clone(),
        };
        let r = self.content_rect(window, cx);
        let area = size(px(r.w), px(r.h));
        match TerminalView::prepare(window, style, spawn, area) {
            Ok(prepared) => {
                let view = cx.new(|cx| TerminalView::new(prepared, window, cx));
                let sub = cx.subscribe_in(&view, window, move |this, _, ev, window, cx| {
                    this.pane_event(id, ev, window, cx);
                });
                self.panes.insert(id, Pane { view, _events: sub });
                true
            }
            Err(e) => {
                self.local_notices.push(format!("could not start a shell: {e}"));
                self.banner_hidden = false;
                self.model.close_pane(id);
                false
            }
        }
    }

    fn pane_event(
        &mut self,
        id: PaneId,
        ev: &PaneEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match ev {
            PaneEvent::Exited | PaneEvent::CloseRequested => self.close_pane(id, window, cx),
            PaneEvent::TitleChanged => {
                self.update_title(window, cx);
                cx.notify();
            }
            PaneEvent::Focused => {
                if self.model.focused() != Some(id) && self.model.focus_pane(id) {
                    self.after_change(window, cx);
                }
            }
            PaneEvent::AttentionChanged => self.attention_changed(id, window, cx),
            PaneEvent::ContextMenu { position, has_selection } => {
                self.open_pane_menu(id, *position, *has_selection, window, cx);
            }
        }
    }

    fn settings_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let generation = self.settings(cx).generation;
        if self.applied_generation == Some(generation) {
            return;
        }
        self.applied_generation = Some(generation);
        self.banner_hidden = false;
        self.local_notices.retain(|n| !n.starts_with("config: key `font.family`"));
        self.apply_settings(window, cx);
    }

    /// Push the current style and colors into every pane.
    fn apply_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let style = self.pane_style(window, cx);
        let palette = settings::engine_palette(&self.theme(cx).palette());
        for p in self.panes.values() {
            let style = style.clone();
            let palette = palette.clone();
            p.view.update(cx, |v, cx| {
                v.set_style(style, window, cx);
                v.set_palette(palette);
            });
        }
        self.refresh_chrome(window, cx);
        cx.notify();
    }

    /// Dim inactive panes and outline the focused one (when there are
    /// several), from the theme's tokens; badge and outline panes with an
    /// agent (`attention.rs`).
    fn refresh_chrome(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_seen(window, cx);
        let theme = self.theme(cx);
        let Some(tab) = self.model.tabs().get(self.model.active_index()) else { return };
        let multi = tab.root.len() > 1 && !tab.zoomed;
        let focused = tab.focused;
        let active = window.is_window_active();
        let mut updates = Vec::with_capacity(self.panes.len());
        for id in self.panes.keys() {
            let mut chrome = if *id == focused {
                PaneChrome {
                    border: (multi && active).then(|| to_hsla(theme.ui.border_focused)),
                    ..PaneChrome::default()
                }
            } else {
                PaneChrome {
                    dim: multi.then(|| rgba(theme.terminal.background, 0.35)),
                    ..PaneChrome::default()
                }
            };
            chrome.failure = Some(to_hsla(theme.ui.danger));
            if let Some((badge, outline)) = self.pane_marks(*id, &theme, cx) {
                chrome.badge = Some(badge);
                chrome.border = outline.or(chrome.border);
            }
            updates.push((*id, chrome));
        }
        for (id, chrome) in updates {
            if let Some(p) = self.panes.get(&id) {
                p.view.update(cx, |v, cx| v.set_chrome(chrome, cx));
            }
        }
    }

    fn focus_active_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.approval_open() {
            return;
        }
        if self.palette.is_some() {
            return;
        }
        if let Some(f) = self.overlay_focus() {
            window.focus(&f);
            return;
        }
        // Parked after a name field closed under the user: stay on the root
        // until the user picks something, or focus moves off it.
        if self.parked {
            if self.focus.is_focused(window) {
                return;
            }
            self.parked = false;
        }
        match self.model.focused().and_then(|id| self.panes.get(&id)) {
            Some(p) => window.focus(&p.view.focus_handle(cx)),
            None => window.focus(&self.focus),
        }
    }

    fn after_change(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.refresh_targets(window);
        self.refresh_chrome(window, cx);
        self.focus_active_pane(window, cx);
        self.update_title(window, cx);
        app_state::request_save(cx);
        cx.notify();
    }

    fn tab_title(&self, index: usize, cx: &App) -> String {
        let Some(tab) = self.model.tabs().get(index) else { return String::new() };
        if let Some(n) = &tab.name {
            return n.clone();
        }
        let Some(p) = self.panes.get(&tab.focused) else { return "Shell".into() };
        let v = p.view.read(cx);
        if !v.title().trim().is_empty() {
            return v.title().trim().to_owned();
        }
        v.cwd()
            .and_then(|c| {
                std::path::Path::new(&c).file_name().map(|f| f.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "Shell".into())
    }

    fn update_title(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.model.is_empty() {
            return;
        }
        let t = self.tab_title(self.model.active_index(), cx);
        window.set_window_title(if t.is_empty() { "Halyard" } else { &t });
    }

    fn close_pane(&mut self, id: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        self.forget_attention(id, cx);
        let outcome = self.model.close_pane(id);
        // Dropping the view drops its session, which ends the program.
        self.panes.remove(&id);
        match outcome {
            Closed::Window => {
                app_state::request_save(cx);
                window.remove_window();
            }
            Closed::NotFound => {}
            Closed::Pane | Closed::Tab => self.after_change(window, cx),
        }
    }

    /// Whether closing these panes would end a running command (and the
    /// config asks to confirm that).
    fn any_busy(&self, ids: &[PaneId], cx: &App) -> bool {
        let setting = self.settings(cx).config.terminal.confirm_close_running;
        ids.iter().any(|id| {
            self.panes.get(id).is_some_and(|p| p.view.read(cx).needs_close_confirmation(setting))
        })
    }

    /// Whether any pane of this window is running a command.
    pub fn busy(&self, cx: &App) -> bool {
        self.any_busy(&self.model.all_panes(), cx)
    }

    /// Close `ids`, asking first if a command is running in any of them.
    /// `what` names them: "pane", "tab" or "other tabs". With `keep` (Close
    /// Other Tabs), nothing closes if that tab has gone by the time the user
    /// answers, which would otherwise close the whole window.
    fn close_panes_confirmed(
        &mut self,
        ids: Vec<PaneId>,
        what: &'static str,
        keep: Option<tab_ui::TabRef>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.any_busy(&ids, cx) {
            for id in ids {
                self.close_pane(id, window, cx);
            }
            return;
        }
        let (title, detail) = if what.ends_with('s') {
            (
                format!("Close the {what}?"),
                "A command is still running in one of them, and closing will end it.",
            )
        } else {
            (
                format!("Close this {what}?"),
                "A command is still running in it, and closing will end it.",
            )
        };
        let answer = window.prompt(
            gpui::PromptLevel::Warning,
            &title,
            Some(detail),
            &["Close", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await == Ok(0) {
                this.update_in(cx, |this, window, cx| {
                    if !tab_ui::still_kept(keep.as_ref(), &this.model) {
                        return;
                    }
                    for id in ids {
                        this.close_pane(id, window, cx);
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    /// Close the focused pane, asking first if a command is running in it.
    fn close_focused(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.model.focused() else { return };
        self.close_panes_confirmed(vec![id], "pane", None, window, cx);
    }

    /// The close button: close at once when idle, else ask.
    fn should_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.busy(cx) {
            return true;
        }
        let answer = window.prompt(
            gpui::PromptLevel::Warning,
            "Close this window?",
            Some("A command is still running in it, and closing will end it."),
            &["Close", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |_, cx| {
            if answer.await == Ok(0) {
                cx.update(|window, _| window.remove_window()).ok();
            }
        })
        .detach();
        false
    }

    fn inherited_cwd(&self, cx: &App) -> Option<PathBuf> {
        let id = self.model.focused()?;
        self.panes.get(&id)?.view.read(cx).cwd().map(PathBuf::from)
    }

    fn blink(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.model.focused() else { return };
        let Some(p) = self.panes.get(&id) else { return };
        let wants = p.view.read(cx).wants_blink(window) && self.palette.is_none();
        self.blink_on = if wants { !self.blink_on } else { true };
        let on = self.blink_on;
        p.view.update(cx, |v, cx| {
            v.set_blink(on, cx);
        });
    }

    /// Run a keymap action.
    pub fn run(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        // An approval dialog is modal: no window action (close, new tab, palette,
        // zoom) runs under it. Quitting the app still works and denies it.
        if self.approval_open() && action != Action::Quit {
            return;
        }
        // An action (a menu item or a shortcut) closes an open menu, and is
        // the user picking where the keyboard goes.
        self.unpark();
        self.close_menu(true, window, cx);
        let bounds = self.content_rect(window, cx);
        let dir = |d| Some(d);
        let focus_dir = match action {
            Action::FocusLeft => dir(Direction::Left),
            Action::FocusRight => dir(Direction::Right),
            Action::FocusUp => dir(Direction::Up),
            Action::FocusDown => dir(Direction::Down),
            _ => None,
        };
        if let Some(d) = focus_dir {
            if self.model.focus_direction(d, bounds, GAP).is_some() {
                self.after_change(window, cx);
            }
            return;
        }
        let resize_dir = match action {
            Action::ResizePaneLeft => dir(Direction::Left),
            Action::ResizePaneRight => dir(Direction::Right),
            Action::ResizePaneUp => dir(Direction::Up),
            Action::ResizePaneDown => dir(Direction::Down),
            _ => None,
        };
        if let Some(d) = resize_dir {
            if self.model.resize_focused(d, RESIZE_STEP, bounds, GAP, MIN_PANE) {
                self.after_change(window, cx);
            }
            return;
        }
        match action {
            Action::NewTab => {
                let inherited = self.inherited_cwd(cx);
                let id = self.model.new_tab();
                self.spawn_pane(id, inherited, None, window, cx);
                self.after_change(window, cx);
            }
            Action::SplitRight | Action::SplitDown => {
                let axis =
                    if action == Action::SplitRight { Axis::Horizontal } else { Axis::Vertical };
                let inherited = self.inherited_cwd(cx);
                match split_or_notice(&mut self.model, axis) {
                    Ok(id) => {
                        self.spawn_pane(id, inherited, None, window, cx);
                        self.after_change(window, cx);
                    }
                    Err(notice) => {
                        if !self.local_notices.contains(&notice) {
                            self.local_notices.push(notice);
                        }
                        self.banner_hidden = false;
                        cx.notify();
                    }
                }
            }
            Action::ClosePane => self.close_focused(window, cx),
            Action::NextTab => {
                self.model.next_tab();
                self.after_change(window, cx);
            }
            Action::PrevTab => {
                self.model.prev_tab();
                self.after_change(window, cx);
            }
            Action::MoveTabLeft | Action::MoveTabRight => {
                let i = self.model.active_index();
                let n = self.model.tabs().len();
                let to = if action == Action::MoveTabLeft { i.checked_sub(1) } else { Some(i + 1) };
                if let Some(to) = to.filter(|t| *t < n)
                    && self.model.move_tab(i, to)
                {
                    self.after_change(window, cx);
                }
            }
            Action::ZoomPane => {
                if self.model.toggle_zoom() {
                    self.after_change(window, cx);
                }
            }
            Action::IncreaseFontSize | Action::DecreaseFontSize | Action::ResetFontSize => {
                let base = self.settings(cx).font().size;
                self.font_delta = match action {
                    Action::IncreaseFontSize => {
                        settings::step_font_size(base, self.font_delta + 1.0) - base
                    }
                    Action::DecreaseFontSize => {
                        settings::step_font_size(base, self.font_delta - 1.0) - base
                    }
                    _ => 0.0,
                };
                self.apply_settings(window, cx);
            }
            Action::RenameTab => self.start_rename(self.model.active_index(), window, cx),
            Action::CommandPalette => self.open_palette(window, cx),
            Action::ReloadConfig => app_state::reload_config(cx),
            Action::OpenConfig => {
                if let Err(e) = app_state::open_config(cx) {
                    self.local_notices.push(e);
                    self.banner_hidden = false;
                    cx.notify();
                }
            }
            Action::NewWindow => crate::open_window(None, cx),
            // Deferred: this window is mid-update here, so the busy check
            // could not read it.
            Action::Quit => cx.defer(crate::request_quit),
            // Copy and paste belong to the focused pane; these arrive here
            // only when no pane has focus (the palette is open).
            Action::Paste => {
                let text = cx.read_from_clipboard().and_then(|i| i.text());
                if let Some(text) = &text
                    && self.paste_into_rename(text, window, cx)
                {
                    return;
                }
                if let Some(text) = text
                    && let Some(p) = &mut self.palette
                {
                    p.model.type_text(&text);
                    self.palette_moved(window, cx);
                    cx.notify();
                }
            }
            Action::AgentDashboard => self.open_dashboard(window, cx),
            Action::PrevPrompt => self.jump_prompt(crate::blocks_ui::Jump::Previous, cx),
            Action::NextPrompt => self.jump_prompt(crate::blocks_ui::Jump::Next, cx),
            Action::CopyLastOutput => self.copy_last_output(cx),
            Action::Copy | Action::Find => {}
            Action::FocusLeft
            | Action::FocusRight
            | Action::FocusUp
            | Action::FocusDown
            | Action::ResizePaneLeft
            | Action::ResizePaneRight
            | Action::ResizePaneUp
            | Action::ResizePaneDown => {}
        }
    }

    // ---------------------------------------------------------------- palette

    fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let cmds = palette::commands();
        let labels = cmds.iter().map(|(_, t)| (*t).to_owned()).collect();
        let model = palette::Model::new(Mode::Commands, labels, String::new(), None);
        self.show_palette(model, cmds.into_iter().map(|(c, _)| c).collect(), window, cx);
    }

    fn open_themes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let s = self.settings(cx);
        let names: Vec<String> = s.registry.names().into_iter().map(str::to_owned).collect();
        let current = self.theme(cx).name;
        let select = names.iter().position(|n| *n == current);
        let previous = s.theme_override.clone();
        let model = palette::Model::new(Mode::Themes { previous }, names, String::new(), select);
        self.show_palette(model, Vec::new(), window, cx);
    }

    fn show_palette(
        &mut self,
        model: palette::Model,
        commands: Vec<Command>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focus = cx.focus_handle();
        window.focus(&focus);
        self.end_preview_unless(Some(&model.mode), window, cx);
        self.palette = Some(PaletteUi {
            model,
            commands,
            focus,
            agents: Vec::new(),
            paired: None,
            paired_loading: false,
        });
        cx.notify();
    }

    fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette = None;
        self.end_preview_unless(None, window, cx);
        self.focus_active_pane(window, cx);
        cx.notify();
    }

    /// Drop a theme preview unless the palette now showing is the theme
    /// picker, whatever path replaced or closed it.
    fn end_preview_unless(
        &mut self,
        next: Option<&Mode>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !palette::preview_survives(next) && self.preview_theme.take().is_some() {
            self.apply_settings(window, cx);
        }
    }

    /// Live theme preview while moving through the theme picker.
    fn palette_moved(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = &self.palette else { return };
        if !matches!(p.model.mode, Mode::Themes { .. }) {
            return;
        }
        let name = p
            .model
            .selected_entry()
            .and_then(|e| p.model.rows().find(|(i, _, _)| *i == e).map(|(_, l, _)| l.to_owned()));
        if name.is_some() && name != self.preview_theme {
            self.preview_theme = name;
            self.apply_settings(window, cx);
        }
    }

    fn palette_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = &mut self.palette else { return };
        let ks = &ev.keystroke;
        let m = ks.modifiers;
        match ks.key.as_str() {
            "escape" => {
                if matches!(p.model.mode, Mode::Themes { .. }) {
                    self.preview_theme = None;
                    self.apply_settings(window, cx);
                }
                self.close_palette(window, cx);
            }
            "enter" => self.palette_confirm(window, cx),
            "up" => {
                p.model.move_by(-1);
                self.palette_moved(window, cx);
            }
            "down" | "tab" => {
                p.model.move_by(1);
                self.palette_moved(window, cx);
            }
            "backspace" => {
                p.model.backspace();
                self.palette_moved(window, cx);
            }
            _ if !(m.control || m.platform) => {
                if let Some(text) = ks.key_char.as_deref() {
                    p.model.type_text(text);
                    self.palette_moved(window, cx);
                }
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn palette_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = self.palette.take() else { return };
        let picked = p.model.selected_entry();
        match p.model.mode.clone() {
            Mode::Commands => {
                self.focus_active_pane(window, cx);
                match picked.and_then(|i| p.commands.get(i).copied()) {
                    Some(Command::SwitchTheme) => self.open_themes(window, cx),
                    Some(Command::CopyShellIntegration) => self.copy_shell_integration(cx),
                    Some(Command::PairedTools) => self.open_paired_tools(window, cx),
                    Some(Command::Run(a)) if palette::runs_in_pane(a) => {
                        // Copy and paste belong to the pane, which has focus
                        // again; dispatch once this update is over.
                        if let Some(action) = keybind::gpui_action(a) {
                            window.defer(cx, move |window, cx| window.dispatch_action(action, cx));
                        }
                    }
                    Some(Command::Run(a)) => self.run(a, window, cx),
                    None => {}
                }
            }
            Mode::Themes { .. } => {
                let name = picked.and_then(|e| {
                    p.model.rows().find(|(i, _, _)| *i == e).map(|(_, l, _)| l.to_owned())
                });
                self.preview_theme = None;
                if let Some(name) = name {
                    cx.update_global::<AppState, _>(|s, _| {
                        s.settings.theme_override = Some(name);
                        s.settings.generation += 1;
                    });
                }
                self.focus_active_pane(window, cx);
                self.apply_settings(window, cx);
            }
            Mode::Agents => {
                self.focus_active_pane(window, cx);
                if let Some(row) = picked.and_then(|i| p.agents.get(i)) {
                    self.jump_to(row.target, window, cx);
                }
            }
            Mode::PairedTools => {
                self.focus_active_pane(window, cx);
                self.revoke_picked(picked, p.paired, window, cx);
            }
        }
        cx.notify();
    }

    fn palette_click(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(p) = &mut self.palette {
            let target = isize::try_from(row).unwrap_or(0);
            let cur = isize::try_from(p.model.selected_row()).unwrap_or(0);
            p.model.move_by(target - cur);
        }
        self.palette_confirm(window, cx);
    }

    // ---------------------------------------------------------------- drawing

    fn render_tab_bar(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let ui = &theme.ui;
        let active = self.model.active_index();
        let mut bar = div()
            .id("tab-bar")
            .flex()
            .flex_row()
            .items_end()
            .h(px(TAB_BAR_H))
            .w_full()
            .px_1()
            .gap_1()
            .bg(to_hsla(ui.surface_sunken))
            .border_b_1()
            .border_color(to_hsla(ui.border))
            // A click on the bar outside any tab keeps the keyboard in the
            // pane (and ends a rename, as a click elsewhere does).
            .on_any_mouse_down(cx.listener(|this, _, window, cx| {
                window.prevent_default();
                this.commit_rename(false, window, cx);
                this.unpark();
                this.focus_active_pane(window, cx);
            }));
        for i in 0..self.model.tabs().len() {
            let title: SharedString = self.tab_title(i, cx).into();
            let is_active = i == active;
            let (bg, fg) = if is_active {
                (to_hsla(ui.tab_active_bg), to_hsla(ui.text))
            } else {
                (to_hsla(ui.tab_inactive_bg), to_hsla(ui.text_muted))
            };
            let zoomed = self.model.tabs()[i].zoomed;
            let Some(tab_ref) = tab_ui::TabRef::of(&self.model, i) else { continue };
            let drag = DraggedTab {
                tab: tab_ref,
                label: title.clone(),
                bg: to_hsla(ui.surface_raised),
                fg: to_hsla(ui.text),
                border: to_hsla(ui.border_focused),
            };
            let tab = div()
                .id(("tab", i))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .h(px(TAB_BAR_H - 5.0))
                .min_w(px(90.0))
                .max_w(px(220.0))
                .px_3()
                .rounded_t_md()
                .bg(bg)
                .text_color(fg)
                .text_sm()
                .when(is_active, |d| d.border_t_2().border_color(to_hsla(ui.accent)))
                .on_any_mouse_down(cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                    this.tab_mouse_down(i, ev, window, cx);
                }))
                .on_drag(drag, |d: &DraggedTab, _, _, cx| cx.new(|_| d.clone()))
                .drag_over::<DraggedTab>(|s, _, _, _| s.opacity(0.6))
                .on_drop(cx.listener(move |this, d: &DraggedTab, window, cx| {
                    if let Some(from) = d.tab.index(&this.model)
                        && this.model.move_tab(from, i)
                    {
                        this.after_change(window, cx);
                    }
                }))
                .children(self.tab_mark(i, theme, cx))
                .child(if self.renaming(i) {
                    self.render_rename_field(theme, cx)
                } else {
                    div()
                        .flex_1()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .child(title)
                        .into_any_element()
                })
                .when(zoomed, |d| d.child(div().text_color(to_hsla(ui.accent)).child("◉")))
                .child(
                    div()
                        .id(("tab-close", i))
                        .px_1()
                        .rounded_sm()
                        .text_color(to_hsla(ui.text_muted))
                        .hover(|s| s.bg(to_hsla(ui.surface_raised)))
                        .child("×")
                        .on_any_mouse_down(|_, window, cx| {
                            window.prevent_default();
                            cx.stop_propagation();
                        })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.close_tab(i, window, cx);
                        })),
                );
            bar = bar.child(tab);
        }
        bar.child(
            div()
                .id("new-tab")
                .h(px(TAB_BAR_H - 5.0))
                .px_3()
                .flex()
                .items_center()
                .rounded_t_md()
                .text_color(to_hsla(ui.text_muted))
                .hover(|s| s.bg(to_hsla(ui.tab_inactive_bg)))
                .child("+")
                .on_click(cx.listener(|this, _, window, cx| this.run(Action::NewTab, window, cx))),
        )
        .into_any_element()
    }

    /// Close every pane of a tab (the tab's × button).
    fn close_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.model.tabs().get(index) else { return };
        let ids = tab.root.panes();
        self.close_panes_confirmed(ids, "tab", None, window, cx);
    }

    fn render_panes(&self, theme: &Theme, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let bounds = self.content_rect(window, cx);
        let mut area = div()
            .id("panes")
            .relative()
            .w_full()
            .h(px(bounds.h))
            .bg(to_hsla(theme.ui.border))
            // A click in a pane is the user picking where the keyboard goes
            // (capture phase: the pane stops the event before it bubbles).
            .capture_any_mouse_down(cx.listener(|this, _, _, _| this.unpark()))
            .on_drag_move::<DraggedDivider>(cx.listener(
                move |this, ev: &gpui::DragMoveEvent<DraggedDivider>, window, cx| {
                    let path = ev.drag(cx).path.clone();
                    let origin = ev.bounds.origin;
                    let x = f32::from(ev.event.position.x - origin.x);
                    let y = f32::from(ev.event.position.y - origin.y);
                    let r = this.content_rect(window, cx);
                    let axis = this.model.active().root.at(&path).and_then(|n| match n {
                        halyard_workspace::Node::Split { axis, .. } => Some(*axis),
                        halyard_workspace::Node::Pane(_) => None,
                    });
                    let pos = if axis == Some(Axis::Horizontal) { x } else { y };
                    if this.model.drag_divider(&path, pos, r, GAP, MIN_PANE) {
                        app_state::request_save(cx);
                        cx.notify();
                    }
                },
            ));
        let Some(tab) = self.model.tabs().get(self.model.active_index()) else {
            return area.into_any_element();
        };
        for (id, r) in tab.layout(bounds, GAP) {
            if let Some(p) = self.panes.get(&id) {
                area = area.child(
                    div()
                        .absolute()
                        .left(px(r.x))
                        .top(px(r.y))
                        .w(px(r.w))
                        .h(px(r.h))
                        .child(p.view.clone()),
                );
            }
        }
        for (n, d) in tab.dividers(bounds, GAP).into_iter().enumerate() {
            let horizontal = d.axis == Axis::Horizontal;
            let half = (DIVIDER_HIT - GAP) / 2.0;
            let (x, y, w, h) = if horizontal {
                (d.rect.x - half, d.rect.y, DIVIDER_HIT, d.rect.h)
            } else {
                (d.rect.x, d.rect.y - half, d.rect.w, DIVIDER_HIT)
            };
            let hit = div()
                .id(("divider", n))
                .absolute()
                .left(px(x))
                .top(px(y))
                .w(px(w))
                .h(px(h))
                .on_drag(DraggedDivider { path: d.path.clone() }, |d, _, _, cx| {
                    cx.new(|_| d.clone())
                })
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
            area = area.child(if horizontal {
                hit.cursor_col_resize()
            } else {
                hit.cursor_row_resize()
            });
        }
        area.into_any_element()
    }

    fn render_banner(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.banner_hidden {
            return None;
        }
        let notices = self.notices(cx);
        let first = notices.first()?.clone();
        let more = notices.len() - 1;
        let ui = &theme.ui;
        let text = if more > 0 { format!("{first}  (+{more} more)") } else { first };
        Some(
            div()
                .id("banner")
                .h(px(BANNER_H))
                .w_full()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_3()
                .bg(to_hsla(ui.surface_raised))
                .border_t_1()
                .border_color(to_hsla(ui.warning))
                .text_xs()
                .text_color(to_hsla(ui.text))
                .child(div().text_color(to_hsla(ui.warning)).child("⚠"))
                .child(div().flex_1().overflow_hidden().whitespace_nowrap().child(text))
                .child(
                    div()
                        .id("banner-close")
                        .px_1()
                        .text_color(to_hsla(ui.text_muted))
                        .child("×")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.banner_hidden = true;
                            cx.notify();
                        })),
                )
                .into_any_element(),
        )
    }

    fn render_palette(
        &self,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let p = self.palette.as_ref()?;
        let ui = &theme.ui;
        let keymap = &self.settings(cx).keymap;
        let width = (f32::from(window.viewport_size().width) - 32.0).clamp(200.0, 560.0);
        let (placeholder, prompt) = match p.model.mode {
            Mode::Commands => ("Type a command", "›"),
            Mode::Themes { .. } => ("Pick a theme (Escape restores)", "Theme"),
            Mode::Agents => ("Agents: type to filter, Enter jumps to the pane", "Agents"),
            Mode::PairedTools if p.paired_loading => ("Loading paired tools\u{2026}", "Revoke"),
            Mode::PairedTools => ("Paired tools: Enter revokes the selected one", "Revoke"),
        };
        let query: SharedString = if p.model.query.is_empty() {
            placeholder.into()
        } else {
            format!("{}▏", p.model.query).into()
        };
        let mut list = div().flex().flex_col().py_1();
        let selected_row = p.model.selected_row();
        let first = selected_row.saturating_sub(11);
        for (row, (entry, label, selected)) in p.model.rows().enumerate().skip(first).take(12) {
            if matches!(p.model.mode, Mode::Agents) {
                if let Some(el) = Self::agent_row(p, entry, selected, theme) {
                    list = list.child(div().id(("palette-row", row)).child(el).on_click(
                        cx.listener(move |this, _, window, cx| this.palette_click(row, window, cx)),
                    ));
                }
                continue;
            }
            let shortcut = match p.commands.get(entry) {
                Some(Command::Run(a)) => {
                    keymap.chords_for(*a).first().map(keybind::label).unwrap_or_default()
                }
                _ => String::new(),
            };
            let (bg, fg, sub) = if selected {
                (to_hsla(ui.accent), to_hsla(ui.accent_text), to_hsla(ui.accent_text))
            } else {
                (rgba(ui.surface_raised, 0.0), to_hsla(ui.text), to_hsla(ui.text_muted))
            };
            list = list.child(
                div()
                    .id(("palette-row", row))
                    .flex()
                    .flex_row()
                    .justify_between()
                    .px_3()
                    .py_1()
                    .mx_1()
                    .rounded_sm()
                    .bg(bg)
                    .text_color(fg)
                    .text_sm()
                    .child(label.to_owned())
                    .child(div().text_xs().text_color(sub).child(shortcut))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.palette_click(row, window, cx);
                    })),
            );
        }
        let empty = p.model.rows().next().is_none() && !p.paired_loading;
        Some(
            div()
                .absolute()
                .top(px(TAB_BAR_H + 8.0))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    div()
                        .id("palette")
                        .key_context("Palette")
                        .track_focus(&p.focus)
                        .on_key_down(cx.listener(Self::palette_key))
                        .w(px(width))
                        .flex()
                        .flex_col()
                        .rounded_md()
                        .border_1()
                        .border_color(to_hsla(ui.border_focused))
                        .bg(to_hsla(ui.surface_raised))
                        .shadow_lg()
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .gap_2()
                                .px_3()
                                .py_2()
                                .border_b_1()
                                .border_color(to_hsla(ui.border))
                                .bg(to_hsla(ui.surface_sunken))
                                .rounded_t_md()
                                .child(div().text_color(to_hsla(ui.accent)).child(prompt))
                                .child(
                                    div()
                                        .text_color(if p.model.query.is_empty() {
                                            to_hsla(ui.text_muted)
                                        } else {
                                            to_hsla(ui.text)
                                        })
                                        .child(query),
                                ),
                        )
                        .when(empty, |d| {
                            d.child(
                                div()
                                    .px_3()
                                    .py_2()
                                    .text_sm()
                                    .text_color(to_hsla(ui.text_muted))
                                    .child("No matches"),
                            )
                        })
                        .child(list),
                )
                .into_any_element(),
        )
    }
}

macro_rules! on_actions {
    ($el:expr, $cx:expr, $($ty:ident => $action:ident),+ $(,)?) => {{
        let el = $el;
        $(
            let el = el.on_action($cx.listener(|this, _: &$ty, window, cx| {
                this.run(Action::$action, window, cx)
            }));
        )+
        el
    }};
}

impl Render for WorkspaceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme(cx);
        let ui = theme.ui.clone();
        let root = div()
            .key_context("Workspace")
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(to_hsla(ui.surface))
            .text_color(to_hsla(ui.text))
            .font_family(".SystemUIFont");
        let root = on_actions!(root, cx,
            NewTab => NewTab,
            ClosePane => ClosePane,
            SplitRight => SplitRight,
            SplitDown => SplitDown,
            FocusLeft => FocusLeft,
            FocusRight => FocusRight,
            FocusUp => FocusUp,
            FocusDown => FocusDown,
            NextTab => NextTab,
            PrevTab => PrevTab,
            CommandPalette => CommandPalette,
            Paste => Paste,
            ZoomPane => ZoomPane,
            IncreaseFontSize => IncreaseFontSize,
            DecreaseFontSize => DecreaseFontSize,
            ResetFontSize => ResetFontSize,
            NewWindow => NewWindow,
            Quit => Quit,
            ResizePaneLeft => ResizePaneLeft,
            ResizePaneRight => ResizePaneRight,
            ResizePaneUp => ResizePaneUp,
            ResizePaneDown => ResizePaneDown,
            MoveTabLeft => MoveTabLeft,
            MoveTabRight => MoveTabRight,
            RenameTab => RenameTab,
            ReloadConfig => ReloadConfig,
            OpenConfig => OpenConfig,
            AgentDashboard => AgentDashboard,
            PrevPrompt => PrevPrompt,
            NextPrompt => NextPrompt,
            CopyLastOutput => CopyLastOutput,
        );
        let root = root.on_action(cx.listener(|this, _: &SwitchTheme, window, cx| {
            if !this.approval_open() {
                this.open_themes(window, cx);
            }
        }));
        if let Some(err) = &self.fatal {
            return root
                .p_4()
                .child(format!("Halyard could not start a shell: {err}"))
                .into_any_element();
        }
        let tab_bar = self.render_tab_bar(&theme, cx);
        let panes = self.render_panes(&theme, window, cx);
        let banner = self.render_banner(&theme, cx);
        let palette = self.render_palette(&theme, window, cx);
        let menu = self.render_menu(&theme, window, cx);
        self.approval_prepaint(window, cx);
        let approval = self.render_approval(&theme, window, cx);
        root.child(tab_bar)
            .child(panes)
            .children(banner)
            .children(palette)
            .children(menu)
            .children(approval)
            .into_any_element()
    }
}

impl Focusable for WorkspaceView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

#[cfg(feature = "debug-script")]
impl crate::debug_script::Host for WorkspaceView {
    fn script_dump(&mut self, window: &mut Window, cx: &mut Context<Self>) -> String {
        let overlay = self.overlay_dump(window);
        let Some(p) = self.model.focused().and_then(|id| self.panes.get(&id)) else {
            return "no focused pane".into();
        };
        let v = p.view.read(cx);
        let f = v.session().latest_frame();
        let names: Vec<Option<&str>> =
            self.model.tabs().iter().map(|t| t.name.as_deref()).collect();
        format!(
            "tabs {} names {names:?} active {} panes {} focused {:?}; {overlay}; frame {} ({}x{}), renders {}:\n{}",
            self.model.tabs().len(),
            self.model.active_index(),
            self.model.active().root.len(),
            self.model.focused(),
            f.generation,
            f.cols,
            f.rows,
            v.renders,
            f.text()
        )
    }

    fn script_select(&mut self, c1: u16, r1: u16, c2: u16, r2: u16, cx: &mut Context<Self>) {
        if let Some(p) = self.model.focused().and_then(|id| self.panes.get(&id)) {
            p.view.update(cx, |v, cx| {
                v.select_cells(c1, r1, c2, r2);
                cx.notify();
            });
        }
    }

    fn script_action(
        &mut self,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let a = Action::from_name(name).ok_or_else(|| format!("unknown action `{name}`"))?;
        match a {
            Action::Copy | Action::Paste => {
                let action = keybind::gpui_action(a).ok_or("not built")?;
                window.dispatch_action(action, cx);
            }
            _ => self.run(a, window, cx),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_appearances() {
        assert!(is_dark(WindowAppearance::Dark));
        assert!(is_dark(WindowAppearance::VibrantDark));
        assert!(!is_dark(WindowAppearance::Light));
        assert!(!is_dark(WindowAppearance::VibrantLight));
    }

    #[test]
    fn a_refused_split_becomes_a_notice() {
        let (mut ws, _) = Workspace::new();
        for _ in 0..halyard_workspace::persist::MAX_DEPTH {
            assert!(split_or_notice(&mut ws, Axis::Horizontal).is_ok());
        }
        let panes = ws.active().root.len();
        let notice = split_or_notice(&mut ws, Axis::Vertical).unwrap_err();
        assert!(notice.starts_with("split not made: "), "{notice}");
        assert!(notice.contains("deeply"), "{notice}");
        assert_eq!(ws.active().root.len(), panes, "nothing was split");
    }

    #[test]
    fn divider_hit_area_is_wider_than_the_line() {
        const { assert!(DIVIDER_HIT > GAP) };
        const { assert!(MIN_PANE.w > DIVIDER_HIT && MIN_PANE.h > DIVIDER_HIT) };
    }
}
