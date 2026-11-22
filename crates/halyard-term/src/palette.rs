//! Terminal color palettes.

use crate::frame::Rgb;

/// The colors a terminal draws with: the 16 ANSI colors plus the defaults.
///
/// Indices 16 to 255 of the 256-color palette are the standard xterm color
/// cube and gray ramp, which the engine supplies. Programs can still override
/// any entry at runtime (OSC 4, 10, 11, 12); the palette is the baseline those
/// overrides start from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Palette {
    /// ANSI colors 0 to 15 (8 normal, then 8 bright).
    pub ansi: [Rgb; 16],
    /// Default text color.
    pub foreground: Rgb,
    /// Default background color.
    pub background: Rgb,
    /// Cursor color.
    pub cursor: Rgb,
    /// Background of selected cells.
    pub selection_background: Rgb,
    /// Text color of selected cells. `None` keeps each cell's own color.
    pub selection_foreground: Option<Rgb>,
}

impl Default for Palette {
    /// Halyard's built-in dark palette.
    fn default() -> Self {
        let c = |hex: u32| {
            let [_, r, g, b] = hex.to_be_bytes();
            Rgb::new(r, g, b)
        };
        Self {
            ansi: [
                c(0x1d_1f24),
                c(0xe0_6c75),
                c(0x98_c379),
                c(0xe5_c07b),
                c(0x61_afef),
                c(0xc6_78dd),
                c(0x56_b6c2),
                c(0xab_b2bf),
                c(0x5c_6370),
                c(0xef_7d86),
                c(0xa9_d48a),
                c(0xf0_d08f),
                c(0x74_bdf5),
                c(0xd6_8cec),
                c(0x6c_cad6),
                c(0xe6_e9ef),
            ],
            foreground: c(0xd7_dae0),
            background: c(0x16_181d),
            cursor: c(0xe6_e9ef),
            selection_background: c(0x3e_4a5e),
            selection_foreground: None,
        }
    }
}
