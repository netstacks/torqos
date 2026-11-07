# The TorqOS image's disk and devices (docs/INSTALL.md): one btrfs file system by label, as
# hosts/image/disk.nix makes it, its last partition, the root, grown to the whole disk at boot; a virtual machine's drivers.
# First-boot setup copies this into the config repo, attached at the top of the tree
# (config-files.json) as any machine's hardware-configuration.nix: it stands alone, importing
# nothing beside it. `/home` and `/var/lib` are snapshotted (ADR-038); `/var/lib/torq`, the ledger, the
# configuration and the password hashes, is a subvolume of its own, so no snapshot of `/var/lib` holds
# it: restoring data must never rewind the record.
{ lib, pkgs, modulesPath, ... }:
let
  subvolume = name: {
    device = "/dev/disk/by-label/nixos";
    fsType = "btrfs";
    options = [ "subvol=${name}" ];
  };
in
{
  imports = [ (modulesPath + "/profiles/qemu-guest.nix") ];

  fileSystems."/" = subvolume "@" // { autoResize = true; };
  # Activation reads and writes them: new accounts' homes, the users' database, the password hashes.
  fileSystems."/home" = subvolume "@home" // { neededForBoot = true; };
  fileSystems."/var/lib" = subvolume "@var-lib" // { neededForBoot = true; };
  # A file system of its own, root's alone: the record has room whoever fills the rest (the audit of M3,
  # M2), and no snapshot of /var/lib holds it.
  fileSystems."/var/lib/torq" = { device = "/dev/disk/by-label/torq"; fsType = "ext4"; neededForBoot = true; };
  fileSystems."/.snapshots" = subvolume "@snapshots";
  fileSystems."/boot" = {
    device = "/dev/disk/by-label/ESP";
    fsType = "vfat";
  };
  # The root grown to the disk at boot, by systemd-repart in the initrd, then btrfs by the file system's
  # autoResize (x-systemd.growfs, after repart). Not growpart (boot.growPartition): cloud-utils needs Python
  # (ADR-032, amended). repart matches its definitions, in name order, to the disk's partitions of the
  # same type, in order: the ESP has none and is left as it is; both others are Linux's generic type, so
  # the record's (the 1 GiB ext4 `torq`, partition 2) must come first, with no share of free space
  # (Weight 0), or the root's definition would match it. Only the last partition, the root, grows; repart
  # never moves, shrinks or deletes a partition, nor writes into one it does not create (the labels are
  # the ones hosts/image/disk.nix gives, so it leaves the record's table entry as it is).
  boot.initrd.systemd.repart.enable = true;
  systemd.repart.partitions = {
    "10-torq" = { Type = "linux-generic"; Label = "torq"; Weight = 0; };
    "20-root" = { Type = "linux-generic"; Label = "nixos"; };
  };
  # VMware ESXi's disk controllers (PVSCSI, LSI Logic SAS and parallel), on x86_64, where ESXi
  # runs: its default for a Linux guest is one of these, which the profile above (virtio, for QEMU)
  # does not cover. SATA and NVMe are covered by NixOS's defaults.
  boot.initrd.availableKernelModules = lib.optionals pkgs.stdenv.hostPlatform.isx86_64 [ "vmw_pvscsi" "mptsas" "mptspi" ];
}
