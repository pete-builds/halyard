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

- [x] PTY sessions running the user's login shell (`halyard-term::Session`; tests
      `default_program_is_a_login_shell`, `shell_echo_reaches_the_screen`,
      `session_environment_is_set`, `shell_exit_reports_status`,
      `program_exit_status_and_signal`, `output_flood_keeps_up`; macOS arm64 run
      locally, Ubuntu in CI)
- [x] libghostty-vt adapter publishing `Frame`s (`halyard-term::Engine`; unit tests in
      `engine.rs`: `sgr_colors_and_attributes_resolve_to_rgb`,
      `wide_glyphs_set_wide_and_spacer_flags`, `device_queries_produce_replies`,
      `palette_change_recolors_ansi_and_defaults`; integration `sgr_red_is_rendered_red`)
- [x] Headless tests: vim, less, htop through PTY plus engine (`vim_uses_the_alternate_screen`,
      `less_pages_and_quits`, `htop_starts_and_quits`)
- [ ] GPUI window rendering the grid (colors, bold, italic, underline, wide glyphs)
- [ ] Keyboard input (including Kitty protocol via the engine's encoder), Ctrl+C.
      Session side done: `keys_encode_with_terminal_modes`,
      `kitty_keyboard_flags_change_encoding`, `ctrl_c_byte_interrupts_sleep`,
      `ctrl_c_key_event_interrupts_sleep`, `typed_keys_run_a_command`. Window input pending.
- [ ] Resize propagation, scrollback, mouse reporting. Session side done:
      `resize_reaches_the_program`, `resize_reflows_and_changes_grid`,
      `scrollback_scrolls_and_typing_returns_to_bottom`, `mouse_reports_only_when_tracking`,
      `bracketed_paste_and_mouse_reach_the_program`. Window side pending.
- [ ] Selection, copy, paste (bracketed). Session side done: `selection_highlights_and_copies`,
      `selection_copies_text`, `paste_is_bracketed_only_when_enabled`. Window side pending.
- Known gaps in the session layer: underline style (double, curly) and underline color are
  reduced to a single underline flag; Kitty graphics are not rendered; Option-as-Alt on
  macOS is off by default and only configurable through `SessionConfig::option_as_alt`.

## Phase 2: workspace and theming

- [ ] Tabs, split panes, keyboard pane navigation, resizing
- [ ] TOML config and token-based themes, bundled themes
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
