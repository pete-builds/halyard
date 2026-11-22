# NixOS module: programs.halyard.
#
# Installs Halyard system wide (its desktop entry and icons come with the
# package) and turns on the graphics stack GPUI renders through: Vulkan via
# Mesa, or the vendor driver if one is configured. Per-user settings and themes
# belong in the Home Manager module.
self:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.programs.halyard;
  system = pkgs.stdenv.hostPlatform.system;
in
{
  options.programs.halyard = {
    enable = lib.mkEnableOption "Halyard, a fast terminal for macOS and Linux";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${system}.default;
      defaultText = lib.literalExpression "halyard.packages.\${system}.default";
      description = "The Halyard package to install.";
    };

    fonts.enable = lib.mkEnableOption ''
      JetBrains Mono, a monospace font for Halyard. It is OFL-1.1, made for
      code, covers box drawing and powerline glyphs, and its ligatures are
      optional. Select it with `font.family = "JetBrains Mono"` in the Home
      Manager module or config.toml; the system-wide default monospace font is
      left alone'';
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];

    # GPUI draws with Vulkan. This provides the loader's ICDs under
    # /run/opengl-driver; desktop setups usually enable it already.
    hardware.graphics.enable = lib.mkDefault true;

    fonts.packages = lib.mkIf cfg.fonts.enable [ pkgs.jetbrains-mono ];
  };
}
