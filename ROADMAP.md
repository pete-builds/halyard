# Roadmap

Living status. Every feature PR updates this file. "Done" means tested, and the test is
named; untested platforms are listed as such.

## Phase 0: architecture and feasibility

- [x] Prior art and license analysis (`docs/PRIOR_ART.md`, `docs/LICENSES.md`)
- [x] ADRs 0001 to 0004
- [x] Spike: `libghostty-vt` 0.2.2 builds on macOS arm64 with Zig 0.15.2 (fails on 0.17.1)
- [x] Spike: GPUI 0.2.2 builds on macOS arm64 with Command Line Tools only (`runtime_shaders`)
- [x] CI: fmt, clippy, test on macOS and Ubuntu; cargo-deny
- [ ] Nix flake with dev shell and `nix build`

## Phase 1: an excellent basic terminal

- [ ] PTY sessions running the user's login shell
- [ ] libghostty-vt adapter publishing `Frame`s
- [ ] Headless tests: vim, less, htop through PTY plus engine
- [ ] GPUI window rendering the grid (colors, bold, italic, underline, wide glyphs)
- [ ] Keyboard input (including Kitty protocol via the engine's encoder), Ctrl+C
- [ ] Resize propagation, scrollback, mouse reporting
- [ ] Selection, copy, paste (bracketed)

## Phase 2: workspace and theming

- [ ] Tabs, split panes, keyboard pane navigation, resizing
- [x] TOML config and token-based themes, bundled themes: library done in `crates/halyard-config` (`default_config_file_parses_to_exactly_the_default_impl`, `every_bundled_theme_meets_the_contrast_rule`, `export_round_trips_every_bundled_theme`, `reloads_on_change_reports_errors_and_handles_delete`). Wiring into the app UI (applying settings, theme picker, `halyard config` subcommand) is pending
  - [ ] Wire config and themes into the app, including live reload notices
- [ ] Command palette

## Phase 3: modern shell experience

- [ ] Shell integration (OSC 133, OSC 7) for zsh, bash, fish
- [ ] Command blocks: navigate, copy output, rerun
- [ ] SQLite history and search

## Phase 4: agent workspace

- [ ] Agent detection and attention states from observable signals
- [ ] `HALYARD_SESSION_ID` and the session notification protocol
- [ ] Agent dashboard with jump-to-pane

## Phase 5: optional assistant and MCP

- [ ] Provider layer (OpenAI-compatible, local endpoints), explicit context only
- [ ] MCP client with a trust model

## Phase 6: production readiness

- [ ] Threat model, benchmarks, packaging (macOS bundle, Nix package), release automation
