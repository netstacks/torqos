# Security model

## Principles

1. **Each laptop is its own root of trust.** Keys are generated on the machine at install.
2. **No vendor signatures required.** No shim, no Microsoft keys (default), no project-held
   signing key for boot. The project only signs its package repository/binary cache.
3. **Tamper → locked disk.** If the boot chain changes, the TPM refuses to release the disk key.
4. **The AI can never touch the trust layer.** Keys, the ledger, and AI permissions are
   user-only, changed through a UI the user operates directly.

## Threat model

| Threat | Primary defence | Notes |
|---|---|---|
| Stolen laptop | LUKS2 full-disk encryption | TPM2 + PIN, recovery key shown once at install |
| Evil maid (boot files tampered while away) | Owner-key Secure Boot + measured boot | Disk won't unseal if PCRs change |
| Firmware keys reset to factory | Firmware admin password; PCR 7 change → disk stays locked | Status panel warns loudly |
| Malware during a session | Wayland, Flatpak+portals, read-only /nix/store, sandboxed services | |
| Malware persistence across boots | Signed UKIs only; config rebuilt from repo | Optional impermanence later |
| Prompt injection steering the AI | Broker tiers + taint tracking | Untrusted-content-derived actions need per-step approval |
| Compromised signing key on device | Only the db key lives on device (broker-only, TPM-sealed); PK/KEK exported offline then deleted | |
| Secure Boot bypass vulns (BlackLotus, LogoFAIL class) | fwupd firmware + dbx updates, applied via broker | |

## Secure Boot: owner keys, no Microsoft

Key hierarchy in firmware: **PK** (owner) → **KEK** → **db** (allowed) / **dbx** (revoked).

Install flow ("Secure this laptop"):

1. User puts firmware in **Setup Mode** (installer shows per-model instructions).
2. Generate PK/KEK/db on the machine (`sbctl create-keys`, wrapped by our installer).
3. Enroll keys **plus option-ROM hashes from the TPM event log**
   (`sbctl enroll-keys --tpm-eventlog`) — keeps GPU/TB/NVMe option ROMs working
   without trusting Microsoft.
4. Toggle (default OFF): "Keep Microsoft keys (needed to dual-boot Windows)" → `--microsoft`.
5. Prompt user to set a **firmware admin password**.
6. Export PK + KEK to USB / printed recovery sheet; delete from disk.
7. Reboot with Secure Boot on; Lanzaboote signs every generation's UKI with the db key.
8. Re-enroll LUKS to TPM2 + PIN bound to the measured boot state.

Before any firmware update the broker must check whether option-ROM hashes will change and
re-enroll first (otherwise: black screen risk).

Hardware guidance: integrated-graphics machines are lowest risk for removing Microsoft keys.

## Disk encryption

- LUKS2, `systemd-cryptenroll --tpm2-device=auto --tpm2-with-pin=yes`, PCR policy managed
  via Lanzaboote's measured boot / systemd-pcrlock support.
- Recovery key generated at install, shown once.

## Hardening defaults (initial list, expand with care)

- Kernel lockdown (integrity), signed modules only.
- Wayland only, no X11 server.
- Minimal enabled services; all Kestrel services use systemd sandboxing.
- `nix.settings.allowed-users` restricted; only the broker may trigger system rebuilds.

## Apple Silicon note

Supported only as a secondary dev profile via nixos-apple-silicon (Asahi). No TPM, no owner-key
UEFI Secure Boot → status panel must report "verified boot: limited on this hardware".
