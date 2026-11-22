# Everything the flake builds for one system: the package, the cargo checks,
# and the dev shell. flake.nix calls this once per system.
{
  pkgs,
  crane,
  ghosttySrc,
}:
let
  inherit (pkgs) lib stdenv;

  # nixpkgs' rustc and cargo, not rust-toolchain.toml. rustup users get the
  # pinned 1.100.0; Nix users get whatever nixpkgs ships, which must stay at or
  # above `rust-version` in Cargo.toml (1.90). CI builds both ways.
  craneLib = crane.mkLib pkgs;

  workspace = builtins.fromTOML (builtins.readFile ../Cargo.toml);
  version = workspace.workspace.package.version;

  # Only what cargo needs, so editing docs or nix files does not rebuild Rust.
  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../rustfmt.toml
      ../crates
      # Crates patched in through [patch.crates-io] (see vendor/README.md).
      (lib.fileset.maybeMissing ../vendor)
    ];
  };

  # The app crate lands separately. Until it exists the package builds the
  # workspace libraries only and there is no `halyard` binary to install.
  hasApp = builtins.pathExists ../crates/halyard-app;

  zigPackages = import ./ghostty-zig-deps.nix { inherit pkgs ghosttySrc; };

  # Libraries GPUI loads with dlopen at runtime on Linux, plus the ones it
  # links. They go on the binary's RUNPATH (not LD_LIBRARY_PATH, which would
  # leak into every shell Halyard starts).
  linuxRuntimeLibs = with pkgs; [
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

  # Vendored from the real source's Cargo.lock. Passed explicitly so crane
  # never reads it out of the dummy source, which would be import from
  # derivation and breaks evaluation without building
  # (`nix flake check --no-build`).
  cargoVendorDir = craneLib.vendorCargoDeps { inherit src; };

  commonArgs = {
    pname = "halyard";
    inherit version src cargoVendorDir;
    strictDeps = true;
    cargoExtraArgs = "--locked";

    nativeBuildInputs = [
      pkgs.zig_0_15
      pkgs.pkg-config
      # Ghostty's build.zig asks git for a version string and copes with a
      # source tree that is not a repository; git just has to exist.
      pkgs.git
      pkgs.rustPlatform.bindgenHook
    ]
    ++ lib.optionals stdenv.hostPlatform.isDarwin [
      # Same tools Ghostty's own nix/libghostty-vt.nix uses on macOS: Zig
      # finds the SDK through xcrun, which xcbuild provides from SDKROOT.
      pkgs.cctools
      pkgs.xcbuild
    ];

    buildInputs = lib.optionals stdenv.hostPlatform.isLinux linuxRuntimeLibs;

    # libghostty-vt-sys: use the pinned source instead of `git clone`, and the
    # prefetched Zig packages instead of downloading them.
    GHOSTTY_SOURCE_DIR = "${ghosttySrc}";
    GHOSTTY_ZIG_SYSTEM_DIR = "${zigPackages}";

    # The Zig setup hook would otherwise replace the cargo phases.
    dontUseZigConfigure = true;
    dontUseZigBuild = true;
    dontUseZigCheck = true;
    dontUseZigInstall = true;

    # HOME is not writable in the sandbox; give Zig its own cache directories.
    preConfigure = ''
      export ZIG_GLOBAL_CACHE_DIR="$TMPDIR/zig-global-cache"
      export ZIG_LOCAL_CACHE_DIR="$TMPDIR/zig-local-cache"
      export XDG_CACHE_HOME="$TMPDIR/xdg-cache"
      mkdir -p "$ZIG_GLOBAL_CACHE_DIR" "$ZIG_LOCAL_CACHE_DIR" "$XDG_CACHE_HOME"
    '';
  };

  # Every dependency, Ghostty included, built once and shared by the package
  # and the checks.
  cargoArtifacts = craneLib.buildDepsOnly (
    builtins.removeAttrs commonArgs [ "src" ]
    // {
      # crane replaces every path crate with an empty stub for the deps-only
      # build, and that includes the [patch.crates-io] crates under vendor/.
      # They are real dependencies (gpui needs xattr's functions), so put the
      # real sources back.
      dummySrc = craneLib.mkDummySrc {
        inherit src;
        extraDummyScript = lib.optionalString (builtins.pathExists ../vendor) ''
          rm -rf "$out/vendor"
          cp -r --no-preserve=mode ${src}/vendor "$out/vendor"
        '';
      };
    }
  );

  desktopFile = ../dist/linux/halyard.desktop;
  iconSvg = ../dist/icons/halyard.svg;
  iconSizes = [
    16
    32
    48
    64
    128
    256
    512
  ];

  halyard = craneLib.buildPackage (
    commonArgs
    // {
      inherit cargoArtifacts;
      cargoExtraArgs = "--locked " + (if hasApp then "-p halyard-app" else "--workspace");
      # Tests run in checks.cargo-test, not in every package build.
      doCheck = false;
      # With no binary to install, crane's reference-stripping hook has no files
      # to edit and its sed exits early, printing "echo: write error: Broken pipe".
      doNotRemoveReferencesToVendorDir = !hasApp;

      nativeBuildInputs =
        commonArgs.nativeBuildInputs
        ++ lib.optionals (hasApp && stdenv.hostPlatform.isLinux) [
          pkgs.librsvg
          pkgs.desktop-file-utils
          pkgs.patchelf
        ];

      postInstall = ''
        mkdir -p "$out"
      ''
      + lib.optionalString (hasApp && stdenv.hostPlatform.isLinux) ''
        install -Dm644 ${desktopFile} "$out/share/applications/halyard.desktop"
        desktop-file-validate "$out/share/applications/halyard.desktop"
        install -Dm644 ${iconSvg} "$out/share/icons/hicolor/scalable/apps/halyard.svg"
        ${lib.concatMapStringsSep "\n" (size: ''
          mkdir -p "$out/share/icons/hicolor/${toString size}x${toString size}/apps"
          rsvg-convert -w ${toString size} -h ${toString size} ${iconSvg} \
            -o "$out/share/icons/hicolor/${toString size}x${toString size}/apps/halyard.png"
        '') iconSizes}
      '';

      # After the fixup phase has shrunk the RUNPATH to what the linker saw,
      # add the libraries GPUI dlopens.
      postFixup = lib.optionalString (hasApp && stdenv.hostPlatform.isLinux) ''
        patchelf --add-rpath "${lib.makeLibraryPath linuxRuntimeLibs}" "$out/bin/halyard"
      '';

      passthru = {
        inherit cargoArtifacts zigPackages linuxRuntimeLibs;
      };

      meta = {
        description = "A fast, open-source terminal for macOS and Linux";
        homepage = "https://github.com/pete-builds/halyard";
        license = with lib.licenses; [
          mit
          asl20
        ];
        platforms = lib.platforms.linux ++ lib.platforms.darwin;
      }
      // lib.optionalAttrs hasApp { mainProgram = "halyard"; };
    }
  );

  # halyard-config's `check_files` example: the real parser, used by the Home
  # Manager module check to validate the files the module writes.
  configChecker = craneLib.buildPackage (
    commonArgs
    // {
      inherit cargoArtifacts;
      pname = "halyard-config-check";
      cargoExtraArgs = "--locked -p halyard-config --example check_files";
      doCheck = false;
      doNotRemoveReferencesToVendorDir = true;
    }
  );

  # Programs the PTY integration tests drive when they are present.
  ptyTestPrograms = [
    pkgs.vim
    pkgs.less
    pkgs.htop
  ];

  checks = {
    inherit halyard;

    cargo-clippy = craneLib.cargoClippy (
      commonArgs
      // {
        inherit cargoArtifacts;
        cargoClippyExtraArgs = "--workspace --all-targets -- --deny warnings";
      }
    );

    cargo-test = craneLib.cargoTest (
      commonArgs
      // {
        inherit cargoArtifacts;
        cargoTestExtraArgs = "--workspace";
        nativeBuildInputs = commonArgs.nativeBuildInputs ++ ptyTestPrograms;
      }
    );

    cargo-fmt = craneLib.cargoFmt {
      pname = "halyard";
      inherit version src;
    };

    package-contents = import ./checks/package.nix { inherit pkgs halyard; };

    ghostty-pin = import ./checks/ghostty-pin.nix {
      inherit pkgs cargoVendorDir;
      ghosttyRev = ghosttySrc.rev;
    };
  };

  devShell = craneLib.devShell {
    inputsFrom = [ halyard ];
    packages = [
      pkgs.rust-analyzer
      pkgs.cargo-deny
    ]
    ++ ptyTestPrograms;

    GHOSTTY_SOURCE_DIR = "${ghosttySrc}";
    GHOSTTY_ZIG_SYSTEM_DIR = "${zigPackages}";

    # `cargo run -p halyard-app` runs an unpatched binary, so the shell points
    # the dynamic loader at the libraries GPUI dlopens.
    shellHook = lib.optionalString stdenv.hostPlatform.isLinux ''
      export LD_LIBRARY_PATH="${lib.makeLibraryPath linuxRuntimeLibs}''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
    '';
  };
in
{
  inherit
    halyard
    hasApp
    checks
    devShell
    configChecker
    ;
}
