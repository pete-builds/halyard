# Halyard prior art (as of 2026-11-22)

Method: WebFetch of primary pages (GitHub READMEs, crates.io API, vendor docs) plus WebSearch. Where a page did not state something, it is marked "unverified". GitHub pages were fetched through a summarizer, so license claims on pages that did not quote LICENSE text are noted.

## Summary table

| Project | License | Stack | VT engine | Renderer | Extension model | AI / agent features |
|---|---|---|---|---|---|---|
| Warp | AGPL-3.0 client; MIT for `warpui_core` and `warpui` | Rust | Alacritty-derived (listed dependency; exact use unverified) | Own UI framework (warpui) | Not described | Built-in agent, BYO CLI agents (Claude Code, Codex, Gemini CLI), cloud agents closed |
| Ghostty | MIT | Zig core, SwiftUI (macOS), GTK (Linux) | Own (libghostty-vt) | Metal / OpenGL | Config file, AppleScript, App Intents | None; has an AI_POLICY.md |
| libghostty-vt | MIT (crate: MIT OR Apache-2.0) | Zig, C ABI | Itself | None (VT only) | C API | None |
| WezTerm | MIT | Rust | Own (`termwiz`, unverified from page) | GPU (API unverified) | Lua, built-in mux | None |
| Wave Terminal | Apache-2.0 | Electron, Go, TypeScript (inferred from file tree) | Unverified | Web (xterm.js, unverified) | `wsh` CLI, blocks; no plugin API described | Wave AI, BYOK, local models |
| Rio | MIT | Rust | Own (`rio-vt`, inferred) | Sugarloaf on wgpu | TOML config | None |
| Zed terminal | `terminal_view` GPL-3.0-or-later; GPUI Apache-2.0 | Rust | `alacritty_terminal` (Apache-2.0) | GPUI | Zed extensions | Zed agent panel, ACP client |
| Alacritty | Apache-2.0 and MIT | Rust | `alacritty_terminal` | OpenGL | TOML | None; no tabs or splits by design |
| Kitty | GPL-3.0 | C, Python, Go | Own | OpenGL | Kittens, remote control | Minimal (unverified) |
| iTerm2 | GPL-3.0 (sidebar says GPL-2.0, README says GPLv3) | Objective-C, Swift, Python API | Own | Metal | Python API, AppleScript, triggers | AI chat, Claude Code hook event file in repo |
| cmux | GPL-3.0-or-later (server pieces BSL 1.1) | Swift, AppKit | libghostty | libghostty (GPU) | Unix socket CLI, `cmux.json` | Agent attention rings, hooks, `claude-teams` |
| Paneflow | MIT | Rust, GPUI | `alacritty_terminal` 0.26 | GPUI | JSON-RPC over Unix socket | Claude Code, Codex, OpenCode launchers, `ai.*` lifecycle events |
| tty7 | Apache-2.0 | Rust, GPUI | `alacritty_terminal` | GPUI | `tty7` CLI | Detects 28 agents, status dots, tray, hooks |
| herdr (ogulcancelik) | Apache-2.0 per that README (other forks say AGPL-3.0) | Rust | Unverified | TUI | Integrations | Blocked / working / done / idle states |
| Sheprd (penso/herdr-gpui) | Apache-2.0 | Rust, GPUI | Unverified | GPUI | MCP | "Idle, working, waiting on you" dot, notifications |
| Ghostex | MIT | Ghostty, GPUI, CEF | Ghostty | GPUI | Plugins | Sidebar for agents, mobile notifications |
| gpui-ghostty (Xuanwo) | Apache-2.0 | Rust, GPUI, Zig | Ghostty core v1.2.3 | GPUI | n/a | None (building block) |
| gpui-cn | Unverified | Rust, GPUI | libghostty-vt via `ghostty` feature | GPUI | Component library | None |
| Codirigent | GPL-3.0 | Rust, GPUI | Unverified | GPUI | Unverified | Agent workspace for Ghostty, Superset, tmux users |
| Superset | ELv2 per official site (one source says Apache-2.0: conflict) | Unverified | Unverified | Unverified | Unverified | Parallel agents in git worktrees |
| Conductor | Proprietary | macOS app | n/a | n/a | n/a | Parallel agents in workspaces |

