# Roadmap

## Stage 1 — Virtual machines (M4 Mac + optional x86 server)

### M1 — The trust loop  ← current
In an ARM NixOS VM (UTM on the M4 Mac):

- [ ] `broker/Cargo.lock` generated; `cargo test` green.
- [ ] UTM VM switches to `/var/lib/kestrel/config#kestrel-utm` (from `templates/config`).
- [ ] `kestrel-broker-init` turns the config dir into a git repo + ledger on boot.
- [ ] `kestrel-broker propose` accepts a diff + reason, shows it.
- [ ] `kestrel-broker apply` commits (with `Ledger-Entry` trailer), rebuilds, appends ledger.
- [ ] `kestrel-broker rollback` reverts, rebuilds, appends ledger.
- [ ] `kestrel-broker verify` detects any edit to the ledger.
- [ ] NixOS VM test `tests/trust-loop.nix` proves propose → apply → rollback.

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
