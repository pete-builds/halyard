Halyard (Linux, prebuilt binary)

This tarball holds the plain release binary, `halyard`, built on Ubuntu 24.04 (glibc 2.39).
It runs on distributions with a glibc at least that new. Nothing in it is installed
for you: copy `halyard` somewhere on your PATH, for example ~/.local/bin.

Recommended on NixOS and with Nix: use the flake instead of this tarball. It pins every
dependency, wires up the runtime libraries and installs the desktop entry and icon:
    nix run github:pete-builds/halyard

Runtime libraries (install with your package manager). Halyard's GPU toolkit links some of
these at build time and loads the rest at runtime, so a missing one shows up as a failure
to start or a blank window:
    libxkbcommon, libxkbcommon-x11, libwayland-client, libvulkan1 (plus a Vulkan driver:
    Mesa or your vendor's), libfontconfig1, libfreetype6, libxcb1, libx11-xcb1
On Ubuntu and Debian:
    sudo apt install libxkbcommon0 libxkbcommon-x11-0 libwayland-client0 libvulkan1 \
        mesa-vulkan-drivers libfontconfig1 libfreetype6 libxcb1 libx11-xcb1

Verify the download against SHA256SUMS from the same release page. Full instructions:
https://github.com/pete-builds/halyard/blob/main/docs/INSTALL.md
Licence: MIT OR Apache-2.0 (LICENSE-MIT, LICENSE-APACHE).
