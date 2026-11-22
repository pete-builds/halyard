//! Token-driven themes: colors, contrast checking, import and export.
//!
//! A [`Theme`] is plain data in three parts: `terminal` colors the engine
//! paints cells with, `ui` tokens the chrome (tabs, borders, panels) uses, and
//! an [`Appearance`]. See `docs/themes.md` for what every token means.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use halyard_term::frame::Rgb;
use serde::{Deserialize, Serialize};

use crate::fsread::{Refusal, read_regular};
use crate::locate::{Index, parse_doc, value_free};

/// Whether a theme is meant for a dark or a light window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    /// Dark background, light text.
    Dark,
    /// Light background, dark text.
    Light,
}

/// Names of the 16 ANSI slots, in index order.
pub const ANSI_NAMES: [&str; 16] = [
    "black",
    "red",
    "green",
    "yellow",
    "blue",
    "magenta",
    "cyan",
    "white",
    "bright_black",
    "bright_red",
    "bright_green",
    "bright_yellow",
    "bright_blue",
    "bright_magenta",
    "bright_cyan",
    "bright_white",
];

/// Colors the terminal grid is painted with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalColors {
    /// Default text color (SGR 39).
    pub foreground: Rgb,
    /// Default cell background (SGR 49).
    pub background: Rgb,
    /// Cursor block or bar color.
    pub cursor: Rgb,
    /// Color of the glyph under a block cursor.
    pub cursor_text: Rgb,
    /// Background of selected cells.
    pub selection_bg: Rgb,
    /// Text color of selected cells.
    pub selection_fg: Rgb,
    /// The 16 ANSI colors: 0 to 7 normal, 8 to 15 bright.
    pub ansi: [Rgb; 16],
}

/// Colors for the window chrome around the grid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UiColors {
    /// Base background of windows and the tab bar area.
    pub surface: Rgb,
    /// Background of elements that sit above the surface: palette, popovers, menus.
    pub surface_raised: Rgb,
    /// Background of recessed areas: input fields, the inactive strip behind tabs.
    pub surface_sunken: Rgb,
    /// Dividers between panes and panel outlines.
    pub border: Rgb,
    /// Outline of the focused pane or control.
    pub border_focused: Rgb,
    /// Primary chrome text.
    pub text: Rgb,
    /// Secondary chrome text: hints, timestamps, inactive labels.
    pub text_muted: Rgb,
    /// Brand and selection highlight: active toggles, links, primary buttons.
    pub accent: Rgb,
    /// Text drawn on top of `accent`.
    pub accent_text: Rgb,
    /// Background of the active tab.
    pub tab_active_bg: Rgb,
    /// Background of inactive tabs.
    pub tab_inactive_bg: Rgb,
    /// Something needs care but nothing failed.
    pub warning: Rgb,
    /// An error or a destructive action.
    pub danger: Rgb,
    /// A finished or healthy state.
    pub success: Rgb,
    /// An agent is waiting for the user. Must stand out from `warning`.
    pub attention: Rgb,
}

/// A complete theme.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Theme {
    /// Display name, unique within a registry (case-insensitive).
    pub name: String,
    /// Dark or light.
    pub appearance: Appearance,
    /// High-contrast themes are held to a stricter contrast rule (see [`check_contrast`]).
    pub high_contrast: bool,
    /// Terminal grid colors.
    pub terminal: TerminalColors,
    /// Chrome colors.
    pub ui: UiColors,
}

/// Plain-data palette for the engine: nothing here depends on the engine crate's own type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalPalette {
    /// Default foreground.
    pub foreground: Rgb,
    /// Default background.
    pub background: Rgb,
    /// Cursor color.
    pub cursor: Rgb,
    /// Glyph color under a block cursor.
    pub cursor_text: Rgb,
    /// The 16 ANSI colors.
    pub ansi: [Rgb; 16],
    /// Selection background.
    pub selection_bg: Rgb,
    /// Selection foreground.
    pub selection_fg: Rgb,
}

/// A problem reading, importing or validating a theme.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThemeError {
    /// File the theme came from, when known.
    pub file: Option<PathBuf>,
    /// Dotted key the problem is about, when known.
    pub key: Option<String>,
    /// 1-based source line, when known.
    pub line: Option<usize>,
    /// What is wrong.
    pub message: String,
}

impl fmt::Display for ThemeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(file) = &self.file {
            write!(f, "{}", file.display())?;
            if let Some(l) = self.line {
                write!(f, ":{l}")?;
            }
            write!(f, ": ")?;
        } else if let Some(l) = self.line {
            write!(f, "line {l}: ")?;
        }
        if let Some(k) = &self.key {
            write!(f, "key `{k}`: ")?;
        }
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ThemeError {}

