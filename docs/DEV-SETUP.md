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

> Prerequisite: `broker/Cargo.lock` must be committed in the Kestrel repo (see CLAUDE.md →
> "First tasks"), otherwise the broker package — and so the system — won't build.

```bash
# a) Enable flakes + SSH once, the classic way:
sudo nano /etc/nixos/configuration.nix
#   nix.settings.experimental-features = [ "nix-command" "flakes" ];
#   services.openssh.enable = true;
#   environment.systemPackages = [ pkgs.git ];
sudo nixos-rebuild switch

# b) Get the Kestrel repo
git clone <your GitHub repo URL> ~/kestrel

# c) Create this machine's config repo from the template
sudo nix flake new -t ~/kestrel#config /var/lib/kestrel/config
sudo cp /etc/nixos/hardware-configuration.nix /var/lib/kestrel/config/
sudo sed -i "s/CHANGE-ME/$USER/" /var/lib/kestrel/config/system.nix          # the one place a user name is set
# SSH on Kestrel is keys only (services.json): from the Mac, before the first switch:
#   ssh-copy-id <user>@<vm-address>
sudo sed -i "s|github:netstacks/kestrel|path:$HOME/kestrel|" /var/lib/kestrel/config/flake.nix  # dev: your checkout

# d) First switch into Kestrel (explicit host name the first time)
sudo nixos-rebuild switch --flake /var/lib/kestrel/config#kestrel-utm
sudo reboot
```

After reboot, `kestrel-broker-init` has turned `/var/lib/kestrel/config` into a git repo with a
ledger. From now on, **all** changes go through the broker:

```bash
# make a change in a scratch copy and produce a diff:
sudo cp -r /var/lib/kestrel/config /tmp/cfg && sudo chown -R $USER /tmp/cfg
sed -i 's/kestrel.desktop.enable = true;/kestrel.desktop.enable = true;\n  services.printing.enable = true;/' /tmp/cfg/desktop.nix
(cd /tmp/cfg && git diff > /tmp/printing.diff)
sudo kestrel-broker propose --diff /tmp/printing.diff --reason "Enable printing" --actor user
sudo kestrel-broker apply <seq> --approved-by $USER      # commits + nixos-rebuild switch
sudo kestrel-broker log
sudo kestrel-broker rollback <applied-seq> --approved-by $USER
sudo kestrel-broker verify
```
(Producing diffs by hand is temporary — the agent and the Activity UI will do this in M2/M3.)

Software has its own command, which writes the diff for you (ADR-011):
```bash
kestrel pkg search ripgrep
kestrel pkg add ripgrep            # for you (user tier); --system for everyone (system tier)
kestrel pkg remove ripgrep
kestrel pkg list
```
History and undo, without the broker's sequence-number plumbing:
```bash
kestrel log                        # what changed, and `kestrel undo N` for each change
kestrel undo                       # the latest change (asks; system changes always ask)
kestrel undo 23                    # a specific one (the applied number, or its proposal number)
kestrel status                     # is the machine as its ledger says?
```
Services come from a small catalog, each with Kestrel's defaults (e.g. SSH: keys only, no root
login). Always system changes, so always confirmed:
```bash
kestrel service list               # what there is, and what is on
kestrel service enable podman      # rootless containers, with a `docker` command
kestrel service disable podman
```
To move the machine to the newest Kestrel (and so nixpkgs) — one ledgered, undoable `flake.lock`
change: `kestrel update`. `sudo kestrel-broker status` says whether the machine is as its ledger
says; it is also checked at boot and every 15 minutes, and a new shell warns if not.
It checks the change against the whole system first, then proposes and applies it through the
broker, so it is ledgered and `kestrel-broker rollback` undoes it.

Do not run `nixos-rebuild` against `/etc/nixos`: on Kestrel those files are stubs that refuse to
build (`kestrel.broker.guardEtcNixos`), because building them replaces the whole system outside
the ledger.

### 3. Editing from macOS
VS Code Remote-SSH (or run Claude Code inside the VM). Develop the distribution in `~/kestrel`;
the config repo picks up changes when its `kestrel` input is updated
(`nix flake update kestrel` in a scratch copy → diff → broker).

### 4. Local model on the Mac host (M2+)
Run llama.cpp (`llama-server`) or Ollama on macOS. From the VM, the Mac is usually the default
gateway (`ip route | grep default`). Set `kestrel.ai.modelEndpoint` accordingly.

## Throwaway VM (no install needed)
```bash
nix build .#nixosConfigurations.dev-vm-aarch64.config.system.build.vm
./result/bin/run-kestrel-dev-vm      # login: dev / kestrel
```
On macOS this needs a Linux builder (nix-darwin `nix.linux-builder.enable = true`); otherwise run
it inside the UTM VM or on a Linux server.

## Optional: x86_64 server
- Dedicated x86 box (Ryzen, 64 GB+), NixOS via nixos-anywhere.
- x86 builds, Secure Boot/TPM emulation tests (OVMF + swtpm), CI.
- Binary cache (Cachix or attic) so the Mac/VM downloads instead of building.
