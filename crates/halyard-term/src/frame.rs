//! Owned, thread-safe snapshots of what a terminal shows.
//!
//! The VT engine is `!Send` and lives on its own thread. After it consumes
//! output it publishes a [`Frame`]: plain data the UI thread can read without
//! touching the engine. Colors are already resolved to RGB, so the renderer
//! never needs the palette.

/// A 24-bit color.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rgb {
    /// Red channel.
    pub r: u8,
    /// Green channel.
    pub g: u8,
    /// Blue channel.
    pub b: u8,
}

impl Rgb {
    /// Build a color from channels.
    #[must_use]
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Parse `#rrggbb` or `rrggbb`.
    #[must_use]
    pub fn from_hex(s: &str) -> Option<Self> {
        let s = s.strip_prefix('#').unwrap_or(s);
        if s.len() != 6 || !s.is_ascii() {
            return None;
        }
        let p = |i: usize| u8::from_str_radix(&s[i..i + 2], 16).ok();
        Some(Self::new(p(0)?, p(2)?, p(4)?))
    }

    /// Format as `#rrggbb`.
    #[must_use]
    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

/// Per-cell text attributes, as a small bit set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct CellFlags(pub u16);

impl CellFlags {
    /// Bold weight.
    pub const BOLD: u16 = 1 << 0;
    /// Italic style.
    pub const ITALIC: u16 = 1 << 1;
    /// Any underline style.
    pub const UNDERLINE: u16 = 1 << 2;
    /// Strikethrough.
    pub const STRIKETHROUGH: u16 = 1 << 3;
    /// Faint (dim) intensity.
    pub const FAINT: u16 = 1 << 4;
    /// Hidden text (SGR 8).
    pub const INVISIBLE: u16 = 1 << 5;
    /// First half of a double-width glyph.
    pub const WIDE: u16 = 1 << 6;
    /// Second half of a double-width glyph: draw nothing here.
    pub const WIDE_SPACER: u16 = 1 << 7;
    /// Inside the current selection.
    pub const SELECTED: u16 = 1 << 8;
    /// Part of a hyperlink (OSC 8).
    pub const HYPERLINK: u16 = 1 << 9;
    /// Overline.
    pub const OVERLINE: u16 = 1 << 10;
    /// Blink (rendered statically).
    pub const BLINK: u16 = 1 << 11;

    /// True if every bit in `mask` is set.
    #[must_use]
    pub const fn has(self, mask: u16) -> bool {
        self.0 & mask == mask
    }

    /// Set the bits in `mask`.
    pub fn insert(&mut self, mask: u16) {
        self.0 |= mask;
    }
}

/// One grid cell. Inverse video is already applied to `fg` and `bg`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cell {
    /// The cell's grapheme cluster. Empty means blank.
    pub text: CellText,
    /// Resolved foreground color.
    pub fg: Rgb,
    /// Resolved background color.
    pub bg: Rgb,
    /// Attributes.
    pub flags: CellFlags,
}

/// Grapheme storage that avoids a heap allocation for the common case of a
/// single code point.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum CellText {
    /// Nothing drawn.
    #[default]
    Empty,
    /// A single code point.
    Char(char),
    /// A multi-code-point grapheme cluster (emoji ZWJ, combining marks).
    Cluster(Box<str>),
}

impl CellText {
    /// Append this cell's text to `out`.
    pub fn push_to(&self, out: &mut String) {
        match self {
            Self::Empty => {}
            Self::Char(c) => out.push(*c),
            Self::Cluster(s) => out.push_str(s),
        }
    }

    /// True if nothing is drawn.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }
}

/// Cursor shape.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CursorShape {
    /// Filled block.
    #[default]
    Block,
    /// Vertical bar.
    Bar,
    /// Underline.
    Underline,
    /// Hollow block, used when the pane is unfocused.
    HollowBlock,
}

