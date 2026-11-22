# ADR 0006: An optional assistant that suggests and never runs, and MCP as a bridge

Status: proposed, 2026-11-22. Design only: no code exists for anything below. The full
design, with the tests that will prove it, is [docs/assistant.md](../assistant.md).

## Decision

1. **The assistant is optional and off by default.** `[ai] enabled = false` is already the
   default (`crates/halyard-config/default-config.toml:76-78`,
   `crates/halyard-config/src/config.rs:322-329`). With it off the app constructs nothing
   from the assistant crate, opens no connection and shows no AI chrome. The terminal is
   complete without it.
2. **v1 does three things, all started by the user:** explain the selected text or the last
   failed command, suggest a command, and answer a question about a chosen part of the
   scrollback. It has no tools, no agent loop, no file access and no background activity.
3. **A suggested command is inserted at the shell prompt, never run.** Insert is allowed
   only for a single line with no control or hidden characters, only when a fail-closed
   check finds the shell holding the terminal (and, with shell integration, sitting at a
   prompt), and only through a session command that checks bracketed paste and writes in
   the same step on the session thread. The user reads it, edits it and presses Enter. No
   code path lets model output produce Enter or any other control byte. Bracketed paste is
   required because without it printable keys are commands in a vi-mode line editor (`k#`
   in zsh and `kvZZ` in bash run a history line).
