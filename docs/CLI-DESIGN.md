# CLI design — `torqcli`, the TorqOS shell

Status: **plan** (milestone M1.5 in `ROADMAP.md`; ADR-016). TorqOS is driven like a network
device: a shell with an operational mode and a configuration mode, a private *candidate*
configuration, `show | compare`, `commit check`, `commit confirmed`, `rollback <n>`. Two syntaxes over
one model — Juniper-style and Cisco-style — switchable at any time with `switch cli`, as in Cisco NSO.
Anyone who has run a router already knows how to drive TorqOS.

## Principles

1. **One model, two syntaxes.** Everything below exists in both styles; they parse to the same
   operations. The style is a setting (`users <you> cli style`), and `switch cli` flips it for the
   session.
2. **The configuration is a tree, and the tree is the config repo's files.** Every setting has a
   path; `show configuration` renders the repo; a commit is a diff of files. Settings are declared in
   a schema (TorqOS's YANG), which also drives `?` help, Tab completion and syntax checks.
3. **Show the change in the user's terms.** `show | compare` shows packages, services, variables —
   with versions and descriptions where useful — never a JSON or `flake.lock` hunk (`| display
   diff` shows the raw change).
4. **Quiet while working.** One spinner line naming the phase and the elapsed time; the full log in
   `~/.local/state/torq/last.log`, shown on failure.
5. **No internals unless asked.** Hashes, store paths, ledger kinds and broker commands appear only
   with `| display detail`.
6. **Commit is the approval, and it says exactly what it approves.** `commit` prints the change it is
   making before it switches; any `.nix` file in it is shown as its raw diff. A system-tier commit
   needs a terminal — which stops cron, services and other background jobs, but **not** anything
   started from your terminal (an AI tool, a script, a package's install hook): until the broker
   identifies its caller (M3, KNOWN-GAPS #4), whatever runs in your terminal can commit as you.
7. **Errors say what, why and what to do** — never `Caused by:`, git hints or Rust error text.
8. **One visual language.** Colour with meaning only; `NO_COLOR` and non-terminals get plain text;
   fits 80 columns.

## The shell

```
$ torqcli                                   # interactive
viper@torq-utm> show system alarms          # operational mode
viper@torq-utm> configure                   # configuration mode
$ torqcli show system alarms                # one command, for scripts
$ torqcli -c "configure; set users viper packages htop; commit"
```

| | Juniper style | Cisco style |
|---|---|---|
| operational prompt | `viper@torq-utm>` | `viper@torq-utm>` |
| enter configuration | `configure` | `config` |
| configuration prompt | `[edit system]` line, then `viper@torq-utm#` | `viper@torq-utm(config-system)#` |
| add / change | `set system packages jq` | `system packages jq` |
| remove | `delete system packages jq` | `no system packages jq` |
| go down / up / to the top | `edit system` · `up` · `top` | `system` · `exit` · `end` |
| compare with the active configuration | `show \| compare` | `show configuration diff` |
| show the configuration | `show` / `show configuration` | `show running-config` |
| leave configuration | `exit` | `end` / `exit` |

The same in both: `commit`, `commit check`, `commit confirmed <minutes>`, `commit comment "<text>"`,
`rollback <n>`, `show system commit`, `switch cli`, `?` help, Tab completion, history (↑).

**Pipes** (our own, not the shell's): `| compare`, `| display set`, `| display detail`,
`| display diff`, `| match <text>`, `| except <text>`, `| count`, `| no-more`.

**One grammar, two renderings.** Both styles parse to one set of operations; a property test renders
every operation in one style and parses it in the other. Words a Cisco-style bare path could collide
with are reserved and can never name a container (`no`, `do`, `exit`, `end`, `top`, `up`, `show`,
`commit`, `rollback`, `set`, `delete`, `edit`, `configure`, `config`, `run`, `switch`). Deleting a
whole container needs `delete <container> all` / `no <container> all`. In Cisco configuration mode
`do <command>` runs an operational command; rollback is `rollback configuration <n>` there.

**Scripts (`torqcli -c`)** use a private candidate that exists only for that invocation — never your
interactive candidate, so a script cannot commit your half-done edits or leave edits behind. They
stop at the first error, always use the Juniper style unless `--style cisco` is given, and quote
values containing `;` or `|` with double quotes. Interactive sessions of the same user share one
candidate (Junos `configure`); `configure private` is for later.

## The configuration tree

Each container is a folder of the config repo; each leaf set is one small data file. The first
settings (more are added one schema entry at a time):

| Path in the tree | File in the config repo | Tier |
|---|---|---|
| `system packages <name>` | `system/packages.json` | system |
| `system host-name <name>` · `system time-zone <tz>` | `system/host.json` | system |
| `system cli style juniper\|cisco` | `system/cli.json` | system |
| `services <name>` (catalog: ssh, …) | `services.json` | system |
| `users <name> packages <pkg>` | `users/<name>/packages.json` | user |
| `users <name> cli style juniper\|cisco` | `users/<name>/cli.json` | user |

Validation (enforced by the NixOS modules' types, the authority — the shell's schema only gives early
errors and help): host names are RFC 1123 labels of at most 63 characters; time zones must exist in
tzdata (the ledger itself is always UTC); `<name>` under `users` must be a normal user of the machine.
Changing the host name never changes which configuration the broker builds: the config repo's flake
always exposes one fixed output, `nixosConfigurations.torq`.

**Held: environment variables.** `system environment` and `users <name> environment` need their own
design first (review N2/N3): variable values are effectively code (`PATH`, `LD_PRELOAD`, `BASH_ENV`,
`PROMPT_COMMAND`, `EDITOR`, `NIX_CONFIG`…), a user-tier value must never reach another user or root,
a system value must never reach the broker's environment, and people will paste secrets into them.
They come back with an allow-list of names, per-user delivery, escaped values and a secrets check.

Files outside the tree (`flake.nix`, `machine.nix`, hardware, any `.nix`) are system tier and edited
as Nix, not through the shell; `show configuration` lists them at the end as "plus N Nix files", and
any change to one is shown as its raw diff in `show | compare` and in `commit`.

```
viper@torq-utm> show configuration
system {
    host-name torq-utm;
    time-zone America/New_York;
    software TorqOS 2026-10-30, nixpkgs 26.05 (6f06d20);
}
services {
    ssh;
}
users {
    viper {
        packages [ htop opencode ripgrep ];
    }
}
```

## The commit model

- **Active configuration:** the config repo at its last commit. **Candidate:** your private copy
  (`~/.local/state/torq/candidate/`, with the commit it is *based on*), created by your first edit.
- **`commit`** sends the change from your base to your candidate as one proposal and applies it.
  If someone committed in between, the broker refuses (it checks the base): `rollback 0` and redo.
  Afterwards the candidate is gone.
- **`commit check`** evaluates and builds the candidate without switching.
- **Commit numbers count backwards:** 0 is the active configuration. They are the ledger entries
  that produced a new configuration (`init`, `applied`, `rolled_back`), newest first. Numbers shift
  with every commit, so `rollback <n>` says which commit it loaded (date, who, comment).
  `show system commit` reads the ledger, so it uses `sudo` like a commit does.
- **`rollback <n>`** loads commit <n>'s *configuration* into the candidate — every file except
  `flake.lock`, so it never downgrades the software (Junos keeps the two apart, too). That
  includes `.nix` files (e.g. turning development mode back on): `show | compare` shows each one as
  a raw diff, flagged. `rollback 0` discards the candidate.
- **`request system software rollback <n>`** is the separate, explicit way to go back to commit
  <n>'s software (its `flake.lock`), marked as a downgrade.
- **`commit confirmed <minutes>`** (2–60, default 10) is for changes that could cut you off:
  - the new system is *activated but not made the boot default* (`switch-to-configuration test`);
  - the window starts when the switch has finished;
  - a plain `commit` with an empty candidate confirms it: the broker makes the running system
    the boot default (no rebuild). With edits in the candidate, `commit` refuses — confirm first,
    then commit them. Only the person who made the confirmed commit confirms it (others are told
    whose it is);
  - confirming is accepted until the broker has *started* the rollback (checked under its lock),
    not by comparing clocks, so a clock jump or a suspended laptop can never leave a commit that
    can be neither confirmed nor rolled back;
  - if not confirmed in time, the broker restores the previous system **directly** (its store path,
    kept alive for the window — no evaluation, no download) and resets the config repo to the
    previous commit, as a ledgered change;
  - a reboot before confirming is itself the rollback: the boot default was never changed, and the
    broker records it at boot;
  - while it is pending, no other change can be made (except confirming, rolling back, or
    recovering that same commit if the broker crashed mid-way), so nobody confirms your risky change
    by committing their own; a second `commit confirmed` is refused until the first is settled.

```
viper@torq-utm# set services ssh
viper@torq-utm# show | compare
[edit services]                                                  system change
+   ssh;            log in from other computers as viper, keys only; opens port 22
      keys that will be able to log in as viper:
        256 SHA256:Xq…k4 viper@mac (ED25519)   from ~/.ssh/authorized_keys
viper@torq-utm# commit confirmed 5 comment "ssh, keys only"
  committing: services ssh (system change)
  ✓ commit complete — rolls back at 15:12 unless confirmed with: commit
viper@torq-utm# commit
  ✓ commit confirmed
```

## Operational commands

| Command | Does |
|---|---|
| `show configuration` | the active configuration |
| `show system commit` | the commit history, numbered for `rollback` |
| `show system rollback <n> [compare <m>]` | commit <n>'s configuration, or its difference to <m> |
| `show system alarms` | is this machine exactly as its history says? (and any pending confirmed commit) |
| `show packages available <term>` | search this machine's nixpkgs |
| `show services` | the catalog, and what is on |
| `request system update [all]` | load the newest TorqOS (and its nixpkgs) into the candidate |
| `request system software rollback <n>` | load commit <n>'s software into the candidate (a downgrade) |
| `request system repair` | return a machine changed outside TorqOS to its last commit |
| `configure` / `config` | enter configuration mode |
| `switch cli` | Juniper ↔ Cisco style for this session |

Later: `commit at <time>`, `load override <file>`, `configure exclusive`.

## How it is built

- **`torqcli` is unprivileged** (ADR-014): the shell, the candidate, `show`, `commit check` and
  `rollback <n>` live in it. A commit is one `propose --base` + `apply` through `sudo torq-broker`.
- **The schema** is defined next to each setting's NixOS module (`torq.schema`) and installed as
  `share/torq/schema.json` in the system, so the shell reads it instantly for parsing, `?` and Tab.
  Syntax is checked as you type; meaning (does the package exist, is it free) at `commit check`.
- **The schema is not the authority.** The NixOS modules' strict types are: every leaf file has a
  consumer that rejects bad data at evaluation, whoever wrote it (the shell, the agent, a raw diff).
  The installed schema belongs to the *running* system, so `commit check` evaluates the candidate
  against the candidate's own TorqOS version.
- **Reading history** for `rollback <n>`: the config repo is world-readable, read with git plumbing
  (`-c safe.directory=… -c core.hooksPath=/dev/null -c core.fsmonitor=false`). Secrets never go in
  the config repo (they get their own mechanism later).
- **The broker command is fixed**: `/run/wrappers/bin/sudo /run/current-system/sw/bin/torq-broker`.
  `TORQ_BROKER` and `TORQ_CONFIG_REPO` overrides exist only in test builds, so nothing in your
  environment can redirect the command your next `commit` runs as root.
- **Line editing**: a small line-editor crate for history, Tab and `?`.

## The broker's part

The broker stays small; its additions for this model:
- `propose --base <commit>`: `apply` refuses if the config repo's HEAD is not that commit.
- `apply --confirm <minutes>`: records the anchor (previous commit and running system) in the
  `applied` entry, activates with a fixed `test` command (not the caller's), keeps the anchor system
  alive (GC root), and schedules the expiry as a transient systemd timer (`AccuracySec=1s`) that
  runs *this* — the old, known-good — broker by its store path. If the `test` activation itself
  fails, the restore also goes straight to the anchor system, not a rebuild.
- `confirm`: sets the system profile to the running system and runs `switch-to-configuration
  boot` on it (no rebuild); ledger `confirmed`. Accepted until an expiry has started (under the
  lock). It is run by the *new* broker and reads what the old one wrote from the ledger — tested
  across the two.
- `expire-confirm`: resets the repo to the anchor commit, activates the anchor system directly;
  ledger `rolled_back` with `unconfirmed: true`, recording any drift it found and keeping anything it
  would discard; bounded retries, then an alarm. It is its own pending operation (crash-safe,
  ADR-010). A boot-time check records "rolled back by reboot".
- While a confirmed commit is pending: every other change-making command is refused, except
  reconciling that same commit.
- The pending confirmation is derived from the ledger, never from a separate state file;
  `pending.json` gains a version field so an older broker refuses a newer record clearly.
- `status` reports a pending confirmation (its deadline) instead of calling it drift.
- The broker clears its environment at start and takes its paths and rebuild commands from fixed
  configuration, not from the caller (closes part of KNOWN-GAPS #4); sudo uses a fixed `secure_path`.
- User-tier files are an explicit allow-list in `policy.rs`, exactly `users/<name>/packages.json`
  and `users/<name>/cli.json` (a test checks the list against the schema's user-tier leaves): regular
  files (git mode 100644), JSON object or list, size-capped. Evaluation rejects unknown files under
  `users/` and names that are not normal users.
- The configuration the broker builds is always `nixosConfigurations.torq`, never derived from the
  host name (a VM test changes the host name, then commits again).
- The single-change undo is renamed `revert <seq>`, so "rollback" means one thing.

## The broker's JSON interface (ADR-014)

`torq-broker --json <command>` prints exactly one JSON value on stdout; `log --json` one per line.
These shapes are an interface: change them deliberately, with tests on both sides.

| Command | stdout |
|---|---|
| `propose`, `apply`, `revert`, `show`, `repair` | the ledger entry |
| `apply --confirm <minutes>` | the `applied` entry, with `data.confirm_by`, `data.anchor_commit`, `data.anchor_system` |
| `confirm` | the `confirmed` entry |
| `expire-confirm` | the `rolled_back` entry (`data.unconfirmed: true`), or `{"expired": null}` |
| `log [--brief]` | one entry per line (`--brief`: without `data.diff`) |
| `reconcile` | `{"reconciled": <entry> \| null}` |
| `status` | `{ok, busy, problems[], …, confirm_pending: {seq, by, deadline} \| null}` |
| `verify` | `{"entries": n}` |
| `init` | `{"initialised": path, "ledger": path}` |
| any failure | `{"error": "...", "busy": bool}` |

A ledger entry is flat: `seq, ts, actor, approved_by, kind, tier, tainted, reason, data,
prev_hash, hash`. `kind` is one of `init, proposal, applied, apply_failed, rolled_back,
rollback_failed, interrupted, repaired, confirmed`.

Exit codes: **0** success; **1** refused or failed; **75** another operation is running; **2** bad
arguments. **JSON-escaped is not terminal-safe**: the CLI neutralises every string before printing.

## Acceptance criteria

- A network engineer drives it without reading this page, in either style.
- Someone who has never seen a router installs a package, undoes it and reads the history with only
  `?` and Tab.
- No command prints a commit hash, store path, ledger kind, `Caused by:` or a git hint by default.
- Every command has a snapshot test of its output (success, refusal, failure), in both styles.
- VM tests: commit confirmed expires and restores; a reboot before confirming rolls back; a stale
  candidate is refused; `rollback <n>` never changes `flake.lock`.

## Related: the terminal itself

The prompt, colours and font around the shell are stock NixOS today; a TorqOS terminal look belongs
with the desktop work.
