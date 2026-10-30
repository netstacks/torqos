# Hardware + boot for this machine. Copy /etc/nixos/hardware-configuration.nix next to this file.
{ ... }:
{
  imports = [ ./hardware-configuration.nix ];

  networking.hostName = "kestrel-utm";

  boot.loader.systemd-boot.enable = true;
  boot.loader.efi.canTouchEfiVariables = true;

  # UTM/QEMU guest niceties.
  services.qemuGuest.enable = true;
  services.spice-vdagentd.enable = true;
}