4. **Local first.** The first-class provider is an OpenAI-compatible endpoint on loopback
   (Ollama, llama.cpp's server, LM Studio). Hosted providers (Anthropic, OpenAI) are opt-in,
   and every request to a host that is not loopback needs its own consent after a preview of
   the exact text. The UI calls such endpoints "loopback", not "local" or "private": a
   process on loopback can forward the text elsewhere.
5. **Keys never live in `config.toml`.** A key comes from the OS keychain (macOS Keychain,
   the freedesktop Secret Service on Linux), an environment variable named by
   `api_key_env`, or a helper program named by an absolute path whose argv is checked for
   key-shaped values. The existing refusal of secret-looking keys (`config.rs:479-492`,
   `config.rs:682`) is not relaxed. It has a gap at `main`: names in the schema are exempt
   at any depth under `[ai]`, so `[ai] api_key_env = "<key>"` only warns. The config PR
   closes that first, by exempting each name only in its own table, before adding any new
   name.
6. **A new crate, `halyard-assistant`, owns all network code.** It has no GPUI and no
   `halyard-term` dependency, so it cannot touch a PTY. HTTP is blocking `ureq` on a worker
   thread, the same "threads, not an async runtime" choice the session API made
   (`docs/session-api.md`, "Design, and why").
7. **MCP in v1 is a server, not a client.** Halyard ships `halyard-mcp`, a stdio MCP server
   that is an ordinary paired controller of the existing session API. It adds no authority:
   each MCP tool maps to one existing grant scope, and typing still goes through the
   per-request approval dialog: the bridge never holds "always allow typing" (it pairs with
   an additive `standing_input: false` parameter and refuses any grant that has it), and it
   keeps one grant per MCP host (`--client-name`). An MCP **client** (the assistant calling
   user-configured tools) is deferred until the assistant has shipped and been reviewed.
8. **No assistant in the Flatpak in v1.** The Flatpak has no network permission
   (`packaging/flatpak/io.github.pete_builds.Halyard.yml:31`) and CI pins that; its own
   loopback is not the host's. Enabling it there is a later, separate decision.

## Why

- The product priorities put a fully functional terminal first and say AI stays optional
  and off by default (`docs/PRODUCT.md:8-9`). The standing requirement is "no automatic
  execution of AI-generated commands" (`docs/PRODUCT.md:26-27`), and `AGENTS.md:58-60`
  says terminal output, MCP responses and file contents are never forwarded to a model
  without explicit user action.
- A model that reads terminal output can be steered by that output (prompt injection from a
  file the user `cat`s). The only robust answer is that the model's words cannot become
  keystrokes. Insert-only, plus the user's own Enter, puts a person between the model and
  the shell every time.
- The session API already solved "a tool wants to type into a pane" with scopes, pairing
  and per-request approval (`docs/session-api.md`, "Typing into a pane";
  `crates/halyard-ipc/src/server.rs:1031`). An MCP server built on it inherits all of that
  and needs no new trust model. An MCP client is the opposite: it lets a model, steered by
  untrusted output, call tools with side effects, and it would need a new approval surface
  for arbitrary tool arguments, process spawning for stdio servers, and network access for
  remote ones. That is a second project.
- Loopback endpoints need no account, and keep text on the machine unless the process
  listening there forwards it (a LiteLLM proxy, a cloud model served by Ollama, an
  `ssh -L` tunnel). Halyard cannot see past the socket, so it says "loopback" and states
  this as a residual risk rather than promising privacy.
- `ureq` (3.5.2, MIT OR Apache-2.0, checked on crates.io 2026-11-22) is small and blocking.
  The TLS stack it uses (`rustls` 0.23.46, `ring` 0.17.14) is already in `Cargo.lock`
  through GPUI. GPUI's own HTTP client is not used: it is async, tied to the GPUI executor,
  and Halyard runs GPUI with its `NullHttpClient` default (`crates/halyard-app/src/main.rs:134`,
  no `with_http_client` call), which keeps the terminal binary's network surface at zero
  while AI is off.
- GPUI's credential API is not used for keys either: on Linux it stores every item under
  the label `zed-github-account` (gpui 0.2.2 `src/platform/linux/platform.rs:40`), which
  would show Halyard's API key in the user's keyring under Zed's name.

## Alternatives considered

- **Run model suggestions after a confirmation dialog.** Rejected: a dialog trains the user
  to press the approve chord. Inserting at the prompt shows the command where the user
  already reads commands, editable, with the shell's own syntax highlighting.
- **Auto-explain every failed command.** Rejected: that is background sending. The failure
  bar may offer "Explain", which only opens the preview.
- **An MCP client in v1.** Deferred, for the reasons above. Its trust model is sketched in
  `docs/assistant.md` so the deferral is a decision, not an omission.
- **A hand-written MCP server.** Possible (it is newline-delimited JSON-RPC on stdio), but
  protocol version negotiation is where a hand-rolled server goes wrong. The bridge uses the
  official Rust SDK `rmcp` (Apache-2.0, 3.11.0 on 2026-11-17) in a separate binary, so its
  `tokio` runtime never shares a process with the terminal.
- **`reqwest`.** Needs an async runtime in the app process. `ureq` on a thread is enough
  for one request at a time.
- **Inline suggestions in the terminal grid.** Rejected: the grid belongs to the engine
  and the program; drawing model text there makes it look like program output. A side
  panel keeps the two visibly apart.

## Consequences

- The session API gains one additive pairing parameter, `standing_input`, for the bridge.
- New crates: `halyard-assistant` (library) and `halyard-mcp` (binary). New dependencies
  are `ureq`, a keychain path on each platform (`security-framework` 3.7.0 on macOS and
  `oo7` 0.5.0 on Linux, both already in `Cargo.lock` through GPUI) and, for the bridge
  only, `rmcp`. Each one passes `cargo deny` in the PR that adds it, and the PR lists the
  `cargo tree` diff.
- `[[ai.providers]]` gains `api_key_keychain`, `api_key_command` and `allow_http`. Secret
  name exemptions become per table first (`secret_names_are_exempt_only_in_their_own_table`),
  then each new name is added to the `PROVIDER` schema (`config.rs:469-475`), and the existing tests
  `other_secret_like_keys_are_rejected_too` and `api_key_in_config_is_rejected_with_guidance`
  (`crates/halyard-config/tests/config.rs:258-277`) must keep passing unchanged.
- The threat model gains a prompt-injection section with its residual risk stated plainly
  (`docs/assistant.md`, "Threat model additions").
- Every Phase 5 item stays unchecked in `ROADMAP.md` until its PR lands with the named test.
