# Halyard dependency license analysis (as of 2026-11-22)

Target: ship Halyard as MIT OR Apache-2.0. Not legal advice. License fields come from the crates.io API unless noted. Transitive trees were NOT resolved (no install allowed), so the first action is `cargo deny check licenses` and `cargo about` on a real lockfile.

## Direct dependencies

| Dependency | Version seen | License | Source | Compatible with MIT OR Apache-2.0 |
|---|---|---|---|---|
| `libghostty-vt` (Rust crate) | 0.2.2, 2026-09-28 | MIT OR Apache-2.0 on crates.io; repo README says MIT | https://crates.io/api/v1/crates/libghostty-vt , https://github.com/uzaaft/libghostty-rs | Yes |
| `libghostty-vt-sys` | not fetched | MIT per repo; crates.io field unverified | https://github.com/uzaaft/libghostty-rs | Yes |
| Ghostty source built by the sys crate | pinned commit; lib.ghostty.org says 1.0.1 | MIT | https://github.com/ghostty-org/ghostty , https://lib.ghostty.org/vt | Yes |
| `gpui` | 0.2.2 (published 2025-10-22) | Apache-2.0 | https://crates.io/api/v1/crates/gpui | Yes |
| `portable-pty` | 0.9.0 (2025-02-11) | MIT | https://crates.io/api/v1/crates/portable-pty | Yes |
| `rusqlite` | 0.43.0 (2026-11-15) | MIT | https://crates.io/api/v1/crates/rusqlite | Yes |
| SQLite (via `libsqlite3-sys` bundled) | n/a | Public domain | sqlite.org (not fetched: unverified here) | Yes |
| `agent-client-protocol` | 5.5.1 (2026-11-21) | Apache-2.0 | https://crates.io/api/v1/crates/agent-client-protocol | Yes (one-way into MIT OR Apache-2.0, see note) |
| `rmcp` | 3.11.0 (2026-11-17) | Apache-2.0 | https://crates.io/api/v1/crates/rmcp | Yes (same note) |
| `alacritty_terminal` (fallback VT) | 0.26.0 (2026-04-06) | Apache-2.0 | https://crates.io/api/v1/crates/alacritty_terminal | Yes |

Note on Apache-2.0-only dependencies: a project licensed "MIT OR Apache-2.0" can depend on Apache-2.0 crates. Downstream users who want MIT-only terms still inherit Apache-2.0 for the combined binary. This is normal in the Rust ecosystem but should be stated in the README: the dual license covers Halyard's own code, and binaries carry the union of dependency licenses. Apache-2.0 is also compatible with GPLv3 but not GPLv2 (relevant only if someone wants to link Halyard into GPL-2.0-only code).

## libghostty-vt and the Ghostty source

- The Rust crate builds Ghostty from source with Zig: requires Zig 0.15.2 exactly for crate 0.2.2 (measured 2026-11-22: 0.17.1 fails, Ghostty's pinned commit sets minimum_zig_version 0.15.2; upstream master's README says 0.17.x because it pins a newer Ghostty), fetches Ghostty at a pinned commit unless `GHOSTTY_SOURCE_DIR` is set, links statically by default, offers `link-dynamic` and `pkg-config` features. https://github.com/uzaaft/libghostty-rs
- Licensing consequence of static link: Halyard binaries contain MIT-licensed Ghostty code. MIT requires the copyright notice and license text to ship with the binary (a THIRD_PARTY_LICENSES file or an About screen).
- Ghostty's top-level `build.zig.zon` lists many Zig dependencies (aro, libxev, vaxis, zigimg, z2d, uucode, harfbuzz, freetype, oniguruma, simdutf, highway, wuffs and others) and bundled fonts (JetBrains Mono, Nerd Fonts Symbols) and iTerm2 color themes. https://raw.githubusercontent.com/ghostty-org/ghostty/main/build.zig.zon
  - lib.ghostty.org says libghostty-vt "has no dependencies" beyond libc. https://lib.ghostty.org/vt So the VT build likely pulls only a small subset (for example uucode for Unicode tables; simdutf and highway are plausible). Which ones actually land in the static library: **unverified**. Check by reading `src/build/LibVtPackages.zig` at the pinned commit and `nm` on the produced archive.
  - Fonts and themes in that manifest belong to the full app, not the VT library. Do not assume they are in our binary; do not redistribute them unless chosen deliberately (JetBrains Mono is OFL, Nerd Fonts has mixed font licenses: unverified).
