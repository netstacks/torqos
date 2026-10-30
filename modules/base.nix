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
