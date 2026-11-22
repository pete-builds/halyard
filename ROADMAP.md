# Roadmap

Living status. Every feature PR updates this file. "Done" means tested, and the test is
named; untested platforms are listed as such. Priorities: [docs/PRODUCT.md](docs/PRODUCT.md).

## Phase 0: architecture and feasibility

- [x] Prior art and license analysis (`docs/PRIOR_ART.md`, `docs/LICENSES.md`)
- [x] ADRs 0001 to 0004
- [x] Spike: `libghostty-vt` 0.2.2 builds on macOS arm64 with Zig 0.15.2 (fails on 0.17.1)
- [x] Spike: GPUI 0.2.2 builds on macOS arm64 with Command Line Tools only (`runtime_shaders`)
- [x] CI: fmt, clippy, test on macOS and Ubuntu; cargo-deny

## Phase 1: a fully functional terminal on macOS and NixOS

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
- [x] GPUI window rendering the grid (colors, bold, italic, underline, strikethrough, inverse,
      wide glyphs, block/bar/underline/hollow cursor, selection highlight): `crates/halyard-app`.
      Unit tests in `grid.rs` (`runs_split_on_style_and_blanks`,
      `wide_glyphs_and_clusters_are_their_own_runs`, `backgrounds_merge_equal_neighbors_and_skip_default`,
      `cursor_recolors_one_cell`). Verified on macOS arm64 with screenshots of a prompt, colored
      `ls`, vim, htop, a title change, a selection and the exit notice. Linux: real NixOS
      26.05 on an RTX 4070 Ti (2026-11-22): headless sway on Vulkan, XWayland, and a
      launch-only check in KDE Plasma 6.6 Wayland; typing, colors, vim, htop, Ctrl+C, resize,
      paste in, and (X11) copy out and wheel scrollback all passed. Not yet: Wayland pointer
      selection and wheel (no pointer in a headless seat).
- [ ] Keyboard input (including Kitty protocol via the engine's encoder), Ctrl+C.
      Session side done: `keys_encode_with_terminal_modes`,
      `kitty_keyboard_flags_change_encoding`, `ctrl_c_byte_interrupts_sleep`,
      `ctrl_c_key_event_interrupts_sleep`, `typed_keys_run_a_command`. Window: key routing in
      `keys.rs` (`named_keys_go_to_the_terminal`, `altgr_types_the_layout_character`,
      `platform_shortcuts_are_reserved_for_the_app`), driven through the real key handler on
      macOS by a debug script. Not verified: a physical keyboard, IME composition, dead keys.
- [ ] Resize propagation, scrollback, mouse reporting. Session side done:
      `resize_reaches_the_program`, `resize_reflows_and_changes_grid`,
      `scrollback_scrolls_and_typing_returns_to_bottom`, `mouse_reports_only_when_tracking`,
      `bracketed_paste_and_mouse_reach_the_program`. Window: resizing the window reflows and
      updates `stty size` (verified on macOS); wheel scrolling (scrollback, alternate scroll,
      mouse reporting) and click/drag reporting are implemented but not exercised in a window.
- [ ] Selection, copy, paste (bracketed). Session side done: `selection_highlights_and_copies`,
      `selection_copies_text`, `paste_is_bracketed_only_when_enabled`. Window: highlight verified;
      drag to select, double-click word, Cmd+C/Cmd+V (Ctrl+Shift+C/V on Linux) implemented
      but not exercised with a real mouse or clipboard.
- Security and lifecycle (review of PR #2): query replies pass an allowlist so title
  reports cannot inject commands (`title_reports_are_never_sent_back`,
  `title_report_cannot_inject_a_command`); dropping a session kills its program and frees
  its threads and descriptors (`dropping_the_session_kills_the_program`,
  `dropping_the_session_kills_a_program_that_ignores_hangup`, `tests/drop.rs`); other
  terminals' identity variables are scrubbed (`tests/env.rs`).
- Known gaps in the session layer: underline style (double, curly) and underline color are
  reduced to a single underline flag; Kitty graphics are not rendered; Option-as-Alt on
  macOS is off by default and only configurable through `SessionConfig::option_as_alt`.
- [ ] `flake.nix`: `nix develop`, `nix build`, `nix run`, offline build (Ghostty source and
      Zig packages pinned as inputs)
- [ ] NixOS VM test: launches under Wayland and X11, screenshots
- [ ] Desktop entry, icon, macOS app bundle
- [ ] Real-hardware runs: macOS arm64 (cypher), NixOS on a real GPU (pending)

## Phase 2: modern UI

- [ ] Tabs, draggable split panes, keyboard pane navigation and resizing
- [x] TOML config and token-based themes, bundled themes (library: `crates/halyard-config`, tests named in docs/themes.md)
- [ ] Settings wired into the app with live reload
- [ ] Command palette
- [ ] Persistent workspaces (layouts, cwd, names) restored on launch
- [ ] Home Manager module: themes, fonts, keybindings, settings

## Phase 3: modern shell experience

- [ ] Shell integration (OSC 133, OSC 7) for zsh, bash, fish
- [ ] Command blocks: navigate, copy output, rerun
- [ ] SQLite history and search

## Phase 4: agent awareness and the open protocol

- [ ] Agent detection (Claude Code, Codex, OpenCode, Gemini CLI) and attention states
- [ ] Session protocol over escape sequences (`HALYARD_SESSION_ID`)
- [ ] Local session API: authenticated socket, scoped capabilities, user approval for
      anything that writes to a pane
- [ ] Agent dashboard with jump-to-pane and notifications

## Phase 5: optional assistant and MCP

- [ ] Provider layer (OpenAI-compatible, local endpoints), explicit context only
- [ ] MCP client with a trust model

## Phase 6: production readiness

- [ ] Benchmarks: startup, memory, input latency, throughput, multi-pane
- [ ] Threat model, security review
- [ ] Release automation (macOS bundle, Nix package)
