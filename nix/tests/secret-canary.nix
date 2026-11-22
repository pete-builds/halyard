# Evaluates a Home Manager config that puts an API key in settings. The
# evaluation must fail, and its error must not contain the key's value. The
# value is built by concatenation, so it only appears in the output if some
# code evaluated and printed it (source excerpts in the trace show the split
# form). Run by .github/workflows/nix.yml:
#   nix eval --impure --expr 'import ./nix/tests/secret-canary.nix ./.'
flakeDir:
let
  flake = builtins.getFlake (toString flakeDir);
  pkgs = flake.inputs.nixpkgs.legacyPackages.${builtins.currentSystem};
  hm = flake.inputs.home-manager.lib.homeManagerConfiguration {
    inherit pkgs;
    modules = [
      flake.homeManagerModules.default
      {
        home = {
          username = "sailor";
          homeDirectory = "/home/sailor";
          stateVersion = "25.11";
        };
        programs.halyard = {
          enable = true;
          package = null;
          settings.ai.providers = [
            {
              name = "claude";
              kind = "anthropic";
              model = "m";
              api_key = "sk-" + "halyard-canary-7f3a";
            }
          ];
        };
      }
    ];
  };
in
hm.config.xdg.configFile."halyard/config.toml".source.outPath
