//! The terminal view: one session drawn into a GPUI window.
//!
//! Built on GPUI's public element and input APIs, following the patterns in
//! GPUI's own Apache-2.0 examples (`input.rs` for the platform input handler,
//! `painting.rs` for low-level painting).

use std::cell::Cell as StdCell;
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::SystemTime;

use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{
    App, Bounds, ClipboardItem, Context, ElementInputHandler, EntityInputHandler, EventEmitter,
    FocusHandle, Focusable, Font, FontStyle, FontWeight, Hsla, KeyDownEvent,
    MouseButton as GpuiButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Rgba,
    ScrollDelta, ScrollWheelEvent, SharedString, StrikethroughStyle, Task, TextRun, UTF16Selection,
    UnderlineStyle, Window, canvas, div, fill, outline, point, prelude::*, px, size,
};
use halyard_term::{
    CursorShape, Frame, KeyCode, KeyInput, Modifiers, MouseAction, MouseButton, MouseInput,
    Palette, Rgb, ScrollRequest, Session, SessionConfig, SessionError, SessionEvent,
};

use crate::blocks_ui::{self, Jump};
use crate::decide::{self, Wheel};
use crate::grid::{self, CellSize, TextRunSpec};
use crate::keybind::{Copy, Paste};
use crate::keys::{KeyRoute, route_keystroke};
use crate::selection::{self, MiddleClick};
use crate::settings;
use crate::signals::{self, SharedMonitor, SignalObserver};

/// Treat Option as Alt on macOS. Off, so Option composes characters as in
/// other macOS apps. Not a config key yet.
const OPTION_AS_ALT: bool = false;

/// Preferred monospace families, first available wins.
#[cfg(target_os = "macos")]
const FONT_CANDIDATES: &[&str] = &["Menlo", "SF Mono", "Monaco"];
#[cfg(not(target_os = "macos"))]
const FONT_CANDIDATES: &[&str] =
    &["DejaVu Sans Mono", "Noto Sans Mono", "Liberation Mono", "Ubuntu Mono", "Monospace"];

/// The family to draw with: the configured one when it is installed, else
/// the first installed platform monospace font. The second value is a notice
/// for the banner when the configured family was not found (it names the
/// fallback, never the configured value).
#[must_use]
pub fn resolve_font_family(configured: &str, installed: &[String]) -> (String, Option<String>) {
    let fallback = pick_font_family(FONT_CANDIDATES, installed).to_owned();
    if configured.eq_ignore_ascii_case("monospace") {
        return (fallback, None);
    }
    match installed.iter().find(|i| i.eq_ignore_ascii_case(configured)) {
        Some(found) => (found.clone(), None),
        None => {
            let notice =
                format!("config: key `font.family`: that font is not installed; using {fallback}");
            (fallback, Some(notice))
        }
    }
}

/// How a pane looks: everything that comes from the configuration.
#[derive(Clone, Debug, PartialEq)]
pub struct PaneStyle {
    /// Resolved font family.
    pub family: String,
    /// Font size in logical pixels.
    pub font_size: f32,
    /// Line height as a multiple of the font size.
    pub line_height: f32,
    /// Horizontal space between the pane edge and the grid.
    pub padding_x: f32,
    /// Vertical space between the pane edge and the grid.
    pub padding_y: f32,
    /// Cursor shape when the program has not asked for one.
    pub cursor: CursorShape,
    /// Whether the cursor blinks.
    pub blink: bool,
    /// Glyph color under a block cursor.
    pub cursor_text: Rgb,
    /// Write the clipboard when a selection is made.
    pub copy_on_select: bool,
    /// Ask before an unbracketed multi-line paste.
    pub confirm_multiline_paste: bool,
}

/// Decoration the window draws on a pane: a dimming veil for inactive
/// panes, an outline for the focused one (or one that needs the user), and
/// the agent badge.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PaneChrome {
    /// Color laid over the whole pane, if dimmed.
    pub dim: Option<Hsla>,
    /// Outline color, if outlined.
    pub border: Option<Hsla>,
    /// Agent state badge in the top right corner.
    pub badge: Option<PaneBadge>,
    /// Color of the margin mark beside commands that failed (shell integration
    /// blocks with a non-zero status), or `None` for no marks.
    pub failure: Option<Hsla>,
}

/// The small label a pane with an agent shows: what it is and its state.
#[derive(Clone, Debug, PartialEq)]
pub struct PaneBadge {
    /// For example "● Claude Code · waiting".
    pub text: SharedString,
    /// Text color (the state's theme token).
    pub fg: Hsla,
    /// Background.
    pub bg: Hsla,
    /// Outline, drawn when the news is unread.
    pub border: Option<Hsla>,
}

/// How to start a pane's program.
#[derive(Clone, Debug, Default)]
pub struct PaneSpawn {
    /// Program and arguments, or `None` for the login shell.
    pub command: Option<(PathBuf, Vec<String>)>,
    /// Working directory, or `None` for home.
    pub cwd: Option<PathBuf>,
    /// Scrollback lines.
    pub scrollback: usize,
    /// Starting colors.
    pub palette: Palette,
    /// The session API, when it is on: the pane gets its variables and joins
    /// the server's registry.
    pub api: Option<crate::session_api::PaneApi>,
}

/// Something a pane tells its window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaneEvent {
    /// The program exited cleanly: close the pane.
    Exited,
    /// The user dismissed the exit notice: close the pane.
    CloseRequested,
    /// The program set a new title.
    TitleChanged,
    /// The pane got keyboard focus.
    Focused,
    /// The agent attention state or its unread flag changed.
    AttentionChanged,
    /// A right click asked for the pane's menu, at this window position.
    /// The selection is untouched, so Copy still has it.
    ContextMenu {
        /// Where the click was, in window coordinates.
        position: Point<Pixels>,
        /// Whether there is a selection to copy.
        has_selection: bool,
    },
}

