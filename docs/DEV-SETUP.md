# Development setup

## On the M4 Mac (primary)

### 1. UTM VM
1. UTM → Create → **Virtualize** → **Linux**. Leave "Use Apple Virtualization" **off** (QEMU).
2. Boot ISO: `latest-nixos-graphical-aarch64-linux.iso` (verify SHA-256).
3. 8 GB RAM (16 if available), 4+ cores, 64 GB disk. Hardware OpenGL **off** for install.
4. Install with the graphical installer. Any (lowercase) user name works; setup below uses `$USER`.
   Desktop choice doesn't matter — it gets replaced.
5. Shut down, remove the ISO from the CD drive, boot the installed system.

### 2. Bootstrap inside the VM

> Prerequisite: `broker/Cargo.lock` must be committed in the TorqOS repo (see CLAUDE.md →
> "First tasks"), otherwise the broker package — and so the system — won't build.

```bash
# a) Enable flakes + SSH once, the classic way:
sudo nano /etc/nixos/configuration.nix
#   nix.settings.experimental-features = [ "nix-command" "flakes" ];
#   services.openssh.enable = true;
#   environment.systemPackages = [ pkgs.git ];
sudo nixos-rebuild switch

# b) Get the TorqOS repo
git clone <your GitHub repo URL> ~/torq

# c) Create this machine's config repo from the template
sudo nix flake new -t ~/torq#config /var/lib/torq/config
sudo cp /etc/nixos/hardware-configuration.nix /var/lib/torq/config/
sudo sed -i "s/CHANGE-ME/$USER/" /var/lib/torq/config/system.nix          # the one place a user name is set
sudo sed -i "s|github:netstacks/torqos|path:$HOME/torq|" /var/lib/torq/config/flake.nix  # dev: your checkout

# d) First switch into TorqOS (explicit host name the first time)
sudo nixos-rebuild switch --flake /var/lib/torq/config#torq-utm
sudo reboot
```

After reboot, `torq-broker-init` has turned `/var/lib/torq/config` into a git repo with a
ledger. From now on, **all** changes go through the broker:

```bash
# make a change in a scratch copy and produce a diff:
sudo cp -r /var/lib/torq/config /tmp/cfg && sudo chown -R $USER /tmp/cfg
sed -i 's/torq.desktop.enable = true;/torq.desktop.enable = true;\n  services.printing.enable = true;/' /tmp/cfg/desktop.nix
(cd /tmp/cfg && git diff > /tmp/printing.diff)
sudo torq-broker propose --diff /tmp/printing.diff --reason "Enable printing" --actor user
sudo torq-broker apply <seq> --approved-by $USER      # commits + nixos-rebuild switch
sudo torq-broker log
sudo torq-broker rollback <applied-seq> --approved-by $USER
sudo torq-broker verify
```
(Producing diffs by hand is temporary — the agent and the Activity UI will do this in M2/M3.)

Software has its own command, which writes the diff for you (ADR-011):
```bash
torq pkg search ripgrep
torq pkg add ripgrep            # for you (user tier); --system for everyone (system tier)
torq pkg remove ripgrep
torq pkg list
```
History and undo, without the broker's sequence-number plumbing:
```bash
torq log                        # what changed, and `torq undo N` for each change
torq undo                       # the latest change (asks; system changes always ask)
torq undo 23                    # a specific one (the applied number, or its proposal number)
torq status                     # is the machine as its ledger says?
```
Services come from a small catalog, each with TorqOS's defaults (e.g. SSH: keys only, no root
login). Always system changes, so always confirmed:
```bash
torq service list               # what there is, and what is on
torq service enable podman      # rootless containers, with a `docker` command
torq service disable podman
```
SSH is off on a new machine (it would only be listening: nobody has a key yet). To SSH in from the
Mac, put your public key in `~/.ssh/authorized_keys` in the VM (paste it in the UTM console, or
`ssh-copy-id` if you enabled SSH in the installer), then `torq service enable ssh`: it shows
exactly which keys will be able to log in.
To move the machine to the newest TorqOS (and so nixpkgs) — one ledgered, undoable `flake.lock`
change: `torq update`. `sudo torq-broker status` says whether the machine is as its ledger
says; it is also checked at boot and every 15 minutes, and a new shell warns if not.
It checks the change against the whole system first, then proposes and applies it through the
broker, so it is ledgered and `torq-broker rollback` undoes it.

Do not run `nixos-rebuild` against `/etc/nixos`: on TorqOS those files are stubs that refuse to
build (`torq.broker.guardEtcNixos`), because building them replaces the whole system outside
the ledger.

### 3. Editing from macOS
VS Code Remote-SSH (or run Claude Code inside the VM). Develop the distribution in `~/torq`;
the config repo picks up changes when its `torq` input is updated
(`nix flake update torq` in a scratch copy → diff → broker).

### 4. Local model on the Mac host (M2+)
Run llama.cpp (`llama-server`) or Ollama on macOS. From the VM, the Mac is usually the default
gateway (`ip route | grep default`). Set `torq.ai.modelEndpoint` accordingly.

## Throwaway VM (no install needed)
```bash
nix build .#nixosConfigurations.dev-vm-aarch64.config.system.build.vm
./result/bin/run-torq-dev-vm      # login: dev / torq
```
On macOS this needs a Linux builder (nix-darwin `nix.linux-builder.enable = true`); otherwise run
it inside the UTM VM or on a Linux server.

## Optional: x86_64 server
- Dedicated x86 box (Ryzen, 64 GB+), NixOS via nixos-anywhere.
- x86 builds, Secure Boot/TPM emulation tests (OVMF + swtpm), CI.
- Binary cache (Cachix or attic) so the Mac/VM downloads instead of building.
