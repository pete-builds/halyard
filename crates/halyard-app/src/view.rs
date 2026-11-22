//! The terminal view: one session drawn into a GPUI window.
//!
//! Built on GPUI's public element and input APIs, following the patterns in
//! GPUI's own Apache-2.0 examples (`input.rs` for the platform input handler,
//! `painting.rs` for low-level painting).

use std::cell::Cell as StdCell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
#[cfg(feature = "debug-script")]
use std::time::Duration;

use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{
    App, Bounds, ClipboardItem, Context, ElementInputHandler, EntityInputHandler, FocusHandle,
    Focusable, Font, FontStyle, FontWeight, Hsla, KeyDownEvent, MouseButton as GpuiButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Rgba, ScrollDelta,
    ScrollWheelEvent, SharedString, StrikethroughStyle, Task, TextRun, UTF16Selection,
    UnderlineStyle, Window, canvas, div, fill, outline, point, prelude::*, px, size,
};
use halyard_term::{
    CursorShape, Frame, KeyCode, KeyInput, Modifiers, MouseAction, MouseButton, MouseInput, Rgb,
    ScrollRequest, Session, SessionConfig, SessionError, SessionEvent,
};

use crate::grid::{self, CellSize, TextRunSpec};
use crate::keys::{KeyRoute, route_keystroke};
use crate::{Copy, Paste};

/// Space between the window edge and the grid, in logical pixels.
pub const PADDING: f32 = 6.0;

/// Font size in logical pixels.
pub const FONT_SIZE: f32 = 14.0;

/// Treat Option as Alt on macOS. Off, so Option composes characters as in
/// other macOS apps; a config option comes with the settings work.
const OPTION_AS_ALT: bool = false;

/// Preferred monospace families, first available wins.
#[cfg(target_os = "macos")]
const FONT_CANDIDATES: &[&str] = &["Menlo", "SF Mono", "Monaco"];
#[cfg(not(target_os = "macos"))]
const FONT_CANDIDATES: &[&str] =
    &["DejaVu Sans Mono", "Noto Sans Mono", "Liberation Mono", "Ubuntu Mono", "Monospace"];

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
    focus: FocusHandle,
    font: Font,
    font_size: Pixels,
    cell: CellSize,
    layout: Rc<StdCell<Layout>>,
    selecting: bool,
    marked: Option<String>,
    text_mods: Modifiers,
    exited: Option<Option<i32>>,
    scroll_accum: f32,
    /// Number of render passes, for debugging repaint problems.
    renders: u64,
    _pump: Task<()>,
}

impl std::fmt::Debug for TerminalView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalView").field("session", &self.session).finish_non_exhaustive()
    }
}

fn measure_cell(window: &Window, font: &Font, font_size: Pixels) -> CellSize {
    let ts = window.text_system();
    let id = ts.resolve_font(font);
    let width =
        ts.advance(id, font_size, 'M').map_or(f32::from(font_size) * 0.6, |s| s.width.into());
    let height = f32::from(ts.ascent(id, font_size)) + f32::from(ts.descent(id, font_size));
    let scale = window.scale_factor();
    CellSize {
        width: grid::snap_to_device(width, scale),
        height: grid::snap_to_device(height.ceil().max(f32::from(font_size)), scale),
    }
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
    wakeups: mpsc::UnboundedReceiver<()>,
    font: Font,
    font_size: Pixels,
    cell: CellSize,
    cols: u16,
    rows: u16,
}

impl std::fmt::Debug for Prepared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Prepared").field("session", &self.session).finish_non_exhaustive()
    }
}

impl TerminalView {
    /// Start a login shell sized to the window.
    pub fn prepare(window: &Window) -> Result<Prepared, SessionError> {
        let installed = window.text_system().all_font_names();
        let font = gpui::font(pick_font_family(FONT_CANDIDATES, &installed));
        let font_size = px(FONT_SIZE);
        let cell = measure_cell(window, &font, font_size);
        let viewport = window.viewport_size();
        let (cols, rows) = grid::grid_dims(
            f32::from(viewport.width) - 2.0 * PADDING,
            f32::from(viewport.height) - 2.0 * PADDING,
            cell,
        );

        let (tx, wakeups) = mpsc::unbounded::<()>();
        let config = SessionConfig {
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
            ..SessionConfig::default()
        };
        let session = Arc::new(Session::spawn(config)?);
        Ok(Prepared { session, wakeups, font, font_size, cell, cols, rows })
    }

