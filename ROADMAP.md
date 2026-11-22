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
- [x] macOS app bundle: `packaging/macos/build-app.sh` builds a universal `Halyard.app` (ad-hoc
      signed, icon from `packaging/icon/`) and a zip. Verified by hand on macOS arm64: built from
      the app-window branch, launched with `open`, process ran, `lipo -info` shows x86_64 and arm64,
      `codesign --verify` passes on the app and on the unzipped copy. Not covered: no automated test,
      the Intel slice has not been run, and the app is NOT notarized (see `docs/INSTALL.md`)
- [ ] Linux desktop entry and icon (in the Nix flake PR)
- [ ] Real-hardware runs: macOS arm64 (cypher), NixOS on a real GPU (pending)

## Phase 2: modern UI

- [ ] Tabs, draggable split panes, keyboard pane navigation and resizing. Model done in
      `crates/halyard-workspace` (tests in `tests/model.rs`: `split_right_and_down_build_the_tree`,
      `closing_a_pane_gives_its_space_to_the_sibling`, `focus_moves_by_geometry`,
      `focus_prefers_the_larger_overlap`,
      `keyboard_resize_moves_the_nearest_divider_and_respects_minimums`,
      `dragging_a_divider_clamps_to_nested_minimums`, `zoom_shows_only_the_focused_pane`,
      `tabs_move_rename_and_cycle`, `random_operations_keep_invariants`); the GPUI side is
      pending.
- [x] TOML config and token-based themes, bundled themes (library: `crates/halyard-config`, tests named in docs/themes.md)
- [ ] Settings wired into the app with live reload
- [ ] Command palette
- [ ] Persistent workspaces (layouts, cwd, names) restored on launch. Model and file format done
      in `crates/halyard-workspace` (`tests/persist.rs`: `serialization_round_trips`,
      `the_file_holds_layout_only`, `corrupt_files_give_a_default_layout_and_a_warning`,
      `hostile_values_are_clamped_or_dropped`, `save_is_atomic_and_private`,
      `state_dir_per_platform`, `osc7_is_used_only_when_local`); the app wiring is pending.
- [ ] Home Manager module: themes, fonts, keybindings, settings

## Phase 3: modern shell experience

- [x] Shell integration scripts (OSC 133 A/B/C/D, OSC 633;E, OSC 7) for zsh, bash and
  fish, as a library (`halyard-shell`): run in real shells under a PTY by
  `crates/halyard-shell/tests/shell_scripts.rs` (`zsh_integration`, `bash_integration`,
  `fish_integration`, `bash_command_lines_with_long_history`,
  `bash_keeps_dollar_underscore`, `bash_ignoreboth_repeats_are_marked`,
  `scripts_are_inert_outside_halyard`, `scripts_are_inert_in_non_interactive_shells`).
  Tested on macOS (zsh 5.9, bash 3.2, fish 4.9.3, whose own OSC 133 marks the script
  leaves alone) and in CI on macOS and Ubuntu, where a missing shell fails the run
  instead of skipping. The fish 3 code path runs only where CI's fish is 3.x.
- [x] Escape sequence scanner (OSC 133, 633, 7, 9, 9;4, 777, 0/1/2, BEL, alternate
  screen, Halyard protocol) over chunks split anywhere:
  `scanner::tests::corpus_every_pair_of_split_points`, `overflow_drops_and_resyncs`
- [x] Command block model as a library (missing marks, nested shells, alternate
  screen, navigation, last failed, bounded history, nesting capped at 32 against
  hostile output): `halyard-shell` `blocks::tests`, `hostile_nesting_is_bounded_and_linear`
- [ ] Command blocks in the UI: wiring the scanner tap and engine rows into panes,
  navigate, copy output, rerun (pending; needs `halyard-term` and `halyard-app`)
- [ ] SQLite history and search

## Phase 4: agent awareness and the open protocol

- [x] Agent detection (Claude Code, Codex, OpenCode, Gemini CLI, Aider, Goose, Amp) from
  the foreground process, as a library (`halyard-agents`): `detect::tests::table`; real
  PTY foreground lookup in `crates/halyard-agents/tests/foreground.rs` (macOS tested;
  Linux path compiled and run by CI)
- [x] Attention states from observable signals, as a library:
  `attention::tests::state_machine_table`, `unread_rules`; rules in `docs/agents.md`
- [x] Session protocol over escape sequences (OSC 7717 and the OSC 777;halyard carrier):
  parsing and `docs/protocol.md`; Claude Code hook script tested in
  `crates/halyard-agents/tests/claude_hook.rs` (not yet run inside a live Claude Code
  session)
- [ ] Setting `HALYARD_SESSION_ID` and `TERM_PROGRAM=Halyard` in each pane's environment
  (pending: belongs to the PTY session code)
- [x] Dashboard model (git branch without running git, attention sort order):
  `dashboard::tests::sort_order`, `git::tests`
- [ ] Local session API: authenticated socket, scoped capabilities, user approval for
      anything that writes to a pane
- [ ] Agent dashboard with jump-to-pane and notifications: UI wiring pending

## Phase 5: optional assistant and MCP

- [ ] Provider layer (OpenAI-compatible, local endpoints), explicit context only
- [ ] MCP client with a trust model

## Phase 6: production readiness

- [ ] Benchmarks: startup, memory, input latency, throughput, multi-pane
- [x] Threat model written: `docs/THREAT_MODEL.md` (cites code in PRs that were still open when it was written)
- [ ] Security review (no outside review yet)
- [ ] Release automation: `.github/workflows/release.yml` (macOS zip, Linux tarballs, SHA256SUMS, provenance,
      draft release; `dry_run` dispatch builds artifacts only) is written and actionlint-clean, but has
      NOT run on a runner yet, because it needs the app crate on main. The macOS bundle script was run
      locally. The Nix package is the flake PR. Notarization is not done