/// Pick the first candidate family that is installed, else the first one
/// (the platform's font fallback then applies).
#[must_use]
pub fn pick_font_family<'a>(candidates: &[&'a str], installed: &[String]) -> &'a str {
    candidates
        .iter()
        .find(|c| installed.iter().any(|i| i.eq_ignore_ascii_case(c)))
        .or_else(|| candidates.first())
        .copied()
        .unwrap_or("Menlo")
}

/// Convert a terminal color to a GPUI color.
#[must_use]
pub fn to_hsla(c: Rgb) -> Hsla {
    Rgba { r: f32::from(c.r) / 255.0, g: f32::from(c.g) / 255.0, b: f32::from(c.b) / 255.0, a: 1.0 }
        .into()
}

// Block rows count from the session's first row; screen rows (what the engine
// scrolls to and reads) start after the rows dropped from the top of scrollback,
// the monitor's `row_base`. These three convert at the view's call sites.

/// The screen row a prompt jump scrolls to, from the viewport's scroll offset.
fn prompt_scroll_row<C: halyard_shell::Clock>(
    blocks: &halyard_shell::BlockTracker<C>,
    scroll_offset: u64,
    base: u64,
    jump: Jump,
) -> Option<u64> {
    blocks_ui::prompt_target(blocks, scroll_offset + base, jump).map(|row| row.saturating_sub(base))
}

/// The screen rows of the last command's output, or `None` once any of them
/// was dropped from scrollback (it cannot be copied whole).
fn last_output_screen_rows<C: halyard_shell::Clock>(
    blocks: &halyard_shell::BlockTracker<C>,
    base: u64,
) -> Option<(u64, u64)> {
    let (first, last) = blocks_ui::last_output_rows(blocks)?;
    Some((first.checked_sub(base)?, last.checked_sub(base)?))
}

/// Failure marks in viewport rows, from the viewport's scroll offset.
fn viewport_failures<C: halyard_shell::Clock>(
    blocks: &halyard_shell::BlockTracker<C>,
    scroll_offset: u64,
    base: u64,
    rows: u16,
) -> Vec<(u16, u16)> {
    blocks_ui::failure_spans(blocks, scroll_offset + base, rows)
}

/// Where the grid sits in the window, and its size in cells.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Layout {
    origin: Point<Pixels>,
    cols: u16,
    rows: u16,
}

/// A terminal session drawn in a window.
pub struct TerminalView {
    session: Arc<Session>,
    monitor: SharedMonitor,
    focus: FocusHandle,
    font: Font,
    font_size: Pixels,
    cell: CellSize,
    style: PaneStyle,
    layout: Rc<StdCell<Layout>>,
    selecting: bool,
    /// The last right press opened the menu, so its release is not the
    /// program's either.
    menu_press: bool,
    marked: Option<String>,
    text_mods: Modifiers,
    exited: Option<Option<i32>>,
    scroll_accum: f32,
    title: String,
    chrome: PaneChrome,
    /// False during the "off" half of a cursor blink.
    blink_on: bool,
    /// Number of render passes, for debugging repaint problems.
    pub renders: u64,
    /// The session API link while the pane is registered with it.
    api: Option<crate::session_api::PaneApi>,
    _pump: Task<()>,
}

impl EventEmitter<PaneEvent> for TerminalView {}

/// Sends a key the user pressed in the pane. The one path from real key events
/// to `reset_typed_since_approval`: a key that ends or abandons the line (Enter,
/// Ctrl-C) means the user has seen it, so a paired tool's standing text is
/// forgotten. Nothing else (tool text, pastes, query replies) comes through here.
pub fn send_user_key(
    session: &Session,
    api: Option<&crate::session_api::PaneApi>,
    key: halyard_term::KeyInput,
) {
    if crate::keys::resets_typed_line(&key)
        && let Some(api) = api
    {
        api.user_line_key(session.id());
    }
    session.send_key(key);
}

impl Drop for TerminalView {
    fn drop(&mut self) {
        self.leave_api();
    }
}

impl std::fmt::Debug for TerminalView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalView").field("session", &self.session).finish_non_exhaustive()
    }
}

fn measure_cell(window: &Window, font: &Font, font_size: Pixels, line_height: f32) -> CellSize {
    let ts = window.text_system();
    let id = ts.resolve_font(font);
    let width =
        ts.advance(id, font_size, 'M').map_or(f32::from(font_size) * 0.6, |s| s.width.into());
    let scale = window.scale_factor();
    CellSize {
        width: grid::snap_to_device(width, scale),
        height: grid::snap_to_device((f32::from(font_size) * line_height).ceil(), scale),
    }
}

fn font_for(style: &PaneStyle) -> Font {
    gpui::font(style.family.clone())
}

fn px_u32(v: f32) -> u32 {
    let v = v.round();
    if (1.0..65_536.0).contains(&v) {
        // Range-checked, so the conversion is exact.
        v as u32
    } else {
        1
    }
}

/// A started session and what the view needs to draw it, made before the
/// view entity exists so a failure can be shown instead.
pub struct Prepared {
    session: Arc<Session>,
    api: Option<crate::session_api::PaneApi>,
    monitor: SharedMonitor,
    wakeups: mpsc::UnboundedReceiver<()>,
    font: Font,
    font_size: Pixels,
    cell: CellSize,
    style: PaneStyle,
    cols: u16,
    rows: u16,
}

impl std::fmt::Debug for Prepared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Prepared").field("session", &self.session).finish_non_exhaustive()
    }
}

