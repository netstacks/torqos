# Known gaps

Open issues found while bringing up M1 (2026-10-30). Fix with a failing test first.

## Broker / trust model
1. ~~**Crash between commit and ledger append.**~~ Fixed (ADR-010): reconciled on the next
   operation. Still manual: leftover git locks other than `index.lock` (`refs/heads/main.lock`,
   `HEAD.lock`, `packed-refs.lock`) after power loss, and HEAD not descending from the recorded
   commit — both refuse loudly; inspect `/var/lib/torq/config`, fix, rerun `reconcile`. A
   process that inherits the broker lock and never exits blocks the broker until it is killed
   (the error names it). Not yet: reconcile at boot; recording who triggered a reconcile beyond
   `reconciled_by`; edits made in the repo *during* a rebuild are still reset by a failed
   apply's restore (only reconcile snapshots them). The broker's git runs no hook and no fsmonitor of
   the repo (`core.hooksPath=/dev/null`, `core.fsmonitor=false`; second audit, M1: fixed).
2. **`verify` cannot detect truncation of the newest entries, and the chain is unkeyed** (second
   audit, H2): it is sha256 over each entry and the one before, so a writer of the file (root)
   can rewrite the history and recompute the chain; it does not cross-check git `Ledger-Entry`
   trailers against the ledger, and commits are not signed (L1). What it does catch: an edited,
   added or removed field or entry (an added field passed before the audit of 2026-10-31).
   Planned (M3): sign the entries and commits with a broker key, anchor the head hash.
3. **Path-based tiers cannot see semantic escalation inside system-tier files** — e.g.
   `torq.broker.enable = false`, a new `torq.broker.stateDir` (fresh empty ledger),
   `torq.security.lockdown = "none"`, or swapping `torq.url` in `flake.nix`. Today only
   human review of the diff catches these. Needs a design (evaluate the old and new config and
   diff protected options?).
