# Evaluation checks for the Home Manager and NixOS modules.
#
# The Home Manager check evaluates real configurations with home-manager,
# renders the files the module writes, and compares them (parsed as TOML) with
# hand-written expectations. Controls: a comparison against a wrong expectation
# must fail, and a conflicting definition must be an evaluation error.
# The written files are also loaded by halyard-config's own parser
# (`configChecker`, its `check_files` example), with a broken file as control.
{
  pkgs,
  self,
  nixpkgs,
  home-manager,
  configChecker,
}:
let
  inherit (pkgs) lib stdenv;
  system = stdenv.hostPlatform.system;

  # A stand-in so the module checks do not build Halyard itself.
  fakeHalyard = pkgs.writeShellScriptBin "halyard" "echo fake halyard";

  homeDirectory = if stdenv.hostPlatform.isDarwin then "/Users/sailor" else "/home/sailor";

  hmEval =
    halyardConfig:
    (home-manager.lib.homeManagerConfiguration {
      inherit pkgs;
      modules = [
        self.homeManagerModules.default
        {
          home = {
            username = "sailor";
            inherit homeDirectory;
            stateVersion = "25.11";
          };
          programs.halyard = halyardConfig;
        }
      ];
    }).config;

  ansi = [
    "#2a2620"
    "#e0705a"
    "#9cc27a"
    "#e6be6a"
    "#7fa6d6"
    "#c89ad0"
    "#76c6bc"
    "#d8d0bc"
    "#4a443a"
    "#f08c78"
    "#b6d896"
    "#f2d08a"
    "#9cc0ea"
    "#dcb6e2"
    "#94dcd2"
    "#f4eee0"
  ];
  terminalColors = {
    foreground = "#e8e2d0";
    background = "#1d1a16";
    inherit ansi;
  };

  full = hmEval {
    enable = true;
    package = fakeHalyard;
    settings = {
      font = {
        line_height = 1.3;
        bold_is_bright = true;
      };
      window = {
        opacity = 0.95;
        initial_cols = 120;
      };
      theme.name = "Rope and Brass";
      # Exempt from the secret-name rule: api_key_env holds a variable name.
      ai = {
        enabled = true;
        providers = [
          {
            name = "claude";
            kind = "anthropic";
            model = "claude-sonnet-4-5";
            api_key_env = "ANTHROPIC_API_KEY";
          }
        ];
      };
    };
    font = {
      family = "JetBrains Mono";
      size = 14.5;
    };
    keybindings = {
      "primary+k" = "command_palette";
      "cmd+q" = "none";
    };
    themes = {
      rope-and-brass = {
        name = "Rope and Brass";
        terminal = terminalColors;
      };
      plain.terminal = terminalColors;
    };
  };

  # What the files must contain, written out by hand rather than derived from
  # the module.
  expected = {
    config = {
      font = {
        family = "JetBrains Mono";
        size = 14.5;
        line_height = 1.3;
        bold_is_bright = true;
      };
      window = {
        opacity = 0.95;
        initial_cols = 120;
      };
      theme.name = "Rope and Brass";
      ai = {
        enabled = true;
        providers = [
          {
            name = "claude";
            kind = "anthropic";
            model = "claude-sonnet-4-5";
            api_key_env = "ANTHROPIC_API_KEY";
          }
        ];
      };
      keybindings = {
        "primary+k" = "command_palette";
        "cmd+q" = "none";
      };
    };
    ropeAndBrass = {
      name = "Rope and Brass";
      terminal = terminalColors;
    };
    plain = {
      name = "plain";
      terminal = terminalColors;
    };
    # Controls for the comparison: a wrong value, and right values of the
    # wrong type (Python alone would call 1 == True and 120 == 120.0 equal).
    wrongValue = lib.recursiveUpdate expected.config { font.size = 12; };
    wrongBoolType = lib.recursiveUpdate expected.config { font.bold_is_bright = 1; };
    wrongNumberType = lib.recursiveUpdate expected.config { window.initial_cols = 120.0; };
  };

  # Secret-looking key names must stop evaluation before anything reaches the
  # store, whether under `ai` or anywhere else.
  rejects =
    settings:
    let
      c = hmEval {
        enable = true;
        package = null;
        inherit settings;
      };
    in
    !(builtins.tryEval (builtins.seq (file c "config.toml").outPath true)).success;

  minimal = hmEval {
    enable = true;
    package = null;
  };

  withDefaultPackage = hmEval { enable = true; };

  conflicting = hmEval {
    enable = true;
    package = null;
    settings.font.family = "Iosevka";
    font.family = "JetBrains Mono";
  };
  conflictRejected =
    !(builtins.tryEval (builtins.deepSeq conflicting.programs.halyard.settings true)).success;

  disabled = hmEval { };

  file = cfg: name: cfg.xdg.configFile."halyard/${name}".source;
  hasPackage = cfg: pkg: builtins.any (p: p.outPath == pkg.outPath) cfg.home.packages;

  evalFacts = {
    "full installs the configured package" = hasPackage full fakeHalyard;
    "package = null installs nothing" =
      !(builtins.any (p: (p.pname or p.name or "") == "halyard") minimal.home.packages);
    "empty settings write no config.toml" = !(minimal.xdg.configFile ? "halyard/config.toml");
    "default package is the flake's package" =
      hasPackage withDefaultPackage self.packages.${system}.default;
    "conflicting font.family is an error" = conflictRejected;
    "an api_key under ai.providers is refused" = rejects {
      ai.providers = [
        {
          name = "claude";
          kind = "anthropic";
          model = "m";
          api_key = "sk-halyard-canary";
        }
      ];
    };
    "a token-like name in any section is refused" = rejects { window.Auth_Token = "x"; };
    "a nested secret-like name is refused" = rejects { terminal.shell.password = "x"; };
    # Control for the rule itself: the same shape without the secret passes.
    "api_key_env and keybindings pass the secret rule" =
      !(rejects {
        keybindings."primary+k" = "command_palette";
        ai.providers = [ { api_key_env = "ANTHROPIC_API_KEY"; } ];
      });
    "disabled module writes nothing" =
      !(builtins.any (lib.hasPrefix "halyard/") (builtins.attrNames disabled.xdg.configFile));
  };
  failedFacts = lib.filterAttrs (_: ok: !ok) evalFacts;

  compare = pkgs.writeText "compare-toml.py" ''
    import json, sys, tomllib

    toml_path, json_path = sys.argv[1], sys.argv[2]
    with open(toml_path, "rb") as f:
        got = tomllib.load(f)
    with open(json_path) as f:
        want = json.load(f)
    # Canonical JSON keeps types apart: true is not 1, 120 is not 120.0.
    if json.dumps(got, sort_keys=True) != json.dumps(want, sort_keys=True):
        print(f"mismatch in {toml_path}:\n got:  {got}\n want: {want}", file=sys.stderr)
        sys.exit(1)
    print(f"ok: {toml_path}")
  '';
  json = name: value: pkgs.writeText "${name}.json" (builtins.toJSON value);