## Per-project notes

### Warp
- Open-sourced late April 2026 (blog April 28; one source says release April 30). Client is AGPL-3.0; `warpui_core` and `warpui` are MIT. https://github.com/warpdotdev/warp , https://www.warp.dev/blog/warp-is-now-open-source , https://www.helpnetsecurity.com/2026/04/30/warp-open-source-client/
- Rust, Tokio, Alacritty and NuShell named as dependencies, `flake.nix` present. https://github.com/warpdotdev/warp
- AI: built-in agent plus BYO CLI agents (Claude Code, Codex, Gemini CLI). OpenAI is founding sponsor and the README says agentic management workflows are powered by GPT models. https://github.com/warpdotdev/warp
- Oz cloud orchestration is not open source (one secondary source). https://www.helpnetsecurity.com/2026/04/30/warp-open-source-client/
- Reuse: the MIT `warpui` crates may be copied or depended on, with the MIT notice. Everything else is AGPL: study behavior only. Unverified: which crate sits under which license beyond the two named ones; I could not fetch LICENSE-AGPL (404 at the guessed path).
- Design note: Claude Code can emit OSC 777 via a `terminalSequence`, with Warp named as an example target. https://code.claude.com/docs/en/hooks

### Ghostty and libghostty-vt
- Ghostty: MIT, Zig core, SwiftUI app on macOS, GTK on Linux, Metal and OpenGL renderers, Kitty graphics protocol, local-only crash reports. https://github.com/ghostty-org/ghostty
- libghostty-vt: portable C library, version 1.0.1, stable C ABI from 1.0, MIT, "no dependencies" (libc optional), builds for Linux, macOS, iOS, Android, FreeBSD, Windows, Wasm. https://lib.ghostty.org/vt
- Rust crate `libghostty-vt` 0.2.2 (2026-09-28), license field "MIT OR Apache-2.0" on crates.io, while the repo README says MIT. Treat the Ghostty C source as MIT. https://crates.io/api/v1/crates/libghostty-vt , https://github.com/uzaaft/libghostty-rs
- Build requires Zig 0.15.2 exactly for crate 0.2.2 (measured 2026-11-22: 0.17.1 fails, Ghostty's pinned commit sets minimum_zig_version 0.15.2; upstream master's README says 0.17.x because it pins a newer Ghostty) on PATH, fetches Ghostty at a pinned commit by default, static link by default, `pkg-config` feature to use a system lib, MSRV Rust 1.96. The crate says libghostty-vt is pre-1.0 while lib.ghostty.org says 1.0.1: a version-story mismatch worth watching. https://github.com/uzaaft/libghostty-rs
- Reuse: fully reusable. Ghostty app code is MIT too, so even ideas and snippets are legally fine with notice retained.

### WezTerm
- MIT license (LICENSE.md). https://raw.githubusercontent.com/wezterm/wezterm/main/LICENSE.md
- Rust, GPU-accelerated terminal and multiplexer with a Lua config; README calls it a "spare time project". https://github.com/wezterm/wezterm
- Reuse: MIT, so `portable-pty` and code are reusable. Maintenance risk is on the dependency side (portable-pty last published 2025-02-11). https://crates.io/api/v1/crates/portable-pty

### Wave Terminal
- Apache-2.0, free, no account required, Wave AI with BYOK (OpenAI, Claude, Gemini, Azure) and local models (Ollama, LM Studio). Block-based UI (terminal, editor, browser, AI). Command execution by the AI is "coming soon". https://github.com/wavetermdev/waveterm
- Electron plus Go is inferred from the file tree, not stated. Telemetry: the README fetch did not mention it, so unverified.
- Reuse: Apache-2.0, ideas and code usable with notices. Different stack, little code to reuse.

### Rio
- MIT, Rust, Sugarloaf renderer on wgpu (inferred from topics), `rio-vt` crate, browser (wasm) target, no AI features. https://github.com/raphamorim/rio
- Reuse: MIT. A candidate for study of a non-alacritty Rust VT and a wgpu renderer.

