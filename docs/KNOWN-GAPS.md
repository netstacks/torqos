# Known gaps

Open issues found while bringing up M1 (2026-10-30). Fix with a failing test first.

## Broker / trust model
1. ~~**Crash between commit and ledger append.**~~ Fixed (ADR-010): reconciled on the next
   operation. Still manual: leftover git locks other than `index.lock` (`refs/heads/main.lock`,
   `HEAD.lock`, `packed-refs.lock`) after power loss, and HEAD not descending from the recorded
   commit — both refuse loudly; inspect `/var/lib/kestrel/config`, fix, rerun `reconcile`. A
   process that inherits the broker lock and never exits blocks the broker until it is killed
   (the error names it). Not yet: reconcile at boot; recording who triggered a reconcile beyond
   `reconciled_by`; edits made in the repo *during* a rebuild are still reset by a failed
   apply's restore (only reconcile snapshots them). The broker runs git hooks as root (the repo
   is root-only, so planting one already needs root); consider `core.hooksPath=/dev/null`.
2. **`verify` cannot detect truncation of the newest entries** and does not cross-check git
   `Ledger-Entry` trailers against the ledger. Planned: anchor the head hash (M3).
3. **Path-based tiers cannot see semantic escalation inside system-tier files** — e.g.
   `kestrel.broker.enable = false`, a new `kestrel.broker.stateDir` (fresh empty ledger),
   `kestrel.security.lockdown = "none"`, or swapping `kestrel.url` in `flake.nix`. Today only
   human review of the diff catches these. Needs a design (evaluate the old and new config and
   diff protected options?).
4. **Caller-supplied identity and configuration:** `--actor`, `--tainted`, `--approved-by`,
   `--repo`, `--state-dir` and `KESTREL_REBUILD_CMD` are trusted as given (under sudo, the
   last three let a wheel user point a root broker anywhere; they must come from fixed config
   before any narrow sudoers rule or the socket). Fine under sudo in M1; the M3 socket must take
   identity from `SO_PEERCRED` and the rebuild command from fixed config. `kestrel pkg` inherits
   this: it passes `--actor user --approved-by $(id -un)` itself, so anything running as the user
   (a script, an AI tool in their terminal) can approve a user-tier change with `--yes`; it does
   refuse `--yes` for system tier and asks first, but that prompt is in the same process. It also
   needs `sudo` for the broker, so only wheel users can use it until the socket exists.
5. **`kestrel.url = "path:…"`** in the config template ignores `.gitignore`, so `broker/target/`
   is copied into the store on every evaluation. Prefer `git+file://`. Related: the rebuild
   builds the config repo's *working tree*; building the exact ledgered commit
   (`git+file://…?rev=<commit>`) would make "running system = ledgered commit" hold by
   construction instead of by fsync discipline.

6a. **User tier is not bound to a user:** `users/alice.packages.json` is user tier whoever proposes
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
6c. **`kestrel update` trusts the user's fetch:** `nix flake update` runs as the user, so their
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
   `xwayland-satellite` (X11 apps fail), no cursor theme. Ship a Kestrel niri config.
8. ~~Template hard-codes user `casey`.~~ Fixed: `kestrel.user` in `system.nix` is the one place
   (asserted set and well-formed); per-user apps are `users/<name>.packages.json`.

## UTM VM quirks (not Kestrel bugs)
- niri needs 3D: UTM display card must be `virtio-gpu-gl-pci` with GPU acceleration on
  (renderer ANGLE/Metal). Without it: black screen, "software EGL renderers are skipped".
- Firefox's GPU WebRender drops most text on virgl. Workaround: `LIBGL_ALWAYS_SOFTWARE=1 firefox`.
- On a Mac keyboard, niri's Mod is ⌘: ⌘⇧Q logs out of macOS and ⌘Q quits UTM. Use UTM's
  "Capture input" before using niri shortcuts.

## Dev mode (`kestrel.dev.enable`)
- **Claude Code's own installer and `claude update` do not work on NixOS.** The native binary is
  a plain dynamically-linked ELF (dies with the `stub-ld` error) and the installer edits
  `~/.bashrc` to put it first on `PATH`, which breaks `claude` in every new terminal. Instead,
  `modules/dev.nix` pins the version by overriding nixpkgs' release manifest with
  `modules/claude-code-manifest.json`. To update, fetch a newer manifest (the commands are in
  `dev.nix`) and ship it through the broker. Running sessions keep their old code until relaunched.