in
{
  hm-module =
    if failedFacts != { } then
      throw "hm-module check failed: ${builtins.concatStringsSep "; " (builtins.attrNames failedFacts)}"
    else
      pkgs.runCommand "halyard-hm-module-check" { nativeBuildInputs = [ pkgs.python3 ]; } ''
        python3 ${compare} ${file full "config.toml"} ${json "config" expected.config}
        python3 ${compare} ${file full "themes/rope-and-brass.toml"} ${json "rope" expected.ropeAndBrass}
        python3 ${compare} ${file full "themes/plain.toml"} ${json "plain" expected.plain}

        # Controls: the comparison must notice a wrong value and a wrong type.
        for wrong in ${json "wrong-value" expected.wrongValue} ${json "wrong-bool" expected.wrongBoolType} ${json "wrong-number" expected.wrongNumberType}; do
          if python3 ${compare} ${file full "config.toml"} "$wrong" 2>/dev/null; then
            echo "control failed: $wrong matched" >&2
            exit 1
          fi
        done
        echo "control ok: wrong values and wrong types are rejected"

        # The real parser: halyard-config must load every file without an error
        # or a warning.
        ${configChecker}/bin/check_files \
          --config ${file full "config.toml"} \
          --theme ${file full "themes/rope-and-brass.toml"} \
          --theme ${file full "themes/plain.toml"}
        # Control: a file halyard-config rejects must fail the checker.
        if ${configChecker}/bin/check_files --config ${pkgs.writeText "bad-config.toml" "[font]\nsize = 3.0\n"}; then
          echo "control failed: check_files accepted font.size = 3.0" >&2
          exit 1
        fi
        echo "control ok: check_files rejects an invalid config"

        cat > "$out" <<'EOF'
        ${builtins.toJSON evalFacts}
        EOF
      '';
}
// lib.optionalAttrs stdenv.hostPlatform.isLinux {
  nixos-module =
    let
      nixosWith =
        halyardConfig:
        (nixpkgs.lib.nixosSystem {
          inherit system;
          modules = [
            self.nixosModules.default
            {
              programs.halyard = halyardConfig;
              boot.loader.grub.enable = false;
              fileSystems."/" = {
                device = "/dev/null";
                fsType = "ext4";
              };
              system.stateVersion = "25.11";
            }
          ];
        }).config;
      nixosWithoutFonts = nixosWith {
        enable = true;
        package = fakeHalyard;
      };
      nixos =
        (nixpkgs.lib.nixosSystem {
          inherit system;
          modules = [
            self.nixosModules.default
            {
              programs.halyard = {
                enable = true;
                package = fakeHalyard;
                fonts.enable = true;
              };
              boot.loader.grub.enable = false;
              fileSystems."/" = {
                device = "/dev/null";
                fsType = "ext4";
              };
              system.stateVersion = "25.11";
            }
          ];
        }).config;
      facts = {
        "package is in systemPackages" = builtins.elem fakeHalyard nixos.environment.systemPackages;
        "graphics stack is enabled" = nixos.hardware.graphics.enable;
        "fonts.enable installs JetBrains Mono" = builtins.elem pkgs.jetbrains-mono nixos.fonts.packages;
        "fonts are off by default" =
          !(builtins.elem pkgs.jetbrains-mono nixosWithoutFonts.fonts.packages);
      };
      failed = lib.filterAttrs (_: ok: !ok) facts;
    in
    if failed != { } then
      throw "nixos-module check failed: ${builtins.concatStringsSep "; " (builtins.attrNames failed)}"
    else
      pkgs.writeText "halyard-nixos-module-check" (builtins.toJSON facts);
}
