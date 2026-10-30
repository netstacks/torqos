{ config, lib, pkgs, ... }:

let
  cfg = config.kestrel.dev;
in
{
  options.kestrel.dev.enable = lib.mkOption {
    type = lib.types.bool;
    default = false;
    description = ''
      Development mode: convenience over security while Kestrel itself is being built.
      Passwordless sudo for wheel, Claude Code + gh installed, the owner is a trusted Nix user.
      An AI coding agent working in this mode has full root and can bypass the broker.
      Never enable on a real install; turning it off is one broker commit (ADR-009).
    '';
  };

  config = lib.mkIf cfg.enable {
    warnings = [
      "kestrel.dev.enable is on: passwordless sudo and an AI coding agent with root. Development only."
    ];

    security.sudo.wheelNeedsPassword = false;
    nix.settings.trusted-users = [ config.kestrel.user ];

    nixpkgs.config.allowUnfreePredicate = pkg: builtins.elem (lib.getName pkg) [ "claude-code" ];
    environment.systemPackages = with pkgs; [ claude-code gh ];
  };
}
