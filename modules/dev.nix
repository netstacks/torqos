{ config, lib, pkgs, ... }:

let
  cfg = config.torq.dev;
in
{
  options.torq.dev.enable = lib.mkOption {
    type = lib.types.bool;
    default = false;
    description = ''
      Development mode: convenience over security while TorqOS itself is being built.
      Passwordless sudo for wheel, Claude Code + gh + tmux installed, the owner is a trusted Nix user.
      An AI coding agent working in this mode has full root and can bypass the broker.
      Never enable on a real install; turning it off is one broker commit (ADR-009).
    '';
  };

  config = lib.mkIf cfg.enable {
    warnings = [
      "torq.dev.enable is on: passwordless sudo and an AI coding agent with root. Development only."
    ];

    security.sudo.wheelNeedsPassword = false;
    nix.settings.trusted-users = lib.optional (config.torq.user != null) config.torq.user;

    # (Unfree: allowed by name, through the one rule for it, modules/unfree.nix.)
    torq.unfree.packages = [ "claude-code" ];
    # tmux: run Claude Code inside it (`tmux new -A -s claude`) so a dropped SSH connection — the
    # Mac sleeping, UTM pausing, the network restarting during a switch — does not kill the session.
    environment.systemPackages = with pkgs; [ claude-code gh tmux vim ];

    # Claude Code ships a new build most days; nixpkgs lags it by weeks, and the upstream
    # native installer is useless here (its binary is a plain dynamically-linked ELF — it
    # needs nixpkgs' autoPatchelfHook to run at all, and the Nix build sets
    # DISABLE_AUTOUPDATER anyway). So pin the version declaratively by overriding the
    # release manifest nixpkgs reads. To bump:
    #   curl -fsSL https://downloads.claude.ai/claude-code-releases/latest
    #   curl -fsSL https://downloads.claude.ai/claude-code-releases/<ver>/manifest.json \
    #     -o modules/claude-code-manifest.json
    nixpkgs.overlays = [
      (final: prev: {
        claude-code = prev.claude-code.override {
          manifest = lib.importJSON ./claude-code-manifest.json;
        };
      })
    ];
  };
}
