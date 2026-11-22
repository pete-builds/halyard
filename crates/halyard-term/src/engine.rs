//! The VT engine adapter: the only module that touches `libghostty-vt`.
//!
//! [`Engine`] wraps a libghostty `Terminal`, its `RenderState`, and the key and
//! mouse encoders. It consumes PTY output, turns Halyard input types into the
//! bytes a program expects, and builds owned [`Frame`] snapshots. Swapping the
//! engine means rewriting this file and nothing else.
//!
//! The engine is `!Send`: it must live on one thread (the session thread).
//!
//! Usage of the libghostty-vt API follows the MIT/Apache-2.0 `ghostling_rs`
//! example in github.com/uzaaft/libghostty-rs (render iteration, encoder
//! setup, effect callbacks), adapted to Halyard's types.

use std::cell::{Cell as StdCell, RefCell};
use std::fmt;
use std::rc::Rc;

use libghostty_vt::fmt::Format;
use libghostty_vt::key::{self, Mods};
use libghostty_vt::mouse;
use libghostty_vt::paste;
use libghostty_vt::render::{CellIterator, CursorVisualStyle, Dirty, RowIterator};
use libghostty_vt::screen::{CellWide, Screen, TrackedGridRef};
use libghostty_vt::selection::{FormatOptions, SelectWordOptions, Selection};
use libghostty_vt::style::{PaletteIndex, RgbColor, Underline};
use libghostty_vt::terminal::{
    ConformanceLevel, DeviceAttributeFeature, DeviceAttributes, DeviceType, Mode, Point,
    PointCoordinate, PrimaryDeviceAttributes, ScrollViewport, SecondaryDeviceAttributes,
    SizeReportSize, TertiaryDeviceAttributes,
};
use libghostty_vt::{RenderState, Terminal, TerminalOptions};

use crate::frame::{Cell, CellFlags, CellText, Cursor, CursorShape, Frame, Rgb, ScrollInfo};
use crate::input::ScrollRequest;
use crate::input::{KeyAction, KeyCode, KeyInput, Modifiers, MouseAction, MouseButton, MouseInput};
use crate::palette::Palette;

/// An error reported by the VT engine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineError {
    /// What the adapter was doing.
    pub context: &'static str,
    /// The engine's description of the failure.
    pub detail: String,
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VT engine error while {}: {}", self.context, self.detail)
    }
}

impl std::error::Error for EngineError {}

fn err(context: &'static str) -> impl FnOnce(libghostty_vt::Error) -> EngineError {
    move |e| EngineError { context, detail: e.to_string() }
}

/// Side effects the engine reported during `vt_write`, collected by the
/// effect callbacks and drained by the owner.
#[derive(Debug, Default)]
struct Effects {
    /// Bytes the terminal wants written back to the PTY (query replies).
    pty_out: RefCell<Vec<u8>>,
    bell: StdCell<bool>,
    title_changed: StdCell<bool>,
    /// Cell size in pixels, for XTWINOPS size reports.
    cell_px: StdCell<(u32, u32)>,
}

/// Effects observed since the last call to [`Engine::take_effects`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EngineEffects {
    /// Bytes to write back to the PTY (replies to device queries).
    pub pty_reply: Vec<u8>,
    /// A BEL was received.
    pub bell: bool,
    /// The window title changed (OSC 0 or 2).
    pub title_changed: bool,
}

/// A headless terminal: VT parser, screen state, scrollback and encoders.
///
/// Feed it program output with [`Engine::write`], read what it shows with
/// [`Engine::frame`], and encode user input with the `encode_*` methods.
pub struct Engine {
    // Field order is drop order: the tracked selection anchor and the
    // iterators go before the terminal that owns their memory.
    selection_anchor: Option<TrackedGridRef>,
    key_event: key::Event<'static>,
    key_encoder: key::Encoder<'static>,
    mouse_event: mouse::Event<'static>,
    mouse_encoder: mouse::Encoder<'static>,
    cell_iter: CellIterator<'static>,
    row_iter: RowIterator<'static>,
    render: RenderState<'static>,
    terminal: Terminal<'static, 'static>,
    effects: Rc<Effects>,
    palette: Palette,
    option_as_alt: bool,
    generation: u64,
}

impl fmt::Debug for Engine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Engine")
            .field("palette", &self.palette)
            .field("generation", &self.generation)
            .field("selecting", &self.selection_anchor.is_some())
            .finish_non_exhaustive()
    }
}

const fn to_engine_rgb(c: Rgb) -> RgbColor {
    RgbColor { r: c.r, g: c.g, b: c.b }
}

const fn from_engine_rgb(c: RgbColor) -> Rgb {
    Rgb::new(c.r, c.g, c.b)
}

