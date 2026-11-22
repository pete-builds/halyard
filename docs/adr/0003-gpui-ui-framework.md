# ADR 0003: GPUI for the desktop UI

Status: accepted, 2026-11-22

## Decision

Build the application on GPUI 0.2.x from crates.io (Apache-2.0), with the
`runtime_shaders` feature on macOS.

## Evidence

- GPUI powers Zed, a daily-driver editor on macOS and Linux, and Zed ships a terminal on it,
  so text-grid rendering at terminal speeds is proven on this framework.
- It supports macOS (Metal), Linux Wayland and X11 (Vulkan through blade), with features
  `wayland` and `x11` on by default.
- Spike on macOS arm64 with only Command Line Tools installed: `gpui = { version = "0.2.2",
  features = ["runtime_shaders"] }` built in about 30 seconds on 12 cores. Without
  `runtime_shaders`, the build needs the full Xcode `metal` compiler.

## Alternatives considered

- winit + wgpu + glyphon: full control, but tabs, splits, palette, settings and text input
  would all be ours to write. Too much UI plumbing before the terminal is good.
- Tauri or Electron with xterm.js: rejected by the brief and by the performance goal.

## Consequences

- GPUI's API changes between releases. We pin an exact version and keep GPUI types out of
  `halyard-term`.
- Zed's `terminal_view` is GPL. We study GPUI's public examples, not Zed's terminal code.
