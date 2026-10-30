# Known gaps

Open issues found while bringing up M1 (2026-10-30). Fix with a failing test first.

## Broker / trust model
1. **Crash between commit and ledger append** leaves a commit whose `Ledger-Entry: N` is then
   reused by the next ledger entry. Needs an intent entry written before the commit, or a
   reconcile step on start.
2. **`verify` cannot detect truncation of the newest entries** and does not cross-check git
   `Ledger-Entry` trailers against the ledger. Planned: anchor the head hash (M3).
3. **Path-based tiers cannot see semantic escalation inside system-tier files** — e.g.
   `kestrel.broker.enable = false`, a new `kestrel.broker.stateDir` (fresh empty ledger),
   `kestrel.security.lockdown = "none"`, or swapping `kestrel.url` in `flake.nix`. Today only
   human review of the diff catches these. Needs a design (evaluate the old and new config and
   diff protected options?).
4. **Caller-supplied identity:** `--actor`, `--tainted`, `--approved-by` and
   `KESTREL_REBUILD_CMD` are trusted as given. Fine under sudo in M1; the M3 socket must take
   identity from `SO_PEERCRED` and the rebuild command from fixed config.
5. **`kestrel.url = "path:…"`** in the config template ignores `.gitignore`, so `broker/target/`
   is copied into the store on every evaluation. Prefer `git+file://`.

## Security defaults that are not real yet
6. **`lockdown=integrity` does nothing:** the kernel command line has it, but the lockdown LSM is
   not active (`/sys/kernel/security/lsm` = `capability,landlock,yama,bpf,ima`). Module signing
   is not built into the kernel either, so "signed modules only" (SECURITY-MODEL.md) is not true.

## Desktop
7. niri runs its stock config: `Mod+T` spawns `alacritty` (not installed; we ship `foot`), no
   `xwayland-satellite` (X11 apps fail), no cursor theme. Ship a Kestrel niri config.
8. Template hard-codes user `casey`; should take the username from one place.

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