4. **Caller-supplied identity:** `--actor`, `--tainted` and `--approved-by` are trusted as given.
   (Configuration no longer is: a root broker takes its repo, state directory, rebuild command and
   PATH/HOME only from `/etc/torq/broker.json`, refuses a flag or `TORQ_*` variable that differs,
   and clears its caller's environment — ADR-016, piece 1e.) Fine under sudo in M1; the M3 socket
   must take identity from `SO_PEERCRED`. `torqcli` inherits this: it passes `--actor user
   --approved-by $(id -un)` itself, so anything running as the user (a script, an AI tool in their
   terminal) can commit a change to their own folder (`torqcli -c`) or revert one; it needs `sudo`
   for the broker, so only wheel users can use it until the socket exists. A system-tier commit
   (or revert) needs a terminal,
   which stops background jobs but not anything started from the user's terminal. The per-user
   candidate is user-writable, so its integrity between `show | compare` and `commit` rests on the
   user's account; the agent must never write a user's candidate (it proposes as `actor=agent`).
   The second audit (H1, H5) is this item: the approver and the actor are whoever the caller says,
   so ledger attribution, and the agent's restrictions (which key on `--actor agent`), hold only
   for callers that do not lie. Not fixed by patches: the M3 socket takes identity from the kernel.
   The broker under sudo is not sandboxed either (L3; the socket service will be).
5. **`torq.url = "path:…"`** in the config template ignores `.gitignore`, so `broker/target/`
   is copied into the store on every evaluation. Prefer `git+file://`. (Fixed for the config repo
   itself: the broker builds the exact commit, `git+file://<repo>?rev=<commit>` (`{flake}`), never
   its working tree, so "running system = ledgered commit" holds by construction; second audit,
   H4.) A `path:` source (the dev VM's)
   is allowed only with `torq dev enable true` (second audit, M3).

6a. **User tier is not bound to a user:** `users/alice/packages.json` is user tier whoever proposes
   it, so an agent session acting for viper could change alice's apps. Needs the caller identity
   from #4 (M3 socket). A user's packages also act in that user's *session*: autostart entries,
   D-Bus session services and systemd user units from their profile (never system-wide).

6b. **Drift detection has blind spots** (ADR-012): a lock that never frees (a leaked holder) makes
   every check "busy" and silent; drift is reported but not written into the ledger, so an outside
   switch followed by a broker apply leaves no trace in the audit trail (`repair` does ledger the
   drift it fixes); the warning reaches interactive shells (and `show system alarms`) only. `repair` checks after its switch that the running system and next boot agree and
   that no other switch is running, but an outside switch that both starts and finishes between
   its own switch and that check would set both alike and pass; comparing against the exact path it
   built would close that.
6e. **OPEN (high): an outside commit made while a change is interrupted is discarded untraced.**
   If HEAD has moved past the interrupted operation's own commit (someone committed on top of it),
   `show system alarms` shows only "a change was interrupted" (the CLI hides head findings while
   interrupted) and `request system repair` → `reconcile_locked` keeps and ledgers only `pending.commit`, then
   resets: the outside commit survives only in the reflog. Fix: broker `keep(head)` and ledger it
   (`discarded_head`) when `head != pending.commit`, with a test; expose `interrupted.commit` in
   `status --json`; the CLI hides the head finding only when HEAD is `before` or that commit.
   Also low: `history::asked_by` credits a failed/interrupted *undo* to the original change's
   actor; undo is user-only, so rollbacks should show "user". (Found by review of f2da8b7.)
6f. **The template's flake.nix is not evaluated.** tests/tree.nix evaluates the template as a whole
   machine (its data, its hardware file attached, an owner set, as setup does: S4a), but through
   `torq.lib.mkSystem`, not the template's own flake.nix, so a flake.nix that stopped exposing
   `nixosConfigurations.torq` would only show at the next install. A config
   repo made from an older template (output `torq-utm`) must be renamed to `torq` before its broker
   is updated; only the dev VM ever had one. Such a repo also sets `networking.hostName` in
   `machine.nix`: adding `system/host.json` with a host name to it is a conflict until that line
   goes (the template now keeps the host name only in `system/host.json`).
6g. **`propose --base` is optional for a person.** Without it a diff applies wherever it applies,
   so a stale change can still mix with a newer commit. `torqcli` always passes it; the broker
   requires it for `--actor agent` (second audit, M2), and nothing builds on a commit that is not
   the last ledgered one (audit of 2026-10-31). The base must be a 40-hex SHA-1 id: a config repo created with git's SHA-256 object
   format could not use it.
6j. **torqcli and a malformed tree file.** torqcli refuses to load a tree file it cannot show
   faithfully (wrong shape, unknown key, a value with spaces), so one bad file blocks `configure`
   and `commit` for everyone until it is fixed — by a raw diff through `torq-broker propose`.
   Evaluation normally keeps such files out of HEAD (a change that does not evaluate is undone), so
   it takes a hand-made system-tier change. (Tested in the VM: an ordinary user reads the root-owned
   repo, and a hostile `GIT_*` environment is ignored.) Not yet tested end to end: a `torqcli`
   commit through the real broker — the test VM's config repo does not evaluate; the cutover's
   config repo will.
6k. **torqcli's line editor drops keys typed ahead.** rustyline reads input in chunks and starts each
   prompt with a fresh buffer: keys typed (not pasted) before the next prompt appears are lost. A
   bracketed paste is kept whole and run line by line. Two interactive sessions of the same user
   share one saved candidate, the last save wins.
6i. **User tier: whose folder?** The broker accepts `users/<name>/<leaf>` for any well-formed name;
   that `<name>` is a normal user of the machine is checked only at evaluation (modules/packages.nix),
   so `users/root/packages.json` is refused late — by the failed rebuild, which is undone — not at
   propose. And a user-tier change may write *any* user's folder: `approved_by` is not tied to
   `<name>`. Today that only lets one user change another's package list; before a
   leaf that matters more joins the user tier, tie user-tier approval to the folder's owner (with
   the socket's `SO_PEERCRED`, #4) and check the name against the machine's users at propose.
6h. **`commit confirmed` limits.** The timer's broker still takes its repo and state directory
   from the *new* system's `/etc/torq/broker.json` (read leniently); its restore commands and PATH
   are the anchor's own. A change that moves `torq.broker.configRepo` or `stateDir` would leave the
   timed rollback looking in the wrong place (taking them from the anchor's file would mean trusting
   a settings path from the caller); a reboot still rolls it back (the boot default is never
   changed before `confirm`). `--confirm` builds the config again even if `commit check` just built
   it (cached by nix, but evaluated twice). "Boot default" is the system profile, not the boot
   loader's entry: a profile set while the boot-loader install failed counts as restored. The
   activation is not run under `systemd-run` as `nixos-rebuild` does: a change that kills the
   user's session kills the broker mid-switch (recovered by reconcile, but noisy).
6c. **`request system update` trusts the user's fetch:** `nix flake update` runs as the user, so their
   environment (proxy, CA, `NIX_CONFIG`) shapes the narHash written to the lock, and root builds
   that store path without re-fetching. The ledgered rev may then not identify what was built.
   Fix: the broker re-fetches changed locked inputs as root and compares narHashes.

6d. **Service catalog limits** (ADR-013): the SSH key check and the key listing run in the CLI only;
   an agent proposing `+ "ssh"` skips them (the broker cannot see home directories) — keep
   `~/.ssh` out of any future user-tier dotfiles, or "plant a key, then enable ssh" becomes
   possible. Tailscale keeps its own settings (Tailscale SSH, exit node) in `/var/lib/tailscale`,
   outside the ledger, and re-enabling it rejoins with the old identity. Firmware updates (fwupd)
   are not offered until they can go through the broker (SECURITY-MODEL.md). The terminal-safe
   printing is duplicated in the broker and the CLI (`printable` / `history::clean`). The SSH key
   listing shows authorized_keys files and declared keys only, not other ways sshd can admit
   someone (`AuthorizedKeysCommand`, `TrustedUserCAKeys`, `AuthorizedPrincipalsFile`), which the
   catalog does not pin either.

6n. **A mistake in how the machine starts shows only at the next restart** (review of S4a). The
   tree sets `fileSystems`, `boot` (loader, initrd modules) and `swapDevices` like any setting, and
   the attached hardware-configuration.nix is plain Nix: a wrong but well-formed value (`fileSystems
   /boot fsType ext4`) builds, switches, and fails at the next boot, into an emergency shell.
   `commit confirmed` cannot help (its timer runs on the new system, which never comes up). torqcli
   says so in the commit question; the way back is an earlier configuration in the boot menu (20
   kept), which needs the machine's console, and `boot loader timeout 0` hides the menu. To do: make
   the new boot entry one-shot until a confirmed commit is confirmed; refuse a zero timeout.

6o. **A flake.lock that does not match flake.nix is fetched before it is refused** (review of S4b).
   The broker builds with `--no-update-lock-file`, so such a commit never builds, but to find out
   that the lock needs changing Nix first fetches the new source, as root (its `inputs` are read,
   nothing is built). torqcli never commits a mismatch (a lock belongs to the flake.nix it is
   committed with); a hand-made proposal still could.

6p. **"TorqOS's fixed flake.nix" is recognised by its one `torq.url` line** (audit of 2026-10-31).
   torqcli does not check the rest of it (outputs, `configDir`, no extra modules or inputs): a
   flake.nix changed by hand beyond that line is still shown as TorqOS's own. It is changed only
   through the broker, as a system-tier change a person reads.
6q. **`| display set` loads back, with three exceptions:** a value that is exactly `|`, or a list
   value that is exactly `[` or `]`, is refused when read back (a quoted word is not told apart from
   the syntax). `save` / `load override` keep them. `load set` saves the candidate as it goes:
   all or nothing for errors, not for a crash or a full disk midway.
6r. **The config repo is readable by every user** (0755/0644; second audit, L4), and so is the Nix
   store it is built into: a secret must never be in it. M2's first-boot setup must keep passwords
   (even hashes, which can be cracked offline) out of the repo, and its images need checksums
   or signatures of their own (a signed binary cache covers store paths, not a qcow2 or ISO).

## Security defaults that are not real yet
6. **`lockdown=integrity` does nothing:** the kernel command line has it, but the lockdown LSM is
   not active (`/sys/kernel/security/lsm` = `capability,landlock,yama,bpf,ima`). Module signing
   is not built into the kernel either, so "signed modules only" (SECURITY-MODEL.md) is not true.

## Desktop (off since ADR-019; kept for later)
7. niri runs its stock config: `Mod+T` spawns `alacritty` (not installed; we ship `foot`), no
   `xwayland-satellite` (X11 apps fail), no cursor theme. Ship a TorqOS niri config.
8. ~~Template hard-codes user `casey`.~~ Fixed: `torq user` (nixos/torq.json) is the one place
   (asserted set and well-formed); per-user apps are `users/<name>/packages.json`.

## UTM VM quirks (not TorqOS bugs)
- niri needs 3D: UTM display card must be `virtio-gpu-gl-pci` with GPU acceleration on
  (renderer ANGLE/Metal). Without it: black screen, "software EGL renderers are skipped".
- Firefox's GPU WebRender drops most text on virgl. Workaround: `LIBGL_ALWAYS_SOFTWARE=1 firefox`.
- On a Mac keyboard, niri's Mod is ⌘: ⌘⇧Q logs out of macOS and ⌘Q quits UTM. Use UTM's
  "Capture input" before using niri shortcuts.

## Dev mode (`torq.dev.enable`)
- **Claude Code's own installer and `claude update` do not work on NixOS.** The native binary is
  a plain dynamically-linked ELF (dies with the `stub-ld` error) and the installer edits
  `~/.bashrc` to put it first on `PATH`, which breaks `claude` in every new terminal. Instead,
  `modules/dev.nix` pins the version by overriding nixpkgs' release manifest with
  `modules/claude-code-manifest.json`. To update, fetch a newer manifest (the commands are in
  `dev.nix`) and ship it through the broker. Running sessions keep their old code until relaunched.
6l. **`show system alarms` has no exit code of its own.** `torq status` exited 1 when the machine was
   not as its ledger says and 75 while a change was being made; `torqcli show system alarms` says it
   in words and exits 0. Scripts and monitoring use `sudo torq-broker --json status` (its `ok`,
   `busy`, exit codes), which is what `torq status` read. Pending-change detail (`torq log <n>`) has
   no torqcli view yet beyond `show system ledger`. (Found by review of 4g.)
6m. **The fence is not the AI's boundary.** modules/settings-fence.nix keeps the broker's own inputs
   out of the configuration tree's data and refuses module directives (`_type`) in it, but settings
   that run code as root — a service's script, sudo rules, wrappers, tmpfiles, activation scripts,
   binary caches and their keys, extra groups — stay settable: ordinary system-tier power when a
   person approves the change, and a list of forbidden paths can never be complete. What an agent
   may propose has to be a default-deny allow-list at the broker, by top-level and subtree, keyed on
   the agent's identity (#4, M3). (Found by review of S3a, which also showed the bypasses now
   tested in tests/tree.nix.)
   Config files (`file edit`, `set <place> config <file>`, S3e) are plain Nix and are not fenced at
   all: like system.nix always was, they can set anything, the broker's settings included. A person
   approves each one as its raw diff; an agent proposing one is for the same M3 allow-list (a
   config file is a whole module, so the allow-list will have to refuse them to agents outright).
