# Architecture

Halyard is a terminal first. Everything else is layered on top and can be turned off.

```
┌──────────────────────── halyard-app (GPUI) ────────────────────────┐
│ windows · tabs · split panes · palette · themes · agent sidebar     │
│ TerminalView: paints Frame cells, maps keys/mouse to input events   │
└───────────────▲──────────────────────────────┬─────────────────────┘
                │ Arc<Frame> + SessionEvent     │ Input (bytes, keys, resize, scroll)
┌───────────────┴──────── halyard-term ────────▼─────────────────────┐
│ Session thread (one per pane)                                       │
│   portable-pty master ──reader thread──► libghostty-vt Terminal     │
│   Terminal ──RenderState──► Frame (owned, colors resolved)          │
│   OSC 133 / OSC 7 / OSC 9 / 777 / BEL ──► SessionEvent              │
└─────────────────────────────────────────────────────────────────────┘
```

## Threading

- `libghostty-vt` handles are `!Send + !Sync`. Each pane gets a dedicated session thread
  that owns its `Terminal`, `RenderState`, and key and mouse encoders.
- A PTY reader thread does blocking reads and forwards byte chunks to the session thread
  over a channel. Writes to the PTY happen on the session thread.
- After consuming a burst of output (or at most every frame interval) the session thread
  builds a new `Frame` and publishes it as `Arc<Frame>`, then notifies the UI.
- The UI thread never blocks on the engine. It draws the latest frame it has.

## Engine boundary

The engine adapter is the only code that touches `libghostty-vt`. The rest of the program
speaks `Frame`, `SessionEvent`, and our own input types. Swapping engines (for example to
`alacritty_terminal`) means writing one adapter, not touching the UI.

## Rendering

The UI paints each row as styled text runs (runs break where style changes) on top of
background quads. Wide glyphs span two cells; wide spacers draw nothing. The cell size
comes from the configured monospace font's advance width and line height.

## Data and privacy

- Config: TOML at `$XDG_CONFIG_HOME/halyard/config.toml` (macOS: same path under
  `~/.config` by default, for dotfile portability).
- History and command blocks: SQLite under the platform data directory.
- Secrets: OS keychain only.
- No network access unless the user configures an AI provider and makes a request.

## Decisions

See `docs/adr/`. Each consequential choice has a record with the evidence behind it.