impl TerminalView {
    /// Start a pane's program. `area` is the expected size of the pane in
    /// logical pixels; the pane resizes to its real size on first paint.
    pub fn prepare(
        window: &Window,
        style: PaneStyle,
        spawn: PaneSpawn,
        area: gpui::Size<Pixels>,
    ) -> Result<Prepared, SessionError> {
        let font = font_for(&style);
        let font_size = px(style.font_size);
        let cell = measure_cell(window, &font, font_size, style.line_height);
        let (cols, rows) = grid::grid_dims(
            f32::from(area.width) - 2.0 * style.padding_x,
            f32::from(area.height) - 2.0 * style.padding_y,
            cell,
        );

        let (tx, wakeups) = mpsc::unbounded::<()>();
        let (program, args) = spawn.command.map_or((None, Vec::new()), |(p, a)| (Some(p), a));
        // The monitor needs the pane's id before the session starts, to drop
        // protocol reports addressed to other panes.
        let session_id = halyard_term::new_session_id();
        let monitor = signals::new_monitor(&session_id);
        // Without valid credentials the pane simply runs without the API.
        let creds = spawn
            .api
            .as_ref()
            .and_then(|api| crate::session_api::pane_credentials(api.socket(), &session_id));
        let (env, env_remove) = crate::session_api::pane_env(creds.as_ref());
        let config = SessionConfig {
            program,
            args,
            cwd: spawn.cwd,
            scrollback_lines: spawn.scrollback,
            palette: spawn.palette,
            cols,
            rows,
            cell_width: px_u32(cell.width),
            cell_height: px_u32(cell.height),
            option_as_alt: OPTION_AS_ALT,
            // The receiver lives as long as the view; after that there is
            // no one left to wake.
            notify: Some(Arc::new(move || {
                tx.unbounded_send(()).ok();
            })),
            observer: Some(Box::new(SignalObserver::new(Arc::clone(&monitor)))),
            session_id: Some(session_id),
            env,
            env_remove,
            ..SessionConfig::default()
        };
        let session = Arc::new(Session::spawn(config)?);
        let api = match (spawn.api, creds) {
            (Some(api), Some(creds)) if api.register(&session, &monitor, &creds.token) => Some(api),
            _ => None,
        };
        Ok(Prepared { session, api, monitor, wakeups, font, font_size, cell, style, cols, rows })
    }

    /// Build the view around a prepared session.
    pub fn new(prepared: Prepared, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let Prepared {
            session,
            api,
            monitor,
            mut wakeups,
            font,
            font_size,
            cell,
            style,
            cols,
            rows,
        } = prepared;
        let pump = cx.spawn_in(window, async move |this, cx| {
            while wakeups.next().await.is_some() {
                // Coalesce a burst of wakeups into one update.
                while wakeups.try_recv().is_ok() {}
                if this.update_in(cx, |view, window, cx| view.drain_events(window, cx)).is_err() {
                    break;
                }
            }
        });
        let origin = point(px(style.padding_x), px(style.padding_y));
        let focus = cx.focus_handle();
        cx.on_focus(&focus, window, |_, _, cx| cx.emit(PaneEvent::Focused)).detach();
        Self {
            session,
            monitor,
            focus,
            font,
            font_size,
            cell,
            style,
            layout: Rc::new(StdCell::new(Layout { origin, cols, rows })),
            selecting: false,
            menu_press: false,
            marked: None,
            text_mods: Modifiers::NONE,
            exited: None,
            scroll_accum: 0.0,
            title: String::new(),
            chrome: PaneChrome::default(),
            blink_on: true,
            renders: 0,
            api,
            _pump: pump,
        }
    }

    /// The session behind this pane.
    #[cfg(feature = "debug-script")]
    #[must_use]
    pub fn session(&self) -> &Arc<Session> {
        &self.session
    }

    /// The pane's agent and shell signals.
    #[must_use]
    pub fn monitor(&self) -> &SharedMonitor {
        &self.monitor
    }

    /// The pane's `HALYARD_SESSION_ID`.
    #[must_use]
    pub fn session_id(&self) -> &str {
        self.session.id()
    }

    /// Polls the foreground process and advances the attention clock. Returns
    /// true when what the pane shows changed.
    pub fn poll_agent(&mut self, extra: &[String]) -> bool {
        if self.exited.is_some() {
            return false;
        }
        let fg = self.session.pid().and_then(|pid| signals::poll_foreground(pid, extra));
        let now = SystemTime::now();
        let mut m = signals::lock(&self.monitor);
        if let Some(fg) = fg {
            m.on_foreground(&fg, now);
        }
        m.tick(now);
        m.take_changed()
    }

    /// Whether the user can see and type into this pane (its tab is shown and
    /// its window is active). Gaining it clears unread. Returns true when
    /// unread changed. A pending state change stays pending for the pane's own
    /// event path, so it still notifies.
    pub fn set_seen(&mut self, seen: bool) -> bool {
        signals::lock(&self.monitor).set_focused(seen, SystemTime::now())
    }

    /// Scrolls so the previous or next command's prompt is the top row. Without
    /// shell integration there are no blocks and nothing moves; past the last
    /// prompt, [`Jump::Next`] returns to the live screen.
    pub fn jump_prompt(&self, jump: Jump) {
        let frame = self.session.latest_frame();
        if frame.alt_screen {
            return;
        }
        let target = {
            let m = signals::lock(&self.monitor);
            prompt_scroll_row(m.blocks(), frame.scroll.offset, m.row_base(), jump)
        };
        match (target, jump) {
            (Some(row), _) => self.session.scroll(ScrollRequest::Row(row)),
            (None, Jump::Next) => self.session.scroll(ScrollRequest::Bottom),
            (None, Jump::Previous) => {}
        }
    }

    /// The output of the last finished command, from the screen, or `None`
    /// without shell integration or once it scrolled out of the scrollback.
    #[must_use]
    pub fn last_output(&self) -> Option<String> {
        let (first, last) = {
            let m = signals::lock(&self.monitor);
            last_output_screen_rows(m.blocks(), m.row_base())?
        };
        self.session.screen_rows_text(first, last).map(|t| blocks_ui::tidy_output(&t))
    }

