{ config, lib, pkgs, ... }:

let
  cfg = config.kestrel;
in
{
  options.kestrel.user = lib.mkOption {
    type = lib.types.str;
    description = "Primary (owner) user account of this laptop.";
  };

  config = {
    assertions = [
      {
        assertion = cfg.user != "CHANGE-ME";
        message = "Set kestrel.user to your login name in the config repo's system.nix.";
      }
      {
        # The same shape the broker accepts for users/<name>.packages.json (user tier).
        assertion = builtins.match "[a-z_][a-z0-9_-]*" cfg.user != null;
        message = "kestrel.user '${cfg.user}' must be a lowercase login name ([a-z_][a-z0-9_-]*).";
      }
    ];

    nix.settings = {
      experimental-features = [ "nix-command" "flakes" ];
      # Only root (i.e. the broker via sudo/systemd) may drive system builds.
      trusted-users = [ "root" ];
      allowed-users = [ "root" "@wheel" ];
      auto-optimise-store = true;
    };

    users.users.${cfg.user} = {
      isNormalUser = true;
      extraGroups = [ "wheel" "networkmanager" "video" "audio" ];
    };

    networking.networkmanager.enable = lib.mkDefault true;
    time.timeZone = lib.mkDefault "America/New_York";
    i18n.defaultLocale = lib.mkDefault "en_US.UTF-8";

    environment.systemPackages = with pkgs; [ git vim curl htop jq ];

    system.stateVersion = lib.mkDefault "26.05";
  };
}
