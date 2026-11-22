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
  `config.toml:3: key `font.size`: is out of range; expected 4 to 128`.
* `ConfigWatcher` reports a reload after a change settles: the new config, or the error for a
  broken edit, so the app can keep the previous settings and show it. A change is a different
  fingerprint, checked about once a second and woken early by file system notifications: for
  `config.toml`, the path it resolves to after following every symlink (fresh each time), its
  size, modification time and a hash of its content; for `themes/` next to it, the sorted
  names of the `*.toml` files with the same data for each. So an edit made through a symlink, a
  symlink swapped by rename or by unlink and link (Home Manager, stow, chezmoi), a directory
  moved into place or away, a `themes/` folder created later, and a file that appears,
  disappears or is recreated are all noticed, and unrelated files in the same folders are not.
  If notifications are unavailable the once-a-second check still works. After the first
  change the reload waits until things are quiet (the debounce), and never longer than 500 ms
  in total. When the file does not exist the event holds the defaults and a warning that says
  so. A config or theme file that is not a regular file (a FIFO, directory, device) or is
  over 4 MiB is never opened blockingly or read whole: it counts as unreadable, the event holds
  the defaults and a warning naming what it is, and no value. At most 512 theme files are
  looked at; extras produce a warning. A file is only re-hashed when its size or modification
  time changed. The poll interval is never shorter than 50 ms. A relative `HALYARD_CONFIG` is resolved against the working directory when Halyard
  starts. Dropping the watcher stops and joins its thread.

### Error messages never contain values

No error or warning ever shows a value from your file, in any section, because any value
might be a pasted secret. A message holds a key path, a line number, and what was expected,
for example `expected a number` or `expected one of `block`, `bar`, `underline``. The same
holds for theme files and for the Ghostty importer.

Which key names can appear in a message:

* Keys Halyard defines (`font.size`, `ai.providers[0].kind`, a chord under `[keybindings]`).
* An unknown key name OUTSIDE `[ai]`, in the "unknown key" warning with its "did you mean" hint,
  and a secret-looking key name outside `[ai]` (top level or any section), in the error that
  rejects it. The value is never shown.
* Never an unknown or secret-looking key name under `[ai]`, at any depth. Those are reported as
  "unknown key in [ai]" (or "in [[ai.providers]]") with the section and the line, because a
  pasted key can end up as a key name. Open that line to see it. The one exception is a
  documented name in the wrong table, such as `api_key_env` directly under `[ai]`: the error
  names it, because the name comes from Halyard's own schema, not from your file.

A documented key name is exempt from the secret-name check only in the table that defines it.
The same name in any other table is checked like any other key.

