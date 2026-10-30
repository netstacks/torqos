# Development setup

## On the M4 Mac (primary)

### 1. UTM VM
1. UTM → Create → **Virtualize** → **Linux**. Leave "Use Apple Virtualization" **off** (QEMU).
2. Boot ISO: `latest-nixos-graphical-aarch64-linux.iso` (verify SHA-256).
3. 8 GB RAM (16 if available), 4+ cores, 64 GB disk. Hardware OpenGL **off** for install.
4. Install with the graphical installer. Create user **casey** (matches the template).
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
#   If your username isn't casey: edit system.nix, users/, flake.nix (kestrel.url path).

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
sudo kestrel-broker apply <seq> --approved-by casey      # commits + nixos-rebuild switch
sudo kestrel-broker log
sudo kestrel-broker rollback <applied-seq> --approved-by casey
sudo kestrel-broker verify
```
(Producing diffs by hand is temporary — the agent and the Activity UI will do this in M2/M3.)

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