    /// Leaves the session API: the pane's program exited or the pane closed.
    fn leave_api(&mut self) {
        if let Some(api) = self.api.take() {
            api.closed(self.session.id(), self.exited.flatten());
        }
    }

    /// The session API link, while the pane is registered.
    #[must_use]
    pub fn api(&self) -> Option<&crate::session_api::PaneApi> {
        self.api.as_ref()
    }

    /// The user typed or pasted.
    fn note_input(&self) {
        signals::lock(&self.monitor).on_user_input(SystemTime::now());
    }

    /// The shell's working directory: the OSC 7 report when it names this
    /// machine, else the shell process's own directory. Used verbatim.
    #[must_use]
    pub fn cwd(&self) -> Option<String> {
        crate::session_api::pane_cwd(&self.session)
    }

    /// Whether a command other than the shell holds the terminal.
    #[must_use]
    pub fn needs_close_confirmation(&self, setting: bool) -> bool {
        self.exited.is_none()
            && crate::procinfo::needs_close_confirmation(
                setting,
                self.session.pid().and_then(crate::procinfo::groups),
            )
    }

    /// The title the program set, empty if none.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Apply a new style (font, padding, cursor). The grid reflows on the
    /// next paint.
    pub fn set_style(&mut self, style: PaneStyle, window: &mut Window, cx: &mut Context<Self>) {
        if style == self.style {
            return;
        }
        self.font = font_for(&style);
        self.font_size = px(style.font_size);
        self.cell = measure_cell(window, &self.font, self.font_size, style.line_height);
        self.style = style;
        // Force a resize report on the next layout: the cell size changed.
        let mut l = self.layout.get();
        l.cols = 0;
        self.layout.set(l);
        cx.notify();
    }

    /// Set the dimming and outline drawn over the pane.
    pub fn set_chrome(&mut self, chrome: PaneChrome, cx: &mut Context<Self>) {
        if self.chrome != chrome {
            self.chrome = chrome;
            cx.notify();
        }
    }

    /// Change the colors.
    pub fn set_palette(&self, palette: Palette) {
        self.session.set_palette(palette);
    }

    /// Show or hide the cursor for blinking. Returns true when it changed.
    pub fn set_blink(&mut self, on: bool, cx: &mut Context<Self>) -> bool {
        if self.blink_on == on {
            return false;
        }
        self.blink_on = on;
        cx.notify();
        true
    }

    /// Whether this pane wants its cursor to blink right now.
    #[must_use]
    pub fn wants_blink(&self, window: &Window) -> bool {
        self.style.blink && self.exited.is_none() && self.has_focus(window)
    }

    /// True when this pane has keyboard focus in the active window. A pane
    /// in a window behind another app draws as unfocused (hollow cursor).
    #[must_use]
    pub fn has_focus(&self, window: &Window) -> bool {
        decide::pane_has_focus(self.focus.is_focused(window), window.is_window_active())
    }

    /// Select all of scrollback and the screen (the pane menu's Select All).
    pub fn select_all(&self) {
        self.session.select_all();
    }

    /// Select between two cells (debug scripts).
    #[cfg(feature = "debug-script")]
    pub fn select_cells(&self, c1: u16, r1: u16, c2: u16, r2: u16) {
        self.session.selection_start(c1, r1);
        self.session.selection_extend(c2, r2);
    }

    fn drain_events(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let mut redraw = false;
        while let Ok(event) = self.session.events().try_recv() {
            match event {
                SessionEvent::FrameReady => {
                    redraw = true;
                    if signals::lock(&self.monitor).take_changed() {
                        cx.emit(PaneEvent::AttentionChanged);
                    }
                }
                SessionEvent::TitleChanged(title) => {
                    self.title = title;
                    cx.emit(PaneEvent::TitleChanged);
                }
                SessionEvent::Bell => {}
                SessionEvent::Exited(code) if decide::exit_closes_pane(code) => {
                    // A clean exit closes the pane, like most terminals.
                    self.exited = Some(code);
                    self.leave_api();
                    cx.emit(PaneEvent::Exited);
                    return;
                }
                SessionEvent::Exited(code) => {
                    self.exited = Some(code);
                    self.leave_api();
                    redraw = true;
                }
            }
        }
        if redraw {
            cx.notify();
        }
    }

    /// Grid cell under a window position, plus the pixel position the
    /// engine's mouse encoder expects (the center of that cell, in the cell
    /// pixel units passed to `Session::resize`).
    fn cell_under(&self, pos: Point<Pixels>) -> (u16, u16, f32, f32) {
        let l = self.layout.get();
        let x = f32::from(pos.x - l.origin.x);
        let y = f32::from(pos.y - l.origin.y);
        let (col, row) = grid::cell_at(x, y, self.cell, l.cols, l.rows);
        let (cw, ch) = (px_u32(self.cell.width) as f32, px_u32(self.cell.height) as f32);
        (col, row, (f32::from(col) + 0.5) * cw, (f32::from(row) + 0.5) * ch)
    }

    fn mouse_input(
        &self,
        action: MouseAction,
        button: Option<MouseButton>,
        pos: Point<Pixels>,
        mods: gpui::Modifiers,
        any_pressed: bool,
    ) -> MouseInput {
        let (_, _, x, y) = self.cell_under(pos);
        MouseInput {
            action,
            button,
            x,
            y,
            mods: Modifiers {
                shift: mods.shift,
                ctrl: mods.control,
                alt: mods.alt,
                super_key: mods.platform,
            },
            any_button_pressed: any_pressed,
        }
    }

    fn key_down(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.handle_key(&ev.keystroke, cx) {
            cx.stop_propagation();
        }
    }

