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

- [x] Fedora, Debian, Arch, Ubuntu CI targets (`.github/workflows/distros.yml`, all four green; headless
      only). Debian stable's fish 4.0.2 sends no B mark, so `halyard.fish` now appends it on fish 4.0 to
      4.2 (tests `fish_integration`, `fish_integration_without_native_marks`)
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
      The window sets app id and WM_CLASS `halyard` (`APP_ID`; Linux only, not yet checked on
      Linux).
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
      Scrollback length: `scrollback_lines` is converted to Ghostty's byte limit, so 10,000
      lines of ordinary text are kept at 80 and 200 columns (`scrollback_keeps_the_configured_lines`,
      bounded by `scrollback_limit_still_bounds_memory`); before, 247 rows were kept at 200
      columns. Dense lines (many styles, emoji or combining marks per line) use more of each
      page, so fewer of them are kept: 1,893 to 3,797 of 10,000 in review measurements
      (`docs/PERFORMANCE.md`).
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
- [x] `flake.nix`: `nix develop`, `nix build`, offline build (Ghostty source and Zig packages
      pinned as inputs), on x86_64-linux, aarch64-linux, aarch64-darwin (`nix` workflow:
      `nix flake check` incl. `checks.*.ghostty-pin`, plus the sandbox network control).
      x86_64-darwin is not offered: the pinned nixpkgs dropped it.
- [x] `nix run`: `apps.default` runs the packaged `halyard`, whose RUNPATH resolves all 14
      libraries GPUI dlopens (`checks.*.package-contents`, with a control). On real NixOS
      hardware (x86_64, KDE Plasma 6 Wayland, RTX 4070 Ti) at 5936fd0: `nix flake check` passed
      and `nix run` opened Halyard. Not covered: aarch64-linux and X11 sessions on hardware
- [ ] NixOS VM test: launches under Wayland and X11, screenshots
- [x] macOS app bundle: `packaging/macos/build-app.sh` builds a universal `Halyard.app` (ad-hoc
      signed, icon from `packaging/icon/`) and a zip. Verified by hand on macOS arm64: built from
      the app-window branch, launched with `open`, process ran, `lipo -info` shows x86_64 and arm64,
      `codesign --verify` passes on the app and on the unzipped copy. Not covered: no automated test,
      the Intel slice has not been run, and the app is NOT notarized (see `docs/INSTALL.md`)
- [x] Linux desktop entry and icon: `dist/linux/halyard.desktop` (`StartupWMClass=halyard`) and
      `dist/icons/halyard.svg` plus PNG 16 to 512, installed by the Nix package and checked by
      `checks.*.package-contents` on x86_64-linux and aarch64-linux (`desktop-file-validate`,
      `StartupWMClass=halyard` matching the app id "halyard") and inspected in the package built on
      the real NixOS box. Not yet seen in a desktop launcher menu
- [x] Flatpak host shells: inside Flatpak each pane runs `flatpak-spawn --host --watch-bus` with
      the pane's directory and environment (`halyard_term::flatpak`; tests
      `login_shell_runs_on_the_host_with_cwd_and_env`, `inside_flatpak_the_login_shell_runs_on_the_host`,
      `outside_flatpak_the_program_runs_directly` and the rest of `flatpak::tests`). Unit tested only:
      no real host shell has been started through `flatpak-spawn` yet. Env files a crash leaves behind are swept at the first pane start (`sweep_removes_an_old_own_file_and_keeps_everything_else`)
- [x] Flatpak package (`packaging/flatpak/`, `docs/flatpak.md`): offline build on Freedesktop 26.08 for
      x86_64 and aarch64 in CI (`.github/workflows/flatpak.yml`), bundle uploaded as an artifact; CI checks
      the source lists against `Cargo.lock` and the Ghostty pin (`check-sources.py`), the installed
      permissions exactly (`check-permissions.sh`), and libraries inside the runtime (`smoke-test.sh`, with a
      control). Not covered: no window opened and no host shell started from the Flatpak; not on Flathub
