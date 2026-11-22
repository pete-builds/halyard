# Vendored and patched crates

Each crate here is an unmodified crates.io release plus the smallest patch that makes it
build, wired in through `[patch.crates-io]` in the root `Cargo.toml`. Every patch is listed
below with the reason and the condition for removing it.

## xattr 0.2.3 (MIT OR Apache-2.0)

- **Path in the graph:** `gpui 0.2.2` > `gpui_http_client 0.2.2` > `zed-async-tar 0.5.0-zed`
  > `xattr 0.2.3`. The dependency is not optional in gpui 0.2.2.
- **Problem:** on Linux, `xattr 0.2.3` reads `libc::ENOATTR`, which current `libc` releases
  (0.2.191) no longer export for Linux targets. GPUI's other dependencies (`rustix` via
  `wayland-backend`) require that newer `libc`, so pinning `libc` down is not possible. The
  build fails with `cannot find value ENOATTR in crate ::libc`. macOS is unaffected.
  Found on NixOS 26.05 (x86_64) on 2026-11-22 and reproduced from macOS with
  `cargo check --target x86_64-unknown-linux-gnu`.
- **Patch:** `src/sys/mod.rs` uses `libc::ENODATA` on Linux and Android. glibc defines
  `ENOATTR` as `ENODATA` in `<sys/xattr.h>`, so the value is identical. Other platforms
  still use `libc::ENOATTR`.
- **Second patch (macOS):** `src/sys/linux_macos/macos.rs` uses `u32` instead of the
  deprecated alias `libc::uint32_t` (identical type). Path dependencies do not get
  `--cap-lints`, so with CI's `RUSTFLAGS=-D warnings` the deprecation fails the macOS build
  as soon as gpui is in the graph (the app crate). Found 2026-11-22 on `feat/app-window`.
- **Check:** CI runs `cargo check --manifest-path vendor/xattr/Cargo.toml` on Ubuntu,
  which resolves the newest `libc` and fails if the patch is lost.
- **Remove when:** gpui moves to `xattr` 1.x, or no longer depends on `zed-async-tar`.
