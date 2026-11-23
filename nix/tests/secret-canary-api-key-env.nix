# Evaluates a Home Manager config with a key pasted into `api_key_env` twice:
# once directly under `ai` (the field is only valid on a provider, and v0.1.1
# let it through there), and once as a provider's `api_key_env` value (it must
# be a variable name). Evaluation must fail, name both places, and print
# neither value. The values are built by concatenation, so they only appear in
# the output if some code evaluated and printed them (source excerpts in the
# trace show the split form). Run by .github/workflows/nix.yml:
#   nix eval --impure --expr 'import ./nix/tests/secret-canary-api-key-env.nix ./.'
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
          settings.ai = {
            api_key_env = "sk-" + "halyard-canary-misplaced-9c2e";
            providers = [
              {
                name = "claude";
                kind = "anthropic";
                model = "m";
                api_key_env = "sk-" + "halyard-canary-value-4d81";
              }
            ];
          };
        };
      }
    ];
  };
in
hm.config.xdg.configFile."halyard/config.toml".source.outPath
