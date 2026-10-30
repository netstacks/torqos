# CLI design — how `torq` should look and feel

Status: **plan** (milestone "CLI polish" in `ROADMAP.md`). The command set follows the Junos
configuration model (ADR-016): a private *candidate* configuration, `show compare`, `commit check`,
`commit confirmed`, `rollback <n>`. Anyone who has run a Juniper box already knows how to drive
TorqOS; anyone who hasn't learns a model that is hard to get wrong.

## Principles

1. **Speak human.** "set packages htop", "commit complete" — never `proposal #22 · tier: user`.
2. **Show the change in the user's terms.** `show compare` shows packages and services (with
   versions and descriptions where useful), never a JSON diff or `flake.lock` hunk (`--diff` shows
   the raw change).
3. **Quiet while working.** One spinner line naming the phase (Checking → Downloading → Switching)
   and the elapsed time. The full Nix/broker log goes to a file; it is shown only on failure.
4. **One line per outcome.** `✓` / `✗` and what happened (`commit complete`).
5. **No internals unless asked.** Hashes, store paths, ledger kinds and broker commands appear only
   with `--verbose`.
6. **Commit is the approval.** Nothing changes until `commit`, and `commit` asks nothing: you have
   seen `show compare`. The tier is visible in `show compare` and in the commit line (a system
   change is marked). Whoever can run `torq commit` approves until the broker identifies its caller
   (M3, KNOWN-GAPS #4).
7. **Errors say what, why and what to do** — in one or two sentences. Never `Caused by:` chains,
   git hints or Rust error text; those go to the log file.
8. **One visual language.** Two-space indent, aligned columns, colour with meaning only
   (green ✓, red ✗, yellow for system changes and warnings, dim for secondary text).
   `NO_COLOR` and non-terminals get plain text; `--json` gives machine output for scripts.

## The model

- **Active configuration:** what the machine runs — the config repo at its last commit.
- **Candidate configuration:** your private working copy (`~/.local/state/torq/candidate/`),
  started from the active one by your first `set` or `delete`. Editing it changes nothing.
- **`commit`** applies the whole candidate as *one* ledgered change, rebuilds and switches. Its tier
  is the highest tier of anything it touches (the broker decides, from the diff). After a commit the
  candidate is empty again.
- **Commit numbers count backwards, like Junos:** `0` is the active configuration, `1` the one before
  the last commit, and so on. `show system commit` lists them; `rollback <n>` uses them.
- **`rollback <n>` restores a whole earlier configuration** into the candidate (then you commit it).
  It is a new change forward — it never conflicts with later changes the way undoing one old change
  can. `rollback 0` (or `rollback`) throws away the candidate's edits.
- **`commit confirmed <minutes>`** (default 10) applies the candidate and rolls back automatically
  unless confirmed with a plain `commit` in time — for changes that could cut you off (SSH, network,
  a bad update). The rollback is done by the broker as root from a system timer, so it happens even
  if your session is gone, and survives a reboot (an unconfirmed commit found at boot is rolled back
  once its time is up).
- **If someone else commits while you edit**, your edits are applied on top of the new active
  configuration when you commit; if they touch the same thing, the commit is refused and says so
  (`rollback 0` and redo them).

## Commands

| Command | Does |
|---|---|
| `torq set packages <name>…` | add packages for you (user tier) |
| `torq set system packages <name>…` | add packages for everyone (system tier) |
| `torq set services <name>…` | turn on services from TorqOS's catalog (system tier) |
| `torq delete packages \| system packages \| services <name>…` | the opposite |
| `torq request system update [all]` | load the newest TorqOS (and the nixpkgs it pins) into the candidate; `all`: every input |
| `torq show configuration` | the active configuration, Junos-style braces |
| `torq show compare` | the candidate against the active configuration (`show \| compare`) |
| `torq show system commit` | the commit history, numbered for `rollback` |
| `torq show system rollback <n> [compare <m>]` | the configuration <n> commits ago, or its difference to <m> |
| `torq show packages available <term>…` | search this machine's nixpkgs |
| `torq show services` | the catalog, and what is on |
| `torq show system alarms` | is this machine exactly as its history says? (exit 1 if not) |
| `torq commit check` | evaluate and build the candidate without switching |
| `torq commit [comment "<text>"]` | apply the candidate; while a confirmed commit is pending, also confirm it |
| `torq commit confirmed [<minutes>] [comment "<text>"]` | apply; roll back unless confirmed within <minutes> |
| `torq rollback [<n>]` | load commit <n>'s configuration into the candidate; `0` (default): discard the candidate |
| `torq request system repair` | return a machine that was changed outside TorqOS to its last commit |

Later: `torq configure` (an interactive configuration mode with the same statements, Junos's
`[edit]` prompt), `commit at <time>`, `load override <file>`.

## Target output

### Editing, comparing, checking, committing
```
$ torq set packages htop
$ torq set services ssh
$ torq show compare
[edit packages]
+   htop;               3.4.1   Interactive process viewer
[edit services]                                                  system change
+   ssh;                log in from other computers as viper, keys only; opens port 22
      keys that will be able to log in as viper:
        256 SHA256:Xq…k4 viper@mac (ED25519)   from ~/.ssh/authorized_keys

$ torq commit check
  ✓ configuration check succeeds                                    2 changes · 14s
$ torq commit comment "htop and ssh"
  ✓ commit complete                                                 system change
```

### Changes that could cut you off
```
$ torq commit confirmed 5 comment "move SSH to keys only"
  ✓ commit complete — rolls back at 15:12 unless confirmed:  torq commit
$ torq commit
  ✓ commit confirmed
```
If it is not confirmed, the broker rolls it back at 15:12, and the next shell says so:
```
  ⚠ commit 0 was not confirmed and was rolled back at 15:12 (show system commit)
```

### History and rollback
```
$ torq show system commit
0   2026-10-30 15:07:12   viper   commit confirmed, rolls back at 15:12
    move SSH to keys only
1   2026-10-30 15:02:51   viper
    request system update: TorqOS 2026-10-29 → 2026-10-30
2   2026-10-30 14:29:19   viper
    set packages ripgrep
$ torq rollback 1
  load complete — the candidate is now commit 1's configuration (show compare)
$ torq commit comment "back to before the SSH change"
  ✓ commit complete
```

### The configuration
```
$ torq show configuration
packages {
    htop;
    ripgrep;
}
system {
    packages;
    version TorqOS 2026-10-30, nixpkgs 26.05 (6f06d20);
}
services {
    ssh;
}
```

### Refused before anything happens
```
$ torq commit check
  ✗ "htpo" isn't a package. Did you mean htop?     torq show packages available htpo
  ✗ vscode can't be installed: it is unfree software, and this machine only installs free
    software. (Allowing unfree packages is a system setting.)
```

### Status
```
$ torq show system alarms
  ✓ No alarms: this machine is exactly as its history says.     commit 0 · 15:07
```
or
```
  ⚠ This machine was changed outside TorqOS.
    Running system: not the one commit 0 built (switched at 13:36).
    To return to it:  torq request system repair
```

### Failure while committing
```
  ✗ commit failed: the download from cache.nixos.org failed (network unreachable).
    Nothing changed — your machine is as it was, and your candidate is kept.
                                                          log: ~/.local/state/torq/last.log
```

## How it is built

- **The broker stays plain and small** (ADR-014). The candidate, `show`, `commit check` and
  `rollback <n>` live entirely in the unprivileged CLI: a commit is one `propose` + `apply` of the
  diff between the candidate and the active configuration; `rollback <n>` reads the config repo's
  history (it is world-readable) into the candidate. Only `commit confirmed` needs the broker: the
  rollback must run as root, without your session.
- **Facts come from evaluating the candidate**: package version and description
  (`meta.description`), what a service opens, whose SSH keys get in, the size to download.
- **Progress**: run the broker with its output captured to `~/.local/state/torq/last.log`;
  show phase + elapsed time; on failure print the summary line and the log path.
- **Dependencies**: reuse `anstream`/`anstyle` (already pulled in by clap) for colour and terminal
  detection; a hand-rolled spinner; no TUI framework.
- **Flags everywhere**: `--verbose` (internals inline), `--json` (scripts), `--diff` (the raw
  change), `NO_COLOR` respected, plain text when stdout is not a terminal.

## The broker's JSON interface (ADR-014)

`torq-broker --json <command>` prints exactly one JSON value on stdout (the rebuild's own
output goes to stderr); `log --json` prints one per line. These shapes are an interface: change
them deliberately, with tests on both sides.

| Command | stdout |
|---|---|
| `propose`, `apply`, `rollback`, `show`, `repair` | the ledger entry (`repair` of an interruption: the `interrupted` entry) |
| `apply --confirm-within <minutes>` | the `applied` entry, with `data.confirm_by` (RFC 3339) |
| `confirm` | the `confirmed` entry, or `{"confirmed": null}` if nothing was pending |
| `expire-confirm` | the `rolled_back` entry (`data.unconfirmed: true`), or `{"expired": null}` |
| `log [--brief]` | one entry per line (`--brief`: without `data.diff`) |
| `reconcile` | `{"reconciled": <entry> \| null}` |
| `status` | `{ok, busy, problems[], ledger_entries, ledger_error, head, ledgered_commit, dirty, interrupted: {op, target} \| null, unrestored, running_system, next_boot, ledgered_system, checked_running_system, confirm_by}` |
| `verify` | `{"entries": n}` |
| `init` | `{"initialised": path, "ledger": path}` |
| any failure | `{"error": "...", "busy": bool}` |

A ledger entry is flat: `seq, ts, actor, approved_by, kind, tier, tainted, reason, data,
prev_hash, hash`. `kind` is one of `init, proposal, applied, apply_failed, rolled_back,
rollback_failed, interrupted, repaired, confirmed`; `tier` is `observe, user, system, forbidden`;
`data` depends on `kind` (e.g. `proposal`: `files, diff, diff_sha256`; `applied`: `proposal,
commit, files, system`, and `confirm_by` for a confirmed commit).

When `status` is busy it reports `{ok: false, busy: "<who holds the lock>", problems: []}` and
nothing else (mid-operation the other facts are in flux). In `status`, `busy` is that string or
null; in an error object it is a boolean. `unrestored` is text (`"#7 apply_failed"`). A failed
`apply` returns `{"error"}`; its `apply_failed` ledger entry (with `restored`) is in `log`.

Exit codes: **0** success; **1** refused or failed; **75** another operation is running (try
later — any command); **2** bad arguments (clap, plain text); anything else, or stdout that is
not JSON, means the broker itself failed — show its stderr.

**JSON-escaped is not terminal-safe.** Text in entries (`reason`, `actor`, `files`, `diff`),
`problems` and `error` comes from whoever proposed and may hold bidi overrides or C1 controls;
the CLI runs every string through its own neutraliser before printing, and never prints a raw
JSON line to a terminal.

## Acceptance criteria

- No command prints a commit hash, store path, ledger kind, `Caused by:` or a git hint by default.
- Every command has a snapshot test of its plain-text output (success, refusal, failure).
- Every error message names what to do next.
- Output fits 80 columns; nothing depends on colour to be understood.
- `--json` output is stable and documented; the CLI itself uses the broker's `--json`.
- A network engineer drives it without reading this page: set, show compare, commit check,
  commit confirmed, show system commit, rollback <n>.
- Someone who has never seen Junos installs a package, undoes it and reads the history with only
  `torq --help`.

## Related: the terminal itself

The prompt, colours and font around the CLI are stock NixOS today. A TorqOS terminal look (prompt,
palette, font, `foot` config) belongs with the desktop work, but should ship with or soon after
this milestone so the CLI doesn't sit inside an unstyled shell.
