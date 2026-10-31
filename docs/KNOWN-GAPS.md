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
   apply's restore (only reconcile snapshots them). The broker runs git hooks as root (the repo
   is root-only, so planting one already needs root); consider `core.hooksPath=/dev/null`.
2. **`verify` cannot detect truncation of the newest entries** and does not cross-check git
   `Ledger-Entry` trailers against the ledger. Planned: anchor the head hash (M3).
3. **Path-based tiers cannot see semantic escalation inside system-tier files** — e.g.
   `torq.broker.enable = false`, a new `torq.broker.stateDir` (fresh empty ledger),
   `torq.security.lockdown = "none"`, or swapping `torq.url` in `flake.nix`. Today only
   human review of the diff catches these. Needs a design (evaluate the old and new config and
   diff protected options?).
4. **Caller-supplied identity:** `--actor`, `--tainted` and `--approved-by` are trusted as given.
   (Configuration no longer is: a root broker takes its repo, state directory, rebuild command and
   PATH/HOME only from `/etc/torq/broker.json`, refuses a flag or `TORQ_*` variable that differs,
   and clears its caller's environment — ADR-016, piece 1e.) Fine under sudo in M1; the M3 socket
   must take identity from `SO_PEERCRED`. `torq pkg` inherits
   this: it passes `--actor user --approved-by $(id -un)` itself, so anything running as the user
   (a script, an AI tool in their terminal) can approve a user-tier change with `--yes`; it does
   refuse `--yes` for system tier and asks first, but that prompt is in the same process. It also
   needs `sudo` for the broker, so only wheel users can use it until the socket exists.
   Under `torqcli` (ADR-016) the same holds for `commit`: a system-tier commit needs a terminal,
   which stops background jobs but not anything started from the user's terminal. The per-user
   candidate is user-writable, so its integrity between `show | compare` and `commit` rests on the
   user's account; the agent must never write a user's candidate (it proposes as `actor=agent`).
5. **`torq.url = "path:…"`** in the config template ignores `.gitignore`, so `broker/target/`
   is copied into the store on every evaluation. Prefer `git+file://`. Related: the rebuild
   builds the config repo's *working tree*; building the exact ledgered commit
   (`git+file://…?rev=<commit>`) would make "running system = ledgered commit" hold by
   construction instead of by fsync discipline.

6a. **User tier is not bound to a user:** `users/alice/packages.json` is user tier whoever proposes
   it, so an agent session acting for viper could change alice's apps. Needs the caller identity
   from #4 (M3 socket). A user's packages also act in that user's *session*: autostart entries,
   D-Bus session services and systemd user units from their profile (never system-wide).

6b. **Drift detection has blind spots** (ADR-012): a lock that never frees (a leaked holder) makes
   every check "busy" and silent; drift is reported but not written into the ledger, so an outside
   switch followed by a broker apply leaves no trace in the audit trail (`repair` does ledger the
   drift it fixes); the warning reaches only interactive shells, not a desktop user (needs the
   Quickshell UI). `repair` checks after its switch that the running system and next boot agree and
   that no other switch is running, but an outside switch that both starts and finishes between
   its own switch and that check would set both alike and pass; comparing against the exact path it
   built would close that.
6e. **OPEN (high): an outside commit made while a change is interrupted is discarded untraced.**
   If HEAD has moved past the interrupted operation's own commit (someone committed on top of it),
   `torq status` shows only "a change was interrupted" (the CLI hides head findings while
   interrupted) and `--repair` → `reconcile_locked` keeps and ledgers only `pending.commit`, then
   resets: the outside commit survives only in the reflog. Fix: broker `keep(head)` and ledger it
   (`discarded_head`) when `head != pending.commit`, with a test; expose `interrupted.commit` in
   `status --json`; the CLI hides the head finding only when HEAD is `before` or that commit.
   Also low: `history::asked_by` credits a failed/interrupted *undo* to the original change's
   actor; undo is user-only, so rollbacks should show "user". (Found by review of f2da8b7.)
6f. **Nothing evaluates `templates/config` itself.** The VM test checks the broker builds `#torq`,
   but no `nix flake check` entry builds a config made from the template (it needs a hardware
   file and a user name), so a template that stopped exposing `nixosConfigurations.torq` would
   only show at the next install. (`tests/tree.nix` does read the template's data files.) A config
   repo made from an older template (output `torq-utm`) must be renamed to `torq` before its broker
   is updated; only the dev VM ever had one. Such a repo also sets `networking.hostName` in
   `machine.nix`: adding `system/host.json` with a host name to it is a conflict until that line
   goes (the template now keeps the host name only in `system/host.json`).
6g. **`propose --base` is optional.** Without it a diff applies wherever it applies, so a stale
   change can still mix with a newer commit. `torqcli` will always pass it; once the agent exists,
   the broker should require a base for `--actor agent` (and for anything approved from a shown
   diff). The base must be a 40-hex SHA-1 id: a config repo created with git's SHA-256 object
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
   bracketed paste is kept whole and run line by line. `show system commit` lists git history, not
   the ledger (which needs the broker); two interactive sessions of the same user share one saved
   candidate, the last save wins.
6i. **User tier: whose folder?** The broker accepts `users/<name>/<leaf>` for any well-formed name;
   that `<name>` is a normal user of the machine is checked only at evaluation (modules/packages.nix),
   so `users/root/cli.json` is refused late — by the failed rebuild, which is undone — not at
   propose. And a user-tier change may write *any* user's folder: `approved_by` is not tied to
   `<name>`. Today that only lets one user change another's package list or CLI style; before a
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
6c. **`torq update` trusts the user's fetch:** `nix flake update` runs as the user, so their
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

## Security defaults that are not real yet
6. **`lockdown=integrity` does nothing:** the kernel command line has it, but the lockdown LSM is
   not active (`/sys/kernel/security/lsm` = `capability,landlock,yama,bpf,ima`). Module signing
   is not built into the kernel either, so "signed modules only" (SECURITY-MODEL.md) is not true.

## Desktop
7. niri runs its stock config: `Mod+T` spawns `alacritty` (not installed; we ship `foot`), no
   `xwayland-satellite` (X11 apps fail), no cursor theme. Ship a TorqOS niri config.
8. ~~Template hard-codes user `casey`.~~ Fixed: `torq.user` in `system.nix` is the one place
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
