# The image's disk: NixOS's image builder makes it whole, boot loader and all, on ext4, the only file
# system it partitions; a second step copies that root onto btrfs and keeps its EFI partition as it
# is. btrfs for snapshots of data (ADR-038). The layout is hosts/image/file-systems.nix's.
{ config, lib, pkgs, modulesPath, ... }:

let
  built = import (modulesPath + "/../lib/make-disk-image.nix") {
    inherit lib config pkgs;
    format = "raw";
    partitionTableType = "efi";
    diskSize = "auto";
    additionalSpace = "512M";
    baseName = "built";
  };
  vm = pkgs.vmTools.override {
    rootModules = [ "virtio_pci" "virtio_mmio" "virtio_blk" "virtio_balloon" "virtio_rng" "ext4" "virtiofs" "crc32c" "btrfs" "vfat" "nls_cp437" "nls_iso8859-1" ];
  };
in
{
  system.build.torqDisk = vm.runInLinuxVM (pkgs.runCommand "torqos-disk"
    {
      memSize = 4096;
      nativeBuildInputs = [ pkgs.btrfs-progs pkgs.util-linux pkgs.qemu-utils pkgs.coreutils ];
      preVM = ''
        src=${built}/built.img
        # Room for btrfs's metadata, more than ext4's.
        size=$(( $(stat -c %s "$src") * 5 / 4 ))
        ${pkgs.qemu-utils}/bin/qemu-img create -f raw disk.raw $size
        diskImage=disk.raw
        QEMU_OPTS="$QEMU_OPTS -drive file=$src,if=virtio,format=raw,readonly=on,cache=unsafe"
      '';
      postVM = ''
        mkdir -p $out
        ${pkgs.qemu-utils}/bin/qemu-img convert -f raw -O qcow2 disk.raw $out/torqos.qcow2
      '';
    } ''
    set -euo pipefail
    # The drive named by runInLinuxVM is vda; the built image vdb, read only.
    esp_start=$(cat /sys/class/block/vdb1/start)
    esp_size=$(cat /sys/class/block/vdb1/size)
    printf 'label: gpt\nstart=%s, size=%s, type=U\ntype=L\n' "$esp_start" "$esp_size" | sfdisk --quiet /dev/vda
    udevadm settle 2>/dev/null || true
    partx -u /dev/vda 2>/dev/null || true
    dd if=/dev/vdb1 of=/dev/vda1 bs=1M status=none
    mkfs.btrfs -q -L nixos /dev/vda2
    mkdir -p /top /new /old
    mount -t btrfs /dev/vda2 /top
    for s in @ @home @var-lib @var-lib-torq @snapshots; do btrfs -q subvolume create /top/$s; done
    umount /top
    mount -t btrfs -o subvol=@ /dev/vda2 /new
    mkdir -p /new/home /new/var/lib /new/.snapshots
    mount -t btrfs -o subvol=@home /dev/vda2 /new/home
    mount -t btrfs -o subvol=@var-lib /dev/vda2 /new/var/lib
    mkdir -p /new/var/lib/torq
    mount -t btrfs -o subvol=@var-lib-torq /dev/vda2 /new/var/lib/torq
    mount -t btrfs -o subvol=@snapshots /dev/vda2 /new/.snapshots
    chmod 700 /new/.snapshots
    mount -t ext4 -o ro,noload /dev/vdb2 /old
    cp -a /old/. /new/
    chmod 755 /new/var/lib/torq
    umount /old /new/.snapshots /new/var/lib/torq /new/var/lib /new/home /new
  '');
}
