# ADR 0005: Linux distribution: Nix, tarballs and Flatpak

Status: accepted, 2026-11-22. Amended the same day when the Flatpak package landed (PR #21).

## Decision

- **Nix flake now.** It is the first-class Linux path (see `docs/PRODUCT.md`).
- **Prebuilt tarballs** from the release workflow, with `SHA256SUMS` and provenance.
- **Official CI targets:** Fedora, Debian, Arch and Ubuntu. `.github/workflows/distros.yml`
  builds and runs the headless tests in each distro's own container with its own
  packages. That proves the build and the PTY tests, not a desktop session.
- **Flatpak exists, alongside the Nix package** (PR #21, `packaging/flatpak/`,
  `docs/flatpak.md`). It is built in CI and shipped as a workflow artifact; it is not on
  Flathub. Shells run on the host through `flatpak-spawn --host`: the sandbox does not
  confine them, by design. Flatpak is used to distribute Halyard to desktops without Nix,
  not to confine it.

## How each Flatpak concern below was settled

The concerns that held Flatpak back, and how the package answers each:

- **Offline build:** `cargo-sources.json` (from `flatpak-cargo-generator`) and
  `ghostty-sources.json` (Ghostty at the commit `libghostty-vt-sys` pins, plus every Zig
  package from Ghostty's `build.zig.zon.json` at that commit). CI checks both against
  `Cargo.lock` and the pin entry by entry (`check-sources.py`), and `test_check_sources.py`
  proves hand edits are caught.
- **Zig 0.15.2:** a build-only module from the official tarballs, pinned by sha256.
- **GPU access:** `--device=dri`; the GL extensions come from the runtime as usual.
- **Host shells:** decided: `--talk-name=org.freedesktop.Flatpak` and
  `flatpak-spawn --host --watch-bus`. Pane variables reach the host through an owner-only
  file and `--env-fd`, never on a command line. The security meaning is written down in
  `docs/flatpak.md` and `docs/THREAT_MODEL.md`.
- **Flathub policy:** still open. Nothing is submitted.

How it is verified: CI builds the bundle offline for x86_64 and aarch64, installs it,
compares its permissions with the documented set exactly (no network), and checks the
binary's libraries inside the runtime. No window has been opened from the Flatpak and no
host shell has been started through it yet; that needs a real Linux desktop.

## Why Flatpak waited (the original reasoning)

- **Offline build.** Flathub builds have no network access
  (https://docs.flathub.org/docs/for-app-authors/requirements). Cargo sources must be
  vendored into the manifest with `flatpak-cargo-generator`, and Ghostty's pinned Zig
  package cache must be supplied the same way.
- **Zig 0.15.2.** `libghostty-vt-sys` needs exactly this Zig. It has to be built or
  bundled inside the Flatpak build, because the Freedesktop SDK does not provide it.
- **GPU access.** Rendering needs the GL and Vulkan runtime extensions plus `--device=dri`.
- **Host shells.** A terminal exists to run the user's shell and tools. Inside the sandbox
  it would see only the runtime. Running host shells needs
  `--talk-name=org.freedesktop.Flatpak` (then `flatpak-spawn --host`), which is close to
  full escape from the sandbox. That is the key UX and security tradeoff, and we would
  rather decide it deliberately than ship it by default.
- **Flathub policy.** Flathub says terminals are generally not well suited to Flatpak and
  accepts them mainly when upstream submits and commits to supporting them, and it keeps
  static permissions to a minimum.

## Consequences

- Fedora, Debian, Arch and Ubuntu users can install the Flatpak from a CI artifact, build
  from source, or use the tarball. `docs/INSTALL.md` lists the exact package commands CI
  uses.
- NixOS real-hardware verification stays separate from distro CI.
