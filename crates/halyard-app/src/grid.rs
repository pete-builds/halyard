//! Window-independent layout: cell geometry and the runs a row is drawn with.
//!
//! Everything here is plain data so it can be unit tested without a window.

use halyard_term::{CellFlags, CellText, Frame, Rgb};

/// Size of one cell in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellSize {
    /// Width (the advance of `M`).
    pub width: f32,
    /// Height (the line height).
    pub height: f32,
}

/// Round a length to whole device pixels so cell edges never straddle a
/// pixel (which would show seams between background quads).
#[must_use]
pub fn snap_to_device(len: f32, scale: f32) -> f32 {
    let scale = if scale > 0.0 { scale } else { 1.0 };
    ((len * scale).round() / scale).max(1.0 / scale)
}

fn to_cells(len: f32, cell: f32) -> u16 {
    if !(len.is_finite() && cell > 0.0) {
        return 1;
    }
    let n = (len / cell).floor();
    if n < 1.0 {
        1
    } else if n >= f32::from(u16::MAX) {
        u16::MAX
    } else {
        // In range and integral, so the conversion is exact.
        n as u16
    }
}

/// How many whole cells fit in an area (at least one each way).
#[must_use]
pub fn grid_dims(width: f32, height: f32, cell: CellSize) -> (u16, u16) {
    (to_cells(width, cell.width), to_cells(height, cell.height))
}

/// The cell under a point given relative to the grid origin, clamped to the
/// grid.
#[must_use]
pub fn cell_at(x: f32, y: f32, cell: CellSize, cols: u16, rows: u16) -> (u16, u16) {
    let clamp = |v: f32, size: f32, count: u16| {
        let last = count.saturating_sub(1);
        if !(v.is_finite() && v > 0.0 && size > 0.0) {
            return 0;
        }
        let n = (v / size).floor();
        if n >= f32::from(last) {
            last
        } else {
            // Non-negative, integral and below `last`, so exact.
            n as u16
        }
    };
    (clamp(x, cell.width, cols), clamp(y, cell.height, rows))
}

/// A horizontal span of cells sharing a background color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BgSpan {
    /// First column.
    pub col: u16,
    /// Number of cells.
    pub len: u16,
    /// Color.
    pub color: Rgb,
}

/// Background spans of one row, merged across equal neighbors. Cells with
/// the default background are left out (the whole grid is cleared to it).
#[must_use]
pub fn row_backgrounds(frame: &Frame, row: u16) -> Vec<BgSpan> {
    let mut spans: Vec<BgSpan> = Vec::new();
    for col in 0..frame.cols {
        let Some(cell) = frame.cell(col, row) else { break };
        if cell.bg == frame.background {
            continue;
        }
        match spans.last_mut() {
            Some(last) if last.color == cell.bg && last.col + last.len == col => last.len += 1,
            _ => spans.push(BgSpan { col, len: 1, color: cell.bg }),
        }
    }
    spans
}

/// Text attributes shared by every cell of a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunStyle {
    /// Text color.
    pub fg: Rgb,
    /// Bold weight.
    pub bold: bool,
    /// Italic.
    pub italic: bool,
    /// Underlined.
    pub underline: bool,
    /// Struck through.
    pub strikethrough: bool,
    /// Drawn dimmer.
    pub faint: bool,
}

/// A piece of a row shaped and drawn as one text run, starting at `col`.
/// Each narrow cell is one character of `text`, so glyph `i` belongs at
/// column `col + i`. A wide glyph or a multi-code-point cluster is always a
/// run of its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextRunSpec {
    /// First column.
    pub col: u16,
    /// Cells covered.
    pub cells: u16,
    /// The text.
    pub text: String,
    /// Attributes.
    pub style: RunStyle,
}

fn style_of(cell: &halyard_term::Cell, fg: Rgb) -> RunStyle {
    let f = cell.flags;
    RunStyle {
        fg,
        bold: f.has(CellFlags::BOLD),
        italic: f.has(CellFlags::ITALIC),
        underline: f.has(CellFlags::UNDERLINE),
        strikethrough: f.has(CellFlags::STRIKETHROUGH),
        faint: f.has(CellFlags::FAINT),
    }
}

