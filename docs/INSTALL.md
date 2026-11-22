# Install

Halyard is early software. Read [ROADMAP.md](../ROADMAP.md) for what works. Releases are
created as drafts and published by a person; until the first one exists, build from source.

Every release has `SHA256SUMS` and build provenance. Verify before you run:

```sh
shasum -a 256 -c SHA256SUMS --ignore-missing      # macOS
sha256sum -c SHA256SUMS --ignore-missing          # Linux
gh attestation verify <downloaded-file> -R pete-builds/halyard
```

The provenance check proves the file came from this repository's release workflow. It does
not prove the code is free of bugs. Read [docs/THREAT_MODEL.md](THREAT_MODEL.md).

## macOS

Requires macOS 13 or newer. The app is universal (Apple silicon and Intel). Only the
Apple silicon half has been run on a real Mac so far; the Intel half is built and
linked but untested.

### The release zip

1. Download `Halyard-<version>-macos-universal.zip` from the release page and verify it.
2. Unzip it and drag `Halyard.app` into `/Applications`.
3. **Halyard is not notarized.** It is ad-hoc signed, which keeps the bundle consistent
   but is not a Developer ID signature, so Gatekeeper blocks a downloaded copy on first
   launch. Either:
   - right-click (or Control-click) `Halyard.app`, choose **Open**, then **Open** in the
     dialog. On macOS 15 you may have to try a normal open first, then go to
     System Settings, Privacy and Security, and choose **Open Anyway**; or
   - remove the quarantine flag yourself:
     ```sh
     xattr -dr com.apple.quarantine /Applications/Halyard.app
     ```
   Only do this for a copy whose checksum you verified. Notarization is open work.

### Build the app yourself

Needs Rust (installed by `rust-toolchain.toml` through rustup), **Zig 0.15.2 exactly**, and
Command Line Tools (full Xcode is not required).

```sh
brew install zig@0.15 rustup
export PATH="$(brew --prefix zig@0.15)/bin:$PATH"
rustup target add aarch64-apple-darwin x86_64-apple-darwin
packaging/macos/build-app.sh          # writes target/macos/Halyard.app and a zip
open target/macos/Halyard.app
```

A copy you build on your own Mac carries no quarantine flag, so Gatekeeper does not
stop it. `build-app.sh --help` lists its options. The icon is built from
`packaging/icon/halyard-1024.png` with `sips` and `iconutil`, which ship with macOS.

For a quick run without a bundle: `cargo run --release -p halyard-app`.

## NixOS and Nix

Use the flake. It pins every dependency, builds offline, wires up the runtime libraries
and installs the desktop entry and icon. The flake, the NixOS module, the Home Manager
module and the nix-darwin setup are documented in [docs/nix.md](nix.md).

```sh
nix run github:pete-builds/halyard
```

## Other Linux

The release has tarballs for x86_64 and aarch64, built on Ubuntu 24.04, so they need a
glibc at least as new as 2.39. Unpack, then copy `halyard` somewhere on your `PATH`:

```sh
tar xzf halyard-<version>-linux-x86_64.tar.gz
install -m 755 halyard-<version>-linux-x86_64/halyard ~/.local/bin/
```

The tarball has no desktop entry or icon. Runtime libraries (the GPU toolkit loads some
at run time, so a missing one shows as a start-up failure or a blank window):
libxkbcommon, libxkbcommon-x11, libwayland-client, libvulkan with a Vulkan driver (Mesa or
your vendor's), fontconfig, freetype, libxcb, libx11-xcb, libx11, libxcursor, libxi, libxrandr. On Ubuntu and Debian:

```sh
sudo apt install libxkbcommon0 libxkbcommon-x11-0 libwayland-client0 libvulkan1 \
    mesa-vulkan-drivers libfontconfig1 libfreetype6 libxcb1 libx11-xcb1 libx11-6 libxcursor1 libxi6 libxrandr2
```

The Linux tarballs are built by the release workflow and have not been run on a Linux
machine by their author yet.

## Build from source on Linux

Install the development packages for the libraries above (`-dev` on Debian and Ubuntu:
`libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libvulkan-dev libx11-xcb-dev
libxcb1-dev libfontconfig-dev libfreetype-dev`), Rust through rustup, and Zig 0.15.2
(from ziglang.org, or `nix develop`, which provides everything):

```sh
cargo build --release --locked -p halyard-app
./target/release/halyard
```

## Not available

No Homebrew formula or cask, no `.deb` or `.rpm`, no Windows build, and no auto-update.
Halyard makes no network calls.
