# Product definition

**Goal:** a terminal developers would choose even without AI, with an exceptional,
vendor-neutral AI development experience built on top.

## Priorities, in order

1. **A fully functional terminal on macOS and NixOS.** Ghostty's VT engine, GPUI rendering,
   real PTYs. Fast and reliable before anything else. AI stays optional and off by default.
2. **NixOS as a first-class platform, equal to macOS.** Wayland and X11, GPU acceleration,
   clipboard, font discovery, desktop entry and icon. A reproducible `flake.nix` where
   `nix develop`, `nix build` and `nix run` work, with every dependency pinned and no network
   access during the build. A Home Manager module for declarative themes, fonts,
   keybindings and settings.
3. **A modern, native-feeling UI.** Tabs, draggable split panes, customizable themes,
   keyboard shortcuts, persistent workspaces.
4. **Agent awareness.** Claude Code, Codex, OpenCode and Gemini CLI run unmodified. Halyard
   shows which agents are working, finished, or need attention, from observable signals.
5. **An open agent protocol.** A secure, documented session interface that lets agents and
   external tools talk to Halyard without vendor lock-in.

## Standing requirements

- **Performance:** fast startup, low memory, responsive input, smooth scrolling, efficient
  with many panes. Measured, never asserted.
- **Security and privacy:** local-first, no accounts, no telemetry, no automatic execution
  of AI-generated commands, explicit permission for sensitive operations.
- **Open source:** MIT OR Apache-2.0; no incompatible dependencies (`cargo deny` enforces).
- **Engineering quality:** automated tests, CI on macOS and Linux, accurate docs, real
  platform testing. Untested functionality is never marked complete.

## How we test platforms

| Platform | How it is verified |
|----------|-------------------|
| macOS arm64 | Real window on a Mac, driven and screenshotted; CI on `macos-15` |
| Linux (Ubuntu) | CI build and headless PTY tests on `ubuntu-24.04` (runner image) and in an `ubuntu:24.04` container |
| Linux (Fedora, Debian, Arch) | Official targets: CI build and headless PTY tests in each distro's own container (`.github/workflows/distros.yml`). No desktop session is run |
| NixOS | NixOS VM test in CI: boots NixOS, launches Halyard under Wayland and X11, screenshots as artifacts. Software rendering (Mesa) in the VM; real-GPU runs are listed separately in `ROADMAP.md` |
| macOS Intel | Unverified until a machine or runner is available |