impl Engine {
    /// Create an engine with a grid of `cols` x `rows` cells, keeping up to
    /// `scrollback_lines` lines of history, drawing with `palette`.
    pub fn new(
        cols: u16,
        rows: u16,
        scrollback_lines: usize,
        palette: &Palette,
    ) -> Result<Self, EngineError> {
        let mut terminal = Terminal::new(TerminalOptions {
            cols: cols.max(1),
            rows: rows.max(1),
            max_scrollback: scrollback_lines,
        })
        .map_err(err("creating the terminal"))?;

        let effects = Rc::new(Effects::default());
        effects.cell_px.set((1, 1));
        {
            let fx = Rc::clone(&effects);
            terminal
                .on_pty_write(move |_t, data| fx.pty_out.borrow_mut().extend_from_slice(data))
                .map_err(err("registering the PTY reply handler"))?;
        }
        {
            let fx = Rc::clone(&effects);
            terminal
                .on_bell(move |_t| fx.bell.set(true))
                .map_err(err("registering the bell handler"))?;
        }
        {
            let fx = Rc::clone(&effects);
            terminal
                .on_title_changed(move |_t| fx.title_changed.set(true))
                .map_err(err("registering the title handler"))?;
        }
        {
            let fx = Rc::clone(&effects);
            terminal
                .on_size(move |t| {
                    let (cell_width, cell_height) = fx.cell_px.get();
                    Some(SizeReportSize {
                        rows: t.rows().ok()?,
                        columns: t.cols().ok()?,
                        cell_width,
                        cell_height,
                    })
                })
                .map_err(err("registering the size report handler"))?;
        }
        terminal
            // Programs such as vim probe device attributes at startup and
            // degrade (or wait) without an answer. Report a VT220 with color.
            .on_device_attributes(|_t| {
                Some(DeviceAttributes {
                    primary: PrimaryDeviceAttributes::new(
                        ConformanceLevel::VT220,
                        &[
                            DeviceAttributeFeature::COLUMNS_132,
                            DeviceAttributeFeature::SELECTIVE_ERASE,
                            DeviceAttributeFeature::ANSI_COLOR,
                        ],
                    ),
                    secondary: SecondaryDeviceAttributes {
                        device_type: DeviceType::VT220,
                        firmware_version: 1,
                        rom_cartridge: 0,
                    },
                    tertiary: TertiaryDeviceAttributes { unit_id: 0 },
                })
            })
            .map_err(err("registering the device attributes handler"))?
            .on_xtversion(|_t| Some(XTVERSION))
            .map_err(err("registering the XTVERSION handler"))?
            .on_color_scheme(|_t| None)
            .map_err(err("registering the color scheme handler"))?;

        let mut engine = Self {
            selection_anchor: None,
            key_event: key::Event::new().map_err(err("creating a key event"))?,
            key_encoder: key::Encoder::new().map_err(err("creating the key encoder"))?,
            mouse_event: mouse::Event::new().map_err(err("creating a mouse event"))?,
            mouse_encoder: mouse::Encoder::new().map_err(err("creating the mouse encoder"))?,
            cell_iter: CellIterator::new().map_err(err("creating the cell iterator"))?,
            row_iter: RowIterator::new().map_err(err("creating the row iterator"))?,
            render: RenderState::new().map_err(err("creating the render state"))?,
            terminal,
            effects,
            palette: palette.clone(),
            option_as_alt: false,
            generation: 0,
        };
        engine.set_palette(palette)?;
        Ok(engine)
    }

    /// Feed program output through the VT parser.
    pub fn write(&mut self, bytes: &[u8]) {
        self.terminal.vt_write(bytes);
    }

    /// Drain the side effects (query replies, bell, title change) produced
    /// by the [`Engine::write`] calls since the last drain.
    pub fn take_effects(&mut self) -> EngineEffects {
        EngineEffects {
            pty_reply: safe_replies(&std::mem::take(&mut *self.effects.pty_out.borrow_mut())),
            bell: self.effects.bell.replace(false),
            title_changed: self.effects.title_changed.replace(false),
        }
    }

    /// The current window title (empty if none was set).
    #[must_use]
    pub fn title(&self) -> String {
        self.terminal.title().map(str::to_owned).unwrap_or_default()
    }

    /// Grid width in cells.
    #[must_use]
    pub fn cols(&self) -> u16 {
        self.terminal.cols().unwrap_or(1)
    }

    /// Grid height in cells.
    #[must_use]
    pub fn rows(&self) -> u16 {
        self.terminal.rows().unwrap_or(1)
    }

    /// True if the application enabled bracketed paste (DEC mode 2004).
    #[must_use]
    pub fn bracketed_paste(&self) -> bool {
        self.terminal.mode(Mode::BRACKETED_PASTE).unwrap_or(false)
    }

    /// Resize the grid (the primary screen reflows) and record the cell size
    /// in pixels for mouse encoding and size reports.
    pub fn resize(
        &mut self,
        cols: u16,
        rows: u16,
        cell_width: u32,
        cell_height: u32,
    ) -> Result<(), EngineError> {
        let (cw, ch) = (cell_width.max(1), cell_height.max(1));
        self.effects.cell_px.set((cw, ch));
        self.terminal.resize(cols.max(1), rows.max(1), cw, ch).map_err(err("resizing"))
    }

    /// Move the viewport through scrollback.
    pub fn scroll(&mut self, request: ScrollRequest) {
        let rows = isize::try_from(self.rows()).unwrap_or(isize::MAX);
        let target = match request {
            ScrollRequest::Lines(n) => ScrollViewport::Delta(n),
            ScrollRequest::Pages(n) => ScrollViewport::Delta(n.saturating_mul(rows)),
            ScrollRequest::Top => ScrollViewport::Top,
            ScrollRequest::Bottom => ScrollViewport::Bottom,
        };
        self.terminal.scroll_viewport(target);
    }

    /// Change the colors. Applies to cells drawn with default or ANSI
    /// colors; per-index overrides a program set with OSC 4 are preserved.
    pub fn set_palette(&mut self, palette: &Palette) -> Result<(), EngineError> {
        self.terminal
            .set_default_fg_color(Some(to_engine_rgb(palette.foreground)))
            .map_err(err("setting the foreground"))?
            .set_default_bg_color(Some(to_engine_rgb(palette.background)))
            .map_err(err("setting the background"))?
            .set_default_cursor_color(Some(to_engine_rgb(palette.cursor)))
            .map_err(err("setting the cursor color"))?;
        let mut table =
            self.terminal.default_color_palette().map_err(err("reading the palette"))?;
        for (i, color) in (0u8..).zip(palette.ansi.iter()) {
            table.set(PaletteIndex(i), to_engine_rgb(*color));
        }
        self.terminal.set_default_color_palette(Some(table)).map_err(err("setting the palette"))?;
        self.palette = palette.clone();
        Ok(())
    }

    /// Treat the macOS Option key as Alt (sending `ESC` prefixes) instead of
    /// as a key that composes characters. Off by default, matching Terminal
    /// and Ghostty. Other platforms always treat Alt as Alt.
    pub const fn set_option_as_alt(&mut self, on: bool) {
        self.option_as_alt = on;
    }

