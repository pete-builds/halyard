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
      anything that writes to a pane ([docs/session-api.md](docs/session-api.md))
  - [x] Library `halyard-ipc`: 0700 socket dir and 0600 socket, peer uid check, versioned
        strict JSON-RPC, size and rate limits (tests in `crates/halyard-ipc/tests/protocol.rs`
        and `setup.rs`, for example `a_0755_socket_directory_is_refused`,
        `oversized_message_is_rejected_and_the_connection_closed`,
        `rate_limit_rejects_bursts_then_disconnects_abusers`)
  - [x] Session scope by process ancestry plus token, with real processes
        (`a_process_in_another_pane_cannot_spoof_this_one`, `child_of_the_pane_process_is_accepted`)
  - [x] Controller pairing, hashed grants file, scopes, revocation, events
        (`pairing_approved_grants_only_the_approved_scopes`, `grants_file_holds_a_hash_not_the_token`,
        `revocation_cuts_off_live_connections_and_the_token`, `events_stream_to_subscribers_until_revoked`)
  - [x] `send_text` needs per-request approval for newlines and control characters even
        with "always allow" (`always_allow_still_asks_for_newlines_and_controls`)
  - [x] `halyard-msg` hook client with OSC 7717 fallback
        (`unreachable_socket_falls_back_to_the_escape_sequence_on_the_tty`,
        `outside_halyard_it_exits_0_silently_and_fast`)
  - [ ] App wiring: start the server, implement `SessionDirectory` and the approval
        dialogs, export the pane env vars, publish events, grants settings page
  - [ ] Peer uid rejection tested with a second real user (code path is a uid compare;
        not exercised by tests)
- [ ] Agent dashboard with jump-to-pane and notifications

## Phase 5: optional assistant and MCP

- [ ] Provider layer (OpenAI-compatible, local endpoints), explicit context only
- [ ] MCP client with a trust model

## Phase 6: production readiness

- [ ] Benchmarks: startup, memory, input latency, throughput, multi-pane
- [ ] Threat model, security review
- [ ] Release automation (macOS bundle, Nix package)
