# Home Manager module: programs.halyard.
#
# Writes ~/.config/halyard/config.toml from `settings` (the schema is
# docs/configuration.md) and one ~/.config/halyard/themes/<name>.toml per entry
# in `themes` (docs/themes.md). `font` and `keybindings` are shortcuts that
# merge into `settings`; setting the same key both ways to different values is
# an evaluation error rather than a silent override.
#
# Halyard reads ~/.config/halyard on macOS as well as Linux, so the same module
# works under nix-darwin and standalone Home Manager on a Mac.
self:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  inherit (lib)
    literalExpression
    mkEnableOption
    mkIf
    mkMerge
    mkOption
    types
    ;

  cfg = config.programs.halyard;
  tomlFormat = pkgs.formats.toml { };
  system = pkgs.stdenv.hostPlatform.system;

  themeModule = types.submodule (
    { name, ... }:
    {
      freeformType = tomlFormat.type;
      options.name = mkOption {
        type = types.str;
        default = name;
        defaultText = literalExpression "<attribute name>";
        description = "Display name of the theme, used to select it in `settings.theme`.";
      };
    }
  );
in
{
  options.programs.halyard = {
    enable = mkEnableOption "Halyard, a fast terminal for macOS and Linux";

    package = mkOption {
      type = types.nullOr types.package;
      default = self.packages.${system}.default or null;
      defaultText = literalExpression "halyard.packages.\${system}.default";
      description = ''
        The Halyard package to install. Set to `null` to manage only the
        configuration files, for example when Halyard is installed another way.
      '';
    };

    settings = mkOption {
      type = tomlFormat.type;
      default = { };
      example = literalExpression ''
        {
          window.opacity = 0.95;
          terminal.scrollback_lines = 50000;
          theme = { light = "Halyard Day"; dark = "Halyard Night"; };
        }
      '';
      description = ''
        Configuration written to {file}`$XDG_CONFIG_HOME/halyard/config.toml`.
        Every key is optional. See `docs/configuration.md` in the Halyard
        repository for the full list.
      '';
    };

    font = {
      family = mkOption {
        type = types.nullOr types.str;
        default = null;
        example = "JetBrains Mono";
        description = "Font family, merged into `settings.font.family`.";
      };
      size = mkOption {
        type = types.nullOr types.number;
        default = null;
        example = 13.5;
        description = "Font size in points (4 to 128), merged into `settings.font.size`.";
      };
    };

    keybindings = mkOption {
      type = types.attrsOf types.str;
      default = { };
      example = {
        "primary+k" = "command_palette";
        "cmd+q" = "none";
      };
      description = ''
        Chord to action map, merged into `settings.keybindings`. Entries are
        added to the platform defaults; the action `none` removes a default.
      '';
    };

    themes = mkOption {
      type = types.attrsOf themeModule;
      default = { };
      example = literalExpression ''
        {
          rope-and-brass = {
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
        }
      '';
      description = ''
        User themes, each written to
        {file}`$XDG_CONFIG_HOME/halyard/themes/<attribute name>.toml`. `name`
        defaults to the attribute name. Select one with
        `settings.theme.name` (or `light` and `dark`). The format is
        `docs/themes.md` in the Halyard repository.
      '';
    };
  };

  config = mkIf cfg.enable {
    programs.halyard.settings = mkMerge [
      (mkIf (cfg.font.family != null) { font.family = cfg.font.family; })
      (mkIf (cfg.font.size != null) { font.size = cfg.font.size; })
      (mkIf (cfg.keybindings != { }) { keybindings = cfg.keybindings; })
    ];

    home.packages = mkIf (cfg.package != null) [ cfg.package ];

    xdg.configFile = mkMerge [
      (mkIf (cfg.settings != { }) {
        "halyard/config.toml".source = tomlFormat.generate "halyard-config.toml" cfg.settings;
      })
      (lib.mapAttrs' (
        fileName: theme:
        lib.nameValuePair "halyard/themes/${fileName}.toml" {
          source = tomlFormat.generate "halyard-theme-${fileName}.toml" theme;
        }
      ) cfg.themes)
    ];
  };
}