    /// The palette currently applied.
    #[must_use]
    pub const fn palette(&self) -> &Palette {
        &self.palette
    }

    /// Encode a key event for the running program, honoring the terminal's
    /// current modes (cursor key application mode, keypad mode, Kitty
    /// keyboard flags, modifyOtherKeys). Returns no bytes for events the
    /// program does not want, such as releases in legacy mode.
    pub fn encode_key(&mut self, input: &KeyInput) -> Result<Vec<u8>, EngineError> {
        let (key, unshifted) = map_key(input.key);
        let mods = map_mods(input.mods);
        // The encoder must not see control characters as text; it derives
        // those from the logical key and modifiers.
        let text =
            input.text.as_deref().filter(|t| !t.is_empty() && !t.chars().any(char::is_control));
        let mut consumed = Mods::empty();
        if text.is_some() && input.mods.shift {
            consumed |= Mods::SHIFT;
        }
        let action = match input.action {
            KeyAction::Press => key::Action::Press,
            KeyAction::Repeat => key::Action::Repeat,
            KeyAction::Release => key::Action::Release,
        };
        self.key_event
            .set_action(action)
            .set_key(key)
            .set_mods(mods)
            .set_consumed_mods(consumed)
            .set_unshifted_codepoint(unshifted)
            .set_utf8(text);
        let mut out = Vec::new();
        let option_as_alt =
            if self.option_as_alt { key::OptionAsAlt::True } else { key::OptionAsAlt::False };
        self.key_encoder
            .set_options_from_terminal(&self.terminal)
            // Must follow set_options_from_terminal, which resets it.
            .set_macos_option_as_alt(option_as_alt)
            .encode_to_vec(&self.key_event, &mut out)
            .map_err(err("encoding a key"))?;
        // A key the encoder has no mapping for (for example a character from
        // a non-US layout with no physical key code) still types its text.
        if out.is_empty()
            && input.action != KeyAction::Release
            && input.mods.is_empty()
            && let Some(t) = text
        {
            out.extend_from_slice(t.as_bytes());
        }
        Ok(out)
    }

    /// Encode a mouse event for the running program. Returns no bytes when
    /// the program has not enabled mouse tracking for this kind of event.
    pub fn encode_mouse(&mut self, input: &MouseInput) -> Result<Vec<u8>, EngineError> {
        let (cw, ch) = self.effects.cell_px.get();
        let cols = u32::from(self.cols());
        let rows = u32::from(self.rows());
        self.mouse_encoder
            .set_options_from_terminal(&self.terminal)
            .set_size(mouse::EncoderSize {
                screen_width: cols * cw,
                screen_height: rows * ch,
                cell_width: cw,
                cell_height: ch,
                padding_top: 0,
                padding_bottom: 0,
                padding_right: 0,
                padding_left: 0,
            })
            .set_any_button_pressed(input.any_button_pressed)
            .set_track_last_cell(true);
        let action = match input.action {
            MouseAction::Press => mouse::Action::Press,
            MouseAction::Release => mouse::Action::Release,
            MouseAction::Motion => mouse::Action::Motion,
        };
        let button = input.button.map(|b| match b {
            MouseButton::Left => mouse::Button::Left,
            MouseButton::Middle => mouse::Button::Middle,
            MouseButton::Right => mouse::Button::Right,
            MouseButton::WheelUp => mouse::Button::Four,
            MouseButton::WheelDown => mouse::Button::Five,
            MouseButton::WheelLeft => mouse::Button::Six,
            MouseButton::WheelRight => mouse::Button::Seven,
        });
        self.mouse_event
            .set_action(action)
            .set_button(button)
            .set_mods(map_mods(input.mods))
            .set_position(mouse::Position { x: input.x, y: input.y });
        let mut out = Vec::new();
        self.mouse_encoder
            .encode_to_vec(&self.mouse_event, &mut out)
            .map_err(err("encoding a mouse event"))?;
        Ok(out)
    }

    /// Encode pasted text: control bytes that could inject escape sequences
    /// are replaced with spaces, and the text is wrapped in bracketed paste
    /// markers when the program enabled that mode (otherwise newlines become
    /// carriage returns, as typed Enter would).
    pub fn encode_paste(&self, text: &str) -> Result<Vec<u8>, EngineError> {
        let bracketed = self.bracketed_paste();
        let mut data = text.as_bytes().to_vec();
        let mut buf = vec![0u8; data.len() + 16];
        let written = match paste::encode(&mut data, bracketed, &mut buf) {
            Ok(n) => n,
            Err(libghostty_vt::Error::OutOfSpace { required }) => {
                buf.resize(required, 0);
                paste::encode(&mut data, bracketed, &mut buf).map_err(err("encoding a paste"))?
            }
            Err(e) => return Err(err("encoding a paste")(e)),
        };
        buf.truncate(written);
        Ok(buf)
    }

    fn viewport_point(&self, col: u16, row: u16) -> Point {
        let col = col.min(self.cols().saturating_sub(1));
        let row = row.min(self.rows().saturating_sub(1));
        Point::Viewport(PointCoordinate { x: col, y: u32::from(row) })
    }

    /// Begin a selection at a viewport cell. Clears any existing selection.
    /// The anchor follows its text as the screen scrolls.
    pub fn selection_start(&mut self, col: u16, row: u16) -> Result<(), EngineError> {
        self.selection_clear()?;
        let anchor = self
            .terminal
            .track_grid_ref(self.viewport_point(col, row))
            .map_err(err("anchoring a selection"))?;
        self.selection_anchor = Some(anchor);
        Ok(())
    }

    /// Extend the selection from its anchor to a viewport cell.
    pub fn selection_extend(&mut self, col: u16, row: u16) -> Result<(), EngineError> {
        let Some(anchor) = &self.selection_anchor else { return Ok(()) };
        let Some(start) = anchor.snapshot(&self.terminal).map_err(err("reading the anchor"))?
        else {
            return Ok(());
        };
        let end = self
            .terminal
            .grid_ref(self.viewport_point(col, row))
            .map_err(err("resolving the selection end"))?;
        let selection = Selection::new(start, end, false);
        self.terminal.set_selection(Some(&selection)).map_err(err("setting the selection"))?;
        Ok(())
    }