/// A non-fatal problem while importing a theme: something was skipped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThemeWarning {
    /// The key the warning is about.
    pub key: String,
    /// 1-based source line.
    pub line: Option<usize>,
    /// What was skipped and why.
    pub message: String,
}

impl fmt::Display for ThemeWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            Some(l) => write!(f, "line {l}: key `{}`: {}", self.key, self.message),
            None => write!(f, "key `{}`: {}", self.key, self.message),
        }
    }
}

impl ThemeError {
    fn new(message: impl Into<String>) -> Self {
        Self { file: None, key: None, line: None, message: message.into() }
    }

    fn at(key: &str, line: Option<usize>, message: impl Into<String>) -> Self {
        Self { file: None, key: Some(key.to_string()), line, message: message.into() }
    }

    fn with_file(mut self, file: &Path) -> Self {
        self.file = Some(file.to_path_buf());
        self
    }
}

// ---------------------------------------------------------------------------
// Contrast (WCAG 2.x)
// ---------------------------------------------------------------------------

/// WCAG 2.1 relative luminance of an sRGB color, in `0.0..=1.0`. The linearization
/// threshold is 0.04045 (the sRGB standard's value; WCAG 2.0 printed 0.03928).
#[must_use]
pub fn relative_luminance(c: Rgb) -> f64 {
    fn lin(v: u8) -> f64 {
        let s = f64::from(v) / 255.0;
        if s <= 0.04045 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
    }
    0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
}

/// WCAG contrast ratio between two colors, in `1.0..=21.0`. Order does not matter.
#[must_use]
pub fn contrast_ratio(a: Rgb, b: Rgb) -> f64 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// One failed contrast requirement.
#[derive(Clone, Debug, PartialEq)]
pub struct ContrastFailure {
    /// What was measured, such as `terminal.ansi[1] (red)`.
    pub what: String,
    /// The measured ratio.
    pub ratio: f64,
    /// The ratio that was required.
    pub required: f64,
}

impl fmt::Display for ContrastFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: contrast {:.2} is below the required {:.1}",
            self.what, self.ratio, self.required
        )
    }
}

/// Check a theme against Halyard's contrast rule and return every failure.
///
/// The rule, measured against `terminal.background`:
///
/// * `foreground`: at least 4.5 in every theme.
/// * ANSI colors 1 to 7 and 9 to 15: at least 3.0, or 4.5 when the theme is
///   `high_contrast`.
/// * Exemptions: in a dark theme colors 0 and 8 (black-ish) are not checked,
///   because they are meant to sit close to the background. In a light theme
///   colors 7 and 15 (white-ish) are not checked, for the same reason.
///
/// Also checked: `selection_fg` on `selection_bg` (3.0), `ui.text` on
/// `ui.surface` (4.5), `ui.text_muted` on `ui.surface` (3.0), and
/// `ui.accent_text` on `ui.accent` (4.5).
#[must_use]
pub fn check_contrast(theme: &Theme) -> Vec<ContrastFailure> {
    let t = &theme.terminal;
    let mut out = Vec::new();
    let mut need = |what: String, fg: Rgb, bg: Rgb, required: f64| {
        let ratio = contrast_ratio(fg, bg);
        if ratio < required {
            out.push(ContrastFailure { what, ratio, required });
        }
    };
    need("terminal.foreground".into(), t.foreground, t.background, 4.5);
    let ansi_required = if theme.high_contrast { 4.5 } else { 3.0 };
    for (i, c) in t.ansi.iter().enumerate() {
        let exempt = match theme.appearance {
            Appearance::Dark => i == 0 || i == 8,
            Appearance::Light => i == 7 || i == 15,
        };
        if !exempt {
            need(
                format!("terminal.ansi[{i}] ({})", ANSI_NAMES[i]),
                *c,
                t.background,
                ansi_required,
            );
        }
    }
    need("terminal.selection_fg on selection_bg".into(), t.selection_fg, t.selection_bg, 3.0);
    need("ui.text on ui.surface".into(), theme.ui.text, theme.ui.surface, 4.5);
    need("ui.text_muted on ui.surface".into(), theme.ui.text_muted, theme.ui.surface, 3.0);
    need("ui.accent_text on ui.accent".into(), theme.ui.accent_text, theme.ui.accent, 4.5);
    out
}

// ---------------------------------------------------------------------------
// Derivation
// ---------------------------------------------------------------------------

