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
- An optional output tap sees every raw chunk before the engine parses it. An
  `OutputObserver` does the same and can also ask for the cursor's absolute row at
  offsets it picks: the session feeds the engine up to each offset, reads the row, and
  continues. The app's shell integration and agent scanner (`halyard-agents::pane`)
  plugs in there, so a prompt mark knows its row without a second parse.
- After consuming a burst of output the session thread builds a new `Frame` and publishes
  it as `Arc<Frame>`, at most once per 8 ms while output streams, then notifies the UI
  through a callback and an event channel (`FrameReady`, `TitleChanged`, `Bell`,
  `Exited`).
- The UI thread never blocks on the engine. It draws the latest frame it has.

## Engine boundary

The engine adapter is the only code that touches `libghostty-vt`. The rest of the program
speaks `Frame`, `SessionEvent`, and our own input types. Swapping engines (for example to
`alacritty_terminal`) means writing one adapter, not touching the UI.

## Replies to terminal queries

Programs ask the terminal questions (cursor position, device attributes, modes) and the
answers are written to their input as if typed. An answer that echoes text the program
chose is an injection vector: a file that sets the window title to a command and then
asks for the title back gets that command typed at the shell prompt (the iTerm2
CVE-2024-38396 class). So the engine adapter filters replies through an allowlist
(`safe_replies` in `engine.rs`), per escape sequence:

| Answered | Reply | Why it is safe |
|----------|-------|----------------|
| DA1, DA2 (`CSI c`, `CSI > c`) | `CSI ? 62;... c`, `CSI > 1;1;0 c` | Fixed numbers |
| DA3 (`CSI = c`) | `DCS ! \| hex ST` | Fixed hex |
| DSR, CPR (`CSI 5 n`, `CSI 6 n`) | `CSI 0 n`, `CSI row;col R` | Numbers |
| DECRQM (`CSI ? n $ p`) | `CSI ? n;v $ y` | Numbers |
| Kitty keyboard flags (`CSI ? u`) | `CSI ? flags u` | Numbers |
| XTWINOPS sizes (`CSI 14/16/18 t`) | `CSI 4/6/8;h;w t` | Numbers |
| XTVERSION (`CSI > q`) | `DCS > \| Halyard <version> ST` | Our own string, matched exactly |
| Color queries (OSC 4, OSC 10 to 19 with `?`) | `OSC n;rgb:hex/hex/hex ST` | Hex color only |

Dropped: title and icon reports (`CSI 20 t`, `CSI 21 t`), DECRQSS, XTGETTCAP, the ENQ
answerback, the color-scheme query (`CSI ? 996 n`, not answered today), and any byte outside a
recognized sequence. A CSI reply is kept only if its parameter bytes are in 0x20 to 0x3f, the
bytes the engine itself writes there, so no application text can ride along. Tests:
`title_reports_are_never_sent_back`, `reply_filter_allowlist`,
`safe_query_replies_still_flow`, and end to end `title_report_cannot_inject_a_command`.

## Closing a session

Dropping a `Session` stops its program the way closing a terminal window does: SIGHUP to
the PTY's foreground process group and to the shell, then SIGKILL to the shell if it is
still running two seconds later. A session whose program has already exited and been
reaped sends no signal at all on close, because its pid and process group may have been
reused (`a_reaped_child_is_never_signalled`). The reader thread waits with `poll` on the PTY and on a
wake pipe; the session thread closes the pipe (and drops the output channel) on shutdown,
so the reader exits even when a background job that ignores SIGHUP keeps the PTY open.
Tests: `dropping_the_session_kills_the_program`,
`dropping_the_session_kills_a_program_that_ignores_hangup`, and `tests/drop.rs`, which
checks on Linux that the reader thread is gone within 1.5 s and that every thread and
descriptor is released.

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
