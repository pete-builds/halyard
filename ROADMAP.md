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

- [ ] PTY sessions running the user's login shell
- [ ] libghostty-vt adapter publishing `Frame`s
- [ ] Headless tests: vim, less, htop through PTY plus engine
- [ ] GPUI window rendering the grid (colors, bold, italic, underline, wide glyphs)
- [ ] Keyboard input (including Kitty protocol via the engine's encoder), Ctrl+C
- [ ] Resize propagation, scrollback, mouse reporting
- [ ] Selection, copy, paste (bracketed)
- [ ] `flake.nix`: `nix develop`, `nix build`, `nix run`, offline build (Ghostty source and
      Zig packages pinned as inputs)
- [ ] NixOS VM test: launches under Wayland and X11, screenshots
- [ ] Desktop entry, icon, macOS app bundle
- [ ] Real-hardware runs: macOS arm64 (cypher), NixOS on a real GPU (pending)

## Phase 2: modern UI

- [ ] Tabs, draggable split panes, keyboard pane navigation and resizing
- [ ] TOML config and token-based themes, bundled themes
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
