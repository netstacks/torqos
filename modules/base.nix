{ config, lib, pkgs, ... }:

let
  cfg = config.torq;
in
{
  options.torq.user = lib.mkOption {
    type = lib.types.nullOr lib.types.str;
    default = null;
    description = "Primary (owner) user account of this machine. None only in the image, before first-boot setup creates it (modules/setup.nix).";
  };

  config = {
    assertions = [
      {
        # Every configured machine has an owner; only the image, before setup, has none.
        assertion = cfg.user != null && cfg.user != "CHANGE-ME" || cfg.user == null && cfg.setup.enable;
        message = "Set the owner's login name: `torq user <name>` (nixos/torq.json in the config repo).";
      }
      {
        # The same shape the broker accepts for users/<name>/ (user tier).
        assertion = cfg.user == null || builtins.match "[a-z_][a-z0-9_-]*" cfg.user != null;
        message = "torq.user '${cfg.user}' must be a lowercase login name ([a-z_][a-z0-9_-]*).";
      }
    ];

    nix.settings = {
      experimental-features = [ "nix-command" "flakes" ];
      # A power cut mid-change must not leave the store broken: by default Nix records a path as
      # there before its files reach the disk, and a cut can leave it empty ("file is empty (possible
      # filesystem corruption)"), failing every later build. Found by cutting the power during
      # first-boot setup. Slower builds of many small paths; a machine that works after a cut.
      fsync-store-paths = true;
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

    users.users = lib.mkIf (cfg.user != null) {
      ${cfg.user} = {
        isNormalUser = true;
        extraGroups = [ "wheel" "networkmanager" "video" "audio" ];
      };
    };

    networking.networkmanager.enable = lib.mkDefault true;
    time.timeZone = lib.mkDefault "America/New_York";
    i18n.defaultLocale = lib.mkDefault "en_US.UTF-8";

    environment.systemPackages = with pkgs; [ git vim curl htop jq ];

    system.stateVersion = lib.mkDefault "26.05";
  };
}
