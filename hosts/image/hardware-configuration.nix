# The TorqOS image's disk and devices (docs/INSTALL.md): its file systems by label, as the image was
# made (NixOS's disk-image module), the root one grown to the whole disk at boot; a virtual
# machine's drivers. First-boot setup copies this into the config repo, attached at the top of the
# tree (config-files.json) as any machine's hardware-configuration.nix.
{ modulesPath, ... }:
{
  imports = [ (modulesPath + "/profiles/qemu-guest.nix") ];

  fileSystems."/" = {
    device = "/dev/disk/by-label/nixos";
    autoResize = true;
    fsType = "ext4";
  };
  fileSystems."/boot" = {
    device = "/dev/disk/by-label/ESP";
    fsType = "vfat";
  };
  boot.growPartition = true;
}