/// Cursor state for the visible viewport.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cursor {
    /// Column in the viewport.
    pub col: u16,
    /// Row in the viewport.
    pub row: u16,
    /// Shape requested by the application.
    pub shape: CursorShape,
    /// False when hidden (DECTCEM) or scrolled out of view.
    pub visible: bool,
    /// Whether the application asked for blinking.
    pub blinking: bool,
    /// Cursor color override, if the application set one.
    pub color: Option<Rgb>,
}

/// Scrollback position, in rows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScrollInfo {
    /// Total rows including scrollback.
    pub total: u64,
    /// Offset of the viewport's top row from the top of scrollback.
    pub offset: u64,
    /// Rows in the viewport.
    pub len: u64,
}

impl ScrollInfo {
    /// True when the viewport shows the live bottom of the screen.
    #[must_use]
    pub const fn at_bottom(&self) -> bool {
        self.offset + self.len >= self.total
    }
}

/// A complete, owned snapshot of a terminal viewport.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Frame {
    /// Columns in the grid.
    pub cols: u16,
    /// Rows in the grid.
    pub rows: u16,
    /// Row-major cells; `len == cols * rows`.
    pub cells: Vec<Cell>,
    /// Cursor.
    pub cursor: Cursor,
    /// Default background, for padding around the grid.
    pub background: Rgb,
    /// Default foreground.
    pub foreground: Rgb,
    /// Window title set by the application (OSC 0/2).
    pub title: String,
    /// Working directory reported by the shell (OSC 7), if any.
    pub cwd: Option<String>,
    /// True while the alternate screen is active (vim, htop, less).
    pub alt_screen: bool,
    /// True when the application has enabled mouse reporting.
    pub mouse_tracking: bool,
    /// Scrollback position.
    pub scroll: ScrollInfo,
    /// Increases every time a new frame is published.
    pub generation: u64,
}

impl Frame {
    /// The cell at `(col, row)`, if in range.
    #[must_use]
    pub fn cell(&self, col: u16, row: u16) -> Option<&Cell> {
        if col >= self.cols || row >= self.rows {
            return None;
        }
        self.cells.get(usize::from(row) * usize::from(self.cols) + usize::from(col))
    }

    /// The text of one row, with trailing blanks trimmed. Wide spacers are
    /// skipped so wide glyphs appear once.
    #[must_use]
    pub fn row_text(&self, row: u16) -> String {
        let mut out = String::new();
        for col in 0..self.cols {
            let Some(cell) = self.cell(col, row) else { break };
            if cell.flags.has(CellFlags::WIDE_SPACER) {
                continue;
            }
            if cell.text.is_empty() {
                out.push(' ');
            } else {
                cell.text.push_to(&mut out);
            }
        }
        out.truncate(out.trim_end().len());
        out
    }

    /// All visible rows joined by newlines, trailing blank rows trimmed.
    #[must_use]
    pub fn text(&self) -> String {
        let rows: Vec<String> = (0..self.rows).map(|r| self.row_text(r)).collect();
        let mut s = rows.join("\n");
        s.truncate(s.trim_end().len());
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trip() {
        let c = Rgb::from_hex("#1e2a3b").unwrap();
        assert_eq!(c, Rgb::new(0x1e, 0x2a, 0x3b));
        assert_eq!(c.to_hex(), "#1e2a3b");
        assert!(Rgb::from_hex("#12345").is_none());
        assert!(Rgb::from_hex("zzzzzz").is_none());
    }

    #[test]
    fn row_text_skips_wide_spacers_and_trims() {
        let mut f = Frame { cols: 4, rows: 1, ..Frame::default() };
        f.cells = vec![Cell::default(); 4];
        f.cells[0].text = CellText::Char('漢');
        f.cells[0].flags.insert(CellFlags::WIDE);
        f.cells[1].flags.insert(CellFlags::WIDE_SPACER);
        f.cells[2].text = CellText::Char('a');
        assert_eq!(f.row_text(0), "漢a");
        assert_eq!(f.text(), "漢a");
    }
}
