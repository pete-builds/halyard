# Testing

## NixOS VM test (Wayland and X11)

`nix/vm-test.nix` boots two NixOS VMs and checks, by OCR on a screenshot,
that a GPUI window renders: cage on Wayland, and IceWM on X11, both on
Mesa software rendering. A window that opens but draws nothing fails.

Run it on an x86_64 Linux host with KVM (on NixOS, `/dev/kvm` and the
`kvm` and `nixos-test` system features are there by default):

```sh
nix build -L .#vmTests.nixos-vm
```

The screenshots (`wayland.png`, `x11.png`) land in `./result`. A run takes
about a minute once the probe is built.

It is deliberately not part of `checks`, so `nix flake check` stays fast
and runs on machines without KVM. CI runs it in
`.github/workflows/nixos-vm.yml` and uploads the screenshots as the
`nixos-vm-screenshots` artifact.

Until the app crate exists, the window is a stand-in
(`nix/vm-test/probe`) that draws `HALYARD-VM-OK`.