- Version-story caveat: libghostty-rs README says libghostty-vt is pre-1.0 and bindings track one pinned commit, while lib.ghostty.org says 1.0.1 with stable C ABI. Pin the commit and re-verify. https://github.com/uzaaft/libghostty-rs , https://lib.ghostty.org/vt
- Build-toolchain cost on NixOS: Zig 0.15.2 must be in the devshell and Zig's network fetch must be disabled for Nix sandbox builds; the crate documents `GHOSTTY_ZIG_SYSTEM_DIR` for network-free builds. https://github.com/uzaaft/libghostty-rs

## GPUI

- `gpui` 0.2.2 is Apache-2.0, but it was published 2025-10-22, over a year old. Zed's repo has moved on; at least one project consumes a `gpui-pre` snapshot crate at 0.3.10 (Sheprd), and gpui-ghostty pins a Zed git commit. https://crates.io/api/v1/crates/gpui , https://raw.githubusercontent.com/penso/herdr-gpui/main/README.md , https://github.com/Xuanwo/gpui-ghostty
  - Planning risk: "GPUI 0.2.x" is stale on crates.io. Decide between 0.2.2 and a git pin before architecture work.
  - License risk with a git pin: the Zed workspace is GPL-3.0-or-later with Apache-2.0 "where marked". Depend only on crates whose own Cargo.toml says Apache-2.0 (gpui, and its helper crates). Any Zed-internal crate pulled by gpui must be checked individually: unverified. https://github.com/zed-industries/zed
- Notable gpui direct dependencies seen: taffy, resvg/usvg, lyon_path, ttf-parser, icu_segmenter, async-task, objc2, windows, `sum_tree`, `scheduler`. https://raw.githubusercontent.com/zed-industries/zed/main/crates/gpui/Cargo.toml `sum_tree` and `scheduler` are Zed-internal; their licenses were not fetched: **unverified**. Most of the external ones are MIT or Apache-2.0 in my prior knowledge, but I did not verify them here.
- Not verified: the Linux text/graphics stack GPUI pulls (font-kit, blade or wgpu, wayland-client, ashpd) and the Vulkan requirement (Sheprd notes Vulkan rendering). https://raw.githubusercontent.com/penso/herdr-gpui/main/README.md
- Run `cargo deny` with an allow list of MIT, Apache-2.0, BSD-2/3-Clause, ISC, Unicode-3.0, Zlib, MPL-2.0 (file-level, acceptable) and treat GPL, AGPL, LGPL (static) as build failures.

## portable-pty, SQLite, TOML

- `portable-pty` 0.9.0, MIT, from the wezterm repo (WezTerm is MIT). Last release 2025-02-11, so maintenance is slow. https://crates.io/api/v1/crates/portable-pty , https://raw.githubusercontent.com/wezterm/wezterm/main/LICENSE.md
- `rusqlite` 0.43.0, MIT. Use the `bundled` feature to compile SQLite (public domain) in; that removes the system-lib variance on NixOS and macOS. https://crates.io/api/v1/crates/rusqlite
- TOML crates (`toml`, `serde`) were not checked: both are MIT OR Apache-2.0 in my knowledge, unverified here.

## ACP and MCP crates

