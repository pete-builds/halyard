# Themes

A theme is plain data: terminal colors for the grid, UI tokens for the chrome around it, and
an appearance. The library is `crates/halyard-config` (`theme.rs`). Wiring themes into the
app UI is pending on the app crate.

## Bundled themes

All six palettes are original to Halyard.

| Name | Appearance | Notes |
|------|------------|-------|
| Halyard Night | dark | Default dark. Navy ground. |
| Halyard Day | light | Default light. Warm paper ground. |
| Harbor Fog | dark | Low-saturation grey blue. |
| Kelp | dark | Deep green. |
| Signal Flare | dark | High contrast: every ANSI color reaches 4.5:1. |
| Paper Chart | light | High contrast: every ANSI color reaches 4.5:1. |

Select one in `[theme]` (see [configuration.md](configuration.md)). Lookup ignores case.

## Tokens

### `[terminal]`

| Token | Meaning |
|-------|---------|
| `foreground` | Default text color. Required. |
| `background` | Default cell background. Required. |
| `cursor` | Cursor color. Default: `foreground`. |
| `cursor_text` | Glyph color under a block cursor. Default: `background`. |
| `selection_bg` | Background of selected cells. Default: `mix(background, foreground, 0.25)`. |
| `selection_fg` | Text color of selected cells. Default: `foreground`. |
| `ansi` | The 16 ANSI colors, indexes 0 to 15 (0 to 7 normal, 8 to 15 bright). Required. |

`ansi` is either an array of 16 `"#rrggbb"` strings or a table with the keys `black`, `red`,
`green`, `yellow`, `blue`, `magenta`, `cyan`, `white` and the same names prefixed
`bright_`. Export writes the array form.

### `[ui]`

| Token | Meaning |
|-------|---------|
| `surface` | Base background of windows and the tab bar area. |
| `surface_raised` | Elements that sit above the surface: palette, popovers, menus. |
| `surface_sunken` | Recessed areas: input fields, the strip behind tabs. |
| `border` | Dividers between panes and panel outlines. |
| `border_focused` | Outline of the focused pane or control. |
| `text` | Primary chrome text. |
| `text_muted` | Secondary text: hints, timestamps, inactive labels. |
| `accent` | Highlight: active toggles, links, primary buttons. |
| `accent_text` | Text drawn on `accent`. |
| `tab_active_bg` | Background of the active tab. |
| `tab_inactive_bg` | Background of inactive tabs. |
| `warning` | Needs care, nothing failed. |
| `danger` | An error or a destructive action. |
| `success` | A finished or healthy state. |
| `attention` | An agent is waiting for the user. Must stand out from `warning`. |

### Top level

| Key | Meaning |
|-----|---------|
| `name` | Display name, unique (case-insensitive). Required. |
| `appearance` | `"dark"` or `"light"`. Default: guessed from `background` (dark when its relative luminance is below 0.179). |
| `high_contrast` | `true` holds the theme to the stricter ANSI rule below. Default `false`. |

Every `[ui]` token is optional in a user theme. Missing ones are derived from the terminal
colors with the algorithm below. Unknown keys are errors, so typos do not pass silently.

## Contrast rule

Measured with the WCAG 2.x relative luminance and contrast ratio formulas, implemented in
`theme.rs`, against `terminal.background`:

* `foreground`: at least 4.5 in every theme.
* ANSI 1 to 7 and 9 to 15: at least 3.0, or 4.5 when `high_contrast = true`.
* Exempt: in a dark theme, ANSI 0 and 8 (black-ish), because they are meant to sit close to
  the background. In a light theme, ANSI 7 and 15 (white-ish), for the same reason.
* Also checked: `selection_fg` on `selection_bg` (3.0), `ui.text` on `ui.surface` (4.5),
  `ui.text_muted` on `ui.surface` (3.0), `ui.accent_text` on `ui.accent` (4.5).

A test runs this over every bundled theme. A second test feeds it a deliberately bad palette
and requires it to fail, so the check cannot pass by being broken. `check_contrast` is a
library function; user themes are not rejected for failing it, so the app can warn instead.

## Import and export

**Halyard TOML.** `Theme::to_toml` writes every token. `Theme::from_toml` reads it back to an
equal theme (a test round-trips all bundled themes). User themes are `*.toml` files in the
`themes/` folder next to `config.toml`; a user theme with the same name as a bundled one
replaces it. A file that fails to parse is reported with its path, line and key, and does not
stop the others from loading.

**Ghostty format.** `Theme::from_ghostty(name, text)` reads `key = value` lines: `palette =
N=#rrggbb` (N 0 to 15, higher indexes ignored), `background`, `foreground`, `cursor-color`,
`cursor-text`, `selection-background`, `selection-foreground`. Comments and other keys are
ignored. `background` and `foreground` are required. Palette slots the file omits take the
classic xterm colors. The appearance is guessed from the background.

### Derivation of `ui` tokens

With `bg` and `fg` the terminal background and foreground, and `mix(a, b, t) = a + t*(b-a)`
per channel, rounded and clamped:

| Token | Value |
|-------|-------|
| `surface` | `bg` |
| `surface_raised` | `mix(bg, fg, 0.06)` |
| `surface_sunken` | `mix(bg, fg, -0.05)` (away from `fg`) |
| `border` | `mix(bg, fg, 0.18)` |
| `border_focused`, `accent` | ANSI 4 (blue) |
| `text` | `fg` |
| `text_muted` | `mix(bg, fg, 0.60)` |
| `accent_text` | whichever of `bg` and `fg` has more contrast on `accent` |
| `tab_active_bg` | `mix(bg, fg, 0.12)` |
| `tab_inactive_bg` | `surface_sunken` |
| `warning` | ANSI 3 (yellow) |
| `danger` | ANSI 1 (red) |
| `success` | ANSI 2 (green) |
| `attention` | ANSI 5 (magenta) |

## Handing a theme to the engine

`Theme::palette()` returns a `TerminalPalette`: foreground, background, cursor, cursor text,
the 16 ANSI colors, and selection colors, as plain `Rgb` values. The engine crate defines its
own palette type and converts from this; there is no dependency in that direction.