    /// Select the word under a viewport cell (double-click behavior).
    pub fn select_word(&mut self, col: u16, row: u16) -> Result<(), EngineError> {
        self.selection_clear()?;
        let at = self
            .terminal
            .grid_ref(self.viewport_point(col, row))
            .map_err(err("resolving the word position"))?;
        if let Some(word) = self
            .terminal
            .select_word(SelectWordOptions::new(at))
            .map_err(err("selecting a word"))?
        {
            self.terminal.set_selection(Some(&word)).map_err(err("setting the selection"))?;
        }
        Ok(())
    }

    /// Remove the selection.
    pub fn selection_clear(&mut self) -> Result<(), EngineError> {
        self.selection_anchor = None;
        self.terminal.set_selection(None).map_err(err("clearing the selection"))?;
        Ok(())
    }

    /// The selected text as plain text (soft wraps joined, trailing blanks
    /// trimmed), or `None` when nothing is selected.
    pub fn selection_text(&self) -> Result<Option<String>, EngineError> {
        let options =
            FormatOptions::new().with_emit_format(Format::Plain).with_unwrap(true).with_trim(true);
        let bytes = self
            .terminal
            .format_selection_alloc(None, options)
            .map_err(err("formatting the selection"))?;
        Ok(bytes.map(|b| String::from_utf8_lossy(&b).into_owned()))
    }

    /// Build an owned snapshot of the viewport.
    pub fn frame(&mut self) -> Result<Frame, EngineError> {
        self.generation += 1;
        let title = self.title();
        let cwd = self.terminal.pwd().ok().filter(|s| !s.is_empty()).map(str::to_owned);
        let alt_screen = matches!(self.terminal.active_screen(), Ok(Screen::Alternate));
        let mouse_tracking = self.terminal.is_mouse_tracking().unwrap_or(false);
        let scroll = self
            .terminal
            .scrollbar()
            .map(|s| ScrollInfo { total: s.total, offset: s.offset, len: s.len })
            .map_err(err("reading the scrollbar"))?;
        let selection_bg = self.palette.selection_background;
        let selection_fg = self.palette.selection_foreground;

        let snapshot = self.render.update(&self.terminal).map_err(err("updating render state"))?;
        let colors = snapshot.colors().map_err(err("reading colors"))?;
        let default_fg = from_engine_rgb(colors.foreground);
        let default_bg = from_engine_rgb(colors.background);
        let cols = snapshot.cols().map_err(err("reading columns"))?;
        let rows = snapshot.rows().map_err(err("reading rows"))?;

        let blank =
            Cell { text: CellText::Empty, fg: default_fg, bg: default_bg, flags: CellFlags(0) };
        let mut cells = Vec::with_capacity(usize::from(cols) * usize::from(rows));
        let mut row_iter = self.row_iter.update(&snapshot).map_err(err("iterating rows"))?;
        let mut row_count: u16 = 0;
        let mut chars = ['\0'; 8];
        while let Some(row) = row_iter.next() {
            if row_count == rows {
                break;
            }
            let selected = row.selection().map_err(err("reading row selection"))?;
            let mut cell_iter = self.cell_iter.update(row).map_err(err("iterating cells"))?;
            let mut col: u16 = 0;
            while let Some(c) = cell_iter.next() {
                if col == cols {
                    break;
                }
                let e = err("reading a cell");
                let raw = c.raw_cell().map_err(e)?;
                let mut flags = CellFlags(0);
                match raw.wide().map_err(err("reading cell width"))? {
                    CellWide::Wide => flags.insert(CellFlags::WIDE),
                    CellWide::SpacerTail | CellWide::SpacerHead => {
                        flags.insert(CellFlags::WIDE_SPACER);
                    }
                    _ => {}
                }
                if raw.has_hyperlink().unwrap_or(false) {
                    flags.insert(CellFlags::HYPERLINK);
                }
                let len = c.graphemes_len().map_err(err("reading grapheme length"))?;
                let text = match len {
                    0 => CellText::Empty,
                    n if n <= chars.len() => {
                        c.graphemes_buf(&mut chars[..n]).map_err(err("reading graphemes"))?;
                        if n == 1 {
                            CellText::Char(chars[0])
                        } else {
                            CellText::Cluster(chars[..n].iter().collect::<String>().into())
                        }
                    }
                    _ => {
                        let all = c.graphemes().map_err(err("reading graphemes"))?;
                        CellText::Cluster(all.iter().collect::<String>().into())
                    }
                };
                let mut fg =
                    c.fg_color().map_err(err("reading fg"))?.map_or(default_fg, from_engine_rgb);
                let mut bg =
                    c.bg_color().map_err(err("reading bg"))?.map_or(default_bg, from_engine_rgb);
                if c.has_styling().map_err(err("reading styling"))? {
                    let style = c.style().map_err(err("reading style"))?;
                    let pairs = [
                        (style.bold, CellFlags::BOLD),
                        (style.italic, CellFlags::ITALIC),
                        (style.underline != Underline::None, CellFlags::UNDERLINE),
                        (style.strikethrough, CellFlags::STRIKETHROUGH),
                        (style.faint, CellFlags::FAINT),
                        (style.invisible, CellFlags::INVISIBLE),
                        (style.overline, CellFlags::OVERLINE),
                        (style.blink, CellFlags::BLINK),
                    ];
                    for (on, bit) in pairs {
                        if on {
                            flags.insert(bit);
                        }
                    }
                    if style.inverse {
                        std::mem::swap(&mut fg, &mut bg);
                    }
                }
                if selected.is_some_and(|s| (s.start_x..=s.end_x).contains(&col)) {
                    flags.insert(CellFlags::SELECTED);
                    bg = selection_bg;
                    fg = selection_fg.unwrap_or(fg);
                }
                cells.push(Cell { text, fg, bg, flags });
                col += 1;
            }
            for _ in col..cols {
                cells.push(blank.clone());
            }
            row.set_dirty(false).map_err(err("clearing row dirty state"))?;
            row_count += 1;
        }
        for _ in row_count..rows {
            for _ in 0..cols {
                cells.push(blank.clone());
            }
        }

        let visible = snapshot.cursor_visible().map_err(err("reading cursor visibility"))?;
        let viewport = snapshot.cursor_viewport().map_err(err("reading cursor position"))?;
        let shape = match snapshot.cursor_visual_style().map_err(err("reading cursor style"))? {
            CursorVisualStyle::Bar => CursorShape::Bar,
            CursorVisualStyle::Underline => CursorShape::Underline,
            CursorVisualStyle::BlockHollow => CursorShape::HollowBlock,
            _ => CursorShape::Block,
        };
        let blinking = snapshot.cursor_blinking().map_err(err("reading cursor blink"))?;
        let cursor = Cursor {
            col: viewport.map_or(0, |v| if v.at_wide_tail { v.x.saturating_sub(1) } else { v.x }),
            row: viewport.map_or(0, |v| v.y),
            shape,
            visible: visible && viewport.is_some(),
            blinking,
            color: colors.cursor.map(from_engine_rgb),
        };
        snapshot.set_dirty(Dirty::Clean).map_err(err("clearing dirty state"))?;

        Ok(Frame {
            cols,
            rows,
            cells,
            cursor,
            background: default_bg,
            foreground: default_fg,
            title,
            cwd,
            alt_screen,
            mouse_tracking,
            scroll,
            generation: self.generation,
        })
    }
}

