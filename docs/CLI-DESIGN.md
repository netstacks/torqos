# CLI design — how `kestrel` should look and feel

Status: **plan** (milestone "CLI polish" in `ROADMAP.md`). Today's output is the broker's plumbing
(git diffs, JSON, store paths, commit hashes, Nix build logs). It is correct and auditable, and it
is not what a person should have to read. This document is the target.

## Principles

1. **Speak human.** "Install htop for you", not `proposal #22 · tier: user · actor: user`.
2. **Show the change in the user's terms.** Package name, version, one-line description; a service
   and what it opens. Never a JSON diff or `flake.lock` hunk by default (`--diff` shows it).
3. **Quiet while working.** One spinner line naming the phase (Checking → Downloading → Switching)
   and the elapsed time. The full Nix/broker log goes to a file; it is shown only on failure.
4. **One line per outcome.** `✓` / `✗`, what happened, the change number and how to undo it.
5. **No internals unless asked.** Hashes, store paths, ledger kinds and broker commands appear only
   with `--verbose` (or `kestrel log --details`).
6. **Make the tier visible.** A system change is marked as affecting the whole machine and always
   asks with a `[y/N]` default; a change to your own apps asks `[Y/n]`.
7. **Errors say what, why and what to do** — in one or two sentences. Never `Caused by:` chains,
   git hints or Rust error text; those go to the log file.
8. **One visual language.** Two-space indent, aligned columns, colour with meaning only
   (green ✓, red ✗, yellow for system changes and warnings, dim for secondary text).
   `NO_COLOR` and non-terminals get plain text; `--json` gives machine output for scripts.

## Target output

### `kestrel pkg add htop`
```
  Install for you
    + htop 3.4.1          Interactive process viewer

  Install? [Y/n] y
  ⠋ Downloading 1 package (412 KB)…  3s
  ✓ Installed htop                                   change 23 · undo: kestrel undo 23
```

### `kestrel pkg add --system docker` (system tier)
```
  Install for everyone on this machine                          system change
    + docker 28.1.1       Pack, ship and run any application as a container

  Install? [y/N] y
  ✓ Installed docker for everyone                    change 27 · undo: kestrel undo 27
```

### Refused before anything happens
```
  ✗ vscode can't be installed: it is unfree software, and this machine only installs free
    software. (Allowing unfree packages is a system setting.)
  ✗ "htpo" isn't a package. Did you mean htop?          kestrel pkg search htpo
```

### `kestrel pkg list`
```
  Yours
    htop        3.4.1     Interactive process viewer
    ripgrep     15.1.0    Fast line-oriented search tool

  Everyone
    (nothing extra)
```

### `kestrel service enable ssh`
```
  Turn on SSH for this machine                                  system change
    · Lets you log in from other computers as viper, with a key only
    · Opens port 22

  Turn on? [y/N] y
  ✓ SSH is on                                        change 28 · undo: kestrel undo 28
```

### `kestrel update`
```
  Update this machine                                           system change
    Kestrel   2026-10-29 → 2026-11-04
    nixpkgs   26.05 (6f06d20) → 26.05 (9a1c3e2)      312 packages updated

  Update? [y/N] y
  ⠋ Downloading 312 packages (1.2 GB)…  1m 12s
  ✓ Updated                                          change 30 · undo: kestrel undo 30
```

### `kestrel log`
```
  Today
    14:29   ✓ Install ripgrep for you                      23
    15:02   ✓ Update Kestrel                               25   (system)
    15:06   ↶ Undid "Install cowsay for you"               27
  Yesterday
    …
                                       undo the latest: kestrel undo · details: kestrel log 25
```
Proposals that were applied are folded into their result; failed and interrupted changes show
`✗` with a one-line reason.

### `kestrel undo`
```
  Undo "Install ripgrep for you" (change 23, today 14:29)
    − ripgrep 15.1.0

  Undo? [Y/n] y
  ✓ Undone                                           change 29 · redo: kestrel pkg add ripgrep
```
When an old change can't be undone because later changes touched the same thing:
```
  ✗ Can't undo change 12 on its own: later changes (19, 23) also changed your package list.
    To remove just htop:  kestrel pkg remove htop
```

### `kestrel status`
```
  ✓ This machine is exactly as its history says.    25 changes · last: Update Kestrel, 15:02
```
or
```
  ⚠ This machine was changed outside Kestrel.
    Running system: not the one change 25 built (switched at 13:36).
    To return to it:  kestrel status --repair      (asks first)
```

### Failure while applying
```
  ✗ Couldn't install htop: the download from cache.nixos.org failed (network unreachable).
    Nothing changed — your machine is as it was.            log: ~/.local/state/kestrel/last.log
```

## How it is built

- **The broker stays plain and small** (ADR-014). It gains `--json` output for `propose`,
  `apply`, `rollback`, `status` and `log`, so the CLI never parses human text (today it scrapes
  `proposal #N`). All presentation lives in the unprivileged CLI.
- **Facts come from the evaluation the CLI already does**: package version and description
  (`meta.description`), what a service opens, and the size to download (`nix path-info`/dry-run).
- **Progress**: run the broker with its output captured to `~/.local/state/kestrel/last.log`;
  show phase + elapsed time; on failure print the summary line and the log path.
- **Dependencies**: reuse `anstream`/`anstyle` (already pulled in by clap) for colour and terminal
  detection; a hand-rolled spinner; no TUI framework.
- **Flags everywhere**: `--verbose` (internals inline), `--json` (scripts), `--diff` (the raw
  change), `NO_COLOR` respected, plain text when stdout is not a terminal.

## The broker's JSON interface (ADR-014)

`kestrel-broker --json <command>` prints exactly one JSON value on stdout (the rebuild's own
output goes to stderr); `log --json` prints one per line. These shapes are an interface: change
them deliberately, with tests on both sides.

| Command | stdout |
|---|---|
| `propose`, `apply`, `rollback`, `show` | the ledger entry |
| `log [--brief]` | one entry per line (`--brief`: without `data.diff`) |
| `reconcile` | `{"reconciled": <entry> \| null}` |
| `status` | `{ok, busy, problems[], ledger_entries, ledger_error, head, ledgered_commit, dirty, interrupted: {op, target} \| null, unrestored, running_system, next_boot, ledgered_system, checked_running_system}` |
| `verify` | `{"entries": n}` |
| `init` | `{"initialised": path, "ledger": path}` |
| any failure | `{"error": "...", "busy": bool}` |

A ledger entry is flat: `seq, ts, actor, approved_by, kind, tier, tainted, reason, data,
prev_hash, hash`. `kind` is one of `init, proposal, applied, apply_failed, rolled_back,
rollback_failed, interrupted`; `tier` is `observe, user, system, forbidden`; `data` depends on
`kind` (e.g. `proposal`: `files, diff, diff_sha256`; `applied`: `proposal, commit, files, system`).

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
- Reviewed by someone new to Kestrel: can they install, remove, undo and read the log without help?

## Related: the terminal itself

The prompt, colours and font around the CLI are stock NixOS today. A Kestrel terminal look (prompt,
palette, font, `foot` config) belongs with the desktop work, but should ship with or soon after
this milestone so the CLI doesn't sit inside an unstyled shell.
