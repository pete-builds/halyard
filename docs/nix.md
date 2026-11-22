# Nix and NixOS

Halyard ships a flake (`flake.nix`) for Linux (x86_64, aarch64) and Apple silicon macOS (aarch64). Intel macOS is not
included: the pinned nixpkgs (26.11 unstable) dropped x86_64-darwin.
It provides the package, a dev shell, a Home Manager module and a NixOS module.

## Commands

```sh
nix build github:pete-builds/halyard      # the package, into ./result
nix run github:pete-builds/halyard        # launch the terminal
nix develop                              # Rust, Zig 0.15.2, libraries, cargo-deny
nix flake check                          # package, tests, clippy, fmt, pins, modules
```

Inside `nix develop`, the usual cargo commands work (`cargo test --workspace --locked`,
`cargo run -p halyard-app`). The shell sets `GHOSTTY_SOURCE_DIR` and `GHOSTTY_ZIG_SYSTEM_DIR`,
so cargo builds Ghostty from the pinned source without cloning or downloading anything, and on
Linux it puts the libraries GPUI loads at runtime on `LD_LIBRARY_PATH`.

## How the build stays offline and pinned

`libghostty-vt-sys` normally runs `git clone` on Ghostty and lets Zig download Ghostty's
dependencies. Neither can happen in the Nix sandbox, so the flake provides both ahead of time:

| What | Where it comes from |
|------|---------------------|
| Ghostty source | Flake input `ghostty`, pinned to the commit in `libghostty-vt-sys`'s `GHOSTTY_COMMIT`. Passed as `GHOSTTY_SOURCE_DIR`. |
| Ghostty's Zig packages | Ghostty's own `build.zig.zon.nix` at that commit: one fixed-output fetch per package (`nix/ghostty-zig-deps.nix`). Passed as `GHOSTTY_ZIG_SYSTEM_DIR`, which makes the build script run `zig build --system`. |
| Zig | `zig_0_15` from nixpkgs (0.15.2, the only version Ghostty accepts). |
| Rust crates | `Cargo.lock`, vendored by crane. |
| Rust compiler | nixpkgs' `rustc`, not `rust-toolchain.toml` (see below). |

`checks.<system>.ghostty-pin` reads the `libghostty-vt-sys` version from `Cargo.lock`, finds
its `build.rs`, and fails if `GHOSTTY_COMMIT` differs from the flake's `ghostty` input. When the
crate is bumped, bump the input to the new commit and run `nix flake lock --update-input ghostty`.

**Rust version.** rustup users get the 1.100.0 pinned in `rust-toolchain.toml`. Nix uses the
`rustc` in the pinned nixpkgs instead, so the flake does not need a Rust overlay input. That is
fine as long as nixpkgs stays at or above `rust-version` in `Cargo.toml` (1.90). Clippy lints can
differ slightly between the two versions; CI runs both.

**Inputs.** `nixpkgs` (unstable), `crane` (builds the dependencies once and shares them between
the package, clippy and tests, so Ghostty and GPUI are not compiled three times), `ghostty`
(source only), and `home-manager`, used only by the module check. Flake inputs are fetched
lazily, so installing Halyard never downloads Home Manager.

## Linux runtime libraries

GPUI links libxcb and libxkbcommon and loads Vulkan, Wayland, fontconfig and the X11 client
libraries with `dlopen` at runtime. The package adds those directories to the binary's
`RUNPATH` after the fixup phase. It does not use a wrapper that sets `LD_LIBRARY_PATH`, because
that would leak into every shell Halyard starts. The Vulkan driver itself (Mesa or a vendor
driver) comes from the system, through `/run/opengl-driver` on NixOS; the NixOS module turns on
`hardware.graphics` for that.

The package also installs `share/applications/halyard.desktop` (checked by
`desktop-file-validate` during the build) and the icon in SVG and PNG sizes under
`share/icons/hicolor`.

## NixOS

```nix
# flake.nix of your system
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    halyard = {
      url = "github:pete-builds/halyard";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { nixpkgs, halyard, ... }: {
    nixosConfigurations.myhost = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        ./configuration.nix
        halyard.nixosModules.default
        { programs.halyard.enable = true; }
      ];
    };
  };
}
```

`programs.halyard.enable` installs the package system wide and sets
`hardware.graphics.enable = true` (with `mkDefault`, so your own setting wins).
`programs.halyard.fonts.enable = true` (off by default) also installs JetBrains Mono: OFL-1.1,
designed for code, with box drawing and powerline glyphs. It does not change the system's
default monospace font; select it with `font.family = "JetBrains Mono"`. Without the
module, add `halyard.packages.${system}.default` to `environment.systemPackages`, or use
`halyard.overlays.default` and `pkgs.halyard`.

