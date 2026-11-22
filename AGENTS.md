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
