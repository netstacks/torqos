{ config, lib, pkgs, ... }:

let
  cfg = config.kestrel.desktop;
  tuigreet = pkgs.tuigreet or pkgs.greetd.tuigreet;
in
{
  options.kestrel.desktop.enable = lib.mkOption {
    type = lib.types.bool;
    default = true;
    description = "Kestrel desktop: niri (Wayland) + Quickshell shell.";
  };

  config = lib.mkIf cfg.enable {
    # Wayland only — no X server.
    services.xserver.enable = false;

    programs.niri.enable = true;

    services.greetd = {
      enable = true;
      settings.default_session.command =
        "${tuigreet}/bin/tuigreet --time --remember --cmd niri-session";
    };

    hardware.graphics.enable = true;

    security.rtkit.enable = true;
    services.pipewire = {
      enable = true;
      alsa.enable = true;
      pulse.enable = true;
    };

    fonts.packages = with pkgs; [ inter jetbrains-mono noto-fonts noto-fonts-color-emoji ];

    environment.systemPackages = with pkgs; [
      quickshell # TODO(M3): ship the Kestrel shell (shell/) instead of the bare toolkit
      foot
      fuzzel
      firefox
      wl-clipboard
    ];
  };
}
