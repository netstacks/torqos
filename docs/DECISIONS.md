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
- **Decision:** `/var/lib/torq/config` is a local repo. Optional user-chosen encrypted backup.
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
  `torq-broker-init` runs only once (`ConditionPathExists=!…/.git`), never during a switch.
- **Consequences:** A failed change costs a second (usually cached) switch. If the restore also
  fails, the ledger says so explicitly rather than claiming the system is unchanged.

## ADR-009 — Development mode: Claude Code with full access, behind one switch
- **Context:** While TorqOS is being built, the fastest loop is Claude Code running *inside*
  the dev VM with root — editing `~/torq`, building with Nix, applying via the broker.
  Security during development is explicitly not a goal; being able to shut it off later is.
- **Decision:** `torq.dev.enable` (default **false**, `modules/dev.nix`): passwordless sudo
  for wheel, `claude-code` + `gh`, owner as trusted Nix user. Enabled only in the dev VM's
  config repo. This is a cloud agent with root, so it is a deliberate, temporary exception to
  ADR-003 (agent only proposes) and ADR-005 (local models by default) — for *building* TorqOS,
  not for the TorqOS agent itself.
- **Consequences:** Every rebuild prints a warning while it is on. Shutting AI access off is
  a single broker commit (`torq.dev.enable = false`), itself ledgered and undoable. The
  shipped product and all tests run with it off.

## ADR-010 — A broker that dies mid-operation is reconciled, never left half-done
- **Context:** apply/rollback commit *before* the minutes-long rebuild and ledger *after* it. A
  broker killed in between (OOM, Ctrl-C under sudo, power loss) left a commit nobody ledgered:
  the next apply failed ("does not apply"), rollback failed (empty commit), and an unaudited
  change stayed live. Killing the broker also does not stop `nixos-rebuild`, which may still
  switch the system after a retry has "restored" it.
- **Decision:**
  - The rebuild gets the broker lock as its stdin, so its whole process tree — including the
    `systemd-run --pipe` switch unit — holds the lock until the last process exits. git writes are
    fsynced (`core.fsync=all`, then `sync --file-system` on the repo).
  - apply/rollback record themselves in `state/pending.json` before committing, add the commit as
    soon as it exists, and remove the record once the outcome is ledgered.
  - The next apply/rollback, or `torq-broker reconcile --approved-by <name>`, finishes an
    interrupted operation: it keeps the unledgered commit (and any uncommitted changes) under
    `refs/torq/discarded/`, resets to the last ledgered commit, **always** rebuilds, and ledgers
    an `interrupted` entry. `propose` (the agent's entry point) and `init` never rebuild: they
    refuse until reconciled.
- **Consequences:** An interrupted change is never silently kept; it is discarded, recorded and
  can be re-applied. A reconcile costs one rebuild. Some states still need a human (see
  `KNOWN-GAPS.md`): a broken ledger, a HEAD that does not descend from the recorded commit, or
  leftover git lock files other than `index.lock`. Until the boot-time hook lands, a machine that
  lost power mid-switch runs whatever generation it booted until the next broker command.

## ADR-011 — Installed software is data in the config repo, not Nix code
- **Context:** Installing software is the most common change anyone makes, and the agent will make
  it too. Having tools edit Nix lists inside `.nix` files is fragile, and `users/<name>.nix` was
  user tier even though it is a full NixOS module (sudo rules, services: an escalation path).
- **Decision:** `packages.json` (system tier) and `users/<name>.packages.json` (user tier) are JSON
  lists of nixpkgs attribute paths, read by `modules/packages.nix` when the config flake passes
  `torq.lib.mkSystem { configDir = ./.; }`. User tier is that exact data-file shape and
  nothing else: any `.nix` file is system tier wherever it lives (`broker/src/policy.rs`). Names
  are a top-level attribute or one level under a known package set; unfree/broken/unsupported
  packages are refused at evaluation with a clear message. `torq pkg` (unprivileged) and later
  the agent's packages MCP server change them by proposing a diff through the broker.
- **Consequences:** A package change is a one-line JSON diff anyone can review, ledgered and
  undoable like any other. A user-tier change can only add nixpkgs packages to that user's profile
  (nothing is linked system-wide). GUI apps via Flatpak (ARCHITECTURE.md) remain a later layer.

## ADR-012 — Drift from the ledger is detected and reported, never silently corrected
- **Context:** Another tool switched the dev VM to a stock NixOS config outside the broker, and
  nothing noticed until things broke. The ledger said one system was running; another was.
- **Decision:** Every rebuild — and `init` — records the system it leaves running.
  `torq-broker status` compares the machine with its ledger: running system and next boot vs the
  last record, config repo HEAD and cleanliness vs the last ledgered commit, failed restores,
  interrupted operations, the chain. It runs under the broker lock (try-lock: mid-switch it reports
  `busy`, exit 75, and compares nothing); operations wait up to 3 s for the lock, so a check never
  fails them. A `torq-status` unit runs it at boot and every 15 minutes, writes problems to
  `/run/torq/drift` (printed by every new interactive shell) and to the journal at warning
  priority, and **never fails**: it can run during a switch, and a failed unit would fail the switch,
  which the broker treats as a failed rebuild and rolls back (ADR-008).
- **Consequences:** Drift is visible within 15 minutes, at boot and in every new shell, but nothing
  is changed automatically (the one exception: an unconfirmed `commit confirmed`, which the person
  consented to in advance, ADR-016): the person decides — `torq status --repair` (the broker's `repair`:
  rebuild the last ledgered configuration, refuse if the config repo itself was changed, record what
  had drifted, and say so if the switch did not take), or apply, rollback, reconcile. Garbage collection
  (`nix.gc`, weekly, generations older than 30 days; `min-free` during builds) is safe for the same
  reason undo works: a broker rollback reverts the commit and *rebuilds*, it never needs an old
  generation on disk.

## ADR-013 — Services are a curated catalog, enabled as data
- **Context:** After software, the most common change is turning a service on or off (SSH,
  containers, VPN). Doing that meant hand-writing a NixOS diff, and the stock NixOS defaults are
  not always TorqOS's (SSH allows passwords and root by default).
- **Decision:** `services.json` in the config repo (system tier) lists names from a catalog in
  `modules/services.nix`; each entry is a service *with TorqOS's defaults* (SSH: keys only, no root
  login, only the owner; Podman: rootless, a `docker` command, no root API socket) and says what
  enabling it implies, because the approved diff is only `+ "name"`. The catalog's settings are
  asserted (a `mkForce` elsewhere fails the build), and SSH enabled outside the catalog with
  passwords is a build warning and is reported by `torq service list`. No `fwupd` yet: firmware
  must go through the broker. `torq service list|enable|disable` changes
  it through the broker like packages (ADR-011). Unknown names are a type error listing the
  catalog. Enabling SSH lists the owner's keys (both `~/.ssh/authorized_keys` and
  `/etc/ssh/authorized_keys.d`) for the person approving, and refuses when there are none.
