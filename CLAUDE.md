# CLAUDE.md — Kestrel

> "Kestrel" is a placeholder codename. Rename freely (search the repo for `kestrel`).

## What this project is

Kestrel is a **laptop operating system** built on **NixOS** with three non-negotiable goals:

1. **Ground-up security owned by the user.** Each installed laptop is its own root of trust:
   owner-generated Secure Boot keys (Microsoft keys removed by default), measured boot,
   TPM2 + PIN disk unlock. No shim, no vendor keys, no one else's signatures.
2. **Deep AI integration that is auditable and reversible.** An AI agent can diagnose and
   change almost anything on the system — drivers, firmware, config, performance — but it can
   only *propose config changes*. A small privileged **broker** applies them as signed git
   commits that become bootable NixOS generations. Every action is in a tamper-evident ledger
   and every change has one-click undo.
3. **Omarchy-level taste, refinement and speed.** Opinionated, beautiful, ready out of the box,
   designed for touchpad/touch first (keyboard shortcuts optional), with measured smoothness
   targets.

Read `docs/VISION.md` first, then `docs/ARCHITECTURE.md`.

## The one rule that shapes everything

**The AI never changes the running system directly.** It edits the system *configuration*
(the local git repo at `/var/lib/kestrel/config`), and the broker builds the system from it.
That is what makes auditing and reversal automatic. Do not add code paths that bypass this.

## Repository layout

```
flake.nix              entry point: VMs, ISO, checks, packages
modules/               the distribution, as NixOS modules (kestrel.* options)
  base.nix             nix settings, users, locale, core packages
  security.nix         boot chain, lanzaboote, TPM/LUKS, hardening
  desktop.nix          compositor (niri), greetd, pipewire, portals
  broker.nix           the privileged change broker service + config repo
  ai.nix               local model runtime + agent service (stubbed)
hosts/
  dev-vm/              throwaway VM (`.#nixosConfigurations.dev-vm-{aarch64,x86_64}`)
templates/config/      the per-machine config repo the broker manages
                       (`nix flake new -t .#config /var/lib/kestrel/config`)
broker/                Rust crate: kestrel-broker (ledger, policy, git, rebuild, rollback)
tests/                 NixOS VM tests
docs/                  design docs and decision log
```

Two repos, on purpose:
- **This repo** = the distribution (modules, broker source, docs). Developed normally, on GitHub.
- **`/var/lib/kestrel/config`** on each machine = that machine's config, a small flake that imports
  this repo via `kestrel.lib.mkSystem`. Local only. Written only by the broker.

## Commands

```bash
# Rust broker
cd broker && cargo test          # unit tests, incl. ledger tamper detection
cd broker && cargo run -- --help

# Nix (run on Linux or inside the NixOS VM; on macOS needs a linux-builder)
nix flake check                                   # eval + VM tests
nix build .#nixosConfigurations.dev-vm-aarch64.config.system.build.vm
./result/bin/run-kestrel-dev-vm                   # boot the throwaway VM (dev / kestrel)
nix build .#checks.aarch64-linux.trust-loop       # run the trust-loop VM test

# Inside the UTM VM, against the machine's config repo
sudo nixos-rebuild switch --flake /var/lib/kestrel/config#kestrel-utm   # first time only
sudo kestrel-broker propose --diff change.diff --reason "..." --actor user
sudo kestrel-broker apply <seq> --approved-by $USER
sudo kestrel-broker rollback <applied-seq> --approved-by $USER
sudo kestrel-broker log && sudo kestrel-broker verify
```

## Current milestone (M2) — "the agent sees" (M1, the trust loop, is done)

A local model is reachable (llama.cpp on the Mac host during dev); `kestrel-agent` runs with
read-only MCP tools (logs, hardware, `config.read`); "Doctor" mode diagnoses and explains but
proposes nothing. See `docs/ROADMAP.md`.

## Engineering principles

- **Privileged code is Rust**, small, and boring. The broker must stay auditable line by line.
- **Unprivileged UI is QML (Quickshell).** The agent runs unprivileged and sandboxed.
- **Everything declarative.** If it isn't in the config repo, it doesn't exist after a rebuild.
- **Test the trust properties, not just features.** Every security or reversibility claim
  should have a NixOS VM test (`tests/`) that would fail if the claim broke.
- **One reference laptop first.** Polish on one machine beats broad mediocre support.
- **Prefer existing, maintained components** (Lanzaboote, sbctl, systemd-cryptenroll, fwupd,
  niri/Hyprland, Quickshell, llama.cpp, MCP). Original work goes into the broker, ledger,
  agent tools, Activity UI, installer and the "Secure this laptop" flow.

## Guardrails for Claude Code working in this repo

- Never weaken a security default to make something work. Add an explicit, documented option
  instead, defaulting to the secure value.
- Never give the agent a code path to: modify its own permissions, disable/edit the ledger,
  touch Secure Boot keys, or run arbitrary root commands. Those are user-only, via direct UI.
- Record significant design choices in `docs/DECISIONS.md` (short ADR entries).
- Keep `docs/` in sync when behaviour changes.
- Things in this scaffold marked `TODO(M1)` / `TODO(M2)` are the intended next steps.

## Current status (read this first) — 2026-10-30

- **M1 is complete** and running on the UTM VM: propose → apply → rebuild → ledger → one-command
  rollback; broken changes reset *and rebuilt* (ADR-008); a broker killed mid-change is reconciled
  on the next operation (ADR-010); `kestrel-broker status` reports any drift from the ledger.
  `tests/trust-loop.nix` passes inside the UTM VM (no `/dev/kvm`: QEMU falls back to TCG, ~2.5 min).
  Broker: 40 tests green (`cargo test`, also run in the Nix build sandbox).
- **Package system works** (ADR-011): `kestrel pkg add|remove|list|search` writes
  `users/<name>.packages.json` (user tier) or `packages.json` (`--system`, system tier) and goes
  through the broker. Verified live (`ripgrep` for viper, ledger #23).
- **A stray `nixos-rebuild` cannot switch the machine away** from its ledgered config:
  `/etc/nixos` holds stubs that refuse (`kestrel.broker.guardEtcNixos`). This happened once from
  another tool on this VM; never build `/etc/nixos` or any config other than the broker's.
- **Way of working:** small steps; an independent reviewer agent behind every meaningful change;
  mutation-check new guards (break them, see a test fail); verify in the Nix sandbox and VM test.
- Next: M2 (`docs/ROADMAP.md`). Open issues and VM quirks: `docs/KNOWN-GAPS.md`.

### You are probably running inside the dev VM
- Host `kestrel-utm`, user **`viper`** (the template no longer names a user; `kestrel.user` in
  the config repo's `system.nix` is the one place). `kestrel.dev.enable = true` there: passwordless sudo, you have root (ADR-009).
- `~/kestrel` = this repo (working copy; push to `github.com/netstacks/kestrel`, branch `main`).
- `/var/lib/kestrel/config` = the machine's config repo; its `kestrel` input is
  `path:/home/viper/kestrel`.
- **Ship a distribution change to the running VM through the broker**, so it is ledgered:
  ```bash
  rm -rf /tmp/cfg && cp -r /var/lib/kestrel/config /tmp/cfg && cd /tmp/cfg
  nix flake update kestrel && git diff > /tmp/bump.diff     # + edit other files if needed
  sudo kestrel-broker propose --diff /tmp/bump.diff --reason "..." --actor user
  sudo kestrel-broker apply <seq> --approved-by viper
  ```
- Dev mode is a temporary exception for *building* Kestrel. Do not design the Kestrel agent around
  having root; the agent itself stays unprivileged and broker-only.