## Home Manager

The module manages `~/.config/halyard/config.toml` and user themes. Halyard reads
`~/.config/halyard` on macOS too, so the same module works on a Mac.

```nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    home-manager = {
      url = "github:nix-community/home-manager";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    halyard = {
      url = "github:pete-builds/halyard";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.home-manager.follows = "home-manager";
    };
  };

  outputs = { nixpkgs, home-manager, halyard, ... }: {
    homeConfigurations.me = home-manager.lib.homeManagerConfiguration {
      pkgs = nixpkgs.legacyPackages.x86_64-linux;
      modules = [
        halyard.homeManagerModules.default
        {
          home = { username = "me"; homeDirectory = "/home/me"; stateVersion = "25.11"; };

          programs.halyard = {
            enable = true;
            font = { family = "JetBrains Mono"; size = 13.5; };
            keybindings = {
              "primary+k" = "command_palette";
              "cmd+q" = "none";
            };
            settings = {
              window.opacity = 0.96;
              terminal.scrollback_lines = 50000;
              theme.name = "Rope and Brass";
            };
            themes.rope-and-brass = {
              name = "Rope and Brass";
              terminal = {
                foreground = "#e8e2d0";
                background = "#1d1a16";
                ansi = [
                  "#2a2620" "#e0705a" "#9cc27a" "#e6be6a" "#7fa6d6" "#c89ad0" "#76c6bc" "#d8d0bc"
                  "#4a443a" "#f08c78" "#b6d896" "#f2d08a" "#9cc0ea" "#dcb6e2" "#94dcd2" "#f4eee0"
                ];
              };
            };
          };
        }
      ];
    };
  };
}
```

| Option | Meaning |
|--------|---------|
| `enable` | Install Halyard and write its config. |
| `package` | The package to install. `null` writes the config only. Default: this flake's package, or `null` on a system the flake does not build (x86_64-darwin). |
| `settings` | Any keys from [configuration.md](configuration.md), written as `config.toml`. Nothing is written when it is empty. |
| `font.family`, `font.size` | Shortcuts for `settings.font.family` and `settings.font.size`. |
| `keybindings` | Shortcut for `settings.keybindings`: `"chord" = "action"`. |
| `themes.<file>` | One theme per entry, written to `themes/<file>.toml`. `name` defaults to the attribute name. Format: [themes.md](themes.md). |

Setting the same key through a shortcut and through `settings` with different values is an
evaluation error, not a silent override.

**Never put a credential in `settings`.** Everything there is copied into the world-readable
`/nix/store` (and into any binary cache you push to). The module refuses, at evaluation time and
without printing the value, any key named like a secret (containing key, token, secret,
password, passwd or auth), the same rule Halyard applies when it reads the file. For an AI
provider, export the key in an environment variable and name it with
`api_key_env = "ANTHROPIC_API_KEY"`. This guards the rendered `config.toml` only: a credential
written in your own `.nix` files is already in the store, because a flake-based configuration
copies its source there.

The written `config.toml` and theme files are checked in CI by halyard-config's own parser
(`crates/halyard-config/examples/check_files.rs`).

### nix-darwin

Use the same module through Home Manager's nix-darwin integration:

```nix
darwinConfigurations.mac = nix-darwin.lib.darwinSystem {
  modules = [
    home-manager.darwinModules.home-manager
    {
      home-manager.users.me = {
        imports = [ halyard.homeManagerModules.default ];
        programs.halyard = {
          enable = true;
          font.family = "SF Mono";
        };
      };
    }
  ];
};
```

## What CI proves

The `nix` workflow (`.github/workflows/nix.yml`) runs on `ubuntu-24.04` (x86_64-linux),
`ubuntu-24.04-arm` (aarch64-linux) and `macos-15` (aarch64-darwin):

* `nix flake check`: the package build, `cargo test`, clippy with `-D warnings`, rustfmt, the
  Ghostty pin check with its control, `package-contents` (`bin/halyard`, and on Linux the
  desktop entry, icons and every dlopened library resolving through RUNPATH, with a control),
  and the module checks (the Home Manager output is loaded by halyard-config's parser).
* On ubuntu-24.04, a Home Manager config with an `api_key` in `settings` must fail to evaluate
  without printing the key's value.
* `nix develop` runs fmt, clippy and the tests the way a contributor would.
* On Linux, a control build first proves the sandbox has no network, so the package building
  in that sandbox shows the build is offline.

On ubuntu-24.04, `nix flake check --all-systems --no-build` also evaluates every output for
all three systems, which catches import from derivation and per-system evaluation errors.
