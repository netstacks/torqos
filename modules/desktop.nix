{ config, lib, pkgs, ... }:

let
  cfg = config.torq.desktop;
in
{
  options.torq.desktop.enable = lib.mkOption {
    type = lib.types.bool;
    # Off in the base: the desktop is a configuration (`torqos-desktop`, ADR-034), not the product.
    default = false;
    description = "TorqOS Desktop: COSMIC on Wayland with the cosmic-greeter (ADR-045). Off in the base.";
  };

  config = lib.mkIf cfg.enable {
    # COSMIC, and only COSMIC (ADR-045), through NixOS's own modules at the version nixpkgs pins. They
    # bring the compositor, the panel and its applets, COSMIC's own terminal, files, editor, launcher
    # and settings, the portal (xdg-desktop-portal-cosmic), the polkit agent (cosmic-osd), PipeWire,
    # Bluetooth, power profiles, geoclue and the keyring.
    services.desktopManager.cosmic.enable = true;
    services.displayManager.cosmic-greeter.enable = true;

    # A desktop's network: NetworkManager, for COSMIC's network applet and settings page, with iwd as its
    # Wi-Fi backend (ADR-046 2): one Wi-Fi stack, so networks joined at setup with iwctl stay joined. The
    # networks and their secrets are NetworkManager's and iwd's state, never the repository's (the fence).
    networking.networkmanager.enable = true;
    networking.networkmanager.wifi.backend = "iwd";
    networking.useNetworkd = false;

    # Applications by rung (ADR-037, amended): Flatpak on, which also brings the COSMIC Store. Its
    # installs are per user, in the person's home, with no password; Flathub is added for each person,
    # never system-wide (a system Flatpak is a tree setting). From Flathub's own repo file (from
    # dl.flathub.org, with its signing key), carried here: adding the remote needs no network, so a
    # login before Wi-Fi is up still gets it.
    services.flatpak.enable = true;
    systemd.user.services.torq-flathub = {
      description = "Flathub for this person's own Flatpaks (ADR-037)";
      wantedBy = [ "default.target" ];
      serviceConfig = {
        Type = "oneshot";
        ExecStart = "${pkgs.flatpak}/bin/flatpak remote-add --user --if-not-exists flathub ${./flathub.flatpakrepo}";
      };
    };

    hardware.graphics.enable = true;

    # What base.nix leaves out of a machine without a screen.
    fonts.packages = with pkgs; [ inter jetbrains-mono noto-fonts noto-fonts-color-emoji ];
    fonts.fontconfig.enable = true;
    xdg = {
      autostart.enable = true;
      icons.enable = true;
      mime.enable = true;
      sounds.enable = true;
    };

    environment.systemPackages = [
      # TorqOS's icons (branding/), for what shows the system's logo (os-release's LOGO).
      config.torq.branding.package
    ];
  };
}
