# Configuration

Halyard reads one TOML file, `config.toml`. Everything is optional: a missing file, an
empty file, or a file with only the keys you care about all work.

Print the fully commented default with `halyard config --print-default` (the text lives in
`crates/halyard-config/default-config.toml`, and a test proves it parses to exactly the
built-in defaults). The app wiring for that command is pending; the library function is
`halyard_config::default_config_toml()`.

## Where the file lives

1. `$HALYARD_CONFIG`, if set and not empty.
2. `$XDG_CONFIG_HOME/halyard/config.toml`, if `XDG_CONFIG_HOME` is set to an absolute path.
3. `~/.config/halyard/config.toml`.

The third form is used on macOS as well as Linux, not `~/Library/Application Support`, so a
dotfiles repo works the same on both. User themes live in a `themes/` folder next to the
config file (see [themes.md](themes.md)).

## Errors and warnings

* An unknown key is a warning: it is ignored and reported with its line number, with a
  "did you mean" hint for near misses.
* An invalid value is an error that names the key and the line, for example
  `config.toml:3: key `font.size`: 3 is out of range; expected 4 to 128`.
* `ConfigWatcher` reports a reload after a change settles (debounced): the new config, or the
  error for a broken edit, so the app can keep the previous settings and show it. Deleting the
  file reloads the defaults.

## `[font]`

| Key | Default | Meaning |
|-----|---------|---------|
| `family` | `"monospace"` | Font family, or `monospace` for the system monospace font. |
| `size` | `13.0` | Size in points, 4 to 128. |
| `line_height` | `1.2` | Multiple of the font size, 0.8 to 3.0. |
| `bold_is_bright` | `false` | Draw bold text in the bright ANSI colors too. |

## `[window]`

| Key | Default | Meaning |
|-----|---------|---------|
| `padding_x`, `padding_y` | `8.0`, `6.0` | Space between window edge and grid, in points, 0 to 200. |
| `opacity` | `1.0` | Background opacity, 0.0 to 1.0. |
| `blur` | `false` | Blur what is behind a translucent window. |
| `decorations` | `"full"` | `"full"`, `"none"`, or `"transparent_titlebar"` (macOS). |
| `initial_cols`, `initial_rows` | `110`, `32` | Size of a new window in cells. Columns 10 to 1000, rows 2 to 1000. |

## `[terminal]`