- [ ] Flatpak on a real Linux desktop: open a window, start a host shell, close a pane (pending)
- [ ] Real-hardware runs: macOS arm64 (cypher), NixOS on a real GPU (pending)

## Phase 2: modern UI

- [x] Tabs, draggable split panes, keyboard pane navigation and resizing. Model in
      `crates/halyard-workspace` (tests in `tests/model.rs`: `split_right_and_down_build_the_tree`,
      `closing_a_pane_gives_its_space_to_the_sibling`, `focus_moves_by_geometry`,
      `focus_prefers_the_larger_overlap`,
      `keyboard_resize_moves_the_nearest_divider_and_respects_minimums`,
      `dragging_a_divider_clamps_to_nested_minimums`, `zoom_shows_only_the_focused_pane`,
      `tabs_move_rename_and_cycle`, `random_operations_keep_invariants`). Window in
      `workspace_view.rs`: tab bar (new, close, rename by double click, reorder by drag or
      `move_tab_*`, title from the focused pane), dividers dragged with the mouse (minimum pane
      size), inactive panes dimmed and the focused one outlined from theme tokens, hollow
      cursor when unfocused, every keymap action wired. With `confirm_close_running`, these
      ask before ending a command that is not the shell (`foreground_job_is_detected_through_a_pty`):
      `close_pane`, a tab's ×, the window's close button, and Quit (menu or chord); verified by
      debug script (an idle tab or window closes, a busy one stays with the prompt). A shell
      that exits closes its pane without asking. Verified on macOS arm64 by
      debug script with real mouse events: screenshots of tabs, a 3-pane split, a dragged
      divider, a dragged tab, 9 panes. Not verified: vertical tabs (not built, follow-up),
      Linux windows, the outline on the focused pane in an active window (scripted windows
      are never active), answering the close prompt (a native dialog a script cannot click).
- [x] TOML config and token-based themes, bundled themes (library: `crates/halyard-config`, tests named in docs/themes.md)
- [x] Settings wired into the app with live reload: font family, size and line height,
      padding, cursor style and blink, scrollback, shell and arguments, working directory,
      theme (following the system appearance with `light`/`dark`), palette into the engine,
      chrome from `ui` tokens; changes apply to open panes; config errors and warnings show in
      a dismissible banner and the last good settings stay. Keybindings come from
      `Config::keymap`. Tests in `settings.rs` (`load_applies_the_file`,
      `a_broken_reload_keeps_the_previous_settings`, `watcher_reload_reaches_settings`,
      `theme_palette_reaches_the_engine_palette`, `unknown_theme_falls_back_with_a_notice`,
      `light_and_dark_follow_the_appearance`), `keybind.rs`
      (`every_default_binding_is_loaded_except_unbuilt_actions`). Screenshots on macOS arm64
      (PR body). Not verified: the bar cursor in a focused window (the scripted window is never
      the active window), the system appearance switching live.
- [x] Command palette: Primary+Shift+P, fuzzy filter (`palette.rs`: `fuzzy_matches_subsequences_ignoring_case`,
      `word_starts_and_runs_rank_higher`, `every_built_action_has_a_command_with_a_shortcut_name`),
      every action with its shortcut, Switch Theme with live preview and revert on Escape,
      Open Config File, Reload Config, Rename Tab. Screenshots of the list, a theme preview, the
      revert and a switch. A theme picked here lasts until the app quits or `[theme]` changes;
      it is not written to `config.toml`. The query box has no IME support.