    /// Act on a key press. Returns true if it was consumed; false means the
    /// platform text input should deliver its text (or it is not ours).
    fn handle_key(&mut self, keystroke: &gpui::Keystroke, cx: &mut Context<Self>) -> bool {
        if self.exited.is_some() {
            cx.emit(PaneEvent::CloseRequested);
            return true;
        }
        // Typing shows the cursor at once instead of mid-blink.
        self.blink_on = true;
        match route_keystroke(keystroke, OPTION_AS_ALT) {
            KeyRoute::Terminal(key) => {
                send_user_key(&self.session, self.api.as_ref(), key);
                self.note_input();
                true
            }
            KeyRoute::Text { mods, .. } => {
                // The platform input handler delivers the text next
                // (`replace_text_in_range`); remember Shift for it.
                self.text_mods = Modifiers { shift: mods.shift, ..Modifiers::NONE };
                false
            }
            KeyRoute::Ignore => false,
        }
    }

    fn type_text(&mut self, text: &str) {
        if !text.is_empty() {
            self.note_input();
        }
        let mods = std::mem::replace(&mut self.text_mods, Modifiers::NONE);
        let mut chars = text.chars();
        match (chars.next(), chars.next()) {
            (None, _) => {}
            (Some(c), None) => {
                let key = KeyCode::Char(if c.is_ascii() { c.to_ascii_lowercase() } else { c });
                self.session.send_key(KeyInput::with_text(key, text, mods));
            }
            // Committed IME text: the program receives the characters.
            _ => self.session.write(text.as_bytes()),
        }
    }

    fn button(b: GpuiButton) -> Option<MouseButton> {
        match b {
            GpuiButton::Left => Some(MouseButton::Left),
            GpuiButton::Right => Some(MouseButton::Right),
            GpuiButton::Middle => Some(MouseButton::Middle),
            GpuiButton::Navigate(_) => None,
        }
    }

