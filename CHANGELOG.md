# Changelog

## 0.1.0 (2026-11-22): first preview

The first public preview. Halyard runs as a usable terminal on macOS and on Linux (NixOS,
Fedora, Debian, Arch, Ubuntu). Changes were reviewed before merging by an agent other than
their author (see the pull requests), and the "not yet verified" list below is part of the
release.

### Terminal
- Real PTY sessions on Ghostty's VT engine (libghostty-vt 0.2.2), GPU rendering through GPUI.
- 256 colors and truecolor, bold, italic, underline, strikethrough, inverse, wide CJK glyphs,
  cursor styles, scrollback, resize with reflow, bracketed paste, mouse reporting, and the
  Kitty keyboard protocol through the engine's encoder.
- `scrollback_lines` means lines: 10,000 lines of ordinary text are kept at any width up to
  256 columns (dense colored, emoji or combining-mark output keeps fewer; see
  `docs/PERFORMANCE.md`).
- Line-buffered floods (one short write per line) no longer cost a full CPU core on macOS.
- Shells get a UTF-8 locale when Halyard is launched without one (for example from the Dock).
- A missing `$HOME` starts the shell in `/` with a warning instead of failing.

### Interface
- Tabs, split panes with draggable dividers, keyboard pane navigation, zoom.
- A command palette with fuzzy search and live theme preview.
- Saved workspaces: layout, tab names and directories restored on launch, with limits on
  what a damaged file can open.
- Config and themes applied live (fonts, colors, keybindings, cursor), with an error banner
  that keeps the last good settings.
- Confirmation before closing a pane, tab or window that runs a command, and before a
  multi-line paste when the program has bracketed paste off.

### Configuration and themes
- `~/.config/halyard/config.toml` with live reload that follows Home Manager symlinks. Error
  messages never contain config values; secret-looking keys are refused.
- Six original themes held to a WCAG contrast rule; Ghostty theme import.

### Shell integration and agents
- zsh, bash and fish integration (prompt marks, working directory), opt-in by sourcing one
  line; Halyard never edits rc files. Command blocks: jump between prompts, copy the last
  command's output, failure marks in the margin.
- Agent awareness for Claude Code, Codex, OpenCode, Gemini CLI and others: working, waiting
  and finished marks on tabs and panes, one desktop notification when an agent in a
  background window needs you, and an agent dashboard that jumps to the pane. Every state
  comes from an observable signal.
- The session API: agents and tools talk to Halyard over a local socket. A pane's own agent
  is recognised by process ancestry and token; outside tools pair with scopes you approve.
  Typing into a pane always shows you the full text first, and approval needs a deliberate
  key after a short delay. Paired tools can be revoked. See `docs/session-api.md`.

### Security
- Terminal query replies pass an allowlist, so a file you `cat` cannot type into your shell
  (the iTerm2 CVE-2024-38396 class).
- Notification text from terminal output is escaped (Linux markup) and only Halyard protocol
  reports carrying the pane's session id count.
- Pasted control bytes are neutralized; carriage returns count as line breaks for the paste
  confirmation.
- `docs/THREAT_MODEL.md` lists what Halyard defends and what it does not.

### Platforms and packaging
- macOS: universal app bundle (arm64 and x86_64), ad-hoc signed, NOT notarized.
- Nix flake: `nix build`, `nix run`, `nix develop`, built offline from pinned inputs on
  x86_64-linux, aarch64-linux and aarch64-darwin, with Home Manager and NixOS modules.
- Flatpak (`io.github.pete_builds.Halyard`): offline build for x86_64 and aarch64; shells run
  on the host through `flatpak-spawn`, which means the sandbox does not confine them.
- Release workflow: tags only from main, SHA256SUMS, SLSA build provenance.

### Verified
- macOS arm64: real windows, driven and screenshotted.
- NixOS 26.05 on an RTX 4070 Ti: the Nix-built app under sway (Vulkan) and XWayland, typing,
  vim, htop, Ctrl+C, resize and paste; launch in KDE Plasma 6.6 Wayland.
- NixOS VM test in CI: the real app under Wayland and X11 (typing, vim, Ctrl+C, paste).
- CI on every change: macOS 15, Ubuntu 24.04, and Fedora, Debian, Arch and Ubuntu containers
  (build plus headless tests), `nix flake check` on three systems, both Flatpak bundles,
  cargo-deny.

### Known issues and not yet verified
- Not notarized on macOS: Gatekeeper asks on first open (see `docs/INSTALL.md`).
- The Intel macOS slice has never been run.
- The Flatpak has not been run on a real desktop (no window or host shell observed yet).
- Wayland pointer selection and wheel scrolling are not exercised in automated tests.
- IME, dead keys and key repeat on a physical keyboard are not verified by a person.
- The session API is off inside Flatpak (process ancestry cannot pass there).
- The small-read pause that cuts flood CPU is macOS only until it is measured on Linux.
- GitHub advisory GHSA-38c5-483c-4qqp (`grid` 0.18 integer overflow) reaches Halyard through
  gpui 0.2.2 > taffy 0.9 with no upgrade path until GPUI moves; Halyard draws no CSS grid
  layouts (`docs/THREAT_MODEL.md`).
- Memory is not measured systematically or optimized yet. Spot readings: about 70 to 120 MB
  on macOS, 255 to 310 MB on NVIDIA Linux, mostly shared driver mappings (`docs/PERFORMANCE.md`).
- Not built yet: the optional AI assistant and MCP (roadmap phase 5), ACP agents, vertical
  tabs.