/// The XTVERSION reply text, the only DCS string Halyard sends unprompted.
const XTVERSION: &str = concat!("Halyard ", env!("CARGO_PKG_VERSION"));

/// Keep only replies that cannot carry application-controlled text.
///
/// A reply is written to the program's input as if typed. Any reply that
/// echoes text the program (or a file it printed) chose, such as the window
/// title report (`CSI 21 t` answered with `OSC l <title> ST`), lets output
/// inject commands into the shell (CVE-2024-38396 for iTerm2). So this is an
/// allowlist, applied per escape sequence:
///
/// - CSI replies whose parameter bytes are in 0x20 to 0x3f (no free text),
///   with optional intermediates: DA1, DA2, DSR, CPR, DECRQM, Kitty keyboard
///   flags, XTWINOPS size reports. The color-scheme query is not answered.
/// - OSC 4 and OSC 10 to 19 color reports in `rgb:` hex form.
/// - DCS `>|` XTVERSION with Halyard's own string, and DCS `!|` DA3 (hex).
///
/// Everything else (title and icon reports, DECRQSS, XTGETTCAP, stray bytes)
/// is dropped.
pub(crate) fn safe_replies(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < data.len() {
        if data[i] != 0x1b || i + 1 >= data.len() {
            i += 1;
            continue;
        }
        let (end, safe) = match data[i + 1] {
            b'[' => {
                let mut j = i + 2;
                while j < data.len() && (0x20..=0x3f).contains(&data[j]) {
                    j += 1;
                }
                if j < data.len() && (0x40..=0x7e).contains(&data[j]) {
                    (j + 1, true)
                } else {
                    (j, false)
                }
            }
            kind @ (b']' | b'P') => {
                let body_start = i + 2;
                let mut j = body_start;
                let mut found = None;
                while j < data.len() {
                    if kind == b']' && data[j] == 0x07 {
                        found = Some((j, j + 1));
                        break;
                    }
                    if data[j] == 0x1b && data.get(j + 1) == Some(&b'\\') {
                        found = Some((j, j + 2));
                        break;
                    }
                    j += 1;
                }
                match found {
                    Some((body_end, end)) => {
                        let body = &data[body_start..body_end];
                        let ok =
                            if kind == b']' { safe_osc_reply(body) } else { safe_dcs_reply(body) };
                        (end, ok)
                    }
                    None => (data.len(), false),
                }
            }
            _ => (i + 2, false),
        };
        if safe {
            out.extend_from_slice(&data[i..end]);
        }
        i = end.max(i + 1);
    }
    out
}

fn safe_osc_reply(body: &[u8]) -> bool {
    let text = String::from_utf8_lossy(body);
    let mut parts = text.splitn(2, ';');
    let Some(Ok(code)) = parts.next().map(str::parse::<u16>) else { return false };
    let Some(rest) = parts.next() else { return false };
    let color = match code {
        4 => match rest.split_once(';') {
            Some((index, color)) if index.bytes().all(|b| b.is_ascii_digit()) => color,
            _ => return false,
        },
        10..=19 => rest,
        _ => return false,
    };
    color.strip_prefix("rgb:").is_some_and(|hex| {
        !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit() || b == b'/')
    })
}

fn safe_dcs_reply(body: &[u8]) -> bool {
    if let Some(version) = body.strip_prefix(b">|") {
        return version == XTVERSION.as_bytes();
    }
    body.strip_prefix(b"!|").is_some_and(|hex| hex.iter().all(u8::is_ascii_hexdigit))
}

/// True if `text` can be pasted without risk: it has no newline (which would
/// run a command) and no bracketed-paste end marker.
#[must_use]
pub fn paste_is_safe(text: &str) -> bool {
    paste::is_safe(text)
}

fn map_mods(m: Modifiers) -> Mods {
    let mut mods = Mods::empty();
    if m.shift {
        mods |= Mods::SHIFT;
    }
    if m.ctrl {
        mods |= Mods::CTRL;
    }
    if m.alt {
        mods |= Mods::ALT;
    }
    if m.super_key {
        mods |= Mods::SUPER;
    }
    mods
}

