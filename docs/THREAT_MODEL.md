# Threat model

Status as of 2026-11-22. This document says what Halyard defends, how, where the code is,
and what it does not defend. Where a mitigation lives in an open pull request, the table says
"open" and the claim holds only once that PR merges. PR states go stale: check
`gh pr view <n>` before relying on one. Nothing here has had an outside security review.

## What Halyard is, for this purpose

A terminal runs a shell as the user and shows whatever that shell and its children print.
Halyard has no privileges beyond the user's. It makes no network calls and has no telemetry
(`AGENTS.md`). The attacker we care about is therefore **not** someone who already runs code
as the user (they can do anything, see below). It is **bytes the user did not choose to
trust**: the output of a program, the content of a file shown with `cat`, a pasted snippet,
a message from an agent, a downloaded binary.

## Assets

| Asset | Why it matters |
|-------|----------------|
| The shell and its keystrokes | Anything typed into a pane runs with the user's authority. The main goal of every mitigation below is that untrusted bytes never become keystrokes. |
| Credentials in the environment and in files | Reachable from any process in a pane. Halyard must not copy them into logs, errors or its own files. |
| The clipboard | Read or written by the app on copy and paste. |
| Repositories and working directories | A pane's cwd is shown and (with checks) read for git state. |
| Agent sessions | Pane state, screen text, notifications, and the ability to type into a pane. |
| The release artifacts | A tampered download is a path to everything above. |

## Trust boundaries