| Key | Default | Meaning |
|-----|---------|---------|
| `scrollback_lines` | `10000` | Scrollback per pane, 0 to 1,000,000. |
| `shell` | unset | `{ program = "/bin/zsh", args = ["-l"] }`. Unset means your login shell (`$SHELL`). |
| `working_directory` | `"home"` | `"home"`, `"inherit"` (the focused pane's directory), or an absolute path or one starting with `~/`. |
| `cursor_style` | `"block"` | `"block"`, `"bar"`, or `"underline"`. |
| `cursor_blink` | `true` | Whether the cursor blinks. |
| `copy_on_select` | `false` | Copy the selection to the clipboard when it is made. |
| `confirm_close_running` | `true` | Ask before closing a pane that is running a command. |

## `[theme]`

Use one of two forms. Do not mix them.

```toml
[theme]
name = "Signal Flare"      # one theme always
```

```toml
[theme]
light = "Halyard Day"      # follow the system appearance
dark = "Halyard Night"
```

Defaults are `light = "Halyard Day"` and `dark = "Halyard Night"`. Combining `name` with
`light` or `dark` is an error. An unknown theme name falls back to the default for the
current appearance, and the app should tell the user. Bundled themes and the theme file
format are in [themes.md](themes.md).

## `[keybindings]`

A table of `"chord" = "action"`. Your entries are added to the defaults for your platform.
A chord that matches a default replaces it. Use the action `none` to remove a default.

```toml
[keybindings]
"primary+k" = "command_palette"
"cmd+q" = "none"
```

Chord syntax: modifiers and one key joined by `+`, case-insensitive, the last part is the key.
Modifiers are `ctrl`, `shift`, `alt` (`opt`, `option`), `cmd` (`command`, `super`) and
`primary`. `primary` is platform neutral: Cmd on macOS, Ctrl+Shift on Linux (so plain Ctrl
chords stay with the shell). Keys are single characters, `plus` for the `+` key, `enter`,
`tab`, `escape`, `space`, `backspace`, `delete`, `insert`, `up`, `down`, `left`, `right`,
`home`, `end`, `pageup`, `pagedown`, and `f1` to `f24`. A character key (or `space`) needs
ctrl, alt, cmd or primary so ordinary typing is never captured. Two entries that resolve to
the same chord on a platform are an error.

### Actions and default chords

| Action | macOS | Linux |
|--------|-------|-------|
| `new_tab` | `cmd+t` | `ctrl+shift+t` |
| `close_pane` | `cmd+w` | `ctrl+shift+w` |
| `split_right` | `cmd+d` | `ctrl+shift+d` |
| `split_down` | `cmd+shift+d` | `ctrl+shift+e` |
| `focus_left`, `focus_right`, `focus_up`, `focus_down` | `cmd+alt+<arrow>` | `alt+<arrow>` |
| `next_tab` | `cmd+shift+]` | `ctrl+pagedown` |
| `prev_tab` | `cmd+shift+[` | `ctrl+pageup` |
| `command_palette` | `cmd+shift+p` | `ctrl+shift+p` |
| `copy` | `cmd+c` | `ctrl+shift+c` |
| `paste` | `cmd+v` | `ctrl+shift+v` |
| `find` | `cmd+f` | `ctrl+shift+f` |
| `zoom_pane` | `cmd+shift+enter` | `ctrl+shift+enter` |
| `increase_font_size` | `cmd+=` | `ctrl+shift+=` |
| `decrease_font_size` | `cmd+-` | `ctrl+shift+-` |
| `reset_font_size` | `cmd+0` | `ctrl+shift+0` |
| `agent_dashboard` | `cmd+shift+a` | `ctrl+shift+a` |
| `new_window` | `cmd+n` | `ctrl+shift+n` |
| `quit` | `cmd+q` | `ctrl+shift+q` |

## `[ai]`

AI features are off by default. With `enabled = false` no AI code runs.

| Key | Default | Meaning |
|-----|---------|---------|
| `enabled` | `false` | Master switch. |
| `providers` | none | Array of `[[ai.providers]]` tables, below. |

```toml
[ai]
enabled = true

[[ai.providers]]
name = "local"
kind = "ollama"
base_url = "http://localhost:11434"
model = "llama3.1"

[[ai.providers]]
name = "claude"
kind = "anthropic"
model = "claude-sonnet-4-5"
api_key_env = "ANTHROPIC_API_KEY"
```

| Provider key | Meaning |
|--------------|---------|
| `name` | Unique label. |
| `kind` | `"openai-compatible"`, `"anthropic"`, or `"ollama"`. |
| `base_url` | `http://` or `https://` URL. Required for `openai-compatible` and `ollama`. |
| `model` | Model identifier. |
| `api_key_env` | The NAME of an environment variable holding the key. |

**Keys never go in this file.** A provider entry with `api_key` (or `token`, `secret`,
`password`, `key`) is rejected with an error telling you to export the key and name the
variable. `api_key_env` must look like a variable name, so pasting a key there is rejected
too, and the error never echoes the value back. If you pasted a real key into a config file
at any point, revoke it.

## `[agents]`

| Key | Default | Meaning |
|-----|---------|---------|
| `notify_on_waiting` | `true` | Notify when an agent starts waiting for you. |
| `detect` | `[]` | Extra process names to treat as agents, besides the built-in list. |

## Status

This is the library layer (`crates/halyard-config`): parsing, validation, keymaps, reload.
Wiring into the app (applying settings, a reload notice, the `config` subcommand) is pending
on the app crate.