### Zed terminal
- Zed is "GPL-3.0-or-later, with Apache-2.0 components where marked". `crates/terminal_view/Cargo.toml` says `GPL-3.0-or-later`; `crates/terminal` depends on `alacritty_terminal`. https://github.com/zed-industries/zed , https://raw.githubusercontent.com/zed-industries/zed/main/crates/terminal_view/Cargo.toml , https://raw.githubusercontent.com/zed-industries/zed/main/crates/terminal/Cargo.toml
- `gpui` crate: Apache-2.0, version 0.2.2 published 2025-10-22, crate record touched 2026-10-30. https://raw.githubusercontent.com/zed-industries/zed/main/crates/gpui/Cargo.toml , https://crates.io/api/v1/crates/gpui
- Zed's `terminal` crate license field was not fetched (only `terminal_view`): unverified, assume GPL until checked.
- Reuse: do not copy `terminal`/`terminal_view`. GPUI itself is fine.

### Alacritty and alacritty_terminal
- Apache-2.0 and MIT dual files. OpenGL. TOML config. Deliberately no tabs or splits. https://github.com/alacritty/alacritty
- `alacritty_terminal` 0.26.0 published 2026-04-06, Apache-2.0. https://crates.io/api/v1/crates/alacritty_terminal
- Reuse: fully usable. Most GPUI terminals (Paneflow, tty7, Zed) use it, which is the alternative VT engine to libghostty-vt.

### Kitty
- GPL-3.0. C, Python, Go, OpenGL. https://github.com/kovidgoyal/kitty
- Protocols that matter to us and are documented openly: OSC 99 notifications (id, urgency, `o=unfocused` filter, click actions) and shell integration marks. https://sw.kovidgoyal.net/kitty/desktop-notifications/ , https://sw.kovidgoyal.net/kitty/shell-integration/
- Reuse: GPL, study specs only. Protocol specs are documentation, implementing them from the spec is fine.

### iTerm2
- GPL-3.0 per README (sidebar says GPL-2.0: conflicting, check LICENSE). Python scripting API, AppleScript, triggers, shell integration with per-prompt marks, AI Chat window that can read terminal contents. Repo contains `ClaudeCodeHookEvent.swift`, `CLAUDE.md`, `.claude/agents`, meaning iTerm2 is already integrating Claude Code hooks (contents unverified). https://github.com/gnachman/iTerm2
- Reuse: GPL, study only.

### cmux (closest competitor on attention management)
- GPL-3.0-or-later app, BSL 1.1 for web/server code. Swift and AppKit, libghostty rendering, macOS only (iOS beta). https://github.com/manaflow-ai/cmux
- Attention: reads OSC 9/99/777, `cmux notify` CLI for agent hooks, blue ring on waiting panes, notification panel, jump to latest unread, sidebar with git branch, PR status, ports. Session resume via hooks (`cmux hooks setup`). `claude-teams` mode spawns teammates as native splits. Unix socket and CLI control everything. https://github.com/manaflow-ai/cmux
- No Linux, not Rust, GPL. No ACP mention (not found).
- Reuse: GPL, study behavior only.

### Rust and GPUI agent terminals (already exist)
- Paneflow: MIT, Rust, GPUI, `alacritty_terminal` 0.26, JSON-RPC 2.0 server with `ai.session_start`, `prompt_submit`, `tool_use`, `notification`, `stop`, `session_end`. https://dev.to/arthurj-dev/building-a-native-terminal-for-ai-coding-agents-in-rust-gpui-2bg4 , https://github.com/ArthurDEV44/paneflow
- tty7: Apache-2.0, Rust, gpui, `alacritty_terminal`, daemon-owned sessions that survive window close, detects 28 agents, hooks install per agent, status dots, tray icon, `tty7 wait --until free`. No ACP. https://raw.githubusercontent.com/l0ng-ai/tty7/main/README.md
- herdr: terminal-based multiplexer with agent states blocked / working / done / idle. Several forks with different licenses (AGPL-3.0, Apache-2.0); the ogulcancelik repo README says Apache-2.0 in one fetch and AGPL-3.0-or-later with a commercial option in a search snippet: conflicting, unverified. https://raw.githubusercontent.com/ogulcancelik/herdr/master/README.md , https://betterstack.com/community/guides/ai/herdr-ai-agent/
- Sheprd (penso/herdr-gpui): Apache-2.0, GPUI front end to the herdr daemon, status dot and notifications, MCP server. https://raw.githubusercontent.com/penso/herdr-gpui/main/README.md
- Ghostex: MIT, Ghostty plus GPUI plus CEF, many agents, mobile app. https://raw.githubusercontent.com/maddada/Ghostex/main/README.md
- Codirigent: GPL-3.0, Rust GPUI workspace. https://feedbagel.com/post/codirigent-open-source-terminal-workspace-for-enhanced-ghostty-superset-and-tmux (secondary source)
- Building blocks: gpui-ghostty (Apache-2.0) https://github.com/Xuanwo/gpui-ghostty ; gpui-cn with a `ghostty` terminal feature, license unverified https://docs.rs/crate/gpui-cn/0.7.0
- Not Rust but relevant: Superset (worktree-per-agent; ELv2 per its site, Apache-2.0 per one FAQ: conflict) https://superset.sh/llms.txt ; Conductor (proprietary Mac app; a third-party post claims an acquisition on 2026-11-04, unverified) https://www.conductor.build/ , https://hashnode.com/blog/conductor-alternatives.md ; Crystal (stravu): nothing found, unverified.