    fn mouse_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus);
        let frame = self.session.latest_frame();
        let Some(button) = Self::button(ev.button) else { return };
        if button == MouseButton::Middle {
            match selection::middle_click(
                selection::LINUX,
                frame.mouse_tracking,
                ev.modifiers.shift,
            ) {
                MiddleClick::SendToApp => {
                    let input = self.mouse_input(
                        MouseAction::Press,
                        Some(button),
                        ev.position,
                        ev.modifiers,
                        true,
                    );
                    self.session.send_mouse(input);
                }
                MiddleClick::PastePrimary => self.paste_primary(window, cx),
                MiddleClick::Ignore => {}
            }
            cx.stop_propagation();
            return;
        }
        if button == MouseButton::Right {
            self.menu_press = crate::context_menu::right_click_opens_menu(
                frame.mouse_tracking,
                ev.modifiers.shift,
            );
            if self.menu_press {
                // Leave the selection alone: Copy in the menu needs it.
                let has_selection = self.session.selection_text().is_some_and(|t| !t.is_empty());
                cx.emit(PaneEvent::ContextMenu { position: ev.position, has_selection });
                cx.stop_propagation();
                return;
            }
        }
        if frame.mouse_tracking && !ev.modifiers.shift {
            let input =
                self.mouse_input(MouseAction::Press, Some(button), ev.position, ev.modifiers, true);
            self.session.send_mouse(input);
        } else if button == MouseButton::Left {
            let (col, row, _, _) = self.cell_under(ev.position);
            if ev.click_count >= 2 {
                self.session.select_word(col, row);
                self.selecting = false;
                self.selection_made(cx);
            } else {
                self.session.selection_start(col, row);
                self.selecting = true;
            }
        }
        cx.stop_propagation();
    }

    fn mouse_up(&mut self, ev: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.selecting && ev.button == GpuiButton::Left {
            self.selecting = false;
            self.selection_made(cx);
            return;
        }
        if ev.button == GpuiButton::Right && std::mem::take(&mut self.menu_press) {
            cx.stop_propagation();
            return;
        }
        let frame = self.session.latest_frame();
        if frame.mouse_tracking
            && let Some(button) = Self::button(ev.button)
        {
            let input = self.mouse_input(
                MouseAction::Release,
                Some(button),
                ev.position,
                ev.modifiers,
                false,
            );
            self.session.send_mouse(input);
            cx.stop_propagation();
        }
    }

    fn mouse_move(&mut self, ev: &MouseMoveEvent, _window: &mut Window, _cx: &mut Context<Self>) {
        if self.selecting {
            if ev.pressed_button == Some(GpuiButton::Left) {
                let (col, row, _, _) = self.cell_under(ev.position);
                self.session.selection_extend(col, row);
            } else {
                // The release happened outside the window.
                self.selecting = false;
            }
            return;
        }
        if self.session.latest_frame().mouse_tracking {
            let button = ev.pressed_button.and_then(Self::button);
            let input = self.mouse_input(
                MouseAction::Motion,
                button,
                ev.position,
                ev.modifiers,
                button.is_some(),
            );
            self.session.send_mouse(input);
        }
    }

    fn scroll_wheel(
        &mut self,
        ev: &ScrollWheelEvent,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        let lines = match ev.delta {
            // A wheel notch moves three lines, as in most terminals.
            ScrollDelta::Lines(p) => p.y * 3.0,
            // Trackpads report pixels: follow the fingers exactly.
            ScrollDelta::Pixels(p) => f32::from(p.y) / self.cell.height,
        };
        self.scroll_accum += lines;
        let whole = self.scroll_accum.trunc();
        self.scroll_accum -= whole;
        if whole == 0.0 {
            return;
        }
        // Bounded, so the conversion is exact.
        let n = whole.clamp(-1000.0, 1000.0) as i32;
        let frame = self.session.latest_frame();
        match decide::wheel(n, frame.mouse_tracking, frame.alt_screen) {
            Some(Wheel::Report(button, count)) => {
                for _ in 0..count {
                    let input = self.mouse_input(
                        MouseAction::Press,
                        Some(button),
                        ev.position,
                        ev.modifiers,
                        false,
                    );
                    self.session.send_mouse(input);
                }
            }
            // Alternate scroll: full-screen programs without mouse support
            // (less, man) scroll with arrow keys.
            Some(Wheel::Arrows(key, count)) => {
                for _ in 0..count {
                    self.session.send_key(KeyInput::new(key, Modifiers::NONE));
                }
            }
            Some(Wheel::Scroll(lines)) => self.session.scroll(ScrollRequest::Lines(lines)),
            None => {}
        }
    }

    /// A drag selection finished: copy it where the platform and config say.
    fn selection_made(&mut self, cx: &mut Context<Self>) {
        let targets = selection::on_selection(selection::LINUX, self.style.copy_on_select);
        if !(targets.clipboard || targets.primary) {
            return;
        }
        let Some(text) = self.session.selection_text() else { return };
        if targets.clipboard {
            cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
        }
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        if targets.primary {
            cx.write_to_primary(ClipboardItem::new_string(text));
        }
    }

    /// Paste the Linux PRIMARY selection through the normal paste path.
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn paste_primary(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_primary().and_then(|item| item.text()) {
            self.paste_text(text, window, cx);
        }
    }

    /// PRIMARY does not exist here.
    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    fn paste_primary(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}

    fn copy(&mut self, _: &Copy, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.session.selection_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.paste_text(text, window, cx);
        }
    }

    /// Paste through the session (which neutralizes control bytes and
    /// brackets when the program asked), asking first when the text holds a
    /// line break and the program has not enabled bracketed paste.
    fn paste_text(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        self.note_input();
        let bracketed = self.session.latest_frame().bracketed_paste;
        if !decide::paste_needs_confirmation(self.style.confirm_multiline_paste, bracketed, &text) {
            self.session.paste(text);
            return;
        }
        let answer = window.prompt(
            gpui::PromptLevel::Warning,
            "Paste text with line breaks?",
            Some(
                "The program has not turned on bracketed paste, so each line may run as a command.",
            ),
            &["Paste", "Cancel"],
            cx,
        );
        let session = Arc::clone(&self.session);
        cx.spawn(async move |_, _| {
            if answer.await == Ok(0) {
                session.paste(text);
            }
        })
        .detach();
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let marked = self.marked.as_deref().unwrap_or("");
        let utf16: Vec<u16> = marked.encode_utf16().collect();
        let end = range.end.min(utf16.len());
        let start = range.start.min(end);
        actual_range.replace(start..end);
        Some(String::from_utf16_lossy(&utf16[start..end]))
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let len = self.marked.as_deref().map_or(0, |m| m.encode_utf16().count());
        Some(UTF16Selection { range: len..len, reversed: false })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked.as_deref().map(|m| 0..m.encode_utf16().count())
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let had_marked = self.marked.take().is_some();
        if self.exited.is_none() {
            self.type_text(text);
        }
        if had_marked {
            cx.notify();
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked = (!new_text.is_empty()).then(|| new_text.to_owned());
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _range: Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        // The IME candidate window goes next to the cursor.
        let l = self.layout.get();
        let cursor = self.session.latest_frame().cursor;
        Some(Bounds::new(
            point(
                l.origin.x + px(f32::from(cursor.col) * self.cell.width),
                l.origin.y + px(f32::from(cursor.row) * self.cell.height),
            ),
            size(px(self.cell.width), px(self.cell.height)),
        ))
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

/// The pane's platform text input. It forwards everything to GPUI's
/// [`ElementInputHandler`] but turns off macOS press-and-hold, so holding a
/// key repeats it (vim's `j`) instead of opening the accent popup.
struct TerminalInput(ElementInputHandler<TerminalView>);

/// Whether macOS press-and-hold (the accent popup) is enabled in panes.
/// A terminal wants key repeat instead.
pub const PRESS_AND_HOLD: bool = false;

impl gpui::InputHandler for TerminalInput {
    fn selected_text_range(
        &mut self,
        ignore_disabled_input: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<UTF16Selection> {
        self.0.selected_text_range(ignore_disabled_input, window, cx)
    }

    fn marked_text_range(&mut self, window: &mut Window, cx: &mut App) -> Option<Range<usize>> {
        self.0.marked_text_range(window, cx)
    }

    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<String> {
        self.0.text_for_range(range_utf16, adjusted_range, window, cx)
    }

    fn replace_text_in_range(
        &mut self,
        replacement_range: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.0.replace_text_in_range(replacement_range, text, window, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.0.replace_and_mark_text_in_range(
            range_utf16,
            new_text,
            new_selected_range,
            window,
            cx,
        );
    }

    fn unmark_text(&mut self, window: &mut Window, cx: &mut App) {
        self.0.unmark_text(window, cx);
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Bounds<Pixels>> {
        self.0.bounds_for_range(range_utf16, window, cx)
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<usize> {
        self.0.character_index_for_point(point, window, cx)
    }

    fn apple_press_and_hold_enabled(&mut self) -> bool {
        PRESS_AND_HOLD
    }
}

/// Everything the paint pass needs, captured by value.
struct PaintState {
    frame: Arc<Frame>,
    cell: CellSize,
    font: Font,
    font_size: Pixels,
    focused: bool,
    cursor_shape: CursorShape,
    cursor_text: Rgb,
    cursor_on: bool,
    marked: Option<String>,
    chrome: PaneChrome,
    /// Viewport row spans of failed commands, for the margin marks.
    failures: Vec<(u16, u16)>,
}

fn run_font(base: &Font, spec: &TextRunSpec) -> Font {
    let mut font = base.clone();
    if spec.style.bold {
        font.weight = FontWeight::BOLD;
    }
    if spec.style.italic {
        font.style = FontStyle::Italic;
    }
    font
}

fn paint_grid(origin: Point<Pixels>, state: &PaintState, window: &mut Window, cx: &mut App) {
    let frame = &state.frame;
    let (cw, ch) = (state.cell.width, state.cell.height);
    let cell_bounds = |col: u16, row: u16, cells: u16| {
        Bounds::new(
            point(origin.x + px(f32::from(col) * cw), origin.y + px(f32::from(row) * ch)),
            size(px(f32::from(cells) * cw), px(ch)),
        )
    };

    for row in 0..frame.rows {
        for span in grid::row_backgrounds(frame, row) {
            window.paint_quad(fill(cell_bounds(span.col, row, span.len), to_hsla(span.color)));
        }
    }

    // Cursor, under the text so a block cursor can show its glyph.
    let cursor = frame.cursor;
    let show_cursor = cursor.visible
        && frame.scroll.at_bottom()
        && cursor.row < frame.rows
        && (state.cursor_on || !state.focused);
    let cursor_color = to_hsla(cursor.color.unwrap_or(frame.foreground));
    let mut cursor_text = None;
    if show_cursor {
        let wide = frame
            .cell(cursor.col, cursor.row)
            .is_some_and(|c| c.flags.has(halyard_term::CellFlags::WIDE));
        let b = cell_bounds(cursor.col, cursor.row, if wide { 2 } else { 1 });
        let shape = decide::drawn_cursor(state.focused, state.cursor_shape);
        match shape {
            CursorShape::Block => {
                window.paint_quad(fill(b, cursor_color));
                cursor_text = Some((cursor.col, state.cursor_text));
            }
            CursorShape::Bar => {
                window.paint_quad(fill(
                    Bounds::new(b.origin, size(px(2.0), b.size.height)),
                    cursor_color,
                ));
            }
            CursorShape::Underline => {
                let y = b.origin.y + b.size.height - px(2.0);
                window.paint_quad(fill(
                    Bounds::new(point(b.origin.x, y), size(b.size.width, px(2.0))),
                    cursor_color,
                ));
            }
            CursorShape::HollowBlock => {
                window.paint_quad(outline(b, cursor_color, gpui::BorderStyle::Solid));
            }
        }
    }

    let line_height = px(ch);
    for row in 0..frame.rows {
        let recolor = cursor_text.filter(|_| row == cursor.row);
        for spec in grid::row_runs(frame, row, recolor) {
            let mut color = to_hsla(spec.style.fg);
            if spec.style.faint {
                color.a = 0.6;
            }
            let run = TextRun {
                len: spec.text.len(),
                font: run_font(&state.font, &spec),
                color,
                background_color: None,
                underline: spec.style.underline.then_some(UnderlineStyle {
                    color: Some(color),
                    thickness: px(1.0),
                    wavy: false,
                }),
                strikethrough: spec
                    .style
                    .strikethrough
                    .then_some(StrikethroughStyle { color: Some(color), thickness: px(1.0) }),
            };
            let shaped = window.text_system().shape_line(
                SharedString::from(spec.text),
                state.font_size,
                &[run],
                Some(px(cw)),
            );
            let at = cell_bounds(spec.col, row, spec.cells).origin;
            if let Err(e) = shaped.paint(at, line_height, window, cx) {
                eprintln!("halyard: painting text failed: {e:#}");
            }
        }
    }

    // IME preedit text, drawn over the cursor cell.
    if let Some(marked) = &state.marked {
        let b = cell_bounds(cursor.col, cursor.row, 1);
        let color = to_hsla(frame.foreground);
        let run = TextRun {
            len: marked.len(),
            font: state.font.clone(),
            color,
            background_color: Some(to_hsla(frame.background)),
            underline: Some(UnderlineStyle { color: Some(color), thickness: px(1.0), wavy: false }),
            strikethrough: None,
        };
        let shaped = window.text_system().shape_line(
            SharedString::from(marked.clone()),
            state.font_size,
            &[run],
            None,
        );
        if let Err(e) = shaped.paint(b.origin, line_height, window, cx) {
            eprintln!("halyard: painting preedit failed: {e:#}");
        }
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.renders += 1;
        let frame = self.session.latest_frame();
        let background = to_hsla(frame.background);
        let cursor_shape = settings::effective_cursor(frame.cursor.shape, self.style.cursor);
        let failures = if self.chrome.failure.is_some() && !frame.alt_screen {
            let m = signals::lock(&self.monitor);
            viewport_failures(m.blocks(), frame.scroll.offset, m.row_base(), frame.rows)
        } else {
            Vec::new()
        };
        let state = PaintState {
            frame,
            cell: self.cell,
            font: self.font.clone(),
            font_size: self.font_size,
            focused: self.has_focus(window),
            cursor_shape,
            cursor_text: self.style.cursor_text,
            cursor_on: self.blink_on || !self.style.blink,
            marked: self.marked.clone(),
            failures,
            chrome: self.chrome.clone(),
        };
        let (pad_x, pad_y) = (self.style.padding_x, self.style.padding_y);
        let layout = Rc::clone(&self.layout);
        let session = Arc::clone(&self.session);
        let cell = self.cell;
        let focus = self.focus.clone();
        let entity = cx.entity();
        let exited = self.exited;
        let badge = self.chrome.badge.clone();

        div()
            .key_context("Terminal")
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .bg(background)
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::paste))
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down(GpuiButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_down(GpuiButton::Middle, cx.listener(Self::mouse_down))
            .on_mouse_down(GpuiButton::Right, cx.listener(Self::mouse_down))
            .on_mouse_up(GpuiButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up(GpuiButton::Middle, cx.listener(Self::mouse_up))
            .on_mouse_up(GpuiButton::Right, cx.listener(Self::mouse_up))
            .on_mouse_up_out(GpuiButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_scroll_wheel(cx.listener(Self::scroll_wheel))
            .child(
                canvas(
                    move |bounds, _window, _cx| {
                        // Fit the grid to the element; tell the session when
                        // the size in cells changes.
                        let (pad_x, pad_y) = (px(pad_x), px(pad_y));
                        let (cols, rows) = grid::grid_dims(
                            f32::from(bounds.size.width - pad_x * 2.0),
                            f32::from(bounds.size.height - pad_y * 2.0),
                            cell,
                        );
                        let origin = point(bounds.origin.x + pad_x, bounds.origin.y + pad_y);
                        let old = layout.get();
                        if (old.cols, old.rows) != (cols, rows) {
                            session.resize(cols, rows, px_u32(cell.width), px_u32(cell.height));
                        }
                        layout.set(Layout { origin, cols, rows });
                        origin
                    },
                    move |bounds, origin, window, cx| {
                        window.handle_input(
                            &focus,
                            TerminalInput(ElementInputHandler::new(bounds, entity)),
                            cx,
                        );
                        paint_grid(origin, &state, window, cx);
                        if let Some(color) = state.chrome.failure {
                            // A thin bar in the left margin beside each failed command.
                            let ch = state.cell.height;
                            for (first, last) in &state.failures {
                                let top = origin.y + px(f32::from(*first) * ch);
                                let h = px(f32::from(last - first + 1) * ch);
                                let bar =
                                    Bounds::new(point(bounds.origin.x, top), size(px(2.0), h));
                                window.paint_quad(fill(bar, color));
                            }
                        }
                        if let Some(dim) = state.chrome.dim {
                            window.paint_quad(fill(bounds, dim));
                        }
                        if let Some(border) = state.chrome.border {
                            window.paint_quad(outline(bounds, border, gpui::BorderStyle::Solid));
                        }
                    },
                )
                .size_full(),
            )
            .when_some(badge, |el, b| {
                el.child(
                    div()
                        .absolute()
                        .top(px(4.0))
                        .right(px(6.0))
                        .px_2()
                        .rounded_sm()
                        .bg(b.bg)
                        .when_some(b.border, |d, c| d.border_1().border_color(c))
                        .text_color(b.fg)
                        .text_xs()
                        .font_family(".SystemUIFont")
                        .child(b.text),
                )
            })
            .when_some(exited, |el, code| {
                let text = match code {
                    Some(c) => format!("Process exited with code {c}. Press any key to close."),
                    None => "Process was terminated. Press any key to close.".to_owned(),
                };
                el.child(
                    div()
                        .absolute()
                        .bottom_0()
                        .left_0()
                        .right_0()
                        .px_3()
                        .py_1()
                        .bg(gpui::rgb(0x3a_2a2a))
                        .text_color(gpui::rgb(0xf0_e0e0))
                        .text_sm()
                        .child(text),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_choice_prefers_installed_candidates() {
        let installed = vec!["Helvetica".to_owned(), "noto sans mono".to_owned()];
        assert_eq!(
            pick_font_family(&["DejaVu Sans Mono", "Noto Sans Mono"], &installed),
            "Noto Sans Mono"
        );
        assert_eq!(pick_font_family(&["Menlo", "Monaco"], &[]), "Menlo");
    }

    #[test]
    fn configured_font_family_is_used_when_installed() {
        let installed = vec!["Menlo".to_owned(), "JetBrains Mono".to_owned()];
        assert_eq!(
            resolve_font_family("jetbrains mono", &installed),
            ("JetBrains Mono".into(), None)
        );
        let (f, notice) = resolve_font_family("Nope Sans", &installed);
        assert_eq!(f, pick_font_family(FONT_CANDIDATES, &installed));
        let notice = notice.expect("a notice");
        assert!(notice.contains("font.family") && !notice.contains("Nope"), "{notice}");
        assert_eq!(resolve_font_family("monospace", &installed).1, None);
    }

    #[test]
    fn colors_convert_exactly() {
        let h = to_hsla(Rgb::new(255, 0, 0));
        let back: Rgba = h.into();
        assert!((back.r - 1.0).abs() < 1e-3 && back.g.abs() < 1e-3 && back.b.abs() < 1e-3);
        assert!((h.a - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn pixel_sizes_round_and_stay_positive() {
        assert_eq!(px_u32(8.4), 8);
        assert_eq!(px_u32(8.5), 9);
        assert_eq!(px_u32(0.0), 1);
        assert_eq!(px_u32(f32::NAN), 1);
    }

    /// Blocks at absolute rows 110, 120, 130 (prompt, output on the next row, D
    /// two below); 100 rows have been dropped from scrollback, so screen row 0
    /// is block row 100.
    #[test]
    fn row_base_converts_between_block_and_screen_rows() {
        use halyard_shell::{BlockTracker, ShellEvent, SystemClock};
        let mut t = BlockTracker::new(SystemClock, 100);
        for (row, code) in [(110, 0), (120, 1), (130, 0)] {
            t.on_event(&ShellEvent::PromptStart { aid: None }, row);
            t.on_event(&ShellEvent::CommandInputStart, row);
            t.on_event(&ShellEvent::CommandExecuted, row + 1);
            t.on_event(&ShellEvent::CommandFinished { exit_code: Some(code), aid: None }, row + 2);
        }
        let base = 100;
        // Viewport at screen row 25 is block row 125: the previous prompt is
        // block row 120, screen row 20.
        assert_eq!(prompt_scroll_row(&t, 25, base, Jump::Previous), Some(20));
        assert_eq!(prompt_scroll_row(&t, 25, base, Jump::Next), Some(30));
        assert_eq!(prompt_scroll_row(&t, 10, base, Jump::Previous), None);
        // Last output: block row 131 is screen row 31.
        assert_eq!(last_output_screen_rows(&t, base), Some((31, 31)));
        // Dropped from scrollback: nothing to copy rather than other text.
        assert_eq!(last_output_screen_rows(&t, 132), None);
        // The failed block (rows 120..=121) in a viewport at screen row 15,
        // block row 115: viewport rows 5 and 6.
        assert_eq!(viewport_failures(&t, 15, base, 10), vec![(5, 6)]);
        assert_eq!(viewport_failures(&t, 22, base, 10), Vec::<(u16, u16)>::new());
    }
}
