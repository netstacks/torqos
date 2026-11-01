# CLI design — `torqcli`, the TorqOS shell

Status: **plan** (milestone M1.5 in `ROADMAP.md`; ADR-016). TorqOS is driven like a network
device: a shell with an operational mode and a configuration mode, a private *candidate*
configuration, `show | compare`, `commit check`, `commit confirmed`, `rollback <n>`. One syntax, the
J style (`set`, `delete`, `edit`, `show | compare`; ADR-021). Anyone who has run a router already
knows how to drive TorqOS.

## Principles

1. **One model, one syntax.** Every command is one operation (`Op`); the J style is the only way to
   type it (ADR-021: one clean language, its help and errors in one voice).
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

| | |
|---|---|
| operational prompt | `viper@torq-utm>` |
| enter configuration | `configure` (or `config`) |
| configuration prompt | `[edit system]` line, then `viper@torq-utm#` |
| add / change | `set system packages jq` |
| remove | `delete system packages jq` (one value); `delete system packages`, `delete networking` (everything below); not set: a warning |
| go down / up / to the top | `edit system` · `up` (or `exit`) · `top` |
| compare with the active configuration | `show \| compare` |
| show the configuration | `show` (candidate) / `show configuration` (operational) |
| run an operational command | `run <command>` |
| leave configuration | `exit` at the top, or `exit configuration-mode` |

Turning SSH on (`set services ssh`) shows at `commit` exactly which keys will be able to log in, as
the changed configuration has them, and refuses if there are none (nobody could log in) — 4g, as
`torq service enable ssh` did.

And: `commit`, `commit check`, `commit confirmed <minutes>`, `commit comment "<text>"`,
`rollback <n>`, `show system commit`, `?` help, Tab completion, history (↑). `?` lists what may come
next, `|` after any whole `show`, and `<[Enter]>` when the line would run as it is (decided as the
session decides it: the line is dry-run on a copy of the candidate).

**Pipes**: `| compare`, and one `| display`: `set` (a configuration as `set` commands), `detail` (what
is behind a view: the config repo files and commit of a configuration, the commit ids of `show system
commit`, the broker's commits and store paths in `show system alarms`), `diff` (a comparison as the
raw diff of the files: for the candidate exactly what `commit` proposes; between two commits, the
files as committed). Each view refuses what it does not offer. Then any
number of **filters**, the real Linux tools with their own options (ADR-017): `| grep`, `| wc`, `| head`,
`| tail`, `| sort`, `| uniq`, `| cut`, `| tr`, after any `show` — e.g.
`show configuration | grep -i host`, `show system commit | head -n 5 | wc -l`. They read the show's
output inside a sandbox that has nothing else (no files, no network), from the system's own
coreutils and grep; words are split as a shell does (`'single'` quotes are literal).

**One grammar.** Every line parses to one operation; a property test renders every operation as it
is typed and parses it back. `config` is reserved: it can never name a list entry, since `<place>
config <file>` attaches a file.

**Delete** works as on a network device (ADR-022): `delete <setting> <value>` takes that value of a
list; `delete <setting>` or `delete <place>` takes everything below it — `show | compare` and
`commit` show exactly what goes before anything does. What is not set is a warning (`! nothing
deleted: … is not set`), not an error, so a paste goes on. `delete` with nothing after it takes
everything at this level, and asks first.

**Rules between settings** (a `path:` source needs `torq dev enable true`) are checked on the whole
configuration, never line by line, where their order would decide: a change that breaks one is
said at once (`! … — commit refuses until then`), and `commit` refuses until it holds.

As built (4a, cli/src/syntax.rs; C style removed by ADR-021): the line editor's `?` and Tab follow
where you are. In a `?` list each help is its setting's first sentence (NixOS's doc markup taken
out, `…` where it was cut), at most three lines (see "Layout" below).

