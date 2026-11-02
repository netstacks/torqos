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
   - how to log in remotely: not at all (the default: Enter turns nothing on), by password, by SSH
     key (pasted), or both.
5. **Setup writes the machine's configuration** — the config repo at `/var/lib/torq/config`, from the
   template, with those answers as tree files (`system/host.json`: name and time zone;
   `nixos/torq.json`: the owner; for remote login, `nixos/services.json` with NixOS's
   `services.openssh` settings, and a pasted key in `nixos/users.json` as the owner's
   `openssh.authorizedKeys.keys`, since a public key is not a secret) — and the owner's password to
   a file only the system can read (`/var/lib/torq/secrets/passwords/<user>`, never in the repo). The broker then takes the
   repo over: it builds it first (a configuration that does not build changes nothing, and setup
   asks again), switches to it, and records change #1, "TorqOS set up", with the system it switched
   to. The broker runs as a service of its own, so the switch (which brings back the console's
   login prompt) cannot cut it short. A machine is set up once its history has change #1: a setup
   cut short before its switch, by a power cut mid-build say, runs again at the next boot and
   starts clean; the image never takes over a repo by itself. Cut short after its switch, the
   machine starts its new system with no history yet: setup kept a record of what it was doing
   (`/var/lib/torq/state/setup.json`, written before the repo had a history), and the new
   system's first start (`torq-broker-init`) finishes it — change #1 for setup's own commit and
   the system running, said so in the entry; a repo holding anything more is not taken over.
   Setup never runs again, and runs nowhere but the image.
6. **The console shows a login prompt.** From here it is TorqOS as everywhere: `torqcli`, every
   change through the broker.

## What is in the image

Only what a TorqOS machine needs: Linux and systemd; Nix, git and nixpkgs's source, with which the
machine builds itself from its configuration (ADR-019); TorqOS (torqcli, the broker, the shell); and
a few tools (curl, htop, jq, nano). No NetworkManager (systemd-networkd, DHCP on wired interfaces),
no manuals but man pages. About 1.5 GB installed; the download is compressed (about 730 MB).

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
  file and the ledger records that it changed, when and by whom — never the hash. Only the people
  the configuration declares (`torq.accounts.people`, in the broker's fixed settings): never root's
  or a system account's.
- A machine from before this (passwords set with `passwd`, in `/etc/shadow` only) keeps them: on its
  first switch to it, each person's password is carried into their file before the accounts are
  written. Only for someone with no file yet and a real password (a locked account stays locked);
  a file there is never replaced. Once only: from then on the files are what counts, and removing
  someone's file removes their password. (`tests/accounts.nix`.)

## SSH

- Setup's choice becomes NixOS's own SSH (`services openssh enable true`; ADR-025): keys only, or
  passwords allowed too (`services openssh settings PasswordAuthentication true`). TorqOS's
  protections apply however SSH is turned on (modules/services.nix): with passwords, repeated wrong
  guesses are blocked (sshguard); 3 tries a connection; only the owner may log in (`AllowUsers`);
  root never logs in remotely. `show system alarms` notes when SSH accepts passwords; `show
  services` says how SSH is protected.
- A password is never in the configuration: setting one there (`users users <name>
  hashedPassword`, …) is refused; `request system password` is the way.

## Steps

Done (2026-11-01; tried end to end in QEMU: the image boots to setup, setup switches, the owner
logs in, change #1 is in the history and `show system alarms` is clean):

1. The image: a qcow2 of the factory system that boots in UTM/QEMU (aarch64 first).
2. Setup on the console (`torqcli setup`, as root, on the image only, until change #1).
3. Passwords outside the repo, and `request system password`; existing passwords carried over.
4. SSH by password, protected, and noted by `show system alarms`.
5. The image's own TorqOS as the config repo's source (`path:` in the store, locked: exactly
   `/nix/store/<hash>-<name>`, nothing after it).

Next:

6. A VM test: the image boots, setup, a commit, a rollback.
7. The release: checksum and signature (`ssh-keygen -Y`), and how to check them.
8. Then the ISO for real hardware (disk choice added).

Known: setup runs on the first text console (tty1) only. A VM started with a serial console alone
shows login prompts there, with no account to log in to: give it a display (UTM does by default).
