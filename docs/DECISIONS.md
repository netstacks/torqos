# Decision log

Short ADRs. Newest at the bottom. Format: context → decision → consequences.

## ADR-001 — Build on NixOS, not Arch/Omarchy
- **Context:** Goals are auditable + reversible AI changes and ground-up security. Omarchy 4 is
  agent-first but Arch-based; recovery from bad agent edits is "reinstall configs".
- **Decision:** NixOS. The whole system is a declarative config; every change is a generation.
- **Consequences:** Reversibility by construction; steeper learning curve hidden behind UI/AI;
  non-FHS binaries need nix-ld/Flatpak/Distrobox. User count is not a goal — quality is.

## ADR-002 — Owner-only Secure Boot keys, no shim, Microsoft removed by default
- **Context:** Users must be able to secure the machine even from Microsoft; we don't want to be
  in the shim-signing business.
- **Decision:** Per-machine PK/KEK/db generated at install; option-ROM hashes from TPM event log;
  Microsoft keys optional (off). Lanzaboote signs UKIs locally.
- **Consequences:** One manual step (Setup Mode) at install; no Windows dual-boot by default;
  firmware updates must re-enroll option-ROM hashes; prefer iGPU hardware.

## ADR-003 — The AI only proposes config changes; a Rust broker applies them
- **Decision:** Agent is unprivileged. Broker is the sole writer of the config repo.
- **Consequences:** Every change is a commit + generation + ledger entry. No raw root shell for AI.

## ADR-004 — Local config git repo, never pushed by default
- **Decision:** `/var/lib/kestrel/config` is a local repo. Optional user-chosen encrypted backup.
- **Consequences:** Network is download-only (packages, updates). Updates are `flake.lock` commits.

## ADR-005 — Local models by default
- **Decision:** llama.cpp locally; cloud agents opt-in, indicated, ledgered.

## ADR-006 — Reference hardware
- **Decision:** Dev on M4 Mac (ARM VM) → old ThinkPad (iGPU, TPM2) → 16" Strix Halo OLED.
  Apple Silicon only as a secondary dev profile (no owner-key boot chain there).

## ADR-007 — Privileged code in Rust, UI in QML
- **Decision:** Broker, ledger, tools in Rust. Shell in Quickshell (QML), unprivileged.

## ADR-008 — A failed rebuild is followed by a restore rebuild
- **Context:** `nixos-rebuild switch` can fail *after* activating the new system
  (switch-to-configuration exits 4 when a unit fails). Found in the UTM VM: the broker reset the
  repo but the running system kept the change, so system and config diverged.
- **Decision:** On a failed apply or rollback, the broker resets the repo to the previous commit
  *and rebuilds it*, then ledgers `apply_failed` / `rollback_failed` with `restored: true|false`.
  `kestrel-broker-init` runs only once (`ConditionPathExists=!…/.git`), never during a switch.
- **Consequences:** A failed change costs a second (usually cached) switch. If the restore also
  fails, the ledger says so explicitly rather than claiming the system is unchanged.

## ADR-009 — Development mode: Claude Code with full access, behind one switch
- **Context:** While Kestrel is being built, the fastest loop is Claude Code running *inside*
  the dev VM with root — editing `~/kestrel`, building with Nix, applying via the broker.
  Security during development is explicitly not a goal; being able to shut it off later is.
- **Decision:** `kestrel.dev.enable` (default **false**, `modules/dev.nix`): passwordless sudo
  for wheel, `claude-code` + `gh`, owner as trusted Nix user. Enabled only in the dev VM's
  config repo. This is a cloud agent with root, so it is a deliberate, temporary exception to
  ADR-003 (agent only proposes) and ADR-005 (local models by default) — for *building* Kestrel,
  not for the Kestrel agent itself.
- **Consequences:** Every rebuild prints a warning while it is on. Shutting AI access off is
  a single broker commit (`kestrel.dev.enable = false`), itself ledgered and undoable. The
  shipped product and all tests run with it off.