/// Text runs of one row. Runs break where the style changes, around wide
/// glyphs and clusters, and at blanks (a blank draws nothing unless it is
/// underlined or struck through). `cursor_fg` recolors the text of one
/// column, used to keep the glyph under a block cursor readable.
#[must_use]
pub fn row_runs(frame: &Frame, row: u16, cursor_fg: Option<(u16, Rgb)>) -> Vec<TextRunSpec> {
    let mut runs: Vec<TextRunSpec> = Vec::new();
    let mut open = false;
    for col in 0..frame.cols {
        let Some(cell) = frame.cell(col, row) else { break };
        if cell.flags.has(CellFlags::WIDE_SPACER) {
            open = false;
            continue;
        }
        let fg = match cursor_fg {
            Some((c, color)) if c == col => color,
            _ => cell.fg,
        };
        let style = style_of(cell, fg);
        let invisible = cell.flags.has(CellFlags::INVISIBLE);
        let ch = match (&cell.text, invisible) {
            (CellText::Char(c), false) if *c != ' ' => Some(Ok(*c)),
            (CellText::Cluster(s), false) => Some(Err(s.as_ref())),
            _ => None,
        };
        let decorated = style.underline || style.strikethrough;
        let Some(ch) = ch else {
            // A blank: extend a decorated run with a space, otherwise end it.
            if decorated
                && open
                && let Some(last) = runs.last_mut()
                && last.style == style
            {
                last.text.push(' ');
                last.cells += 1;
            } else if decorated {
                runs.push(TextRunSpec { col, cells: 1, text: " ".to_owned(), style });
                open = true;
            } else {
                open = false;
            }
            continue;
        };
        let wide = cell.flags.has(CellFlags::WIDE);
        match ch {
            Ok(c) if !wide => {
                if open
                    && let Some(last) = runs.last_mut()
                    && last.style == style
                {
                    last.text.push(c);
                    last.cells += 1;
                } else {
                    runs.push(TextRunSpec { col, cells: 1, text: c.to_string(), style });
                    open = true;
                }
            }
            other => {
                let text = match other {
                    Ok(c) => c.to_string(),
                    Err(s) => s.to_owned(),
                };
                runs.push(TextRunSpec { col, cells: if wide { 2 } else { 1 }, text, style });
                open = false;
            }
        }
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;
    use halyard_term::Cell;

    fn frame(cols: u16, text: &str) -> Frame {
        let mut f = Frame { cols, rows: 1, ..Frame::default() };
        f.background = Rgb::new(0, 0, 0);
        f.foreground = Rgb::new(200, 200, 200);
        f.cells = vec![Cell { fg: f.foreground, bg: f.background, ..Cell::default() }; cols.into()];
        for (i, c) in text.chars().enumerate() {
            if c != ' ' {
                f.cells[i].text = CellText::Char(c);
            }
        }
        f
    }

    #[test]
    fn snapping_rounds_to_device_pixels() {
        assert!((snap_to_device(8.43, 2.0) - 8.5).abs() < 1e-6);
        assert!((snap_to_device(8.43, 1.0) - 8.0).abs() < 1e-6);
        assert!((snap_to_device(0.1, 1.0) - 1.0).abs() < 1e-6);
        assert!((snap_to_device(8.0, 0.0) - 8.0).abs() < 1e-6);
    }

    #[test]
    fn grid_dims_floor_and_clamp() {
        let cell = CellSize { width: 8.0, height: 16.0 };
        assert_eq!(grid_dims(800.0, 480.0, cell), (100, 30));
        assert_eq!(grid_dims(807.9, 495.9, cell), (100, 30));
        assert_eq!(grid_dims(3.0, 3.0, cell), (1, 1));
        assert_eq!(grid_dims(f32::NAN, 100.0, cell), (1, 6));
        assert_eq!(grid_dims(1e9, 1e9, cell), (u16::MAX, u16::MAX));
    }

    #[test]
    fn cell_at_maps_and_clamps() {
        let cell = CellSize { width: 8.0, height: 16.0 };
        assert_eq!(cell_at(0.0, 0.0, cell, 80, 24), (0, 0));
        assert_eq!(cell_at(7.9, 15.9, cell, 80, 24), (0, 0));
        assert_eq!(cell_at(8.0, 16.0, cell, 80, 24), (1, 1));
        assert_eq!(cell_at(-5.0, -5.0, cell, 80, 24), (0, 0));
        assert_eq!(cell_at(10_000.0, 10_000.0, cell, 80, 24), (79, 23));
    }

    #[test]
    fn backgrounds_merge_equal_neighbors_and_skip_default() {
        let mut f = frame(6, "");
        let red = Rgb::new(255, 0, 0);
        let blue = Rgb::new(0, 0, 255);
        f.cells[1].bg = red;
        f.cells[2].bg = red;
        f.cells[3].bg = blue;
        f.cells[5].bg = red;
        assert_eq!(
            row_backgrounds(&f, 0),
            vec![
                BgSpan { col: 1, len: 2, color: red },
                BgSpan { col: 3, len: 1, color: blue },
                BgSpan { col: 5, len: 1, color: red },
            ]
        );
    }

    #[test]
    fn runs_split_on_style_and_blanks() {
        let mut f = frame(12, "ab cd efgh");
        f.cells[1].flags.insert(CellFlags::BOLD);
        let red = Rgb::new(255, 0, 0);
        f.cells[7].fg = red;
        f.cells[8].fg = red;
        let runs = row_runs(&f, 0, None);
        let summary: Vec<(u16, &str, bool, Rgb)> =
            runs.iter().map(|r| (r.col, r.text.as_str(), r.style.bold, r.style.fg)).collect();
        let fg = f.foreground;
        assert_eq!(
            summary,
            vec![
                (0, "a", false, fg),
                (1, "b", true, fg),
                (3, "cd", false, fg),
                (6, "e", false, fg),
                (7, "fg", false, red),
                (9, "h", false, fg),
            ]
        );
    }

    #[test]
    fn wide_glyphs_and_clusters_are_their_own_runs() {
        let mut f = frame(6, "a");
        f.cells[1].text = CellText::Char('漢');
        f.cells[1].flags.insert(CellFlags::WIDE);
        f.cells[2].flags.insert(CellFlags::WIDE_SPACER);
        f.cells[3].text = CellText::Char('b');
        f.cells[4].text = CellText::Cluster("e\u{301}".into());
        f.cells[5].text = CellText::Char('c');
        let runs = row_runs(&f, 0, None);
        let cols: Vec<(u16, u16, &str)> =
            runs.iter().map(|r| (r.col, r.cells, r.text.as_str())).collect();
        assert_eq!(
            cols,
            vec![(0, 1, "a"), (1, 2, "漢"), (3, 1, "b"), (4, 1, "e\u{301}"), (5, 1, "c")]
        );
    }

    #[test]
    fn underlined_blanks_stay_in_the_run() {
        let mut f = frame(5, "a b");
        for cell in &mut f.cells[..3] {
            cell.flags.insert(CellFlags::UNDERLINE);
        }
        let runs = row_runs(&f, 0, None);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text, "a b");
        assert!(runs[0].style.underline);
    }

    #[test]
    fn invisible_cells_draw_nothing() {
        let mut f = frame(3, "abc");
        f.cells[1].flags.insert(CellFlags::INVISIBLE);
        let texts: Vec<String> = row_runs(&f, 0, None).into_iter().map(|r| r.text).collect();
        assert_eq!(texts, vec!["a", "c"]);
    }

    #[test]
    fn cursor_recolors_one_cell() {
        let f = frame(3, "abc");
        let ink = Rgb::new(1, 2, 3);
        let runs = row_runs(&f, 0, Some((1, ink)));
        let parts: Vec<(&str, Rgb)> = runs.iter().map(|r| (r.text.as_str(), r.style.fg)).collect();
        assert_eq!(parts, vec![("a", f.foreground), ("b", ink), ("c", f.foreground)]);
    }
}
