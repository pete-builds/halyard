# Testing

## Window scripts (macOS)

`scripts/ui-check.sh` builds the app with the `debug-script` feature and runs
each script in `crates/halyard-app/ui-tests/` against a real window, with its
own config and state directory. Scripts drive the window through AppKit
(clicks, right clicks, keys) and check the `dump` text with `expect` and
`reject`, so a key that reaches the wrong place fails the run. They cover what
unit tests cannot see: a click on a tab keeping the keyboard with the shell, a
tab name typed in place (including while a pane in another tab exits, and
saved by a click elsewhere), and right clicks going to the menu or to a
program with mouse reporting. The command list is in `AGENTS.md`.

```sh
scripts/ui-check.sh                      # build and run them all
scripts/ui-check.sh crates/halyard-app/ui-tests/rename-in-place.txt
```

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

A failing `nix build` keeps no screenshots. To see what the screen showed
when a step failed, run the driver directly; it writes into the directory
you name, pass or fail:

```sh
nix build .#vmTests.nixos-vm.driver
./result/bin/nixos-test-driver --output_directory shots
```

It is deliberately not part of `checks`, so `nix flake check` stays fast
and runs on machines without KVM. CI runs the driver this way in
`.github/workflows/nixos-vm.yml` and uploads the screenshots as the
`nixos-vm-screenshots` artifact, on failure too.

Not covered yet: mouse selection and copying out of Halyard, and wheel
scrollback. Both need a pointer device in the VM.
