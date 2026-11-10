# AI layer

**Status (2026-11-11): experimental. M5 (ADR-034) is built in part: the account (ADR-035, ADR-041), doctor mode, proposals and session trust (ADR-042) exist and are in v0.3.x. Doctor is off by default, no model ships, and no real model has ever been tried (the tests use a scripted endpoint). The resident agent, torq-mcp and ADR-051 are not on main: they are parked on branch `work/agents` until after 0.4.0-rc1.**
Everything built so far (the broker as the only writer, actors, tiers, the ledger) was built so
this could be added without a bypass. Each person's agent is an *account* of its own,
`torq-agent-<person>` (ADR-051 3, amending ADR-041 1: `torq.agent.people`, the owner by default),
whose identity the kernel vouches for (`SO_PEERCRED`) — never a flag the caller sets. The broker
knows which agent serves whom from its map in /etc/torq/broker.json: an account is an agent only if
the map names it and its uid is no person's.

## One agent per person (ADR-051 3)

- **Its account:** `torq-agent-<person>`, a static system user in its own group, no password, no
  login, no other group, its home `/var/lib/torq-agent-<person>` (0700, its own). Neither agent reads
  the other's home.
- **What it may propose:** packages only in its own person's folder (`users/<person>/packages.json`),
  never another person's; settings only as `torq.agent.mayPropose.settings` allows (the machine's
  list, the same for every agent). A refusal is ledgered with the agent account that tried.
- **Sessions and trust:** a session is its agent account's: no other agent reports into it or cites
  it. A person's trust (`request system agent trust`) applies only their own agent's proposals,
  judged by the broker's map at the time of the apply, and only one the agent sent itself over the socket.
  Limits (proposals waiting, refusals ledgered,
  sessions started and kept) are counted per agent, so one person's agent cannot crowd out another's.
- **`torq agent run`** runs the caller's own agent (the account of sudo's `SUDO_UID`, which the launcher's rule never
  lets a caller set), never one the caller
  names; a person with none is told so. A restore from a backup into any agent's folder is
  refused. With the agent's service running, `--share` holds it still (systemd's freezer) through the walk.
- **Its service** (ADR-051 3, step 2): `torq-agent@<person>.service`, reached only by that person (`torq ask
  "<question>"`), keeps their conversation in its own state directory, which no other program of the agent's sees.

## What it is for

Three things a person says to their machine; each is an acceptance test (ROADMAP M5):

| | The agent does | The gate |
|---|---|---|
| "Fix my wifi." | reads logs and hardware (doctor), proposes a system-tier change | the person's one answer; `commit confirmed`, so it reverts itself if the machine does not come back |
| "Build me an application for this." | writes and builds it in the person's home, installs it through the tree (ADR-037, rung 3) | none: user tier; every step a ledger line; `rollback` undoes it |
| "Watch while I try it." | screenshots and input through the compositor's protocols | the person's session; nothing of the broker's |