/// Blend `a` toward `b` by `t` per channel: `a + (b - a) * t`, rounded and
/// clamped. A negative `t` moves `a` away from `b`.
#[must_use]
pub fn mix(a: Rgb, b: Rgb, t: f64) -> Rgb {
    let ch = |x: u8, y: u8| {
        let v = f64::from(x) + (f64::from(y) - f64::from(x)) * t;
        v.round().clamp(0.0, 255.0) as u8
    };
    Rgb::new(ch(a.r, b.r), ch(a.g, b.g), ch(a.b, b.b))
}

/// Luminance at which black text and white text have equal contrast. A
/// background below it reads as dark.
const DARK_LUMINANCE_THRESHOLD: f64 = 0.179;

/// Guess the appearance from a background color: dark when its relative
/// luminance is below 0.179.
#[must_use]
pub fn appearance_of_background(bg: Rgb) -> Appearance {
    if relative_luminance(bg) < DARK_LUMINANCE_THRESHOLD {
        Appearance::Dark
    } else {
        Appearance::Light
    }
}

/// Derive the `ui` tokens from terminal colors. Used by the Ghostty importer
/// and for any user theme that leaves `[ui]` out. With `bg` and `fg` the
/// terminal background and foreground:
///
/// | token | value |
/// |-------|-------|
/// | `surface` | `bg` |
/// | `surface_raised` | `mix(bg, fg, 0.06)` |
/// | `surface_sunken` | `mix(bg, fg, -0.05)` (away from `fg`, clamped) |
/// | `border` | `mix(bg, fg, 0.18)` |
/// | `border_focused`, `accent` | ANSI 4 (blue) |
/// | `text` | `fg` |
/// | `text_muted` | `mix(bg, fg, 0.60)` |
/// | `accent_text` | whichever of `bg` and `fg` has more contrast on `accent` |
/// | `tab_active_bg` | `mix(bg, fg, 0.12)` |
/// | `tab_inactive_bg` | `surface_sunken` |
/// | `warning` | ANSI 3 (yellow) |
/// | `danger` | ANSI 1 (red) |
/// | `success` | ANSI 2 (green) |
/// | `attention` | ANSI 5 (magenta), so it differs from `warning` |
#[must_use]
pub fn derive_ui(t: &TerminalColors) -> UiColors {
    let (bg, fg) = (t.background, t.foreground);
    let accent = t.ansi[4];
    let accent_text =
        if contrast_ratio(bg, accent) >= contrast_ratio(fg, accent) { bg } else { fg };
    let sunken = mix(bg, fg, -0.05);
    UiColors {
        surface: bg,
        surface_raised: mix(bg, fg, 0.06),
        surface_sunken: sunken,
        border: mix(bg, fg, 0.18),
        border_focused: accent,
        text: fg,
        text_muted: mix(bg, fg, 0.60),
        accent,
        accent_text,
        tab_active_bg: mix(bg, fg, 0.12),
        tab_inactive_bg: sunken,
        warning: t.ansi[3],
        danger: t.ansi[1],
        success: t.ansi[2],
        attention: t.ansi[5],
    }
}