/// Map a Halyard key to libghostty's physical key code (US layout) and the
/// unshifted code point the Kitty protocol reports.
fn map_key(code: KeyCode) -> (key::Key, char) {
    use key::Key as K;
    let fixed = |k: K| (k, '\0');
    match code {
        KeyCode::Enter => fixed(K::Enter),
        KeyCode::Tab => fixed(K::Tab),
        KeyCode::Backspace => fixed(K::Backspace),
        KeyCode::Escape => fixed(K::Escape),
        KeyCode::Space => (K::Space, ' '),
        KeyCode::Up => fixed(K::ArrowUp),
        KeyCode::Down => fixed(K::ArrowDown),
        KeyCode::Left => fixed(K::ArrowLeft),
        KeyCode::Right => fixed(K::ArrowRight),
        KeyCode::Home => fixed(K::Home),
        KeyCode::End => fixed(K::End),
        KeyCode::PageUp => fixed(K::PageUp),
        KeyCode::PageDown => fixed(K::PageDown),
        KeyCode::Insert => fixed(K::Insert),
        KeyCode::Delete => fixed(K::Delete),
        KeyCode::F(n) => fixed(function_key(n)),
        KeyCode::Char(c) => {
            let c = c.to_ascii_lowercase();
            let k = match c {
                'a'..='z' => letter_key(c),
                '0'..='9' => digit_key(c),
                '`' => K::Backquote,
                '\\' => K::Backslash,
                '[' => K::BracketLeft,
                ']' => K::BracketRight,
                ',' => K::Comma,
                '=' => K::Equal,
                '-' => K::Minus,
                '.' => K::Period,
                '\'' => K::Quote,
                ';' => K::Semicolon,
                '/' => K::Slash,
                ' ' => K::Space,
                _ => K::Unidentified,
            };
            (k, c)
        }
    }
}

fn letter_key(c: char) -> key::Key {
    use key::Key as K;
    const LETTERS: [key::Key; 26] = [
        K::A,
        K::B,
        K::C,
        K::D,
        K::E,
        K::F,
        K::G,
        K::H,
        K::I,
        K::J,
        K::K,
        K::L,
        K::M,
        K::N,
        K::O,
        K::P,
        K::Q,
        K::R,
        K::S,
        K::T,
        K::U,
        K::V,
        K::W,
        K::X,
        K::Y,
        K::Z,
    ];
    let i = (u32::from(c) - u32::from('a')) as usize;
    LETTERS.get(i).copied().unwrap_or(K::Unidentified)
}

fn digit_key(c: char) -> key::Key {
    use key::Key as K;
    const DIGITS: [key::Key; 10] = [
        K::Digit0,
        K::Digit1,
        K::Digit2,
        K::Digit3,
        K::Digit4,
        K::Digit5,
        K::Digit6,
        K::Digit7,
        K::Digit8,
        K::Digit9,
    ];
    let i = (u32::from(c) - u32::from('0')) as usize;
    DIGITS.get(i).copied().unwrap_or(K::Unidentified)
}

