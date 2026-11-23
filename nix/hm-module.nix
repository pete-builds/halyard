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

  # Mirrors halyard-config's rule: a key NAME containing one of these
  # (case-insensitive) is refused, because its value may be a credential.
  # Checked at evaluation time, before anything is written to the
  # world-readable store. As in halyard-config, a documented name is exempt
  # only in the table that defines it: `api_key_env` (it holds the name of a
  # variable, not a key) only directly on an `ai.providers` entry, and
  # everything under the top-level `keybindings` table (chords, not secrets).
  # `api_key_env` anywhere else is treated as a pasted secret.
  secretFragments = [
    "key"
    "token"
    "secret"
    "password"
    "passwd"
    "auth"
  ];
  looksSecret = name: lib.any (f: lib.hasInfix f (lib.toLower name)) secretFragments;

  # A provider's `api_key_env` must be a variable name. The value is matched
  # here and never printed.
  validEnvName = v: builtins.isString v && builtins.match "[A-Za-z_][A-Za-z0-9_]*" v != null;

  # Every problem as `{ path; env; }`: `env` is false for a secret-looking
  # name, true for a provider `api_key_env` that is not a variable name.
  # Paths look like `window.auth_token`; under `ai` only the section is
  # reported (`ai` or `ai.providers[N]`), as halyard-config does, in case a
  # name itself was pasted from a credential. `where` is the documented table
  # `value` is: "top", "ai", "providers" (the list), "provider", or "" for
  # anything undocumented. `section` is the `ai` section to report, or null
  # outside `ai`. Values are never printed.
  secretPaths =
    let
      walk =
        path: where: section: value:
        if builtins.isAttrs value then
          lib.concatLists (
            lib.mapAttrsToList (
              name: v:
              let
                here = if path == "" then name else "${path}.${name}";
                isAi = where == "top" && name == "ai";
                child =
                  if isAi then
                    "ai"
                  else if where == "ai" && name == "providers" then
                    "providers"
                  else
                    "";
                exempt = where == "provider" && name == "api_key_env";
              in
              if where == "top" && name == "keybindings" then
                [ ]
              else
                lib.optional (looksSecret name && !exempt) {
                  path = if section == null then here else section;
                  env = false;
                }
                ++ lib.optional (exempt && !(validEnvName v)) {
                  path = "${section}.api_key_env";
                  env = true;
                }
                ++ walk here child (if isAi then "ai" else section) v
            ) value
          )
        else if builtins.isList value then
          lib.concatLists (
            lib.imap0 (
              i: v:
              let
                isProvider = where == "providers" && builtins.isAttrs v;
              in
              walk "${path}[${toString i}]" (if isProvider then "provider" else "") (
                if isProvider then "ai.providers[${toString i}]" else section
              ) v
            ) value
          )
        else
          [ ];
    in
    walk "" "top" null;

  checkedSettings =
    settings:
    let
      found = secretPaths settings;
      paths =
        env:
        lib.concatStringsSep ", " (
          lib.unique (map (x: x.path) (lib.filter (x: x.env == env) found))
        );
      names = paths false;
      envs = paths true;
    in
    if found == [ ] then
      settings
    else
      throw (
        lib.optionalString (names != "") ''
          programs.halyard.settings: refusing to write ${names}: a key is
          named like a secret (key, token, secret, password, passwd or auth). Nix would copy it into
          the world-readable /nix/store. Export the credential in an environment variable and set
          `api_key_env = "NAME_OF_VARIABLE"` on the `ai.providers` entry instead. `api_key_env` is
          only valid directly on an `ai.providers` entry; anywhere else it is treated as a pasted
          secret.
        ''
        + lib.optionalString (envs != "") ''
          programs.halyard.settings: refusing to write ${envs}: it must be the NAME of an
          environment variable (letters, digits, underscore, not starting with a digit), not a
          key. The value is not shown. Nix would copy it into the world-readable /nix/store. If
          you pasted a key there, revoke it.
        ''
      );

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
      # null on a system the flake does not build for (such as x86_64-darwin,
      # which nixpkgs dropped), so the config files can still be managed there.
      default = self.packages.${system}.default or null;
      defaultText = literalExpression "halyard.packages.\${system}.default, or null where the flake has no package";
      description = ''
        The Halyard package to install. Set to `null` to manage only the
        configuration files, for example when Halyard is installed another way.
        Defaults to `null` on systems the flake does not build for
        (x86_64-darwin).
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
        repository for the full list. Never put a credential here: the file
        lands in the world-readable Nix store. A key named like a secret (key,
        token, secret, password, passwd, auth) is an evaluation error; use
        `api_key_env` on an `ai.providers` entry.
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
        "halyard/config.toml".source = tomlFormat.generate "halyard-config.toml" (
          checkedSettings cfg.settings
        );
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
