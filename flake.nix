{
  description = "Halyard: a fast, open-source terminal for macOS and Linux";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    # Builds the dependencies once and shares them between the package and
    # the clippy and test checks, instead of rebuilding Ghostty and GPUI for
    # each. Has no inputs of its own.
    crane.url = "github:ipetkov/crane";

    # Only used by checks.*.hm-module, to evaluate the Home Manager module
    # against real Home Manager. Flake inputs are fetched lazily, so building
    # or running Halyard never downloads it.
    home-manager = {
      url = "github:nix-community/home-manager";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    # Ghostty source for libghostty-vt-sys, pinned to the exact commit that
    # crate's build script pins (GHOSTTY_COMMIT). checks.*.ghostty-pin fails if
    # they ever differ.
    ghostty = {
      url = "github:ghostty-org/ghostty/a887df42c56f6de86c0fe6da9c4eeca37931e083";
      flake = false;
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      crane,
      home-manager,
      ghostty,
    }:
    let
      inherit (nixpkgs) lib;

      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
        "x86_64-darwin"
      ];

      forAllSystems = f: lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});

      build = forAllSystems (
        pkgs:
        import ./nix {
          inherit pkgs crane;
          ghosttySrc = ghostty;
        }
      );
    in
    {
      packages = forAllSystems (
        pkgs:
        let
          b = build.${pkgs.stdenv.hostPlatform.system};
        in
        {
          default = b.halyard;
          halyard = b.halyard;
        }
      );

      apps = forAllSystems (
        pkgs:
        let
          b = build.${pkgs.stdenv.hostPlatform.system};
        in
        lib.optionalAttrs b.hasApp {
          default = {
            type = "app";
            program = lib.getExe b.halyard;
            meta.description = "Run the Halyard terminal";
          };
        }
      );

      devShells = forAllSystems (pkgs: {
        default = build.${pkgs.stdenv.hostPlatform.system}.devShell;
      });

      checks = forAllSystems (
        pkgs:
        build.${pkgs.stdenv.hostPlatform.system}.checks
        // import ./nix/checks/modules.nix {
          inherit
            pkgs
            self
            nixpkgs
            home-manager
            ;
        }
      );

      overlays.default = final: _prev: {
        halyard = self.packages.${final.stdenv.hostPlatform.system}.default;
      };

      homeManagerModules.default = import ./nix/hm-module.nix self;
      homeModules.default = self.homeManagerModules.default;
      nixosModules.default = import ./nix/nixos-module.nix self;
    };
}