- [x] Persistent workspaces (layouts, cwd, names) restored on launch. Model and file format
      in `crates/halyard-workspace` (`tests/persist.rs`: `serialization_round_trips`,
      `the_file_holds_layout_only`, `corrupt_files_give_a_default_layout_and_a_warning`,
      `hostile_values_are_clamped_or_dropped`, `save_is_atomic_and_private`,
      `state_dir_per_platform`, `osc7_is_used_only_when_local`,
      `restore_caps_the_total_number_of_panes`, `restore_drops_a_tab_nested_deeper_than_the_limit`,
      `restored_directories_must_exist`, `load_does_not_block_on_a_fifo`,
      `save_refuses_symlinks_and_tightens_the_folder`; live splits stop at the same limits,
      `splits_stop_at_the_restore_limits`). The app saves every window on
      change and on quit, and restores them with fresh shells in their directories (OSC 7 when
      it names this machine, else the shell's own directory). Verified on macOS arm64: a
      2-tab, 3-pane layout with a dragged divider, tab names and four directories came back
      after a relaunch (screenshots). Closing the last window saves an empty layout, so the
      next launch opens one new window.
- [x] Home Manager module: themes, fonts, keybindings, settings (`checks.*.hm-module`
      evaluates it with real Home Manager and compares the written TOML; NixOS module checked by
      `checks.x86_64-linux.nixos-module`). The written files are loaded by halyard-config's own
      parser in the check, and a key named like a secret in `settings` is an evaluation error.

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
- [x] Scanner and block tracker wired into every pane with absolute rows: the session
  feeds the engine up to each mark and reads the cursor row there
  (`halyard-term::OutputObserver`, `observer_sees_the_cursor_row_at_each_mark`); a real
  zsh or bash with the integration script sourced makes blocks with rows, statuses and
  command lines (`signals::tests::integration_script_in_a_real_shell_makes_blocks`).
  Rows survive a full scrollback dropping its oldest rows: the engine counts dropped
  rows with a pinned row (`Engine::dropped_rows`) and block rows count from the
  session's first row (`rows_stay_on_their_line_when_scrollback_drops_old_rows`,
  `pane::tests::dropped_scrollback_moves_the_base_and_forgets_dropped_blocks`).
- [x] Command blocks in the UI: `prev_prompt` and `next_prompt` scroll the prompt to the
  top (`blocks_ui::tests::prompt_jumps_go_to_the_nearest_block_start`, and in a real zsh
  `signals::tests::integration_script_in_a_real_shell_makes_blocks`;
  `rows_text_and_scroll_to_row_use_screen_rows`), `copy_last_output`
  (`last_output_is_the_newest_finished_block`, the same real-shell test), a danger-colored
  margin bar beside failed commands (`failure_spans_are_clipped_to_the_viewport`), and a
  palette entry that writes the script and copies the `install_line` for the configured
  shell (`integration_line_writes_the_script_and_never_follows_a_symlink`). Blocks on rows
  that `clear` erased are forgotten (`a_mark_above_the_newest_block_forgets_the_erased_blocks`,
  `cleared_scrollback_forgets_closed_blocks`). A command with no output copies nothing
  (`last_output_is_the_newest_finished_block`). Verified by debug-script screenshots on
  macOS arm64 (prompt jumps, failure bars). Not built: rerun. Not handled: rows after a
  window resize reflows lines, and a last output line without a trailing newline (it
  shares the row of the next prompt, so it is not copied).
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
- [x] `HALYARD_SESSION_ID` and `TERM_PROGRAM=Halyard` in each pane's environment
  (`session_environment_is_set`); the app picks the id before spawning so the pane's
  monitor can drop reports for other panes (`an_invalid_session_id_is_replaced`,
  `pane::tests::protocol_report_for_another_session_is_ignored`)
- [x] Per-pane signal pipeline in the app: output scanned on the session thread, the
  foreground process polled every 500 ms (argv read only while a command runs), user
  input and focus fed to the attention machine (`pane::tests`,
  `signals::tests::fake_claude_notification_means_waiting`). macOS verified live with a
  fake `claude` (debug-script screenshots, described in the PR); Linux runs the same tests in CI, no window run.
- [x] Attention in the UI: a state mark on each tab and a badge on each pane from theme
  tokens, unread drawn as a filled pill, a waiting pane outlined
  (`attention_ui::tests::indicators_follow_the_theme_tokens`,
  `tab_mark_is_the_most_urgent_pane_and_unread_if_any_is`); verified by screenshots on
  macOS arm64