## Escape sequences and agent signals

### OSC 133 (FinalTerm semantic prompts)
- A: just before the prompt. B: just after the prompt, before user input. C: just before command output. D: command finished, `OSC 133 ; D ; [exit status] ST`, zero is success; D after B without C aborts. https://iterm2.com/documentation-escape-codes.html
- Kitty's shell scripts emit A (with `k=s` for secondary prompt), C (with `cmdline=`), D (with status) and do not use B. https://sw.kovidgoyal.net/kitty/shell-integration/
- WezTerm describes it as defining Input, Output and Prompt zones and links to the semantic-prompts proposal. The freedesktop spec page was blocked by Anubis, so the canonical text is unverified here. https://wezterm.org/shell-integration.html
### OSC 7 (cwd)
- `ESC ] 7 ; file://HOST/PATH ST`, emitted on directory change or each prompt. https://wezterm.org/shell-integration.html Kitty uses a `kitty-shell-cwd://` scheme in its own scripts. https://sw.kovidgoyal.net/kitty/shell-integration/
### OSC 9, 777, 99
- OSC 9 `; message ST` posts a notification (iTerm2 origin). `OSC 9;4` is a progress variant, and ConEmu numeric subcommands `1;` to `12;` are not notifications. https://iterm2.com/documentation-escape-codes.html , https://sw.kovidgoyal.net/kitty/desktop-notifications/
- OSC 99 (kitty) adds ids, update and close, urgency 0 to 2, `o=unfocused|invisible|always`, click actions with focus reporting. https://sw.kovidgoyal.net/kitty/desktop-notifications/
- OSC 777: supported in Claude Code's own hook example (`terminalSequence`). Format and origin (`notify;title;body`) not verified from a primary page.

### How agents signal "needs input" today
- Claude Code Notification hook fires on notification types including `permission_prompt`, `idle_prompt`, `agent_needs_input`, `agent_completed`, `elicitation_dialog`. Stop hook fires when Claude finishes a turn, includes `last_assistant_message`. `PermissionRequest` hook fires when a tool call needs a decision and can return allow or deny. A hook can return `terminalSequence`. https://code.claude.com/docs/en/hooks
- Claude Code's built-in channel setting `preferredNotifChannel` (auto, terminal_bell, iterm2, kitty, ghostty, and others); `auto` notifies in iTerm2, Ghostty, Kitty; bell elsewhere; reported bugs with `ghostty` and `none` values. https://claudeissues.com/issue/19636-bug-preferrednotifchannel-none-does-not-suppress-osc-notifications-in-ghostty , https://claudeissues.com/issue/41484-add-ghostty-desktop-notification-support-osc-9-99 (secondary tracker mirrors)
- A recurring gap: the hook payload lacks a terminal or tab identifier, so per-tab attention needs the terminal to inject an env var. https://claudeissues.com/issue/57592-feature-per-tab-focus-aware-notifications-hook-payload-lacks-terminal-tab-identi
- Codex CLI: `[tui] notification_method = "osc9"`, `tui.notifications` events `agent-turn-complete` and `approval-requested`; external `notify` program reportedly fires only for turn complete. Sources are community write-ups; the official reference did not load. https://codex.danielvaughan.com/2026/04/13/codex-cli-notification-pipeline-osc9-hooks-alerts/ , https://developers.openai.com/codex/config-reference
- Codex also has lifecycle hooks (a config doc mentions hooks and `allow_managed_hooks_only`). https://github.com/openai/codex/blob/main/docs/config.md