These checks read the parsed document, not the text lines, so quoted headers (`["ai"]`),
dotted keys (`"ai".enabled = ...`) and multi-line strings cannot hide a section.

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
| `scrollback_lines` | `10000` | Lines of scrollback per pane, 0 to 1,000,000. Lines of ordinary text are kept in full at any width up to 256 columns (or the pane's starting width, if wider), and a narrower pane can keep more. Dense lines, with many colors, emoji or combining marks, use more memory each, so fewer are kept (as few as about 1,900 of 10,000; see `docs/PERFORMANCE.md`). Memory grows only as scrollback fills, up to about 2.4 KB per configured line (24 MB for 10,000 lines). |
| `shell` | unset | `{ program = "/bin/zsh", args = ["-l"] }`. Unset means your login shell (`$SHELL`). |
| `working_directory` | `"home"` | `"home"`, `"inherit"` (the focused pane's directory), or an absolute path or one starting with `~/`. |
| `cursor_style` | `"block"` | `"block"`, `"bar"`, or `"underline"`. |
| `cursor_blink` | `true` | Whether the cursor blinks. |
| `copy_on_select` | `false` | Copy the selection to the clipboard when it is made. |
| `confirm_close_running` | `true` | Ask before closing a pane that is running a command. |
| `confirm_multiline_paste` | `true` | Ask before pasting text with a line break while the program has not turned on bracketed paste (each line would run as a command). |

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
| `resize_pane_left`, `resize_pane_right`, `resize_pane_up`, `resize_pane_down` | `cmd+ctrl+<arrow>` | `ctrl+shift+alt+<arrow>` |
| `move_tab_left`, `move_tab_right` | `cmd+shift+alt+left`, `cmd+shift+alt+right` | `ctrl+shift+pageup`, `ctrl+shift+pagedown` |
| `rename_tab` | `cmd+shift+i` | `ctrl+shift+i` |
| `reload_config` | `cmd+shift+,` | `ctrl+shift+,` |
| `open_config` | `cmd+,` | `ctrl+,` |
| `prev_prompt`, `next_prompt` | `cmd+up`, `cmd+down` | `ctrl+shift+up`, `ctrl+shift+down` |
| `copy_last_output` | `cmd+alt+c` | `ctrl+shift+alt+c` |

`prev_prompt` and `next_prompt` scroll so the previous or next command's prompt is the top
line, and `copy_last_output` copies the output of the last finished command. Both need
shell integration; without it they do nothing. Halyard never edits your rc files. The
palette's "Copy Shell Integration Line" writes the script for your configured shell (zsh,
bash or fish) to `shell-integration/` next to `config.toml` and copies the one line that
loads it, for you to paste into `~/.zshrc`, `~/.bashrc` or `config.fish`. The line checks
`TERM_PROGRAM` itself, so other terminals never read the script.

On fish 4.0 to 4.2 (Debian stable ships 4.0.2), put the line after anything that defines
`fish_prompt` (a custom prompt, `starship init fish | source`, tide). Those versions need
Halyard to add the "input starts here" mark to your prompt, and a prompt defined later
replaces that. Command blocks still work without it, but without the input start.

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

**Keys never go in this file.** Any key under `[ai]`, at any depth, whose name contains
`key`, `token`, `secret`, `password`, `passwd` or `auth` (in any letter case, for example
`API_KEY`, `Authorization`, `x-api-key`) is rejected with an error telling you to export the key and name the
variable. The one documented name with such a fragment, `api_key_env`, is allowed only
directly on an `[[ai.providers]]` entry. Anywhere else (directly under `[ai]`, in a table
nested under `[ai]` or under a provider, or outside `[ai]`) it is treated as a pasted secret
and rejected; the error names `api_key_env` and where it belongs, never the value. Its value
must look like a variable name (letters, digits, underscore, not starting with a digit), so
pasting a key there is rejected too, and so is a value that passes that check but is shaped
like a credential: a well-known token prefix (`ghp_`, `github_pat_`, `sk_live_` and similar),
24 or more characters of random-looking mixed case and digits, or 32 or more hex digits.
A `base_url` with credentials in it (`https://user:KEY@host/`) is rejected. No error ever
echoes a value back. If you pasted a real key into a config file
at any point, revoke it.

In 0.1.0 and 0.1.1, `api_key_env` was exempt by name at any depth under `[ai]`, so a key pasted as
`[ai] api_key_env = "sk-..."` (or in a nested table) was kept in the file with only an
"unknown key, ignored" warning. If that happened to you, revoke the key.

## `[agents]`

| Key | Default | Meaning |
|-----|---------|---------|
| `notify_on_waiting` | `true` | Send a desktop notification when an agent in a pane starts waiting for you or finishes while its window is not the active one. One per change, at most one per pane every 10 s and three per 30 s. |
| `detect` | `[]` | Extra process names to treat as agents, besides the built-in list. Compared without case against the foreground program's name. |

Desktop notifications use `NSUserNotificationCenter` on macOS, which needs the app bundle
(`Halyard.app`); a bare `cargo run` binary has no bundle identifier and says so once on
stderr. On Linux they go to the freedesktop notification service over the session D-Bus.
The text comes from the program in the pane, so control characters, bidi overrides,
direction marks and zero-width characters are removed and it is capped (80 characters
for the title, 200 for the body); on Linux `&`, `<` and `>` are also escaped. macOS shows
nothing while Halyard is the frontmost app; the unread pill on the tab covers that case.


## `[session_api]`

| Key | Default | Meaning |
|-----|---------|---------|
| `enabled` | `true` | Start the local session API (`docs/session-api.md`): a private Unix socket for hooks such as `halyard-msg` and for tools you pair. Read at startup, so a change needs a restart. With it off, panes get no `HALYARD_SOCKET` or `HALYARD_SESSION_TOKEN`, and `halyard-msg` falls back to escape sequences. |

Nothing the API receives runs on its own. Pairing a tool, and every request to type into
a pane, opens a dialog first (see "Typing into a pane" in `docs/session-api.md`). Paired
tools are listed, and revoked, with the command palette's "Paired Tools" command.

## Saved workspaces

Halyard saves the layout of every window (tabs, their names, splits and their sizes, the
focused pane, and each pane's working directory) and restores it on the next launch with
fresh shells. It never saves scrollback, environment variables, command history or window
titles. The file is `workspace.json` in `~/Library/Application Support/Halyard/` on macOS
and `$XDG_STATE_HOME/halyard/` (default `~/.local/state/halyard/`) on Linux, written with
mode 0600. `HALYARD_STATE_DIR` overrides the folder. A damaged file is ignored with a notice
and Halyard opens one new window. A working directory is saved only when it is an absolute
local path (an OSC 7 report naming another host is ignored).

## Status

The library layer is `crates/halyard-config`: parsing, validation, keymaps, reload. The app
applies every key above except `window.opacity`, `window.blur`, `window.decorations`,
`font.bold_is_bright` and `[ai]` (not built yet). Edits apply to open windows
within about a second; a broken edit shows in a banner at the bottom of the window and the
previous settings stay. `cursor_style` is used whenever the program has not asked for a
shape of its own (a program asking for a steady block is drawn with the configured style,
because the engine reports both the same way). `scrollback_lines`, `shell` and
`working_directory` apply to shells started after the change. The `config` subcommand is
still pending.
