# Throwaway VM for fast iteration:
#   nix build .#nixosConfigurations.dev-vm-aarch64.config.system.build.vm
#   ./result/bin/run-kestrel-dev-vm
# Login: dev / kestrel
{ lib, ... }:
{
  networking.hostName = "kestrel-dev";
  kestrel.user = "dev";
  users.users.dev.initialPassword = "kestrel";

  services.openssh.enable = true;

  # Needed for the non-VM toplevel to evaluate; the VM variant overrides these.
  fileSystems."/" = { device = "/dev/disk/by-label/nixos"; fsType = "ext4"; };
  boot.loader.systemd-boot.enable = true;

  virtualisation.vmVariant.virtualisation = {
    memorySize = 4096;
    cores = 4;
    graphics = true;
  };
}