### ACP status
- Protocol created by Zed; about 60 agents listed, including Claude Agent, Codex CLI, Cursor, Gemini CLI, GitHub Copilot, Goose, OpenCode, OpenHands, Kiro CLI. Editors listed: Zed, JetBrains, VS Code, Neovim, Emacs, Obsidian. https://zed.dev/acp , https://agentclientprotocol.com/overview/agents
- Claude: via a Zed adapter wrapping the Claude Code SDK (listed as "Claude Agent"). Codex: `codex-acp` adapter, open source. Gemini CLI: native (Google reference implementation). OpenCode: `opencode acp`, newline-delimited JSON-RPC on stdio. https://zed.dev/blog/claude-code-via-acp , https://zed.dev/blog/codex-is-live-in-zed , https://opencode.ai/docs/acp
- The `--experimental-acp` flag for Gemini CLI: not confirmed (a third-party config uses `--acp`). Unverified.
- Rust SDK `agent-client-protocol` 5.5.1 (2026-11-21), Apache-2.0, repo agentclientprotocol/rust-sdk. https://crates.io/api/v1/crates/agent-client-protocol
- Important: ACP is a structured channel that replaces the TUI. An ACP client does not show the native Claude Code TUI, so "run the real CLI in a PTY" and "drive via ACP" are two different products. tty7 explicitly runs agents unwrapped in a PTY. https://raw.githubusercontent.com/l0ng-ai/tty7/main/README.md

## What makes Halyard different

### Claimed differentiators that already exist
1. **Agent attention management (working / finished / waiting).** Exists in cmux (rings, panel, jump to unread), tty7 (status dots, tray, hooks), herdr (blocked/working/done/idle), Sheprd (waiting on you), Paneflow (`ai.*` events). Not open.
2. **Rust and GPUI agent terminal.** Paneflow, tty7, Sheprd, Ghostex, Codirigent are all Rust or GPUI-based today.
3. **Ghostty VT engine in GPUI.** gpui-ghostty and gpui-cn's `ghostty` feature, plus Ghostex, already do it.
4. **Vendor-neutral, many agents.** tty7 detects 28, herdr and Ghostex list Claude Code, Codex, OpenCode and more.
5. **Local-first, no account, BYOK.** Wave (no account, BYOK, Ollama), Ghostty (crash reports local only), most of the above.
6. **Hooks plus OSC notifications as the signal path.** cmux and tty7 do exactly this.
7. **Strong Nix support at the packaging level.** Warp ships a `flake.nix`; no one found with NixOS as a headline. Weak existing claim only.

### Genuinely open (as far as this research found)
1. **Permissive license on the whole app, with this combination.** cmux is GPL, Warp AGPL, Codirigent GPL, Zed's terminal GPL. Permissive entrants (tty7 Apache-2.0, Paneflow MIT, Ghostex MIT) exist but are small. A polished MIT OR Apache app is open ground, not an empty one.
2. **ACP as a first-class session type next to raw-PTY agents, with one attention model across both.** None of the Rust terminals checked mention ACP. Unverified for tty7 internals and Ghostex.
3. **A Linux-first, macOS-equal story.** cmux is macOS only; Conductor is macOS only.
4. **Permission requests handled in-terminal via `PermissionRequest` and ACP permission calls.** Not found anywhere (no one found offering approve/deny from the sidebar). Weak evidence: search was shallow.
5. **A terminal-injected session id (env var) that agent hooks echo back, solving the "hook payload lacks tab id" gap in a vendor-neutral way.** cmux has `cmux notify`; a documented open protocol is not found. Moderate.

### Honest read
The differentiator is not the idea (many people shipped it in 2026) but execution quality, licensing, and Linux/Nix support. Positioning as "terminal-first, useful with AI off" is fair but shared with Ghostty, WezTerm, and tty7. Check tty7 and Paneflow before building: contributing or forking (both permissive) may beat a from-scratch start.