- **Consequences:** Common services are one confirmed, ledgered, undoable command. Anything outside
  the catalog is still a system-tier `.nix` change. The catalog grows deliberately, one reviewed
  entry at a time.

## ADR-014 — The broker speaks data; presentation lives in the CLI
- **Context:** The CLI prints the broker's output (git diffs, hashes, ledger kinds) and scrapes its
  text for numbers (`proposal #N`). That is both ugly and fragile, and making the broker "pretty"
  would grow the privileged code the project wants small and auditable.
- **Decision:** The broker keeps its plain text for people who run it directly and gains `--json`
  output for every command the CLI uses. All formatting, colour, progress, wording and help live in
  the unprivileged `torq` CLI (`docs/CLI-DESIGN.md`).
- **Consequences:** Polishing the experience never touches privileged code. The JSON shapes become
  an interface: changes to them are deliberate and tested on both sides.

## ADR-015 — The project is named TorqOS (code: `torq`); renamed from the placeholder "Kestrel"
- **Context:** "Kestrel" was a placeholder codename (and an existing nixpkgs package name).
- **Decision:** TorqOS in prose, `torq` for commands, code, options, paths and units, `TORQ_*` for
  environment variables; GitHub `netstacks/torqos`; the dev VM's host is `torq-utm`. A clean
  replace, no compatibility aliases: the dev VM was the only install.
- **Consequences:** The dev VM is cut over with a fresh config repo and a fresh ledger under
  `/var/lib/torq`; its old history is not migrated. Anything still saying Kestrel is either history
  (old ledger, git log) or a miss.

## ADR-016 — TorqOS is driven like a network device: `torqcli`, a candidate, commit confirmed
- **Context:** Commands applied each change immediately, and undo reverted one old change, which
  conflicts when later changes touched the same file. The owner is a network engineer; the network
  device model (a shell with operational and configuration modes, a candidate configuration,
  `show | compare`, `commit check`, `commit confirmed`, `rollback <n>`) is the best-tested answer to
  "change a live system safely".
- **Decision:**
  - `torqcli` is the shell (interactive, or one command per invocation for scripts). Two syntaxes
    over one model — Juniper-style and Cisco-style — switched with `switch cli` (as in Cisco NSO);
    the default is a setting. The earlier `torq pkg/service/update/log/undo/status` are removed.
  - The configuration is a tree whose containers are the config repo's folders and whose leaves are
    small data files; each setting is declared in a schema next to its NixOS module and installed
    with the system (help, completion, syntax). Tiers follow the paths: `users/<name>/` is user tier.
  - Edits go into a per-user candidate that remembers its base commit; `commit` applies the change
    from base to candidate as one ledgered change, and the broker refuses it if the base is stale.
    A system-tier commit prints exactly what it commits and needs a terminal.
  - Commit numbers count backwards (0 = active). `rollback <n>` restores commit <n>'s
    configuration but never `flake.lock`; going back in software is the separate
    `request system software rollback <n>`.
  - `commit confirmed <minutes>` activates without changing the boot default; `commit` confirms
    (makes it the boot default, no rebuild); otherwise the broker restores the previous system
    directly from its store path, from a transient timer running the previous (known-good) broker;
    a reboot before confirming is itself the rollback. While one is pending, no other change can be
    made.
- **Consequences:** Most of this is unprivileged CLI work. The broker gains `propose --base`,
  `apply --confirm`, `confirm`, `expire-confirm`, the pending-confirm interlock and boot check, the
  ledger kind `confirmed`, JSON-only user-tier files at `users/<name>/<leaf>.json`, and renames
  single-change undo to `revert`. Rollback is always a new change forward, so it cannot conflict.
  An automatic rollback of an unconfirmed commit is consented to in advance by `commit confirmed`
  and ledgered; it is not the silent drift correction ADR-012 rules out.