- [x] Desktop notifications when a pane starts waiting or finishes in an inactive
  window, rate limited, `[agents] notify_on_waiting`
  (`attention_ui::tests::one_notification_when_waiting_in_an_inactive_window`,
  `rate_limits_per_pane_and_per_burst`). Text: control, bidi, direction-mark and
  zero-width characters removed and capped (`notification_text_is_sanitized_and_capped`),
  markup-escaped on Linux (`markup_is_escaped_for_the_notification_server`). Protocol
  reports count only with this pane's `session=`, notifications and bells only with an
  agent active (`pane::tests::reports_without_this_panes_session_are_plain_output`,
  `notifications_and_bells_count_only_with_an_agent_in_the_foreground`). macOS uses the
  deprecated `NSUserNotificationCenter`, which shows nothing while Halyard is frontmost;
  `UNUserNotificationCenter` is a follow-up.
  Not verified: a notification actually appearing. macOS delivers only from
  `Halyard.app` (a bare binary has no bundle id and logs that once, seen live); the
  Linux D-Bus call is compiled by CI but never run against a notification daemon.
- [x] Dashboard model (git branch without running git, attention sort order):
  `dashboard::tests::sort_order`, `git::tests`
- [x] Local session API: authenticated socket, scoped capabilities, user approval for
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
        `revocation_cuts_off_live_connections_and_the_token`, `events_stream_to_subscribers_until_revoked`,
        and across instances sharing the grants file:
        `revocation_in_one_store_holds_in_another_and_after_restart`)
  - [x] `send_text` needs per-request approval for newlines, control and hidden Unicode
        characters even with "always allow", and the dialog sees the text typed since the
        last approval (`always_allow_still_asks_for_newlines_and_controls`,
        `every_hidden_range_needs_approval_and_is_escaped`,
        `approving_enter_shows_the_text_typed_under_standing_permission`)
  - [x] `halyard-msg` hook client with OSC 7717 fallback
        (`unreachable_socket_falls_back_to_the_escape_sequence_on_the_tty`,
        `outside_halyard_it_exits_0_silently_and_fast`)
  - [x] App wiring: the app binds the server on its own thread at
        `<socket dir>/<pid>.sock` (`[session_api] enabled`, default on; a failed bind is one
        banner notice), every pane gets `HALYARD_SOCKET`, `HALYARD_SESSION_ID` and
        `HALYARD_SESSION_TOKEN`, and `SessionDirectory` is backed by the real panes
        (`crates/halyard-app/src/session_api.rs`). Tests run a real server, real shells in
        real PTYs and the app's own directory and approver, with only the UI thread faked:
        `a_report_from_inside_a_pane_updates_its_state`,
        `attach_from_outside_the_pane_is_rejected_by_ancestry`,
        `send_text_with_a_newline_needs_approval`,
        `send_text_never_resets_typed_since_approval`,
        `revoking_in_the_paired_tools_view_cuts_the_tool_off`, `a_closed_pane_leaves_the_api`,
        `a_denied_pairing_cools_down_without_a_second_dialog`,
        `events_are_published_once_per_change`; config `session_api` in
        `default_config_file_parses_to_exactly_the_default_impl` and `full_config_round_trips_values`
  - [x] Approval dialogs in the window, keyboard first and themed: pairing (scopes as
        checkboxes, a warning on `input:write` and `screen:read`, "always allow typing" only
        when `input:write` is ticked) and typing (exact escaped text, the text typed since
        the last approval, the client and the pane, with its size, in one scrolling box that
        must be scrolled to the end before Approve works; over 4 KiB escaped is refused
        without a dialog). Plain, Shift and Ctrl+Enter never approve; nothing approves for
        700 ms after opening, a stray key or refocus; approving is the dialog's own action
        (Cmd+Enter, or Ctrl+Shift+Enter over Zoom Pane) or a click; "always allow" is
        click-only; the window's actions are blocked while it is open; Escape denies; no
        answer in 90 s denies (`approval::tests`,
        `the_approve_chord_reaches_the_dialog_over_workspace_bindings`,
        `a_dialog_nobody_answers_is_a_denial`,
        `text_too_long_to_show_is_refused_without_a_dialog`). Pairing prompts are limited to 3 a minute across
        connections, with a 30 s cooldown after a denial
        (`pairing_prompts_are_limited_across_connections`).
  - [x] `reset_typed_since_approval` only from real key events in the pane: Enter, Ctrl-C
        (`keys::tests::only_enter_and_ctrl_c_reset_the_typed_line`,
        `the_pane_key_path_resets_on_enter_but_not_on_ctrl_u`), never
        from `send_text` (`send_text_never_resets_typed_since_approval`), paste or query
        replies (those paths never call it)
  - [x] "Paired Tools (Session API)" palette view: name, scopes, last use; Enter revokes;
        the grants file is read and written off the UI thread
        (`revoking_in_the_paired_tools_view_cuts_the_tool_off`)
  - [x] Off in the Flatpak, with one banner notice (host shells cannot pass ancestry from
        the sandbox; `docs/flatpak.md`). Not run inside a real Flatpak.
  - [x] Verified live on macOS arm64 with the debug-script build: `halyard-msg state
        waiting` in a pane puts the "waiting" badge and the unread tab mark up, `halyard-msg
        info --strict` answers over the socket, a Python client's pairing and `send_text`
        dialogs appear, a plain Enter leaves the dialog open, Cmd+Enter approves, the
        Paired Tools view revokes, `enabled = false` leaves `HALYARD_SOCKET` unset, and a
        0755 socket directory gives the banner while the terminal works; a 2036-character
        padded send shows its size, keeps Approve disabled ("Scroll to the end of the text
        first") through a Cmd+Enter, enables it after End, and a 5000-byte send is refused
        with `too_long_to_review`. Not verified:
        Linux, a second window (the dialog opens in the active window, or the window that
        holds the pane), the 90 s timeout with a real window.
  - [ ] Peer uid rejection tested with a second real user (code path is a uid compare;
        not exercised by tests)
- [x] Agent dashboard: `agent_dashboard` (palette mode), every pane of every window in
  the model's order with agent, tab title, cwd, branch, state with "since", last
  notification and unread; Enter focuses the pane, its tab and its window
  (`dashboard_ui::tests::rows_follow_the_dashboard_order_and_carry_their_targets`,
  `ago_reads_naturally`). Verified by debug-script screenshots on macOS arm64 (the list,
  and Enter switching to the waiting pane's tab). Not verified: jumping to a pane in
  another window (code path written, no scripted second window).

## Phase 5: optional assistant and MCP

- [ ] Provider layer (OpenAI-compatible, local endpoints), explicit context only
- [ ] MCP client with a trust model

## Phase 6: production readiness

- [ ] Flatpak package (see `docs/adr/0005-linux-distribution.md`)
- [ ] Benchmarks: startup, memory, input latency, throughput, multi-pane. Throughput is measured:
      `crates/halyard-term/examples/throughput.rs` (engine parse, frame build, real PTY sessions,
      headless key echo), results in `docs/PERFORMANCE.md`, and a `--quick` floor runs in CI. Line
      buffered output (`yes | head`) went from 4.6 to 6.1 MB/s and from 33 s to 3.4 s of CPU per
      100 MB on macOS (`trickling_output_is_read_in_batches`). The reader pause is macOS only
      until a Linux A/B on the NixOS box is run. Not yet: startup, memory, end to end input
      latency, GPU render time, multi-pane, Linux numbers
- [x] Threat model written: `docs/THREAT_MODEL.md` (cites code in PRs that were still open when it was written)
- [ ] Security review (no outside review yet)
- [ ] Release automation: `.github/workflows/release.yml` (macOS zip, Linux tarballs, SHA256SUMS, provenance,
      draft release; `dry_run` dispatch builds artifacts only) is written and actionlint-clean, but has
      NOT run on a runner yet, because it needs the app crate on main. The macOS bundle script was run
      locally. The Nix package is the flake PR. Notarization is not done