// ---------------------------------------------------------------------------
// TOML format
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTheme {
    name: String,
    appearance: Option<Appearance>,
    #[serde(default)]
    high_contrast: bool,
    terminal: RawTerminal,
    #[serde(default)]
    ui: RawUi,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTerminal {
    foreground: String,
    background: String,
    cursor: Option<String>,
    cursor_text: Option<String>,
    selection_bg: Option<String>,
    selection_fg: Option<String>,
    ansi: RawAnsi,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawAnsi {
    List(Vec<String>),
    Named(Box<NamedAnsi>),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NamedAnsi {
    black: String,
    red: String,
    green: String,
    yellow: String,
    blue: String,
    magenta: String,
    cyan: String,
    white: String,
    bright_black: String,
    bright_red: String,
    bright_green: String,
    bright_yellow: String,
    bright_blue: String,
    bright_magenta: String,
    bright_cyan: String,
    bright_white: String,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawUi {
    surface: Option<String>,
    surface_raised: Option<String>,
    surface_sunken: Option<String>,
    border: Option<String>,
    border_focused: Option<String>,
    text: Option<String>,
    text_muted: Option<String>,
    accent: Option<String>,
    accent_text: Option<String>,
    tab_active_bg: Option<String>,
    tab_inactive_bg: Option<String>,
    warning: Option<String>,
    danger: Option<String>,
    success: Option<String>,
    attention: Option<String>,
}

#[derive(Serialize)]
struct ExportTheme {
    name: String,
    appearance: Appearance,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    high_contrast: bool,
    terminal: ExportTerminal,
    ui: ExportUi,
}

#[derive(Serialize)]
struct ExportTerminal {
    foreground: String,
    background: String,
    cursor: String,
    cursor_text: String,
    selection_bg: String,
    selection_fg: String,
    ansi: Vec<String>,
}

#[derive(Serialize)]
struct ExportUi {
    surface: String,
    surface_raised: String,
    surface_sunken: String,
    border: String,
    border_focused: String,
    text: String,
    text_muted: String,
    accent: String,
    accent_text: String,
    tab_active_bg: String,
    tab_inactive_bg: String,
    warning: String,
    danger: String,
    success: String,
    attention: String,
}

struct Ctx<'a> {
    ix: &'a Index,
}

impl Ctx<'_> {
    fn color(&self, section: &str, key: &str, value: &str) -> Result<Rgb, ThemeError> {
        Rgb::from_hex(value.trim()).ok_or_else(|| {
            ThemeError::at(
                &format!("{section}.{key}"),
                self.ix.line(&format!("{section}.{key}")),
                "is not a color; expected #rrggbb (the value is not shown)",
            )
        })
    }

    fn opt(
        &self,
        section: &str,
        key: &str,
        v: Option<&String>,
        default: Rgb,
    ) -> Result<Rgb, ThemeError> {
        v.map_or(Ok(default), |s| self.color(section, key, s))
    }
}

impl Theme {
    /// Parse a theme from Halyard's TOML theme format (see `docs/themes.md`).
    ///
    /// Optional terminal keys and any missing `ui` tokens are derived from the
    /// required colors. Unknown keys are errors, so typos do not pass silently.
    ///
    /// # Errors
    /// Returns a [`ThemeError`] naming the key and line of the first problem.
    pub fn from_toml(src: &str) -> Result<Theme, ThemeError> {
        let doc = parse_doc(src).map_err(|e| ThemeError {
            file: None,
            key: None,
            line: e.line,
            message: e.message,
        })?;
        let ix = Index::build(src, &doc, false);
        let raw: RawTheme = toml::from_str(src).map_err(|e| ThemeError {
            file: None,
            key: e.span().and_then(|s| ix.path_at(s.start)),
            line: e.span().map(|s| crate::locate::line_of_offset(src, s.start)),
            message: value_free(e.message()),
        })?;
        let cx = Ctx { ix: &ix };
        let t = &raw.terminal;
        let foreground = cx.color("terminal", "foreground", &t.foreground)?;
        let background = cx.color("terminal", "background", &t.background)?;
        let ansi = parse_ansi(&cx, &t.ansi)?;
        let terminal = TerminalColors {
            foreground,
            background,
            cursor: cx.opt("terminal", "cursor", t.cursor.as_ref(), foreground)?,
            cursor_text: cx.opt("terminal", "cursor_text", t.cursor_text.as_ref(), background)?,
            selection_bg: cx.opt(
                "terminal",
                "selection_bg",
                t.selection_bg.as_ref(),
                mix(background, foreground, 0.25),
            )?,
            selection_fg: cx.opt(
                "terminal",
                "selection_fg",
                t.selection_fg.as_ref(),
                foreground,
            )?,
            ansi,
        };
        let d = derive_ui(&terminal);
        let u = &raw.ui;
        let ui = UiColors {
            surface: cx.opt("ui", "surface", u.surface.as_ref(), d.surface)?,
            surface_raised: cx.opt(
                "ui",
                "surface_raised",
                u.surface_raised.as_ref(),
                d.surface_raised,
            )?,
            surface_sunken: cx.opt(
                "ui",
                "surface_sunken",
                u.surface_sunken.as_ref(),
                d.surface_sunken,
            )?,
            border: cx.opt("ui", "border", u.border.as_ref(), d.border)?,
            border_focused: cx.opt(
                "ui",
                "border_focused",
                u.border_focused.as_ref(),
                d.border_focused,
            )?,
            text: cx.opt("ui", "text", u.text.as_ref(), d.text)?,
            text_muted: cx.opt("ui", "text_muted", u.text_muted.as_ref(), d.text_muted)?,
            accent: cx.opt("ui", "accent", u.accent.as_ref(), d.accent)?,
            accent_text: cx.opt("ui", "accent_text", u.accent_text.as_ref(), d.accent_text)?,
            tab_active_bg: cx.opt(
                "ui",
                "tab_active_bg",
                u.tab_active_bg.as_ref(),
                d.tab_active_bg,
            )?,
            tab_inactive_bg: cx.opt(
                "ui",
                "tab_inactive_bg",
                u.tab_inactive_bg.as_ref(),
                d.tab_inactive_bg,
            )?,
            warning: cx.opt("ui", "warning", u.warning.as_ref(), d.warning)?,
            danger: cx.opt("ui", "danger", u.danger.as_ref(), d.danger)?,
            success: cx.opt("ui", "success", u.success.as_ref(), d.success)?,
            attention: cx.opt("ui", "attention", u.attention.as_ref(), d.attention)?,
        };
        if raw.name.trim().is_empty() {
            return Err(ThemeError::at("name", ix.line("name"), "theme name must not be empty"));
        }
        Ok(Theme {
            name: raw.name,
            appearance: raw.appearance.unwrap_or_else(|| appearance_of_background(background)),
            high_contrast: raw.high_contrast,
            terminal,
            ui,
        })
    }

    /// Export the theme as TOML with every token written out. The output
    /// parses back to an equal theme.
    ///
    /// # Panics
    /// Never in practice: the exported structure is plain strings and arrays,
    /// which TOML can always represent.
    #[must_use]
    pub fn to_toml(&self) -> String {
        let h = |c: Rgb| c.to_hex();
        let (t, u) = (&self.terminal, &self.ui);
        let export = ExportTheme {
            name: self.name.clone(),
            appearance: self.appearance,
            high_contrast: self.high_contrast,
            terminal: ExportTerminal {
                foreground: h(t.foreground),
                background: h(t.background),
                cursor: h(t.cursor),
                cursor_text: h(t.cursor_text),
                selection_bg: h(t.selection_bg),
                selection_fg: h(t.selection_fg),
                ansi: t.ansi.iter().map(|c| c.to_hex()).collect(),
            },
            ui: ExportUi {
                surface: h(u.surface),
                surface_raised: h(u.surface_raised),
                surface_sunken: h(u.surface_sunken),
                border: h(u.border),
                border_focused: h(u.border_focused),
                text: h(u.text),
                text_muted: h(u.text_muted),
                accent: h(u.accent),
                accent_text: h(u.accent_text),
                tab_active_bg: h(u.tab_active_bg),
                tab_inactive_bg: h(u.tab_inactive_bg),
                warning: h(u.warning),
                danger: h(u.danger),
                success: h(u.success),
                attention: h(u.attention),
            },
        };
        toml::to_string_pretty(&export).expect("a struct of strings always serializes to TOML")
    }

    /// Import a Ghostty-format theme file, discarding warnings. See
    /// [`Theme::from_ghostty_with_warnings`], which reports what was skipped.
    ///
    /// # Errors
    /// Same as [`Theme::from_ghostty_with_warnings`].
    pub fn from_ghostty(name: &str, src: &str) -> Result<Theme, ThemeError> {
        Self::from_ghostty_with_warnings(name, src).map(|(t, _)| t)
    }

    /// Import a Ghostty-format theme file: `key = value` lines with
    /// `palette = N=color`, `background`, `foreground`, `cursor-color`,
    /// `cursor-text`, `selection-background` and `selection-foreground`.
    /// Other keys and `#` comment lines are ignored. Palette slots 0 to 15 that
    /// the file leaves out take the classic xterm defaults. The appearance is
    /// guessed from the background and the `ui` tokens come from [`derive_ui`].
    ///
    /// Color syntax. Ghostty's reference (<https://ghostty.org/docs/config/reference>,
    /// read 2026-11-22) says `background` and `foreground` take "either hex (`#RRGGBB` or
    /// `RRGGBB`) or a named X11 color", that `palette` takes `N=COLOR` with the same
    /// color forms, and that since 1.2.0 `selection-foreground` and `selection-background`
    /// "can also be set to `cell-foreground` ... or `cell-background`". So this importer
    /// accepts hex with or without `#`, a built-in subset of the X11 names (see
    /// `x11_color`; the full X11 table is not bundled), and treats the two `cell-*`
    /// words as "unset", which derives the selection colors. The reference does not
    /// list the names themselves, so the subset is the common ones, with X11 values
    /// (for example X11 `green` is `#00ff00` and `gray` is `#bebebe`).
    ///
    /// A color this importer cannot read on any key other than `background` and
    /// `foreground` becomes a [`ThemeWarning`] and the key is treated as unset, so one
    /// exotic value does not discard a whole theme file.
    ///
    /// # Errors
    /// Returns a [`ThemeError`] with the line number for an unreadable `background` or
    /// `foreground`, a `palette` entry that is not in `N=color` form, or when `background`
    /// or `foreground` is missing.
    pub fn from_ghostty_with_warnings(
        name: &str,
        src: &str,
    ) -> Result<(Theme, Vec<ThemeWarning>), ThemeError> {
        let mut ansi: [Option<Rgb>; 16] = [None; 16];
        let mut warnings = Vec::new();
        let (mut fg, mut bg, mut cursor, mut cursor_text, mut sel_bg, mut sel_fg) =
            (None, None, None, None, None, None);
        for (i, line) in src.lines().enumerate() {
            let n = i + 1;
            let l = line.trim();
            if l.is_empty() || l.starts_with('#') {
                continue;
            }
            let Some((k, v)) = l.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim().trim_matches('"'));
            let essential = |v: &str| {
                ghostty_color(v).ok_or_else(|| {
                    ThemeError::at(
                        k,
                        Some(n),
                        "is not a color Halyard can read (the value is not shown)",
                    )
                })
            };
            let mut optional = |v: &str, cell_is_unset: bool| -> Option<Rgb> {
                if cell_is_unset && matches!(v, "cell-background" | "cell-foreground") {
                    return None;
                }
                let c = ghostty_color(v);
                if c.is_none() {
                    warnings.push(ThemeWarning {
                        key: k.to_string(),
                        line: Some(n),
                        message:
                            "is not a color Halyard can read; ignored (the value is not shown)"
                                .to_string(),
                    });
                }
                c
            };
            match k {
                "palette" => {
                    let Some((idx, c)) = v.split_once('=') else {
                        return Err(ThemeError::at(
                            "palette",
                            Some(n),
                            "is not in N=color form (the value is not shown)",
                        ));
                    };
                    let idx: usize = idx.trim().parse().map_err(|_| {
                        ThemeError::at(
                            "palette",
                            Some(n),
                            "the palette index is not a number (the value is not shown)",
                        )
                    })?;
                    let c = optional(c.trim(), false);
                    if let (Some(slot), Some(c)) = (ansi.get_mut(idx), c) {
                        *slot = Some(c);
                    }
                }
                "background" => bg = Some(essential(v)?),
                "foreground" => fg = Some(essential(v)?),
                "cursor-color" => cursor = optional(v, true),
                "cursor-text" => cursor_text = optional(v, true),
                "selection-background" => sel_bg = optional(v, true),
                "selection-foreground" => sel_fg = optional(v, true),
                _ => {}
            }
        }
        let need = |v: Option<Rgb>, k: &str| {
            v.ok_or_else(|| ThemeError::at(k, None, "required key is missing"))
        };
        let background = need(bg, "background")?;
        let foreground = need(fg, "foreground")?;
        let mut full = XTERM_DEFAULT_ANSI;
        for (slot, got) in full.iter_mut().zip(ansi) {
            if let Some(c) = got {
                *slot = c;
            }
        }
        let terminal = TerminalColors {
            foreground,
            background,
            cursor: cursor.unwrap_or(foreground),
            cursor_text: cursor_text.unwrap_or(background),
            selection_bg: sel_bg.unwrap_or_else(|| mix(background, foreground, 0.25)),
            selection_fg: sel_fg.unwrap_or(foreground),
            ansi: full,
        };
        let ui = derive_ui(&terminal);
        let theme = Theme {
            name: name.to_string(),
            appearance: appearance_of_background(background),
            high_contrast: false,
            terminal,
            ui,
        };
        Ok((theme, warnings))
    }

    /// The engine-facing palette as plain data.
    #[must_use]
    pub fn palette(&self) -> TerminalPalette {
        let t = &self.terminal;
        TerminalPalette {
            foreground: t.foreground,
            background: t.background,
            cursor: t.cursor,
            cursor_text: t.cursor_text,
            ansi: t.ansi,
            selection_bg: t.selection_bg,
            selection_fg: t.selection_fg,
        }
    }
}

/// A color in Ghostty syntax: hex with or without `#`, or a common X11 name
/// (case-insensitive, spaces ignored). Not the full X11 table; see
/// [`Theme::from_ghostty_with_warnings`].
fn ghostty_color(v: &str) -> Option<Rgb> {
    if let Some(c) = Rgb::from_hex(v) {
        return Some(c);
    }
    let name: String =
        v.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_lowercase();
    let (r, g, b) = match name.as_str() {
        "black" => (0x00, 0x00, 0x00),
        "white" => (0xff, 0xff, 0xff),
        "red" => (0xff, 0x00, 0x00),
        "green" => (0x00, 0xff, 0x00),
        "blue" => (0x00, 0x00, 0xff),
        "yellow" => (0xff, 0xff, 0x00),
        "cyan" | "aqua" => (0x00, 0xff, 0xff),
        "magenta" | "fuchsia" => (0xff, 0x00, 0xff),
        "gray" | "grey" => (0xbe, 0xbe, 0xbe),
        "darkgray" | "darkgrey" => (0xa9, 0xa9, 0xa9),
        "lightgray" | "lightgrey" => (0xd3, 0xd3, 0xd3),
        "orange" => (0xff, 0xa5, 0x00),
        "purple" => (0xa0, 0x20, 0xf0),
        "pink" => (0xff, 0xc0, 0xcb),
        "brown" => (0xa5, 0x2a, 0x2a),
        _ => return None,
    };
    Some(Rgb::new(r, g, b))
}

/// Classic xterm colors, used for palette slots a Ghostty file leaves out.
const XTERM_DEFAULT_ANSI: [Rgb; 16] = [
    Rgb::new(0x00, 0x00, 0x00),
    Rgb::new(0xcd, 0x00, 0x00),
    Rgb::new(0x00, 0xcd, 0x00),
    Rgb::new(0xcd, 0xcd, 0x00),
    Rgb::new(0x00, 0x00, 0xee),
    Rgb::new(0xcd, 0x00, 0xcd),
    Rgb::new(0x00, 0xcd, 0xcd),
    Rgb::new(0xe5, 0xe5, 0xe5),
    Rgb::new(0x7f, 0x7f, 0x7f),
    Rgb::new(0xff, 0x00, 0x00),
    Rgb::new(0x00, 0xff, 0x00),
    Rgb::new(0xff, 0xff, 0x00),
    Rgb::new(0x5c, 0x5c, 0xff),
    Rgb::new(0xff, 0x00, 0xff),
    Rgb::new(0x00, 0xff, 0xff),
    Rgb::new(0xff, 0xff, 0xff),
];

fn parse_ansi(cx: &Ctx<'_>, raw: &RawAnsi) -> Result<[Rgb; 16], ThemeError> {
    let mut out = [Rgb::default(); 16];
    match raw {
        RawAnsi::List(list) => {
            if list.len() != 16 {
                return Err(ThemeError::at(
                    "terminal.ansi",
                    cx.ix.line("terminal.ansi"),
                    format!("expected 16 colors, found {}", list.len()),
                ));
            }
            for (i, s) in list.iter().enumerate() {
                out[i] = Rgb::from_hex(s.trim()).ok_or_else(|| {
                    ThemeError::at(
                        &format!("terminal.ansi[{i}]"),
                        cx.ix.line(&format!("terminal.ansi[{i}]")),
                        "is not a color; expected #rrggbb (the value is not shown)",
                    )
                })?;
            }
        }
        RawAnsi::Named(n) => {
            let values = [
                &n.black,
                &n.red,
                &n.green,
                &n.yellow,
                &n.blue,
                &n.magenta,
                &n.cyan,
                &n.white,
                &n.bright_black,
                &n.bright_red,
                &n.bright_green,
                &n.bright_yellow,
                &n.bright_blue,
                &n.bright_magenta,
                &n.bright_cyan,
                &n.bright_white,
            ];
            for (i, s) in values.iter().enumerate() {
                out[i] = cx.color("terminal.ansi", ANSI_NAMES[i], s)?;
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// Where a registry entry came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// Shipped inside Halyard.
    Bundled,
    /// Loaded from the user's themes directory.
    User,
}

/// A theme and where it came from.
#[derive(Clone, Debug)]
pub struct Entry {
    /// The theme.
    pub theme: Theme,
    /// Bundled or user.
    pub origin: Origin,
}

/// Name of the default dark theme.
pub const DEFAULT_DARK: &str = "Halyard Night";
/// Name of the default light theme.
pub const DEFAULT_LIGHT: &str = "Halyard Day";

const BUNDLED: [(&str, &str); 6] = [
    ("halyard-night.toml", include_str!("../themes/halyard-night.toml")),
    ("halyard-day.toml", include_str!("../themes/halyard-day.toml")),
    ("harbor-fog.toml", include_str!("../themes/harbor-fog.toml")),
    ("kelp.toml", include_str!("../themes/kelp.toml")),
    ("signal-flare.toml", include_str!("../themes/signal-flare.toml")),
    ("paper-chart.toml", include_str!("../themes/paper-chart.toml")),
];

/// The most `*.toml` files read from a themes directory; extras are ignored with a warning.
pub const MAX_THEME_FILES: usize = 512;

/// The set of themes available to pick from: bundled plus user themes.
#[derive(Clone, Debug)]
pub struct ThemeRegistry {
    entries: Vec<Entry>,
}

impl Default for ThemeRegistry {
    fn default() -> Self {
        Self::bundled()
    }
}

impl ThemeRegistry {
    /// A registry holding only the bundled themes.
    ///
    /// # Panics
    /// If an embedded theme file does not parse. A unit test guards this, so it
    /// cannot happen in a released build.
    #[must_use]
    pub fn bundled() -> Self {
        let entries = BUNDLED
            .iter()
            .map(|(file, src)| Entry {
                theme: Theme::from_toml(src)
                    .unwrap_or_else(|e| panic!("bundled theme {file} is invalid: {e}")),
                origin: Origin::Bundled,
            })
            .collect();
        Self { entries }
    }

    /// Add a theme. A theme with the same name (case-insensitive) replaces the
    /// existing one, so a user theme overrides a bundled theme of that name.
    pub fn insert(&mut self, theme: Theme, origin: Origin) {
        match self.entries.iter_mut().find(|e| e.theme.name.eq_ignore_ascii_case(&theme.name)) {
            Some(slot) => *slot = Entry { theme, origin },
            None => self.entries.push(Entry { theme, origin }),
        }
    }

    /// Load every `*.toml` file in `dir` as a user theme, in file name order.
    /// A missing directory is fine and adds nothing. A file that fails to read
    /// or parse is skipped and reported in the returned list; it never stops
    /// the others from loading.
    pub fn load_user_dir(&mut self, dir: &Path) -> Vec<ThemeError> {
        let mut errors = Vec::new();
        let rd = match fs::read_dir(dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return errors,
            Err(e) => {
                errors.push(
                    ThemeError::new(format!("cannot read themes directory: {e}")).with_file(dir),
                );
                return errors;
            }
        };
        let mut files: Vec<PathBuf> = rd
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "toml"))
            .collect();
        files.sort();
        if files.len() > MAX_THEME_FILES {
            errors.push(
                ThemeError::new(format!(
                    "{} theme files beyond the first {MAX_THEME_FILES} are ignored",
                    files.len() - MAX_THEME_FILES
                ))
                .with_file(dir),
            );
            files.truncate(MAX_THEME_FILES);
        }
        for file in files {
            let loaded = read_regular(&file).and_then(|bytes| {
                String::from_utf8(bytes)
                    .map_err(|_| Refusal::Io(std::io::Error::other("not valid UTF-8")))
            });
            match loaded {
                Ok(src) => match Theme::from_toml(&src) {
                    Ok(theme) => self.insert(theme, Origin::User),
                    Err(e) => errors.push(e.with_file(&file)),
                },
                Err(refusal) => errors.push(
                    ThemeError::new(format!("theme file {}; skipped", refusal.phrase()))
                        .with_file(&file),
                ),
            }
        }
        errors
    }

    /// Look a theme up by name, ignoring case.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Theme> {
        self.entry(name).map(|e| &e.theme)
    }

    /// Look an entry (theme plus origin) up by name, ignoring case.
    #[must_use]
    pub fn entry(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.theme.name.eq_ignore_ascii_case(name))
    }

    /// Every entry, in registration order (bundled first).
    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter()
    }

    /// Theme names in registration order.
    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        self.entries.iter().map(|e| e.theme.name.as_str()).collect()
    }

    /// The default theme for an appearance: Halyard Night for dark, Halyard Day for light.
    ///
    /// If a user theme with the same name replaced the bundled default, the
    /// replacement is returned, whatever its appearance.
    ///
    /// # Panics
    /// Never in practice: bundled themes cannot be removed, only replaced, so
    /// the default name always resolves.
    #[must_use]
    pub fn default_for(&self, appearance: Appearance) -> &Theme {
        let name = match appearance {
            Appearance::Dark => DEFAULT_DARK,
            Appearance::Light => DEFAULT_LIGHT,
        };
        self.get(name).expect("bundled default themes are always registered")
    }

    /// Find `name`, or fall back to [`Self::default_for`] when it is unknown.
    /// The boolean is `true` when the fallback was used, so the caller can warn.
    #[must_use]
    pub fn get_or_default(&self, name: &str, appearance: Appearance) -> (&Theme, bool) {
        match self.get(name) {
            Some(t) => (t, false),
            None => (self.default_for(appearance), true),
        }
    }
}
