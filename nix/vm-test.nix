# NixOS VM test: a GPUI window must render inside a VM, under Wayland (cage)
# and under X11 (IceWM), with software Vulkan (Mesa lavapipe). Checked by OCR
# on a screenshot, so a window that opens but draws nothing fails.
#
# Until the app crate lands, the window is a stand-in (vm-test/probe) that
# draws a fixed marker string. Kept out of `checks` so `nix flake check`
# stays fast and needs no KVM. Run on an x86_64 Linux host with KVM:
#   nix build -L .#vmTests.nixos-vm
# Screenshots land in ./result. See docs/testing.md.
{ pkgs }:
let
  inherit (pkgs) lib;

  marker = "HALYARD-VM-OK";

  # Libraries GPUI dlopens at runtime (same list as nix/default.nix).
  runtimeLibs = with pkgs; [
    vulkan-loader
    wayland
    libxkbcommon
    fontconfig
    freetype
    (pkgs.libx11 or pkgs.xorg.libX11)
    (pkgs.libxcb or pkgs.xorg.libxcb)
    (pkgs.libxcursor or pkgs.xorg.libXcursor)
    (pkgs.libxi or pkgs.xorg.libXi)
    (pkgs.libxrandr or pkgs.xorg.libXrandr)
  ];

  probe = pkgs.rustPlatform.buildRustPackage {
    pname = "halyard-vm-probe";
    version = "0.0.0";
    src = lib.fileset.toSource {
      root = ../.;
      fileset = lib.fileset.unions [
        ./vm-test/probe/Cargo.toml
        ./vm-test/probe/Cargo.lock
        ./vm-test/probe/src
        ../vendor
      ];
    };
    cargoRoot = "nix/vm-test/probe";
    buildAndTestSubdir = "nix/vm-test/probe";
    cargoLock.lockFile = ./vm-test/probe/Cargo.lock;
    nativeBuildInputs = [
      pkgs.pkg-config
      pkgs.patchelf
    ];
    buildInputs = runtimeLibs;
    doCheck = false;
    postFixup = ''
      patchelf --add-rpath "${lib.makeLibraryPath runtimeLibs}" "$out/bin/halyard-vm-probe"
    '';
    meta.mainProgram = "halyard-vm-probe";
  };

  probeExe = lib.getExe probe;

  common = {
    # Mesa, which carries lavapipe (software Vulkan) for a VM with no GPU.
    hardware.graphics.enable = true;
    environment.systemPackages = [
      probe
      pkgs.vulkan-tools
    ];
    fonts.enableDefaultPackages = true;
    virtualisation.memorySize = 2048;
    virtualisation.cores = 2;
  };
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
        services.cage = {
          enable = true;
          user = "alice";
          program = "${pkgs.writeShellScript "probe-logged" ''
            exec ${probeExe} 2>/tmp/probe.log
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
      };
  };

  testScript = ''
    from datetime import timedelta

    start_all()

    with subtest("Wayland: GPUI window renders under cage"):
        wayland.wait_for_unit("graphical.target")
        wayland.wait_until_succeeds("pgrep -f halyard-vm-probe", timeout=timedelta(seconds=120))
        try:
            wayland.wait_for_text("${marker}", timeout=timedelta(seconds=180))
        finally:
            wayland.screenshot("wayland")
            print(wayland.execute("cat /tmp/probe.log")[1])

    with subtest("X11: GPUI window renders under IceWM"):
        x11.wait_for_x()
        x11.wait_for_file("/home/alice/.Xauthority")
        x11.succeed("su - alice -c 'DISPLAY=:0 ${probeExe} >/tmp/probe.log 2>&1 &'")
        x11.wait_until_succeeds("pgrep -f halyard-vm-probe", timeout=timedelta(seconds=120))
        try:
            x11.wait_for_text("${marker}", timeout=timedelta(seconds=180))
        finally:
            x11.screenshot("x11")
            print(x11.execute("cat /tmp/probe.log")[1])
  '';
}
