# Unfree software (nixpkgs' licence check): refused unless allowed — by name (`set torq unfree
# packages [ postman vscode ]`, each as nixpkgs names it: lib.getName), or all of it (`set torq unfree
# all true`). System settings in the tree, shown and approved like any change.
#
# This is the one allowUnfreePredicate: whatever else needs an unfree package adds its name to
# torq.unfree.packages (modules/dev.nix: claude-code), so the rules never conflict.
{ config, lib, ... }:

let
  cfg = config.torq.unfree;
in
{
  options.torq.unfree = {
    packages = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      example = [ "postman" "vscode" ];
      description = "Unfree packages allowed, by name (as nixpkgs names them).";
    };
    all = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Allow every unfree package.";
    };
  };

  config.nixpkgs.config.allowUnfreePredicate = pkg: cfg.all || builtins.elem (lib.getName pkg) cfg.packages;
}
