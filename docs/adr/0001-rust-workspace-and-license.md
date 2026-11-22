# ADR 0001: Rust workspace, MIT OR Apache-2.0

Status: accepted, 2026-11-22

## Decision

Halyard is a Rust Cargo workspace licensed MIT OR Apache-2.0, the Rust ecosystem default.

## Why

- The main candidate dependencies are permissive: `libghostty-vt` (MIT OR Apache-2.0, and
  Ghostty itself is MIT), GPUI (Apache-2.0), `portable-pty` (MIT).
- A permissive license lets the terminal be embedded or redistributed, and keeps the door
  open for contributors from any company.

## Consequences

- We cannot copy code from AGPL or GPL terminals (Warp's app, Zed's `terminal_view`).
  `cargo deny` in CI refuses copyleft licenses in the dependency graph.