- `agent-client-protocol` 5.5.1, Apache-2.0, repo agentclientprotocol/rust-sdk. https://crates.io/api/v1/crates/agent-client-protocol
- `rmcp` 3.11.0, Apache-2.0, repo modelcontextprotocol/rust-sdk. https://crates.io/api/v1/crates/rmcp
- Both pull large async trees (tokio, serde, schemars, possibly reqwest/hyper): run the same `cargo deny` pass. Not resolved here.
- Agent adapters (separate processes, not linked): `claude-code-acp` and `codex-acp` from Zed are Apache-2.0 per search results (https://zed.dev/blog/claude-code-via-acp , https://alternativeto.net/news/2025/10/zed-editor-integrates-openai-codex-via-acp-and-open-sources-adapter). The Claude Code CLI itself is proprietary Anthropic software; Halyard only launches it, never bundles it. Do not redistribute agent binaries.

## What we may reuse

| Source | License | Reuse by Halyard |
|---|---|---|
| Ghostty and libghostty-vt | MIT | Code, ideas, docs, with notice |
| Alacritty and `alacritty_terminal` | Apache-2.0 and MIT | Code with notices |
| WezTerm (`portable-pty`, `termwiz`) | MIT | Code with notice |
| Rio | MIT | Code with notice |
| Warp `warpui_core` and `warpui` crates only | MIT | Code with notice (verify per-file headers; unverified) |
| GPUI | Apache-2.0 | Depend on it; keep NOTICE if present |
| Wave Terminal | Apache-2.0 | Code with notice and state changes |
| Paneflow | MIT | Code with notice |
| tty7, gpui-ghostty, Sheprd | Apache-2.0 | Code with notice |
| Ghostex | MIT | Code with notice |

## Things we must NOT copy (study behavior only)

| Source | License | Why |
|---|---|---|
| Warp client (everything except `warpui_core`, `warpui`) | AGPL-3.0 | Network copyleft would force Halyard to AGPL. https://github.com/warpdotdev/warp |
| Zed `terminal_view` (and treat `terminal` as GPL until checked) | GPL-3.0-or-later | https://raw.githubusercontent.com/zed-industries/zed/main/crates/terminal_view/Cargo.toml |
| Any Zed crate not marked Apache-2.0 | GPL-3.0-or-later | https://github.com/zed-industries/zed |
| cmux | GPL-3.0-or-later; server parts BSL 1.1 | https://github.com/manaflow-ai/cmux |
| Kitty (including kittens) | GPL-3.0 | https://github.com/kovidgoyal/kitty |
| iTerm2 | GPL-3.0 (sidebar GPL-2.0; check LICENSE) | https://github.com/gnachman/iTerm2 |
| Codirigent | GPL-3.0 | https://feedbagel.com/post/codirigent-open-source-terminal-workspace-for-enhanced-ghostty-superset-and-tmux |
| herdr forks marked AGPL-3.0 (and possibly the main repo) | AGPL-3.0, conflicting | https://betterstack.com/community/guides/ai/herdr-ai-agent/ |
| Superset | ELv2 per its site (conflicting) | Not OSI open source. https://superset.sh/llms.txt |
| Conductor | Proprietary | https://www.conductor.build/ |

Study rules: implementing a published protocol (OSC 133, OSC 99, OSC 9, OSC 777, Kitty keyboard) from its spec is fine. Reading GPL/AGPL code, then writing similar code is a contamination risk; keep a short written behavior spec, and write from the spec, ideally by someone who has not copied from the source. Do not paste snippets, even small ones, from GPL/AGPL repos or from LLM output that reproduces them.

## Actions

1. Add `deny.toml` allowing only permissive licenses, run on every PR.
2. Generate THIRD_PARTY_LICENSES with `cargo about`, plus a manual entry for the Ghostty source and any Zig-built code.
3. Pin libghostty-rs by git SHA or exact version, and the Ghostty commit it fetches.
4. Decide GPUI source (crates.io 0.2.2 vs git pin) and audit each Zed-internal crate in the tree for its license.
5. Confirm lib-vt's actual Zig dependency set at the pinned commit.
6. Confirm whether `libghostty-vt-sys` and the Ghostty commit carry any NOTICE files.
7. Telemetry note: Ghostty's crash reporter stores reports locally and does not send them (https://github.com/ghostty-org/ghostty); that applies to the app, not libghostty-vt, which has no I/O.
