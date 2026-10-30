# AI layer

## Components

| Component | Privilege | Language | Role |
|---|---|---|---|
| `kestrel-agent` | unprivileged, sandboxed | Rust (or Python early) | Talks to the model, plans, calls MCP tools, *proposes* changes |
| MCP tool servers | unprivileged, read-mostly | Rust | Typed access per subsystem |
| `kestrel-broker` | privileged (root, minimal) | Rust | Only writer of the config repo; tiers, commits, rebuild, rollback, ledger |
| Model runtime | unprivileged | llama.cpp | Local inference (Vulkan/ROCm on Strix Halo) |
| Activity UI | user session | QML | Timeline, approvals, diffs, undo |

## Permission tiers

| Tier | Examples | Requirement |
|---|---|---|
| `observe` | read logs, hardware info, settings, config repo | automatic, still ledgered |
| `user` | one user's *data* files: today `users/<name>.packages.json` (apps); later themes, dotfiles. Never a `.nix` module (that is `system`, wherever it lives) | approve once per category; /home snapshot first |
| `system` | services, drivers, kernel params, packages | always preview + explicit approval; commit + generation |
| `forbidden` | AI permissions, ledger, Secure Boot keys, firmware password, disabling audit | **never** via agent; user-only UI |

## Taint tracking

Any agent session that has ingested untrusted content (web pages, email, downloaded files,
clipboard from unknown sources) is marked **tainted**. Tainted sessions cannot use
"approve once" shortcuts: every `user` or `system` action requires explicit per-step approval
with the taint source displayed.

## Ledger format (v0)

Append-only JSON Lines at `/var/lib/kestrel/state/ledger.jsonl`, owned by the broker.
Entry kinds: `init`, `proposal`, `applied`, `apply_failed`, `rolled_back`, `rollback_failed`
(failures carry `restored: true|false` — see ADR-008).

```json
{"seq":1042,"ts":"2026-10-30T15:20:11Z","actor":"agent","approved_by":"casey",
 "kind":"applied","tier":"user","tainted":false,
 "reason":"User asked for night light at sunset",
 "data":{"proposal":1041,"commit":"8f3a2c1…","files":["desktop.nix"]},
 "prev_hash":"…","hash":"…"}
```

- `hash = sha256(prev_hash || canonical_json(entry_without_hash))`
- Genesis entry has `prev_hash` = 64 zeros.
- `kestrel-broker verify` recomputes the chain and reports the first broken link.
- Git commits carry a `Ledger-Entry: <seq>` trailer; the ledger stores the commit id → each
  checks the other. TODO(M3): `verify` does not cross-check trailers yet, and cannot detect
  truncation of the newest entries until the head hash is anchored.
- Later (M3+): anchor the latest hash periodically (e.g. TPM NV index or an external witness).

## Change lifecycle

1. `propose` — agent or user submits a unified diff against the config repo + a reason.
2. Broker validates: diff applies cleanly, touches only allowed paths for its tier, tier ≤ allowed.
   Paths are what the diff actually changes (applied to a scratch index): both sides of a
   rename, raw names — so renames, deletions and quoted names cannot dodge the forbidden tier.
3. Preview shown; approval collected (UI → broker over a local socket; M1: CLI flag).
4. `apply` — commit (signed with the broker's key), `nixos-rebuild switch`, ledger append.
5. `rollback <seq|commit>` — `git revert`, rebuild, ledger append.

## Tool inventory (initial)

`hw.inventory`, `hw.firmware_status`, `logs.query`, `logs.coredump`, `pkg.search`,
`config.read`, `config.propose`, `display.status`, `net.status`, `power.status`,
`secureboot.status` (read-only), `files.read_scoped`.

## Ideas borrowed from Omarchy (do them with the trust layer)

- Crash watcher (systemd-coredump → agent briefing → proposed fix as a commit).
- System-tailoring skill → becomes typed tools + broker.
- Agent usage widget → part of the Activity timeline.