    /// Build the view around a prepared session.
    pub fn new(prepared: Prepared, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let Prepared { session, mut wakeups, font, font_size, cell, cols, rows } = prepared;
        let pump = cx.spawn_in(window, async move |this, cx| {
            while wakeups.next().await.is_some() {
                // Coalesce a burst of wakeups into one update.
                while wakeups.try_recv().is_ok() {}
                if this.update_in(cx, |view, window, cx| view.drain_events(window, cx)).is_err() {
                    break;
                }
            }
        });
        #[cfg(feature = "debug-script")]
        Self::start_debug_script(window, cx);
        let origin = point(px(PADDING), px(PADDING));
        Self {
            session,
            focus: cx.focus_handle(),
            font,
            font_size,
            cell,
            layout: Rc::new(StdCell::new(Layout { origin, cols, rows })),
            selecting: false,
            marked: None,
            text_mods: Modifiers::NONE,
            exited: None,
            scroll_accum: 0.0,
            renders: 0,
            _pump: pump,
        }
    }

    /// Run `HALYARD_DEBUG_SCRIPT` (see `debug_script`), if set.
    #[cfg(feature = "debug-script")]
    fn start_debug_script(window: &mut Window, cx: &mut Context<Self>) {
        use crate::debug_script::{self, Step};
        let Some(path) = std::env::var_os("HALYARD_DEBUG_SCRIPT") else { return };
        let steps = match std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|s| debug_script::parse(&s))
        {
            Ok(steps) => steps,
            Err(e) => {
                // A scripted run with a broken script would otherwise sit
                // idle forever.
                eprintln!("halyard: debug script: {e}");
                cx.quit();
                return;
            }
        };
        cx.spawn_in(window, async move |this, cx| {
            for step in steps {
                let result = match step {
                    Step::Sleep(d) => {
                        cx.background_executor().timer(d).await;
                        Ok(())
                    }
                    Step::Type(text) => this
                        .update(cx, |view, _| {
                            for c in text.chars() {
                                view.type_text(&c.to_string());
                            }
                        })
                        .map_err(|e| e.to_string()),
                    Step::Key(k) => match gpui::Keystroke::parse(&k) {
                        // Through the real key handler, then the text path
                        // the platform input method would take.
                        Ok(keystroke) => this
                            .update_in(cx, |view, window, _| {
                                if !view.handle_key(&keystroke, window)
                                    && let Some(text) = &keystroke.key_char
                                {
                                    view.type_text(text);
                                }
                            })
                            .map_err(|e| e.to_string()),
                        Err(e) => Err(e.to_string()),
                    },
                    Step::Shot(p) => {
                        match this
                            .update_in(cx, |_, window, _| debug_script::native_view(window))
                            .map_err(|e| e.to_string())
                            .and_then(|view| view)
                        {
                            Ok(view) => {
                                // Outside the update: the draw it triggers
                                // updates the window. Then let Core Animation
                                // commit before reading the window back.
                                debug_script::force_display(view);
                                cx.background_executor().timer(Duration::from_millis(250)).await;
                                debug_script::capture(view, &p)
                                    .map(|()| eprintln!("halyard: saved {}", p.display()))
                            }
                            Err(e) => Err(e),
                        }
                    }
                    Step::Select(c1, r1, c2, r2) => this
                        .update(cx, |view, cx| {
                            view.session.selection_start(c1, r1);
                            view.session.selection_extend(c2, r2);
                            cx.notify();
                        })
                        .map_err(|e| e.to_string()),
                    Step::Resize(w, h) => this
                        .update_in(cx, |_, window, _| window.resize(size(px(w), px(h))))
                        .map_err(|e| e.to_string()),
                    Step::Dump => this
                        .update(cx, |view, _| {
                            let f = view.session.latest_frame();
                            eprintln!(
                                "halyard: frame {} ({}x{}), renders {}:\n{}",
                                f.generation,
                                f.cols,
                                f.rows,
                                view.renders,
                                f.text()
                            );
                        })
                        .map_err(|e| e.to_string()),
                    Step::Quit => cx.update(|_, cx| cx.quit()).map_err(|e| e.to_string()),
                };
                if let Err(e) = result {
                    eprintln!("halyard: debug script step failed: {e}");
                }
            }
        })
        .detach();
    }

    fn drain_events(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut redraw = false;
        while let Ok(event) = self.session.events().try_recv() {
            match event {
                SessionEvent::FrameReady => redraw = true,
                SessionEvent::TitleChanged(title) => {
                    window.set_window_title(if title.is_empty() { "Halyard" } else { &title });
                }
                SessionEvent::Bell => {}
                SessionEvent::Exited(Some(0)) => {
                    // A clean exit closes the window, like most terminals.
                    window.remove_window();
                    return;
                }
                SessionEvent::Exited(code) => {
                    self.exited = Some(code);
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

    fn key_down(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.handle_key(&ev.keystroke, window) {
            cx.stop_propagation();
        }
    }

    /// Act on a key press. Returns true if it was consumed; false means the
    /// platform text input should deliver its text (or it is not ours).
    fn handle_key(&mut self, keystroke: &gpui::Keystroke, window: &mut Window) -> bool {
        if self.exited.is_some() {
            window.remove_window();
            return true;
        }
        match route_keystroke(keystroke, OPTION_AS_ALT) {
            KeyRoute::Terminal(key) => {
                self.session.send_key(key);
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
        if frame.mouse_tracking && !ev.modifiers.shift {
            let input =
                self.mouse_input(MouseAction::Press, Some(button), ev.position, ev.modifiers, true);
            self.session.send_mouse(input);
        } else if button == MouseButton::Left {
            let (col, row, _, _) = self.cell_under(ev.position);
            if ev.click_count >= 2 {
                self.session.select_word(col, row);
                self.selecting = false;
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
        let up = n > 0;
        let frame = self.session.latest_frame();
        if frame.mouse_tracking {
            let button = if up { MouseButton::WheelUp } else { MouseButton::WheelDown };
            for _ in 0..n.unsigned_abs().min(10) {
                let input = self.mouse_input(
                    MouseAction::Press,
                    Some(button),
                    ev.position,
                    ev.modifiers,
                    false,
                );
                self.session.send_mouse(input);
            }
        } else if frame.alt_screen {
            // Alternate scroll: full-screen programs without mouse support
            // (less, man) scroll with arrow keys.
            let key = if up { KeyCode::Up } else { KeyCode::Down };
            for _ in 0..n.unsigned_abs().min(10) {
                self.session.send_key(KeyInput::new(key, Modifiers::NONE));
            }
        } else {
            // Wheel up moves into history.
            self.session.scroll(ScrollRequest::Lines(-(n as isize)));
        }
    }

    fn copy(&mut self, _: &Copy, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.session.selection_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn paste(&mut self, _: &Paste, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.session.paste(text);
        }
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

/// Everything the paint pass needs, captured by value.
struct PaintState {
    frame: Arc<Frame>,
    cell: CellSize,
    font: Font,
    font_size: Pixels,
    focused: bool,
    marked: Option<String>,
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
    let show_cursor = cursor.visible && frame.scroll.at_bottom() && cursor.row < frame.rows;
    let cursor_color = to_hsla(cursor.color.unwrap_or(frame.foreground));
    let mut cursor_text = None;
    if show_cursor {
        let wide = frame
            .cell(cursor.col, cursor.row)
            .is_some_and(|c| c.flags.has(halyard_term::CellFlags::WIDE));
        let b = cell_bounds(cursor.col, cursor.row, if wide { 2 } else { 1 });
        let shape = if state.focused { cursor.shape } else { CursorShape::HollowBlock };
        match shape {
            CursorShape::Block => {
                window.paint_quad(fill(b, cursor_color));
                let under = frame.cell(cursor.col, cursor.row).map_or(frame.background, |c| c.bg);
                cursor_text = Some((cursor.col, under));
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
        let state = PaintState {
            frame,
            cell: self.cell,
            font: self.font.clone(),
            font_size: self.font_size,
            focused: self.focus.is_focused(window),
            marked: self.marked.clone(),
        };
        let layout = Rc::clone(&self.layout);
        let session = Arc::clone(&self.session);
        let cell = self.cell;
        let focus = self.focus.clone();
        let entity = cx.entity();
        let exited = self.exited;

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
                        let pad = px(PADDING);
                        let (cols, rows) = grid::grid_dims(
                            f32::from(bounds.size.width - pad * 2.0),
                            f32::from(bounds.size.height - pad * 2.0),
                            cell,
                        );
                        let origin = point(bounds.origin.x + pad, bounds.origin.y + pad);
                        let old = layout.get();
                        if (old.cols, old.rows) != (cols, rows) {
                            session.resize(cols, rows, px_u32(cell.width), px_u32(cell.height));
                        }
                        layout.set(Layout { origin, cols, rows });
                        origin
                    },
                    move |bounds, origin, window, cx| {
                        window.handle_input(&focus, ElementInputHandler::new(bounds, entity), cx);
                        paint_grid(origin, &state, window, cx);
                    },
                )
                .size_full(),
            )
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
}
