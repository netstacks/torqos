# Install: the TorqOS image and first-boot setup (M2)

Status: **design, being built** (ROADMAP M2; ADR-023; SECURITY-MODEL.md, "Accounts and passwords",
agreed 2026-11-01). Nobody builds TorqOS to install it: a release is an image to download, check and
boot.

## From download to a running machine

1. **The project builds a release**: the image (`torqos-<version>-<arch>.qcow2`, later an ISO), its
   checksum file, and a signature of that file made with the TorqOS release key (`ssh-keygen -Y
   sign`). Plain files on a release page: no service.
2. **Someone downloads and (optionally) checks it**: one command, `ssh-keygen -Y verify`, with the
   published public key.
3. **They boot it.** The image is a complete TorqOS — the same modules as any TorqOS machine — with
   no user account: `root` is locked, SSH is off, nothing listens. The console shows setup instead of
   a login prompt.
4. **Setup asks, once**, like a network device's initial configuration dialog:
   - the machine's name and time zone;
   - the owner's user name and password (twice);
   - how to log in remotely: not at all, by password, by SSH key (pasted), or both.
5. **Setup writes the machine's configuration** — the config repo at `/var/lib/torq/config`, from the
   template, with those answers as tree files — and the owner's password to a file only the system
   can read (`/var/lib/torq/secrets/passwords/<user>`, never in the repo). The broker then takes the
   repo over (`init`: the ledger's first entry, change #1, "TorqOS set up") and switches the machine
   to it. Setup never runs again.
6. **The console shows a login prompt.** From here it is TorqOS as everywhere: `torqcli`, every
   change through the broker.

## Where a machine's TorqOS comes from

The config repo's `flake.nix` names TorqOS's source (`system software torq source`). An image's
machine starts from the TorqOS **inside the image**: its own source, in the read-only store, locked by
hash in the config repo's `flake.lock` — so setup needs no network for TorqOS itself. A `path:`
source is allowed outside dev mode only there, in the store: nothing can change it (a folder anyone
can write to stays a dev-mode source). Updates move the source to a newer release (where releases are
published: decided before any machine outside this VM needs updates; ROADMAP M2).

NixOS's own software (the kernel, services, applications) comes signed from NixOS's binary cache, as
on any NixOS machine; the machine builds TorqOS's few small pieces itself (ADR-023). Setup's switch
needs only what differs from the image (the host name, the user): usually nothing to download.

## Accounts and passwords

- `users.mutableUsers = false`: accounts are the configuration's. Each person's password is a hash in
  `/var/lib/torq/secrets/passwords/<user>` (0600, root), which their account points at
  (`hashedPasswordFile`). No file: no password (an account that logs in by key only, or not at all).
- Changing a password is a TorqOS command (`request system password [<user>]`): the broker writes the
  file and the ledger records that it changed, when and by whom — never the hash.

## SSH

- Setup's choice becomes the catalog's SSH: keys only (as today), or passwords allowed too
  (`torq ssh passwords true`), with guessing slowed down (sshguard), few attempts per connection, and
  root never logging in remotely. `show system alarms` notes when SSH accepts passwords.

## Steps

1. The image: a qcow2 of the factory system that boots in UTM/QEMU (aarch64 first).
2. Setup on the console (`torqcli setup`, as root, only while there is no config repo).
3. Passwords outside the repo, and `request system password`.
4. SSH by password, protected.
5. The image's own TorqOS as the config repo's source (`path:` in the store, locked).
6. A VM test: the image boots, setup, a commit, a rollback.
7. The release: checksum and signature (`ssh-keygen -Y`), and how to check them.
8. Then the ISO for real hardware (disk choice added).
