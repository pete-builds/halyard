# NixOS VM test: the real Halyard app, driven through QEMU's virtual keyboard
# under Wayland (cage) and X11 (IceWM), on Mesa software rendering.
#
# Every step is proven two ways: the shell inside Halyard writes a file the
# test reads back (so keystrokes really reached the PTY), and OCR on a
# screenshot shows the result was drawn (so the window renders).
#
# Kept out of `checks` so `nix flake check` stays fast and needs no KVM.
# Run on an x86_64 Linux host with KVM:
#   nix build -L .#vmTests.nixos-vm
# Screenshots land in ./result. See docs/testing.md.
{ pkgs, halyard }:
let
  inherit (pkgs) lib;
  exe = lib.getExe halyard;

  common = {
    # Mesa, which carries lavapipe (software Vulkan) for a VM with no GPU.
    hardware.graphics.enable = true;
    environment.systemPackages = [
      halyard
      pkgs.vim
    ];
    fonts.enableDefaultPackages = true;
    virtualisation.memorySize = 2048;
    virtualisation.cores = 2;
  };

  # One shared script for both sessions. `m` is the machine, `tag` names the
  # files and screenshots, `copy_cmd` puts text on that session's clipboard and
  # `read_cmd` succeeds once the clipboard serves exactly that text.
  steps = ''
    def drive(m, tag, copy_cmd, read_cmd):
        home = "/home/alice"

        def run(line):
            m.send_chars(line + "\n")

        with subtest(f"{tag}: typing reaches the shell and is drawn"):
            run(f"echo halyard-ok-{tag} > {home}/typed")
            m.wait_until_succeeds(f"grep -qx halyard-ok-{tag} {home}/typed", timeout=timedelta(seconds=60))
            # The command line never contains the marker, so only drawn output can match.
            # Tesseract sometimes reads a hyphen as " - ", so allow spaces around them.
            run("printf 'HALYARD-%s-CHECK\\n' RENDER")
            m.wait_for_text(r"HALYARD\s*-\s*RENDER\s*-\s*CHECK", timeout=timedelta(seconds=120))
            m.screenshot(f"{tag}-1-typed")

        with subtest(f"{tag}: vim edits and saves a file"):
            run(f"vim {home}/fromvim.txt")
            # vim creates its swap file once the buffer is open. OCR on the file name
            # would match the command line that is already on screen.
            m.wait_until_succeeds(f"test -e {home}/.fromvim.txt.swp", timeout=timedelta(seconds=60))
            m.screenshot(f"{tag}-2-vim")
            m.send_chars("ihello-from-vim")
            m.send_key("esc")
            m.send_chars(":wq\n")
            m.wait_until_succeeds(f"grep -qx hello-from-vim {home}/fromvim.txt", timeout=timedelta(seconds=60))

        with subtest(f"{tag}: Ctrl+C interrupts the foreground job"):
            run(f"touch {home}/started; sleep 60; touch {home}/not-interrupted")
            # Send Ctrl+C only once sleep is the foreground job, so an early Ctrl+C
            # cannot pass by cancelling a line that never ran.
            m.wait_until_succeeds(f"test -e {home}/started", timeout=timedelta(seconds=60))
            m.wait_until_succeeds("pgrep -fx 'sleep 60'", timeout=timedelta(seconds=60))
            m.send_key("ctrl-c")
            run(f"echo $? > {home}/rc")
            m.wait_until_succeeds(f"grep -qx 130 {home}/rc", timeout=timedelta(seconds=60))
            m.fail(f"test -e {home}/not-interrupted")

        with subtest(f"{tag}: paste from the session clipboard with Ctrl+Shift+V"):
            text = f"echo pasted-ok-{tag} > {home}/pasted"
            m.succeed(copy_cmd(text))
            # Paste only once the clipboard owner is serving the text.
            m.wait_until_succeeds(read_cmd(text), timeout=timedelta(seconds=30))
            m.send_key("ctrl-shift-v")
            m.send_key("ret")
            m.wait_until_succeeds(f"grep -qx pasted-ok-{tag} {home}/pasted", timeout=timedelta(seconds=60))
            m.screenshot(f"{tag}-3-after-paste")
  '';
in
pkgs.testers.runNixOSTest {
  name = "halyard-vm";
  enableOCR = true;

  nodes = {
    wayland =
      { ... }:
      {
        imports = [ common ];
        users.users.alice = {
          isNormalUser = true;
          uid = 1000;
        };
        environment.systemPackages = [ pkgs.wl-clipboard ];
        services.cage = {
          enable = true;
          user = "alice";
          program = "${pkgs.writeShellScript "halyard-logged" ''
            exec ${exe} 2>/tmp/halyard.log
          ''}";
        };
        # cage needs a KMS device; the default -vga std has none.
        virtualisation.qemu.options = [ "-vga none -device virtio-gpu-pci" ];
      };

    x11 =
      { ... }:
      {
        imports = [
          common
          "${pkgs.path}/nixos/tests/common/x11.nix"
          "${pkgs.path}/nixos/tests/common/user-account.nix"
        ];
        test-support.displayManager.auto.user = "alice";
        environment.systemPackages = [
          pkgs.xclip
          pkgs.xdotool
        ];
      };
  };

  testScript = ''
    from datetime import timedelta
    import shlex

    start_all()
    ${steps}

    def as_alice(env, cmd):
        return f"su - alice -c {shlex.quote(env + ' ' + cmd)}"

    WL = "XDG_RUNTIME_DIR=/run/user/1000 WAYLAND_DISPLAY=wayland-0"

    try:
        wayland.wait_for_unit("graphical.target")
        wayland.wait_until_succeeds("pgrep -x halyard", timeout=timedelta(seconds=120))
        wayland.wait_for_text("alice", timeout=timedelta(seconds=180))
        drive(
            wayland,
            "wayland",
            lambda text: as_alice(WL, f"wl-copy {shlex.quote(text)}"),
            lambda text: as_alice(WL, f"wl-paste --no-newline | grep -qxF {shlex.quote(text)}"),
        )
    finally:
        wayland.screenshot("wayland-final")
        print(wayland.execute("cat /tmp/halyard.log")[1])

    try:
        x11.wait_for_x()
        x11.wait_for_file("/home/alice/.Xauthority")
        x11.succeed(as_alice("DISPLAY=:0", "${exe} >/tmp/halyard.log 2>&1 &"))
        x11.wait_until_succeeds("pgrep -x halyard", timeout=timedelta(seconds=120))
        x11.wait_until_succeeds(
            as_alice("DISPLAY=:0", "xdotool search --name alice windowactivate --sync"),
            timeout=timedelta(seconds=120),
        )
        x11.wait_for_text("alice", timeout=timedelta(seconds=180))
        drive(
            x11,
            "x11",
            # No -loops: the readiness check reads the selection too, and xclip
            # keeps serving until the VM stops.
            lambda text: as_alice(
                "DISPLAY=:0",
                f"printf %s {shlex.quote(text)} | xclip -selection clipboard >/dev/null 2>&1 &",
            ),
            lambda text: as_alice(
                "DISPLAY=:0",
                f"xclip -o -selection clipboard | grep -qxF {shlex.quote(text)}",
            ),
        )
    finally:
        x11.screenshot("x11-final")
        print(x11.execute("cat /tmp/halyard.log")[1])
  '';
}
