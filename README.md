# Halyard

A fast, open-source terminal for macOS and Linux, built to host coding agents without
depending on any of them.

> **Status: early development.** See [ROADMAP.md](ROADMAP.md) for what works today and what
> does not. Nothing here is claimed until it is tested.

## Why another terminal

Plenty of terminals now show which AI agent is busy. Halyard aims at the gaps that are
still open:

- **Terminal first.** Correct emulation (Ghostty's VT engine), GPU rendering, real PTYs.
  Everything AI-related can be switched off and the terminal stays complete.
- **Permissive all the way down.** MIT OR Apache-2.0 for the whole app. No AGPL or GPL code.
- **Linux and NixOS as an equal platform**, with a Nix flake, not an afterthought port.
- **One attention model for every agent.** Claude Code, Codex CLI, OpenCode and Gemini CLI
  run as ordinary terminal programs; Halyard tracks working, waiting and finished from
  signals it can observe, and says "unknown" instead of guessing.
- **An open session protocol.** Each pane exports `HALYARD_SESSION_ID`, so any agent hook
  can report back to the right pane through a documented interface.
- **Local-first.** No account, no telemetry, no network unless you configure a model.

Prior art and how Halyard differs is documented honestly in
[docs/PRIOR_ART.md](docs/PRIOR_ART.md).

## Building

Requires Rust (installed by `rust-toolchain.toml`) and **Zig 0.15.2 exactly** (Ghostty's VT
library is built from source).

```sh
# macOS
brew install zig@0.15
export PATH="$(brew --prefix zig@0.15)/bin:$PATH"
cargo run --release -p halyard-app

# NixOS / Nix
nix develop
cargo run --release -p halyard-app
```

See [AGENTS.md](AGENTS.md) for the full contributor guide and
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the design.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
