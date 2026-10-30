# Roadmap

## Stage 1 — Virtual machines (M4 Mac + optional x86 server)

### M1 — The trust loop  ✓ done
In an ARM NixOS VM (UTM on the M4 Mac):

- [x] `broker/Cargo.lock` generated; `cargo test` green.
- [x] UTM VM switches to `/var/lib/kestrel/config#kestrel-utm` (from `templates/config`).
- [x] `kestrel-broker-init` turns the config dir into a git repo + ledger on boot.
- [x] `kestrel-broker propose` accepts a diff + reason, shows it.
- [x] `kestrel-broker apply` commits (with `Ledger-Entry` trailer), rebuilds, appends ledger.
- [x] `kestrel-broker rollback` reverts, rebuilds, appends ledger.
- [x] `kestrel-broker verify` detects any edit to the ledger.
- [x] NixOS VM test `tests/trust-loop.nix` proves propose → apply → rollback.
  (Runs inside the UTM VM without `/dev/kvm`: QEMU falls back to TCG; ~75 s of test time.)

### M1.5 — CLI polish: the everyday commands feel finished  ← current
The commands work (`kestrel pkg`, `service`, `update`, `log`, `undo`, `status`), but they print
the broker's plumbing. Target and acceptance criteria: `docs/CLI-DESIGN.md`.
- [x] Broker `--json` for every command (shapes and exit codes in `CLI-DESIGN.md`).
- [x] The CLI uses it and stops parsing broker text (ADR-014).
- [x] Human output for every command: changes shown as packages/services with versions and
      descriptions, not diffs; one result line with the change number and how to undo it.
- [x] Quiet progress: phase + elapsed time; full log to `~/.local/state/kestrel/last.log`, shown
      on failure.
- [ ] Errors say what, why and what to do; no `Caused by:`, git hints, hashes or store paths by
      default — done; still to add: CLI `--verbose` and `--json` (only `--diff` exists).
- [x] Tier visible in every prompt: system changes marked and defaulting to No.
- [x] Colour with meaning; plain text for `NO_COLOR` and non-terminals; fits 80 columns.
- [ ] Snapshot tests of every command's output — done for log, status, rows and errors; still to
      do: whole-command snapshots of pkg/service/update/undo (success, refusal, failure).
- [x] New, small: "did you mean" for misspelled packages; `kestrel status --repair` to return a
      drifted machine to its ledgered system (asks first; ADR-012 still: never automatic).
- [ ] Terminal look to match (prompt, palette, font) — with the desktop work, soon after.
- [ ] Usability check: someone new to Kestrel installs, removes, undoes and reads the log unaided.

### M2 — The agent sees
- Local model reachable (llama.cpp on the Mac host during dev).
- `kestrel-agent` with read-only MCP tools (logs, hardware, config.read).
- "Doctor" mode: diagnose and explain, propose nothing.

### M3 — The agent acts
- `config.propose` tool → broker; tiers enforced; taint tracking.
- Quickshell Activity timeline with diff view and undo.
- Signed commits (broker key), ledger anchoring.

### M4 — Secure boot in emulation (x86 server)
- QEMU + OVMF (Secure Boot, Setup Mode) + swtpm.
- "Secure this laptop" flow end to end; tamper test → disk refuses to unlock.

## Stage 2 — Old ThinkPad (T480/T490/X1C6–7/T14, iGPU only, TPM 2.0)
- Real firmware owner-key enrollment, option-ROM hashes, firmware password.
- Suspend/resume, battery, gestures, installer ISO from USB.

## Stage 3 — Reference laptop (16" Strix Halo, OLED, 128 GB)
- Smoothness targets, real local models, polish.
- Windows app compatibility layer.