**Scripts (`torqcli -c`)** use a private candidate that exists only for that invocation — never your
interactive candidate, so a script cannot commit your half-done edits or leave edits behind. They
stop at the first error, and quote
values containing `;` or `|` with double quotes. `torqcli <command>` runs one operational command
the same way (`torqcli show system alarms`, `torqcli show configuration '|' grep host`): its words as
your shell split them, or one argument read as a whole line (`torqcli "show configuration | grep
host"`); its exit code says whether it worked. Configuration needs `-c`. Interactive sessions of the same user share one
candidate (as `configure` on a network device); `configure private` is for later.

## The configuration tree

**Since S3c (docs/CONFIG-IS-THE-DEVICE.md) the tree is every NixOS setting of the system, one to one**
(`set virtualisation docker enable true`, `set services httpd virtualHosts example.com forceSSL
true`), generated from the system's own options (/etc/torq/nixos-schema.json) and kept in
`nixos/<top-level>.json`. Seven settings keep the files described below — for their own checks,
and for the user tier of a person's own folder — at their NixOS paths; the old names still work for
setting them, and the tree shows the NixOS one:

| NixOS path | Old name | File |
|---|---|---|
| `networking hostName` | `system host-name` | `system/host.json` |
| `time timeZone` | `system time-zone` | `system/host.json` |
| `torq packages system` | `system packages` | `system/packages.json` |
| `torq services enabled` | (`services`: now NixOS's own) | `services.json` |
| `torq packages user <name>` | `users <name> packages` | `users/<name>/packages.json` (user tier) |

Old names set and delete (and `delete users dev`, `delete system` take what they always did), but the old *places* are not places to go: `edit users dev` is gone, as the tree now groups
by NixOS. A setting lives in exactly one place: the five above are refused inside `nixos/` files (by
torqcli on reading, and by the evaluation). Text with a line break or another control character is
refused in the tree: multi-line text goes in a config file. Whether a commit turns SSH on is read
from the evaluated configuration, before and after, so every way counts; outside the catalog the
question says its rules do not apply, and whether passwords are allowed.
Everything else in `nixos/` is system tier. What the tree may never change is fenced
(modules/settings-fence.nix). Ledger reasons carry each change's section (`+ virtualisation docker
enable true;`). The rest of this section describes the files TorqOS keeps, as first designed.

Each container is a folder of the config repo; each leaf set is one small data file. The first
settings (more are added one schema entry at a time):

| Path in the tree | File in the config repo | Tier |
|---|---|---|
| `system packages <name>` | `system/packages.json` | system |
| `system host-name <name>` · `system time-zone <tz>` | `system/host.json` (done: `modules/host.nix`) | system |
| `services <name>` (catalog: ssh, …) | `services.json` | system |
| `users <name> packages <pkg>` | `users/<name>/packages.json` | user |

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

Files outside the tree (`flake.nix`, any `.nix` not attached) are system tier and edited
as Nix, not through the shell; `show configuration` names them at the end (`## also set outside this
configuration, in Nix: …`, 5a), with the software the machine runs (`## software: …`), and
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

As built (cli/src/tree.rs): statements are sorted by name at each level (so `services` comes
before `system`), and `| compare` shows changes under the innermost `[edit …]` section that holds
them, a whole leaf-list as one `-`/`+` pair, a new container as its leaves (`[edit users dev cli]
+ style c;`, where a network device would show `+ cli { … }` under `[edit users dev]`).

Config files, for what is not a value (docs/CONFIG-IS-THE-DEVICE.md, S3e):

```
[edit]
you# file edit services/httpd.nix          # $EDITOR; waits in the candidate
you# set services httpd config services/httpd.nix
you# show | compare
[edit services httpd]
+   config services/httpd.nix;
[edit config files]
--- /dev/null
+++ b/services/httpd.nix
…
```

`file list`, `file show <file>` (also in operational mode, of the active configuration), `file
delete <file>`. A place may hold several files: `set <place> config <file>` adds one (`set config
motd.nix` keeps `hardware-configuration.nix`), shown as `config [ a.nix b.nix ];`; `delete <place>
config <file>` unattaches one, `delete <place> config` all of them. `config-files.json` holds a list
per place (one name, the first form, still reads as a list of one).

## The commit model

- **Active configuration:** the config repo at its last commit. **Candidate:** your private copy
  (`~/.local/state/torq/candidate/`, with the commit it is *based on*), created by your first edit.
- **`commit`** sends the change from your base to your candidate as one proposal and applies it.
  If someone committed in between, the broker refuses (it checks the base): `rollback 0` and redo.
  Afterwards the candidate is gone.
- **`commit check`** evaluates and builds the candidate without switching.
- **Commit numbers count backwards:** 0 is the active configuration. They are the ledger entries
  that produced a new configuration (`init`, `applied`, `reverted`, `rolled_back`), newest first.
  Numbers shift
  with every commit, so `rollback <n>` says which commit it loaded (date, who, comment).
  `show system commit` reads the ledger, so it uses `sudo` like a commit does.
- **`rollback <n>`** loads commit <n>'s *configuration* into the candidate — every file except
  `flake.lock`, so it never downgrades the software (network devices keep the two apart, too). That
  includes `.nix` files (e.g. turning development mode back on): `show | compare` shows each one as
  a raw diff, flagged. `rollback 0` discards the candidate.
- **`request system software rollback <n>`** is the separate, explicit way to go back to commit
  <n>'s software (its `flake.lock`), marked as a downgrade.
- **`commit confirmed <minutes>`** (1–1440, default 10; with `comment "…"` in either order) is for
  changes that could cut you off:
  - the new system is *activated but not made the boot default* (`switch-to-configuration test`);
  - the window starts when the switch has finished;
  - a plain `commit` with an empty candidate confirms it: the broker makes the running system
    the boot default (no rebuild). With edits in the candidate, `commit` refuses — confirm first,
    then commit them. Only the person who made the confirmed commit confirms it (others are told
    whose it is; the broker refuses them too). As built (3c), torqcli says all this before
    evaluating, asking or proposing anything;
  - confirming is accepted until the broker has *started* the rollback (checked under its lock),
    not by comparing clocks, so a clock jump or a suspended machine can never leave a commit that
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

As built (3c/3d): `commit check` evaluates the candidate on the committed base (`git archive`, as
the broker builds it). A system change, or a change to another user's folder, shows the
comparison and needs a person's yes at a terminal; a change to your own folder does not (as
`torq pkg --yes` did; KNOWN-GAPS #4). Every value reaches the broker as `--name=value`. If the broker's
answer to a commit is lost, the ledger decides whether it was made (the candidate is discarded
only if it was). On a terminal the shell has history, Tab and `?`.

**Pasting** (how configurations travel): a paste arrives as typed — each line runs at its newline,
none is lost however the terminal sends it (with or without bracketed paste; rustyline keeps what it
read past one line). Every line of a paste runs and each error names its line, as on a network
device. A `commit` while lines have failed since you last looked at `show | compare` asks first
(`2 lines failed since you last looked at show | compare. Commit anyway? [y/N]`) — and a question
takes only an answer typed after it, so a paste can never answer it: a commit never takes part of a
paste unseen. No timing decides that. A question, or `load … terminal` asking for input, ends what
was pasted: the lines still waiting are shown as not run. While a command runs, keys typed are not
echoed over its output; they wait for the next line. Ctrl-C stops the running command (its child
process) and returns to the prompt. Scripts (`-c`, piped input) still stop at their first error.

## Operational commands

| Command | Does |
|---|---|
| `show configuration` | the active configuration |
| `show system commit` | the commit history, numbered for `rollback` |
| `show system rollback <n> [compare <m>]` | commit <n>'s configuration, or its difference to <m> |
| `show system alarms` | is this machine exactly as its history says? (and any pending confirmed commit) |
| `show system ledger [<count>]` | every change as the ledger has it, refused and failed ones too |
| `show packages available <term>` | search this machine's nixpkgs |
| `show services` | the catalog, and what is on |
| `request system update [all]` | load the newest TorqOS (and its nixpkgs) into the candidate |
| `request system software rollback <n>` | load commit <n>'s software into the candidate (a downgrade) |
| `set system software torq source <source>` | (configuration mode) where TorqOS comes from: locks that source into the candidate |
| `save <file>` | (configuration mode) the whole candidate and its config files' content to a file, private |
| `load override\|merge\|set <file>\|terminal` | (configuration mode) a saved configuration into the candidate, all or nothing; `terminal`: what you paste next, ended by Ctrl-D (brace format, or `set` lines; `#` and `/* … */` comments are left out) |
| `request system repair` | return a machine changed outside TorqOS to its last commit |
| `request system revert <change>` | undo one change (a new change forward; asks unless it is only your own folder) |
| `configure` / `config` | enter configuration mode |

Later: `commit at <time>`, `configure exclusive`. (`save` / `load`: built, S5.)

As built (3e, 4a, 4e): all of the above. The candidate holds the software too (4e): `request system
update [all]` runs `nix flake update` on the commit the candidate is based on and loads the new
`flake.lock`; `request system software rollback <n>` loads commit <n>'s; `show | compare` shows it as
`[edit system software]` (each input's date, a downgrade said so), `| display diff` as the raw
`flake.lock` diff, and `commit` (system tier: it asks) installs it. `rollback <n>` loads a configuration and keeps the
software loaded into the candidate (and says so); `rollback 0` starts over without it. Loading onto a
candidate whose base is no longer the active configuration warns that `commit` will be refused.

`show system commit` reads the ledger (4f) — through the broker, as a commit does (`sudo`: the
ledger is root's, and stays unreadable to users) — its configurations, newest first, with who
approved each; without the broker there are no numbers, rather than git's; `rollback <n>`, `show system rollback <n>`
and `request system software rollback <n>` use the same numbers (`rollback 0`: what is active). `show packages available` is `nix search` on this machine's nixpkgs.

## How it is built

- **`torqcli` is unprivileged** (ADR-014): the shell, the candidate, `show`, `commit check` and
  `rollback <n>` live in it. A commit is one `propose --base` + `apply` through `sudo torq-broker`.
- **The schema** is defined next to each setting's NixOS module (`torq.schema."<path>"`; the node
  format is documented in `modules/schema.nix`) and installed as `/etc/torq/schema.json`, so the
  shell reads it instantly for parsing, `?` and Tab. Closed sets carry their values (time zones from
  tzdata's index, catalog services, styles, the machine's users); open ones a type (`package`,
  `host-name`). `tests/tree.nix` keeps it consistent: parents exist, files and keys fit the kind,
  `users <name> …` is user tier and nothing else is, and its user-tier files are exactly the
  allowed user leaves. Syntax is checked as you type; meaning (does the package exist, is it free)
  at `commit check`.
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
- `apply --confirm <minutes>` (1–1440; done, 1f): records the anchor (previous commit and running
  system) and the kernel boot ID in the `applied` entry. In this order: builds the system (fixed
  `build` command, not the caller's), schedules the expiry, and only then activates it (`activate`,
  `switch-to-configuration test`) — so the new system never runs without a timer to undo it. The
  expiry is a transient wall-clock timer at the deadline (`--on-calendar`, `AccuracySec=1s`: it
  fires on resume if the machine slept through the deadline), retried every 15 s, at most 60 times
  in 20 minutes, while the broker is busy or the restore has not taken; it runs *this* — the old,
  known-good — broker by its store path. Instead of a separate GC root for the anchor, it requires
  the running system to be the boot default: the anchor is then the profile's current generation
  (rooted, and what a reboot starts). The new build is rooted by `state/confirm-system` until the
  change settles. If any step fails, or the broker is killed in any step, the restore goes
  straight to the anchor system, not a rebuild (`pending.json` records the anchor; the next
  operation — or the timer, once scheduled — reconciles). The apply's own activation is judged by
  its exit code, like a plain rebuild's (ADR-008): a unit that fails to start in the new system
  undoes the change at once.
- Every restore of an anchor is judged by the machine (running system and boot default are the
  anchor), not by exit codes (`switch-to-configuration` exits non-zero when a unit fails, after
  switching), and uses the anchor system's own commands and `PATH`
  (`<anchor>/etc/torq/broker.json`): the change being undone may have broken the new system's. The
  timer's broker reads the fixed file leniently: an unknown field (a newer system added it) is
  ignored, a missing one (renamed or removed) takes the default.
- `confirm`: requires the running system to be the one the commit activated, and the same boot
  (boot ID); sets the system profile to it and runs `switch-to-configuration boot` (no rebuild);
  ledger `confirmed`. Accepted until an expiry has started (under the lock), not by the clock. It
  reads what the applying broker wrote from the ledger only.
- `expire-confirm [<seq>] [--reboot]`: resets the repo to the anchor commit (keeping the discarded
  commit and any uncommitted change under `refs/torq/discarded/`), activates the anchor system
  directly unless it is already running, puts the boot default back if something moved it; ledger
  `rolled_back` with `unconfirmed: true`, `by_reboot` and what it found — once the restore has
  taken; until then it fails, stays pending and the change unsettled, so it is retried. After 40
  attempts that did not take it is ledgered `restored: false` and settled, so the machine is not
  frozen for ever; `status` reports it, and `repair` or a reboot fixes it. Nothing
  waiting is not an error (a confirmed commit's timer still fires). It is its own pending operation
  (`op: expire`): an interrupted one is finished by the next operation, not undone. The boot-time
  unit `torq-confirm-boot` (`Type=exec`: boot does not wait for it) runs `expire-confirm
  --reboot`, which acts only if the boot ID differs from the recorded one (a switch that first adds
  the unit starts it too).
- While a confirmed commit is pending, `apply`, `revert` and `repair` are refused (reconciling is
  not).
- The pending confirmation is derived from the ledger, never from a separate state file;
  `pending.json` has a version field so an older broker refuses a newer record clearly.
- `status` reports a pending confirmation (its deadline) instead of calling the boot default drift,
  and reports it as a problem once it is more than two minutes overdue.
- The broker clears its environment at start and takes its paths and rebuild commands from fixed
  configuration, not from the caller (closes part of KNOWN-GAPS #4). Done (1e): as root it reads
  only `/etc/torq/broker.json` (written by the module from `torq.broker.*`, including
  `rebuildCommand`) and refuses a flag or `TORQ_*` variable that differs, instead of ignoring it;
  without the file a root broker refuses to run. It clears its caller's environment (keeping only
  `TERM`; `LANG=C.UTF-8`), sets `PATH` and `HOME` from the file, tells git to read no global or
  system configuration, works from `/` (path arguments made absolute first) with umask 022. The
  fixed `PATH` starts with store paths of nixos-rebuild, nix, git, coreutils, bash and systemd, so a
  change that drops one of them from the system cannot break its own `revert`; it takes the place
  of a sudo `secure_path`. The CLI calls sudo and the broker by absolute path. Without root
  (tests), flags apply.
- User-tier files are an explicit allow-list in `policy.rs` (`USER_LEAVES`, kept in step with the
  module's): `users/<name>/packages.json` (`cli.json`, the CLI style, until ADR-021) — a leaf joins
  only with its typed reader. Each must be a regular file (git mode 100644) of at most 64 KiB holding
  exactly its leaf's shape (`packages.json`: a list of at most 256 names). Evaluation rejects unknown files and symlinks under `users/` and
  names that are not normal users.
- The broker builds the commit, never the working tree: `{flake}` in its commands is
  `git+file://<repo>?rev=<commit>` (H4 of the second audit, 2026-10-31).
- The configuration the broker builds is always `nixosConfigurations.torq`, never derived from the
  host name (a VM test changes the host name, then commits again).
- The single-change undo is renamed `revert <seq>`, so "rollback" means one thing.

## The broker's JSON interface (ADR-014)

`torq-broker --json <command>` prints exactly one JSON value on stdout; `log --json` one per line.
torqcli itself has no `--json` (it is for people; programs use the broker's); `torqcli --verbose`
(`-v`, before the command) prints an error with every cause and where the whole log of the last
broker or Nix command is, instead of one line.
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
prev_hash, hash`. `kind` is one of `init, proposal, applied, apply_failed, reverted,
revert_failed, interrupted, repaired, confirmed, rolled_back`. `reverted` is one change undone by
`revert`; `rolled_back` is only an unconfirmed `commit confirmed` being undone.

Exit codes: **0** success; **1** refused or failed; **75** another operation is running; **2** bad
arguments. **JSON-escaped is not terminal-safe**: the CLI neutralises every string before printing.

## Acceptance criteria

- A network engineer drives it without reading this page.
- Someone who has never seen a router installs a package, undoes it and reads the history with only
  `?` and Tab.
- No command prints a commit hash, store path, ledger kind, `Caused by:` or a git hint by default.
- Every command has a snapshot test of its output (success, refusal, failure).
- VM tests: commit confirmed expires and restores; a reboot before confirming rolls back; a stale
  candidate is refused; `rollback <n>` never changes `flake.lock`.

## Related: the terminal itself

The login shell is TorqOS's (modules/shell.nix): zsh with oh-my-zsh, autosuggestions, syntax
highlighting, fzf history search and the TorqOS prompt (modules/shell/torq.zsh-theme: who and
where, the git branch, a warning when the machine is not as its history says, a marker when a
candidate of yours is waiting; plain Unicode, no patched font). All defaults: `set programs zsh
ohMyZsh theme <any oh-my-zsh theme>`, `set users defaultUserShell …`, a person's own ~/.zshrc, or
`torq shell enable false`. torqcli itself is in colour on a terminal (cli/src/paint.rs): `[edit …]`
sections bold cyan, `+`/`-` green/red, a configuration's places bold, settings cyan, braces and `;`
dim, `## …` comments dim, ✓/⚠ green/yellow, `!` warnings yellow, `?` lists with the word cyan and
its help dim, the prompt (user, host, mode) and `error:` in colour. It is added last, to text
already cleaned of anything that could fool a terminal, so a value can never bring its own codes;
and never with NO_COLOR, TERM=dumb, or when the output is not a terminal. There is no desktop
(ADR-019).

**Layout** (cli/src/ui.rs) — one set of rules for everything torqcli prints:

- **One width:** the window's, read for each command (a resized window is followed), at most 80
  columns and at least 20 (`ui::fit`).
- **A sentence is wrapped by the code that writes it** (`Ui::line`, `ok`, `warn`, `error`, with
  `ui::hang` underneath): its next lines start where its text started (after `! `, `✓ `, `error: `,
  a commit's date and author), never back at the left edge. Beside a lead wider than 4 with under
  20 columns left, the text goes on the lines after it, indented 4. Nothing guesses at print time
  which lines are sentences.
- **Tables** (`show system ledger`, the `?` list) lay out to the same width.
- **Data is never wrapped or cut:** a configuration, `show | compare`, a diff, `| display set`;
  the terminal wraps a long line itself.
- **No window, nothing wrapped:** when a filter reads the output (`| grep`, `| wc -l` count lines)
  or it does not go to a terminal (a script), every sentence and row stays one line, as before.
- **Colour takes no room:** widths do not count colour codes. The test `every_window_size` runs
  real commands at 20, 40, 80 and 200 columns and without a window.
