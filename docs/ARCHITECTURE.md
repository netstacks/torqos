# Architecture

Six layers, bottom up. Each lists what we **reuse** and what we **build**.

```
┌──────────────────────────────────────────────────────────────┐
│ 5  AI layer        agent (unprivileged) ─► broker (privileged)│
│                    MCP tools · ledger · Activity timeline     │
├──────────────────────────────────────────────────────────────┤
│ 4  Desktop         niri/Hyprland · Quickshell shell · greetd  │
├──────────────────────────────────────────────────────────────┤
│ 3  Speed           sched_ext · zram · power profiles · Mesa   │
├──────────────────────────────────────────────────────────────┤
│ 2  Isolation       Wayland · Flatpak+portals · Podman · bwrap │
├──────────────────────────────────────────────────────────────┤
│ 1  Base            NixOS flake · local config git repo ·      │
│                    generations · btrfs snapshots for /home    │
├──────────────────────────────────────────────────────────────┤
│ 0  Trust           owner Secure Boot keys · Lanzaboote UKIs · │
│                    measured boot · TPM2+PIN LUKS2 · fwupd     │
└──────────────────────────────────────────────────────────────┘
```

## Layer 0 — Trust (see `SECURITY-MODEL.md`)

- **Reuse:** Lanzaboote (Secure Boot + measured boot for NixOS), sbctl (key enrollment,
  incl. option-ROM hashes from the TPM event log), systemd-cryptenroll (TPM2+PIN), fwupd/LVFS.
- **Build:** the "Secure this laptop" first-boot flow; security status panel.

## Layer 1 — Base

- The **system config lives in a local git repo** at `/var/lib/torq/config`
  (a flake that imports the TorqOS modules plus per-machine settings).
- Every rebuild = a NixOS generation, listed in the boot menu.
- `/home` on btrfs with snapper snapshots (covers user files, which Nix does not).
- Optional later: impermanence (root wiped each boot; only declared state persists).
- **Reuse:** NixOS, nixpkgs, home-manager, disko, snapper. **Build:** the distribution modules.

## Layer 2 — Isolation

- Wayland only. GUI apps via Flatpak + xdg-desktop-portal (file access by picker = capability).
- Dev environments in rootless Podman / Distrobox.
- Our own services hardened with systemd sandboxing; agent additionally in bubblewrap
  (later: its own microVM, Qubes-style).
- Windows apps: Steam/Proton/Wine; fallback QEMU VM with WinApps-style window integration.

## Layer 3 — Speed and smoothness

Targets (measured, CI-tested on the reference laptop):

| Metric | Target |
|---|---|
| Boot to desktop | < 10 s |
| Resume from suspend | < 1 s |
| Shell animations @120 Hz | 0 dropped frames in standard scenarios |
| Input-to-photon latency | tracked per release, never regresses |

**Reuse:** sched_ext (e.g. scx_lavd), zram, power-profiles-daemon, Mesa/RADV.

## Layer 4 — Desktop

- Compositor: **niri** (scrollable tiling, great with touchpads) — Hyprland as alternative.
- Shell: **Quickshell** (QML): bar, launcher/command bar, AI panel, Activity timeline,
  security status panel. One design system; one-step themes.
- Command bar accepts app names, files, and natural language.
- Keyboard shortcuts are an optional power layer, never required.

## Layer 5 — AI (see `AI-LAYER.md`)

- **Agent daemon** (unprivileged, sandboxed): talks to a local model (llama.cpp), calls tools.
- **MCP tool servers** per subsystem (hardware, logs, packages, display, network, power,
  firmware, scoped files). Read tools are free; write tools only *propose*.
- **Broker** (Rust, privileged, small): the only writer of the config repo; enforces
  permission tiers; commits, rebuilds, rolls back, snapshots; appends to the ledger.
- **Ledger**: append-only, hash-chained JSONL; every prompt/tool call/diff/commit.
- **Activity timeline** (Quickshell): what, why, diff, undo.

## Change flow

```
user / agent ──propose(diff, reason)──► broker
                                         │ 1. classify tier, check taint
                                         │ 2. show preview, wait for approval (if needed)
                                         │ 3. snapshot /home (if user-tier)
                                         │ 4. git commit (signed, Ledger-Entry trailer)
                                         │ 5. nixos-rebuild switch  ──► new generation
                                         │ 6. ledger.append(result)
                                         ▼
                              Activity timeline: [what] [why] [diff] [undo]
undo ──► broker: git revert <commit> → rebuild → ledger.append
broken boot ──► pick previous generation in boot menu
```