fn function_key(n: u8) -> key::Key {
    use key::Key as K;
    const KEYS: [key::Key; 25] = [
        K::F1,
        K::F2,
        K::F3,
        K::F4,
        K::F5,
        K::F6,
        K::F7,
        K::F8,
        K::F9,
        K::F10,
        K::F11,
        K::F12,
        K::F13,
        K::F14,
        K::F15,
        K::F16,
        K::F17,
        K::F18,
        K::F19,
        K::F20,
        K::F21,
        K::F22,
        K::F23,
        K::F24,
        K::F25,
    ];
    usize::from(n).checked_sub(1).and_then(|i| KEYS.get(i)).copied().unwrap_or(K::Unidentified)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> Engine {
        let mut e = Engine::new(20, 5, 100, &Palette::default()).unwrap();
        e.resize(20, 5, 8, 16).unwrap();
        e
    }

    #[test]
    fn plain_text_appears_in_frame() {
        let mut e = engine();
        e.write(b"hello\r\nworld");
        let f = e.frame().unwrap();
        assert_eq!(f.cols, 20);
        assert_eq!(f.rows, 5);
        assert_eq!(f.cells.len(), 100);
        assert_eq!(f.row_text(0), "hello");
        assert_eq!(f.row_text(1), "world");
        assert_eq!((f.cursor.col, f.cursor.row), (5, 1));
        assert!(f.cursor.visible);
    }

    #[test]
    fn sgr_colors_and_attributes_resolve_to_rgb() {
        let p = Palette::default();
        let mut e = engine();
        e.write(b"\x1b[31mR\x1b[0m\x1b[1;4;3mB\x1b[0m\x1b[38;2;1;2;3mT\x1b[0m\x1b[7mI\x1b[0m");
        let f = e.frame().unwrap();
        let red = f.cell(0, 0).unwrap();
        assert_eq!(red.fg, p.ansi[1]);
        assert_eq!(red.bg, p.background);
        let styled = f.cell(1, 0).unwrap();
        assert!(styled.flags.has(CellFlags::BOLD));
        assert!(styled.flags.has(CellFlags::UNDERLINE));
        assert!(styled.flags.has(CellFlags::ITALIC));
        assert_eq!(f.cell(2, 0).unwrap().fg, Rgb::new(1, 2, 3));
        let inverse = f.cell(3, 0).unwrap();
        assert_eq!((inverse.fg, inverse.bg), (p.background, p.foreground));
        // A default cell keeps the palette defaults.
        assert_eq!(f.cell(10, 0).unwrap().fg, p.foreground);
    }

    #[test]
    fn wide_glyphs_set_wide_and_spacer_flags() {
        let mut e = engine();
        e.write("漢a".as_bytes());
        let f = e.frame().unwrap();
        assert!(f.cell(0, 0).unwrap().flags.has(CellFlags::WIDE));
        assert!(f.cell(1, 0).unwrap().flags.has(CellFlags::WIDE_SPACER));
        assert_eq!(f.row_text(0), "漢a");
    }

    #[test]
    fn title_change_and_bell_are_reported() {
        let mut e = engine();
        e.write(b"\x1b]2;my title\x07\x07");
        let fx = e.take_effects();
        assert!(fx.title_changed);
        assert!(fx.bell);
        assert_eq!(e.title(), "my title");
        assert_eq!(e.frame().unwrap().title, "my title");
        let fx = e.take_effects();
        assert!(!fx.bell && !fx.title_changed);
    }

    #[test]
    fn device_queries_produce_replies() {
        let mut e = engine();
        // DSR cursor position report.
        e.write(b"ab\x1b[6n");
        let fx = e.take_effects();
        assert_eq!(fx.pty_reply, b"\x1b[1;3R");
        // DA1 must be answered or vim waits.
        e.write(b"\x1b[c");
        assert!(e.take_effects().pty_reply.starts_with(b"\x1b[?62"));
    }

    #[test]
    fn title_reports_are_never_sent_back() {
        let mut e = engine();
        // The iTerm2 CVE-2024-38396 pattern: set a title holding a command,
        // then ask for it back. The reply would be typed into the shell.
        e.write(b"\x1b]2;;touch /tmp/pwned;\x07\x1b[21t");
        assert_eq!(e.take_effects().pty_reply, b"");
        e.write(b"\x1b]1;icon\x07\x1b[20t");
        assert_eq!(e.take_effects().pty_reply, b"");
    }

    #[test]
    fn safe_query_replies_still_flow() {
        let mut e = engine();
        e.write(b"\x1b[6n");
        assert_eq!(e.take_effects().pty_reply, b"\x1b[1;1R");
        e.write(b"\x1b[?2004$p");
        assert_eq!(e.take_effects().pty_reply, b"\x1b[?2004;2$y");
        e.write(b"\x1b[?u");
        assert_eq!(e.take_effects().pty_reply, b"\x1b[?0u");
        e.write(b"\x1b[>q");
        let xt = e.take_effects().pty_reply;
        assert_eq!(xt, format!("\x1bP>|{XTVERSION}\x1b\\").as_bytes());
        e.write(b"\x1b]10;?\x1b\\");
        let fg = String::from_utf8(e.take_effects().pty_reply).unwrap();
        assert!(fg.starts_with("\x1b]10;rgb:"), "{fg:?}");
        e.write(b"\x1b[18t");
        assert_eq!(e.take_effects().pty_reply, b"\x1b[8;5;20t");
    }

    #[test]
    fn reply_filter_allowlist() {
        let keep: &[&[u8]] = &[
            b"\x1b[?62;1;6;22c",
            b"\x1b[>1;10;0c",
            b"\x1b[12;40R",
            b"\x1b[0n",
            b"\x1b]4;1;rgb:ff/00/00\x07",
            b"\x1b]11;rgb:1616/1818/1d1d\x1b\\",
            b"\x1bP!|00000000\x1b\\",
        ];
        for r in keep {
            assert_eq!(safe_replies(r), *r, "{}", r.escape_ascii());
        }
        let drop: &[&[u8]] = &[
            b"\x1b]l;touch x;\x1b\\",
            b"\x1b]L icon\x1b\\",
            b"\x1b]2;title\x07",
            b"\x1b]10;rgb:12/34/56;rm -rf\x07",
            b"\x1bP1$r0m\x1b\\",
            b"\x1bP1+r6b6273\x1b\\",
            b"\x1bP>|Evil 1.0\x1b\\",
            b"plain text\r",
            b"\x1b[1;2",
        ];
        for r in drop {
            assert_eq!(safe_replies(r), b"", "{}", r.escape_ascii());
        }
        // A mixed buffer keeps the safe parts only.
        assert_eq!(safe_replies(b"\x1b[1;1R\x1b]l;x\x1b\\\x1b[0n"), b"\x1b[1;1R\x1b[0n");
    }

    #[test]
    fn cursor_on_wide_tail_reports_the_glyph_column() {
        let mut e = engine();
        // Print a wide glyph, then step left once: the cursor sits on the
        // glyph's spacer half and must be drawn over the glyph itself.
        e.write("漢\x1b[D".as_bytes());
        let f = e.frame().unwrap();
        assert_eq!((f.cursor.col, f.cursor.row), (0, 0));
    }

    #[test]
    fn control_characters_in_key_text_are_ignored() {
        let mut e = engine();
        // Some platforms report the C0 character as the key's text; the
        // encoder must still produce exactly one byte.
        let ctrl_c = KeyInput::with_text(KeyCode::Char('c'), "\u{3}", Modifiers::CTRL);
        assert_eq!(e.encode_key(&ctrl_c).unwrap(), b"\x03");
        let enter = KeyInput::with_text(KeyCode::Enter, "\r", Modifiers::NONE);
        assert_eq!(e.encode_key(&enter).unwrap(), b"\r");
        let tab = KeyInput::with_text(KeyCode::Tab, "\t", Modifiers::NONE);
        assert_eq!(e.encode_key(&tab).unwrap(), b"\t");
    }

    #[test]
    fn alt_screen_and_mouse_tracking_flags() {
        let mut e = engine();
        assert!(!e.frame().unwrap().alt_screen);
        e.write(b"\x1b[?1049h\x1b[?1000h");
        let f = e.frame().unwrap();
        assert!(f.alt_screen);
        assert!(f.mouse_tracking);
        e.write(b"\x1b[?1049l\x1b[?1000l");
        let f = e.frame().unwrap();
        assert!(!f.alt_screen);
        assert!(!f.mouse_tracking);
    }

    #[test]
    fn keys_encode_with_terminal_modes() {
        let mut e = engine();
        let ctrl_c = KeyInput::new(KeyCode::Char('c'), Modifiers::CTRL);
        assert_eq!(e.encode_key(&ctrl_c).unwrap(), b"\x03");
        let a = KeyInput::with_text(KeyCode::Char('a'), "a", Modifiers::NONE);
        assert_eq!(e.encode_key(&a).unwrap(), b"a");
        let shift_a = KeyInput::with_text(KeyCode::Char('a'), "A", Modifiers::SHIFT);
        assert_eq!(e.encode_key(&shift_a).unwrap(), b"A");
        let enter = KeyInput::new(KeyCode::Enter, Modifiers::NONE);
        assert_eq!(e.encode_key(&enter).unwrap(), b"\r");
        let bs = KeyInput::new(KeyCode::Backspace, Modifiers::NONE);
        assert_eq!(e.encode_key(&bs).unwrap(), b"\x7f");
        let up = KeyInput::new(KeyCode::Up, Modifiers::NONE);
        assert_eq!(e.encode_key(&up).unwrap(), b"\x1b[A");
        // Cursor key application mode (DECCKM) switches arrows to SS3.
        e.write(b"\x1b[?1h");
        assert_eq!(e.encode_key(&up).unwrap(), b"\x1bOA");
        let f1 = KeyInput::new(KeyCode::F(1), Modifiers::NONE);
        assert_eq!(e.encode_key(&f1).unwrap(), b"\x1bOP");
        let alt_b = KeyInput::with_text(KeyCode::Char('b'), "b", Modifiers::ALT);
        e.set_option_as_alt(true);
        assert_eq!(e.encode_key(&alt_b).unwrap(), b"\x1bb");
    }

    #[test]
    fn kitty_keyboard_flags_change_encoding() {
        let mut e = engine();
        let esc = KeyInput::new(KeyCode::Escape, Modifiers::NONE);
        assert_eq!(e.encode_key(&esc).unwrap(), b"\x1b");
        // Push Kitty flags: disambiguate escape codes.
        e.write(b"\x1b[>1u");
        assert_eq!(e.encode_key(&esc).unwrap(), b"\x1b[27u");
    }

    #[test]
    fn unknown_layout_character_types_its_text() {
        let mut e = engine();
        let k = KeyInput::with_text(KeyCode::Char('é'), "é", Modifiers::NONE);
        assert_eq!(e.encode_key(&k).unwrap(), "é".as_bytes());
    }

    #[test]
    fn mouse_reports_only_when_tracking() {
        let mut e = engine();
        let click = MouseInput {
            action: MouseAction::Press,
            button: Some(MouseButton::Left),
            x: 8.0 * 2.0 + 1.0,
            y: 16.0 * 3.0 + 1.0,
            mods: Modifiers::NONE,
            any_button_pressed: true,
        };
        assert!(e.encode_mouse(&click).unwrap().is_empty());
        e.write(b"\x1b[?1000h\x1b[?1006h");
        assert_eq!(e.encode_mouse(&click).unwrap(), b"\x1b[<0;3;4M");
    }

    #[test]
    fn paste_is_bracketed_only_when_enabled() {
        let mut e = engine();
        assert_eq!(e.encode_paste("a\nb").unwrap(), b"a\rb");
        e.write(b"\x1b[?2004h");
        assert_eq!(e.encode_paste("a\nb").unwrap(), b"\x1b[200~a\nb\x1b[201~");
        // An embedded end marker cannot escape the bracket.
        let evil = e.encode_paste("x\x1b[201~rm -rf /\n").unwrap();
        assert!(!evil[6..evil.len() - 6].windows(6).any(|w| w == b"\x1b[201~"));
        assert!(!paste_is_safe("ls\n"));
        assert!(paste_is_safe("ls"));
    }

    #[test]
    fn scrollback_scrolls_viewport() {
        let mut e = engine();
        for i in 0..20 {
            e.write(format!("line {i}\r\n").as_bytes());
        }
        let f = e.frame().unwrap();
        assert!(f.scroll.at_bottom());
        assert!(f.scroll.total > u64::from(f.rows));
        e.scroll(ScrollRequest::Top);
        let f = e.frame().unwrap();
        assert_eq!(f.row_text(0), "line 0");
        assert_eq!(f.scroll.offset, 0);
        e.scroll(ScrollRequest::Lines(2));
        assert_eq!(e.frame().unwrap().row_text(0), "line 2");
        e.scroll(ScrollRequest::Bottom);
        assert!(e.frame().unwrap().scroll.at_bottom());
    }

    #[test]
    fn resize_reflows_and_changes_grid() {
        let mut e = engine();
        e.write(b"0123456789abcdefghij");
        e.resize(10, 5, 8, 16).unwrap();
        let f = e.frame().unwrap();
        assert_eq!(f.cols, 10);
        assert_eq!(f.cells.len(), 50);
        assert!(f.text().contains("0123456789\nabcdefghij"), "{}", f.text());
    }

    #[test]
    fn selection_highlights_and_copies() {
        let p = Palette::default();
        let mut e = engine();
        e.write(b"hello world\r\nsecond");
        e.selection_start(6, 0).unwrap();
        e.selection_extend(10, 0).unwrap();
        assert_eq!(e.selection_text().unwrap().as_deref(), Some("world"));
        let f = e.frame().unwrap();
        assert!(f.cell(6, 0).unwrap().flags.has(CellFlags::SELECTED));
        assert_eq!(f.cell(6, 0).unwrap().bg, p.selection_background);
        assert!(!f.cell(5, 0).unwrap().flags.has(CellFlags::SELECTED));
        e.select_word(2, 1).unwrap();
        assert_eq!(e.selection_text().unwrap().as_deref(), Some("second"));
        e.selection_clear().unwrap();
        assert_eq!(e.selection_text().unwrap(), None);
        assert!(!e.frame().unwrap().cell(6, 0).unwrap().flags.has(CellFlags::SELECTED));
    }

    #[test]
    fn palette_change_recolors_ansi_and_defaults() {
        let mut e = engine();
        e.write(b"\x1b[32mG");
        let mut p = Palette::default();
        p.ansi[2] = Rgb::new(9, 9, 9);
        p.background = Rgb::new(1, 1, 1);
        e.set_palette(&p).unwrap();
        let f = e.frame().unwrap();
        assert_eq!(f.cell(0, 0).unwrap().fg, Rgb::new(9, 9, 9));
        assert_eq!(f.background, Rgb::new(1, 1, 1));
    }
}
