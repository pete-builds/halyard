# ADR 0002: libghostty-vt as the VT engine

Status: accepted, 2026-11-22

## Decision

Use Ghostty's VT library through the `libghostty-vt` crate (0.2.2, MIT OR Apache-2.0,
github.com/uzaaft/libghostty-rs), behind our own adapter in `halyard-term`.

## Evidence (spike on macOS arm64, 2026-11-22)

- The crate's build script clones Ghostty at a pinned commit and builds the static library
  with Zig. With Zig 0.17.1 it failed: Ghostty enforces `minimum_zig_version = "0.15.2"`
  and its build uses APIs removed in later Zig. With Zig 0.15.2 (`brew install zig@0.15`)
  it built cleanly in about 25 seconds.
- The safe API covers what a full terminal needs: `Terminal::vt_write`, `resize` with
  reflow, scrollback and viewport scrolling, `RenderState` with per-cell graphemes, styles
  and resolved colors, cursor style, title and pwd, mode queries (mouse tracking,
  alternate screen), a key encoder that follows the Kitty keyboard flags, a mouse encoder,
  selection, and paste safety helpers.
- Handles are `!Send + !Sync`, which drives our one-thread-per-session design.

## Alternatives considered

- `alacritty_terminal` (Apache-2.0): mature and pure Rust (no Zig), used by Zed. Kept as
  the fallback engine. Ghostty's engine wins on Kitty keyboard and graphics support,
  grapheme handling, and active upstream investment in the library form.
- Writing our own parser: rejected. The brief says not to build a VT parser when a mature
  one exists, and correctness is the hard part.

## Consequences

- Builds need Zig 0.15.2 exactly. CI installs it; the Nix flake pins it.
- The upstream API is marked unstable. All engine calls sit in one adapter module so a
  version bump touches one file.
- The build fetches Ghostty source with git at build time. For hermetic Nix builds,
  `GHOSTTY_SOURCE_DIR` and `GHOSTTY_ZIG_SYSTEM_DIR` let the flake supply both ahead of time.
