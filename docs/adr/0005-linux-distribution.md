# ADR 0005: Linux distribution: Nix now, tarballs, Flatpak planned

Status: accepted, 2026-11-22

## Decision

- **Nix flake now.** It is the first-class Linux path (see `docs/PRODUCT.md`).
- **Prebuilt tarballs** from the release workflow, with `SHA256SUMS` and provenance.
- **Official CI targets:** Fedora, Debian, Arch and Ubuntu. `.github/workflows/distros.yml`
  builds and runs the headless tests in each distro's own container with its own
  packages. That proves the build and the PTY tests, not a desktop session.
- **Flatpak is planned, not in this release.** It follows the Nix package.

## Why Flatpak waits

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

- Fedora, Debian, Arch and Ubuntu users build from source or use the tarball until Flatpak
  exists. `docs/INSTALL.md` lists the exact package commands CI uses.
- NixOS real-hardware verification stays separate from distro CI.
