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
  over a bounded channel, so a flood of output applies backpressure instead of growing
  memory.
- A PTY writer thread does the blocking writes. Encoding (keys, mouse, paste, query
  replies) happens on the session thread, but the write itself does not, so a program
  that stops reading its input cannot stall rendering.
- A waiter thread blocks on the child process and reports its exit status. The session
  keeps draining output for a short grace period after the exit so the last lines reach
  the screen before `Exited` is reported.
- Commands from the UI (`Session` is `Send + Sync`) and output chunks meet in one
  `crossbeam_channel::select!` loop on the session thread.
- An optional output tap sees every raw chunk before the engine parses it; the shell
  integration scanner plugs in there.
- After consuming a burst of output the session thread builds a new `Frame` and publishes
  it as `Arc<Frame>`, at most once per 8 ms while output streams, then notifies the UI
  through a callback and an event channel (`FrameReady`, `TitleChanged`, `Bell`,
  `Exited`).
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
