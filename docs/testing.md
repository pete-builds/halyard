# Testing

## NixOS VM test (Wayland and X11)

`nix/vm-test.nix` boots two NixOS VMs and drives the real Halyard package
through QEMU's virtual keyboard: cage on Wayland, and IceWM on X11, both on
Mesa software rendering. In each session it:

- types a command and checks the shell wrote the expected file,
- checks by OCR on a screenshot that the output was drawn,
- edits and saves a file in vim,
- interrupts `sleep` with Ctrl+C and checks the exit status is 130,
- pastes from the session clipboard (wl-copy on Wayland, xclip on X11) with
  Ctrl+Shift+V.

Every keyboard step is proven by a file the shell inside Halyard writes, so a
key that never reaches the PTY fails the test. OCR only checks that the
window draws.

Run it on an x86_64 Linux host with KVM (on NixOS, `/dev/kvm` and the
`kvm` and `nixos-test` system features are there by default):

```sh
nix build -L .#vmTests.nixos-vm
```

The screenshots land in `./result`. A run takes about a minute once
Halyard is built.

It is deliberately not part of `checks`, so `nix flake check` stays fast
and runs on machines without KVM. CI runs it in
`.github/workflows/nixos-vm.yml` and uploads the screenshots as the
`nixos-vm-screenshots` artifact.

Not covered yet: mouse selection and copying out of Halyard, and wheel
scrollback. Both need a pointer device in the VM.
