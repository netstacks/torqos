{ config, lib, pkgs, ... }:

let
  cfg = config.torq;
in
{
  options.torq.user = lib.mkOption {
    type = lib.types.str;
    description = "Primary (owner) user account of this machine.";
  };

  config = {
    assertions = [
      {
        assertion = cfg.user != "CHANGE-ME";
        message = "Set the owner's login name: `torq user <name>` (nixos/torq.json in the config repo).";
      }
      {
        # The same shape the broker accepts for users/<name>/ (user tier).
        assertion = builtins.match "[a-z_][a-z0-9_-]*" cfg.user != null;
        message = "torq.user '${cfg.user}' must be a lowercase login name ([a-z_][a-z0-9_-]*).";
      }
    ];

    nix.settings = {
      experimental-features = [ "nix-command" "flakes" ];
      # Only root (i.e. the broker via sudo/systemd) may drive system builds.
      trusted-users = [ "root" ];
      allowed-users = [ "root" "@wheel" ];
      auto-optimise-store = true;
      # During a build, collect garbage when less than 1 GiB is free, until 5 GiB is.
      min-free = lib.mkDefault (1024 * 1024 * 1024);
      max-free = lib.mkDefault (5 * 1024 * 1024 * 1024);
    };

    # The store must not fill the disk. Collecting old generations does not weaken undo: a broker
    # revert undoes the commit and rebuilds it (from the binary cache), it never needs an old
    # generation on disk (ADR-012). The running system is never collected.
    nix.gc = {
      automatic = lib.mkDefault true;
      dates = lib.mkDefault "weekly";
      options = lib.mkDefault "--delete-older-than 30d";
      persistent = lib.mkDefault true; # catch up after the machine was off
    };
    boot.loader.systemd-boot.configurationLimit = lib.mkDefault 20;

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