In order: **doctor mode** (read, explain, propose nothing; built, ADR-042: `torq doctor "<question>"`), **proposals** (built: `torq agent propose "<ask>"`), **session trust** (built: `request system agent trust <minutes>`; "for
the next hour, user-tier changes without asking" — ledgered, revertible, never for system tier).
On a desktop the approval is a dialog with the owner's password, the one thing a program in the
owner's shell does not have; in a terminal it is torqcli's prompt, the same text.

## Components

| Component | Privilege | Language | Role |
|---|---|---|---|
| `torq-doctor` (`agent/`), as the person's `torq-agent-<person>` | unprivileged, sandboxed (`torq agent run`, or the agent service) | Rust | TorqOS's harness: talks to the model (OpenAI-compatible chat API), calls its read-only tools, *proposes* changes (ADR-042); one process per question or task |
| `torq-agent@<person>.service` (`agent/`: `torq-agent-service`), as `torq-agent-<person>` | unprivileged, the launcher's sandbox and systemd's hardening; socket-activated, its socket the person's alone, SO_PEERCRED checked | Rust | The person's agent, resident (ADR-051 3, step 2): holds their conversation (its last turns), runs one task at a time as its own `torq-doctor --propose --task`, streams its events to `torq ask` (later the panel's window); `stop` ends a task |
| MCP tool servers (`torq-mcp`) | unprivileged, read-mostly | Rust | Typed access per subsystem |
| `torq-broker` | privileged (root, minimal) | Rust | Only writer of the config repo; tiers, commits, rebuild, rollback, ledger |
| Model runtime | unprivileged (DynamicUser) | llama.cpp (`llama-server`, a unix socket) | Local inference; or a remote endpoint as a setting (ADR-042) |
| torqcli | the person's | Rust | Shows proposals, diffs and the ledger; a person approves at a terminal |

## Permission tiers

| Tier | Examples | Requirement |
|---|---|---|
| `observe` | read logs, hardware info, settings, config repo | automatic, still ledgered |
| `user` | one user's *data* files: today `users/<name>/packages.json` (apps); later themes, dotfiles. Never a `.nix` module (that is `system`, wherever it lives) | approve once per category; /home snapshot first |
| `system` | services, drivers, kernel params, packages | always preview + explicit approval; commit + generation |
| `forbidden` | AI permissions, ledger, Secure Boot keys, firmware password, disabling audit | **never** via agent; user-only, at a terminal |

## Taint tracking

Any agent session that has ingested untrusted content (logs, device names, the ledger, configuration
values; web pages, email, downloaded files) is marked **tainted**. Tainted sessions cannot use
"approve once" shortcuts: every `user` or `system` action requires explicit per-step approval
with the taint source displayed.

Built (ADR-042 6, amended): the taint is the **broker's**. TorqOS's agent starts its session at the
broker and reports each tainting read there before the model sees it; the broker keeps the record in
its own state directory, where the agent cannot write, and nothing clears it. The ledger holds a
session where a proposal cites it: the proposal's entry carries the session and what tainted it.
A proposal names its session and is tainted as the broker's record says;
one that names no session — any other program run as the agent — is tainted; one that names another
agent's session is refused. `request system apply`
says what the session read; session trust never covers a tainted proposal, nor another person's
agent's. The broker holds what it
is told, and cannot see what a program reads. So taint is reported by TorqOS's own doctor and held by the broker; a different program run as the agent (Claude Code under `torq agent run`, say) can open a session and simply not report, and its proposals then look untainted. Taint is a defence against a prompt injection reaching TorqOS's own doctor, not against a hostile program running as the agent. What bounds that is the rest of the design (trust is a person's own folder, at most 60 minutes, never the system tier): KNOWN-GAPS 6zq.

## Ledger format (v0)

Append-only JSON Lines at `/var/lib/torq/state/ledger.jsonl`, owned by the broker.
Entry kinds: `init`, `proposal`, `applied`, `apply_failed`, `reverted`, `revert_failed`,
`interrupted`, `repaired`, `confirmed`, `rolled_back` (an unconfirmed `commit confirmed`), and a
person's `trust_granted`, `trust_revoked`, `trust_expired`
(failures and restores carry `restored: true|false` — see ADR-008).

```json
{"seq":1042,"ts":"2026-10-30T15:20:11Z","actor":"agent","approved_by":"casey",
 "kind":"applied","tier":"user","tainted":false,
 "reason":"User asked for night light at sunset",
 "data":{"proposal":1041,"commit":"8f3a2c1…","files":["desktop.nix"]},
 "prev_hash":"…","hash":"…"}
```

- `hash = sha256(prev_hash || canonical_json(entry_without_hash))`
- Genesis entry has `prev_hash` = 64 zeros.
- `torq-broker verify` recomputes the chain and reports the first broken link.
- Git commits carry a `Ledger-Entry: <seq>` trailer; the ledger stores the commit id → each
  checks the other: `status` does that cross-check (KNOWN-GAPS 2); `verify` alone does not, and cannot detect
  truncation of the newest entries except against a remote copy of the ledger (ADR-036; built, off by default).
- Built, opt-in: the ledger and configuration pushed to a remote after every change (ADR-036). Not built: anchoring in a TPM NV index or an external witness.

## Change lifecycle

1. `propose` — agent or user submits a unified diff against the config repo + a reason.
2. Broker validates: diff applies cleanly, touches only allowed paths for its tier, tier ≤ allowed.
   Paths are what the diff actually changes (applied to a scratch index): both sides of a
   rename, raw names — so renames, deletions and quoted names cannot dodge the forbidden tier.
3. Preview shown; approval collected (torqcli at a terminal, or the desktop's dialog). Who approves is the uid the kernel reports on the broker's socket, never a flag; `--approved-by` is taken only on the root recovery path (`sudo torq-broker`).
4. `apply` — snapshot data (ADR-038), commit, build and switch, ledger append, push and anchor
   (ADR-036). (Signing on the machine is not the defence — ADR-035; the anchor is.)
5. `revert <seq>` (`request system revert` in torqcli) — a new commit that reverses one applied change, rebuild, ledger append. Going back to an earlier whole configuration is `rollback <n>` in torqcli, which loads it into the candidate to commit.

## Tool inventory

Built (doctor mode, ADR-042, `agent/src/tools.rs`; all read-only): `read_journal`,
`hardware_facts`, `show_configuration`, `show_ledger`, `show_alarms`, `nixos_option_docs`. All but
the option docs return untrusted text (a configuration value too) and
taint the session. In propose mode (ADR-042 6): `propose_change`, structured words of the tree, a
proposal through torqcli as the agent.
Later: `logs.coredump`, `pkg.search`, `power.status`, `secureboot.status`, `files.read_scoped`.