| Boundary | Trust | Notes |
|----------|-------|-------|
| PTY output (everything a program prints) | **Untrusted** | Includes titles, escape sequences, OSC 7 cwd, OSC 9 and 777 notifications, protocol reports, bells. |
| Terminal replies written back to the program | Must carry no program-chosen text | A reply is read as typed input. |
| Paste | Untrusted | Clipboard content comes from anywhere. |
| OSC 52 clipboard access from a program | Not handled yet | See open items. |
| The local session API socket | Same-uid processes only, further gated | `docs/session-api.md` (PR #6). |
| Agent hooks and the `HALYARD_SESSION_ID` protocol | Display only | Reports can be forged by anything printing to the pane. |
| Config and theme files | User-owned, but values may be secrets | `crates/halyard-config`. |
| MCP servers and AI providers | **Not built yet** | Planned boundary: untrusted responses, explicit user context, no auto-run. |
| Distribution (Nix, release zip, tarballs, Homebrew) | Supply chain | See the last section. |

## Threats and mitigations

"Where" names the code or document. Tests are named so a reader can run them.

### Output tries to type into the shell

| Threat | Mitigation | Where |
|--------|-----------|-------|
| A file sets the window title to a command, then the program asks the terminal to report the title (`CSI 21 t`); the reply lands in the shell as input (the iTerm2 CVE-2024-38396 class). | Terminal replies pass an **allowlist** per escape sequence. CSI replies whose parameter bytes are 0x20 to 0x3f with no free text, OSC 4 and 10 to 19 color reports in `rgb:` hex form, and the XTVERSION and DA3 DCS strings are kept. Title and icon reports, DECRQSS, XTGETTCAP and anything else are dropped. | `safe_replies` in `crates/halyard-term/src/engine.rs` (PR #2, open). Tests `title_reports_are_never_sent_back`, `reply_filter_allowlist`, `safe_query_replies_still_flow`, and end to end `title_report_cannot_inject_a_command`. |
| A pasted snippet ends the bracketed paste early (`ESC [ 201 ~`) and the rest runs as commands, or carries escape and control bytes. | Paste goes through the engine's encoder, which wraps the text in bracketed paste when the program enabled it and cannot be ended early by an embedded `ESC [ 201 ~`. Neutralizing other control bytes (replaced with spaces) is libghostty's `paste::encode` behavior and is not separately tested here. `paste_is_safe` lets the UI confirm multi-line pastes. | `Engine::encode_paste` and `paste_is_safe` (PR #2, open). Test `paste_is_bracketed_only_when_enabled`. |
| A tool or agent types a command through the session API. | `session.send_text` needs the `input:write` scope **and** a per-request approval dialog that shows control and hidden characters escaped. A standing "always allow" never covers text containing any control (`Cc`), format (`Cf`), line or paragraph separator (`Zl`, `Zp`) or `Default_Ignorable_Code_Point` character (tags, variation selectors, zero-width and bidi characters, Hangul fillers). Every dialog also shows the text typed under "always allow" since the last approval, so approving Enter shows what it runs; if that text exceeds 4 KiB the request is refused rather than shown in part. The grant is re-checked after waiting for the dialog and after it closes. The server enforces all of this; the dialog cannot weaken it. | `crates/halyard-ipc/src/text.rs`, `server.rs`, `docs/session-api.md` "Typing into a pane" (PR #6, merged). Tests `send_text_without_standing_permission_asks_every_time`, `always_allow_still_asks_for_newlines_and_controls`, `every_hidden_range_needs_approval_and_is_escaped`, `approving_enter_shows_the_text_typed_under_standing_permission`, `padding_that_truncates_the_pending_line_is_refused_not_shown`, `standing_sends_wait_for_an_open_dialog_and_fail_if_revoked_meanwhile`. |

### Output forges what Halyard displays

| Threat | Mitigation | Where |
|--------|-----------|-------|
| Control or bidi-override characters in titles, notifications, cwd or agent messages make the UI show something other than the truth. | Every untrusted string is sanitized (controls and bidi overrides stripped, length capped) before display. | `crates/halyard-shell/src/sanitize.rs` (PR #5, open). Test `strips_controls_and_bidi`. |
| OSC 7 reports a cwd. A path with a newline would be displayed as a different path, or a remote host's path is looked up on this machine. | The cwd is always shown, but used as a local directory only when the host is empty, `localhost` or this machine, **and** the path survived sanitizing unchanged. | `docs/agents.md` "working directory", `crates/halyard-shell` (PR #5, open). |
| A planted FIFO, device or socket at `.git/HEAD` hangs the reader. | Git state is read only from regular files, type-checked before and after a non-blocking open, at most 4 KiB. | `read_small` in `crates/halyard-agents/src/git.rs` (PR #5, open). Test `fifo_head_does_not_block`. |
| Hostile sequences exhaust memory or CPU: huge OSC strings, endless prompt marks, deep block nesting. | OSC strings are capped at 8 KiB, CSI at 32 bytes, running command blocks at 32 (the outermost is closed), history at 1000. | `DEFAULT_MAX_OSC_LEN`, `MAX_NESTING` in `crates/halyard-shell` (PR #5, open). Tests `hostile_nesting_is_bounded_and_linear`, `hostile_marks_through_the_scanner`. |
| A forged agent report (`OSC 7717`) hides a real permission prompt behind a fake `working`. | A real notification or bell still marks the pane unread and sorts it above working panes. A report never runs, focuses or sends anything. | `docs/protocol.md` "Trust" (PR #5, open). Test `protocol_state_cannot_hide_a_real_notification`. |

### Local processes and the session API

| Threat | Mitigation | Where |
|--------|-----------|-------|
| Another user, the network, or a web page reaches the API. | Unix domain socket only (no TCP). Directory created 0700 and refused if not ours, if a symlink, or if group or world accessible. Socket 0600. Peer uid from the kernel must equal ours on **every** connection. | `crates/halyard-ipc/src/paths.rs`, `peer.rs` (PR #6, open). Tests `creates_dir_with_mode_0700`, `refuses_symlink_to_private_dir`. |
| A process in pane A speaks for pane B, or a process outside any pane poses as an agent. | `session.attach` needs both **process ancestry** (the kernel-reported peer pid must descend from that pane's PTY child) and the pane's 256-bit token. | `crates/halyard-ipc/src/peer.rs` (ancestry), `auth.rs` (tokens). Tests `a_process_in_another_pane_cannot_spoof_this_one`, `an_unrelated_process_is_rejected`, `the_right_process_with_the_wrong_token_is_rejected`. |
| An unapproved tool lists panes, reads screens or focuses. | Pairing shows the user the requested scopes, the kernel-reported pid and executable path, and the tool's self-reported name. Scopes are named and revocable, re-checked on every request. | Tests `pairing_denied_creates_no_grant`, `scopes_gate_each_method`, `revocation_cuts_off_live_connections_and_the_token`. |
| A stolen grants file. | It stores SHA-256 hashes of 256-bit tokens, never tokens, is written atomically at 0600, and is refused at load if group or world accessible. | Tests `grants_file_never_contains_the_raw_token`, `refuses_group_readable_grants_file`. |
| A buggy or hostile client exhausts the server. | 64 KiB request cap, per-connection rate limit, 64 connections, a 10 second handshake, bounded event queues, three pairing attempts, one dialog at a time. | Tests `oversized_message_is_rejected_and_the_connection_closed`, `rate_limit_rejects_bursts_then_disconnects_abusers`, `connection_limit_closes_extra_connections`. |

### Secrets in config

| Threat | Mitigation | Where |
|--------|-----------|-------|
| A user pastes an API key into `config.toml`, and an error message prints it. | No error or warning ever contains a value. Messages hold a key path, a line number and the expectation. Under `[ai]`, unknown key names are hidden too, because the name may be the secret. Secret-looking keys are rejected with a message that does not echo them. The config format has no field for secrets at all. | `crates/halyard-config/src/locate.rs`, `docs/configuration.md` "Error messages never contain values" (PR #1, merged). Tests `secret_in_kind_is_not_echoed`, `secret_as_integer_api_key_env_is_not_echoed`, `other_secret_like_keys_are_rejected_too`. |

### Shell integration and the engine

| Threat | Mitigation | Where |
|--------|-----------|-------|
| Halyard edits a user's rc files or runs something at login. | It does not. The integration scripts are embedded, and Halyard prints the one line the user adds to their own rc file. The scripts do nothing outside a Halyard pane. | `crates/halyard-shell/src/integration.rs` (PR #5, open). |
| A memory-safety bug in the VT engine, which parses hostile bytes. | The engine is Ghostty's `libghostty-vt`, written in Zig, pinned to one commit. The Rust side keeps it on its own thread and treats its output as untrusted. This lowers the risk. It does not remove it. | ADR 0002; `ghostty-pin` Nix check (PR #4, open). |

## Not mitigated, or not built yet

Said plainly, so nobody assumes otherwise.

1. **Not notarized.** The macOS app is ad-hoc signed only. Gatekeeper warns on a downloaded
   copy and the user has to override it. There is no Developer ID certificate or notarization
   (`docs/INSTALL.md`). Verify downloads with `SHA256SUMS` and
   `gh attestation verify`.
2. **OSC 52 (a program reading or writing the clipboard) has no Halyard policy yet.** No
   handler is wired in the app, and what the engine does with the sequence by default has not
   been verified. The intended rule when it is built: write only, user-visible, with a size
   cap, and reading the clipboard off by default. Until then, treat this as open.
3. **Agent state is spoofable.** Any output can print the protocol sequence. It affects only
   what is displayed (see the table above), and it is documented as untrusted. A pane's
   `session=` field filters crossed wires and is not authentication.
4. **A same-uid attacker can do anything.** Code running as the user can read each pane's
   `HALYARD_SESSION_TOKEN` from the process environment, `ptrace` Halyard or the shell
   (where the OS allows it), open the PTY device and write to it, or edit the grants file.
   The session API is not made weaker by this and does not claim to be stronger. Its
   approval rules protect against a confused or prompt-injected tool that only uses the
   documented API. Root can do anything.
5. **The session API is not wired into the app yet.** The server library and `halyard-msg`
   are implemented and tested; the running terminal does not start the server
   (`docs/session-api.md`, `ROADMAP.md`). The approval dialogs do not exist in the UI.
6. **Pid reuse** in the ancestry check is narrowed by the token, not closed. Linux
   `SO_PEERPIDFD` is not used.
7. **`screen:read` exposes secrets on screen.** It is gated once at pairing, not per request.
8. **Same-user denial of service** (holding the connection cap, repeated pairing dialogs) is
   limited, not prevented.
9. **Output can still do harmful things within the terminal's job.** A hostile program can
   clear the screen, hide text with matching colors, or print text that looks like a prompt.
   Halyard does not try to detect this.
10. **Programs that act on single keystrokes.** Even with the controls filtered, plain typed
    text can be dangerous in a TUI that accepts `y` without Enter. That is why "always allow
    typing" is off by default.
11. **AI providers and MCP are not built.** When they are, responses are untrusted, context
    goes to a model only by explicit user action, and nothing a model returns is run
    automatically. This section needs real design and review at that point.
12. **No outside review, no fuzzing of Halyard's own parsers yet.** The parsers are unit
    tested and have hostile-input tests, which is weaker than fuzzing.

## Supply chain and distribution

| Risk | Control | Gap |
|------|---------|-----|
| A tampered or substituted release download | `SHA256SUMS` attached to each release, and SLSA build provenance generated by `actions/attest-build-provenance` in `.github/workflows/release.yml`. The release is created as a **draft** and published by a person. | Provenance proves which workflow built a file, not that the source is free of bugs. It has not run on a real tag yet. Checksums on the same page as the files protect against corruption, not against someone who can edit the release. |
| A compromised GitHub Action | Every action is pinned by full commit SHA, workflows default to no permissions, and each job gets the least it needs (`contents: write` only on the publish job). Release builds do not restore build caches. | Pins need manual bumps. |
| A malicious or vulnerable dependency | `cargo deny` (licenses, advisories, bans, sources) in CI. `Cargo.lock` committed and builds use `--locked`. A vendored patch for one dependency is documented in `vendor/README.md` (PR #3, merged). | `cargo deny` only knows published advisories. Crates are not individually audited. |
| Nix | The flake pins every input, builds offline, and a check fails if Ghostty's pinned commit drifts from the crate's (PR #4, open). | Not yet verified on a real GPU. |
| Flatpak | Every source is pinned by sha256 or commit and the build runs offline; CI checks the crate list against `Cargo.lock` and the Ghostty commit against the crate's, and fails if the installed bundle gains network access. The CI container image and `flatpak-builder` action are pinned by digest and SHA. See `docs/flatpak.md`. | **The sandbox does not confine the shell, by design:** `--talk-name=org.freedesktop.Flatpak` lets the app run anything on the host through `flatpak-spawn --host`, so the Flatpak is as trusted as a native install. CI bundles are unsigned and Halyard is not on Flathub. |
| Homebrew | No formula or cask exists, so there is nothing to attack and nothing to install this way. | Planned only after notarization. |
| The build host | The macOS bundle is built on a GitHub-hosted runner from a tag. | Builds are not yet reproducible bit for bit, and nobody independent rebuilds them. |
