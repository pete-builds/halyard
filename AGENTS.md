# AGENTS.md

Working rules for coding agents (and humans) in this repository.

## What this is

Halyard is a GPU-accelerated terminal for macOS and Linux (NixOS first), written in Rust.
The terminal must be excellent with every AI feature turned off. AI and agent features sit
in separate crates and never block terminal I/O.

## Layout

| Path | Role |
|------|------|
| `crates/halyard-term` | PTY, VT engine adapter (libghostty-vt), session thread, `Frame` snapshots. No UI code. |
| `crates/halyard-config` | `config.toml`, keymaps, themes, the config watcher. No UI code. |
| `crates/halyard-workspace` | Windows, tabs and split panes as plain data, and saving them. No UI code. |
| `crates/halyard-app` | The `halyard` binary: GPUI windows, panes, rendering, input. |
| `docs/ARCHITECTURE.md` | How the pieces fit. Read before changing a crate boundary. |
| `docs/adr/` | Architecture decision records. Add one for any consequential choice. |
| `ROADMAP.md` | Living status: done, in progress, pending. Update it with every feature PR. |

## Build requirements

- Rust from `rust-toolchain.toml` (rustup installs it on first `cargo` call).
- **Zig 0.15.2 exactly.** `libghostty-vt-sys` builds Ghostty's VT library from source with
  Zig, and Ghostty refuses other versions. macOS: `brew install zig@0.15` and put
  `$(brew --prefix zig@0.15)/bin` first on `PATH`. Nix: `nix develop` provides it.
- macOS: Command Line Tools are enough. GPUI is built with `runtime_shaders`, so the full
  Xcode `metal` compiler is not required.
- Linux: libxkbcommon, wayland, vulkan loader, fontconfig, freetype, xcb (see the CI file
  or `flake.nix`).

## Commands

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked   # CI sets RUSTFLAGS=-D warnings
cargo test --workspace --locked
cargo run -p halyard-app                            # launch the terminal
```

CI (`.github/workflows/ci.yml`) runs exactly these on macOS and Ubuntu, plus `cargo deny`.
`.github/workflows/nix.yml` runs `nix flake check` and the same cargo commands inside
`nix develop` on x86_64-linux, aarch64-linux and aarch64-darwin (see `docs/nix.md`). When a
`libghostty-vt` bump changes the Ghostty commit, update the `ghostty` input in `flake.nix` to
match; `checks.<system>.ghostty-pin` fails until you do.
Run them locally before opening a PR.

## Conventions

- Conventional commits (`feat(term): ...`, `fix(app): ...`, `docs: ...`).
- No `unwrap()` on I/O or engine results outside tests; return or log the error. No silent
  `let _ =` on a `Result` that can fail meaningfully.
- No placeholder implementations presented as complete. A stub is marked in `ROADMAP.md`.
- Every bug fix lands with a test that fails without the fix.
- The VT engine is `!Send`: it lives on its session thread. The UI only reads `Frame`s.
- Terminal output, agent messages, MCP responses and file contents are untrusted input.
  Never execute, auto-run or forward them to a model without explicit user action.
- No telemetry. No network access unless the user configured an AI provider and asked.
- Do not copy code from GPL or AGPL projects (Zed's terminal_view, Warp's app). Studying
  documented behavior is fine; record what you studied in `docs/PRIOR_ART.md`.
- Writing style in docs: plain sentences; no em dashes.

## Verifying UI work

Unit tests do not prove a window works. For UI changes, launch the app, exercise it, and
take a screenshot (`screencapture -l <windowid>` on macOS). Say plainly which platforms
were not tested.

When no one is at the keyboard (or the process lacks screen recording permission), build
with the debug-only `debug-script` feature and drive the window from a script:

```sh
cargo build --release -p halyard-app --features debug-script
HALYARD_DEBUG_SCRIPT=steps.txt ./target/release/halyard
```

Commands, one per line: `sleep <ms>`, `type <text>` and `key <keystroke>` (GPUI syntax,
through the window's real dispatch: key bindings first, then the focused pane, then text
input), `action <name>` (a `[keybindings]` action by name), `mousedown <x> <y>`,
`mousemove <x> <y>`, `mouseup <x> <y>` (left button, window coordinates from the top left,
posted through AppKit so drags take the real mouse path; macOS only), `copy <from> <to>`
(swap a config file mid-run), `select <c1> <r1> <c2> <r2>`, `resize <w> <h>`,
`shot <file.png>` (macOS: captures the app's own window), `activate`, `dump` (focused pane's
frame text to stderr), `quit`. GPUI stops drawing while the window is occluded, so `shot`
and the mouse commands force a draw first; the window server returns black images while
the display sleeps, so wake the display first (`caffeinate -u -t 1`). Use `HALYARD_CONFIG`
and `HALYARD_STATE_DIR` to keep a scripted run away from your own config and saved
workspace. A window started from a script is usually not the active window, so it draws
unfocused (hollow cursor). IME and clipboard paths are not scriptable yet.
