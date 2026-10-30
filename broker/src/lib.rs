//! kestrel-broker: the only writer of the Kestrel config repo.
//!
//! Lifecycle: propose (validated + ledgered, diff stored in the ledger) → apply (approved,
//! committed, rebuilt, ledgered) → rollback (reverted, rebuilt, ledgered).
//! If a rebuild fails, the repo is reset to the previous commit, that commit is rebuilt (the
//! failed rebuild may already have activated), and the failure is ledgered.

pub mod ledger;
pub mod policy;
pub mod repo;

use anyhow::{bail, Context, Result};
use ledger::{Entry, Ledger, NewEntry};
use policy::{Actor, Tier};
use repo::Repo;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::os::fd::AsFd;
use std::process::{Command, Stdio};

pub struct Broker {
    pub repo: Repo,
    pub ledger: Ledger,
    state_dir: PathBuf,
    /// Shell command run after each commit. `{repo}` is replaced with the repo path.
    rebuild_cmd: String,
    /// The link to the running system (`/run/current-system`), recorded after every rebuild.
    current_system: PathBuf,
    /// The system profile (`/nix/var/nix/profiles/system`): what the next boot starts.
    system_profile: PathBuf,
}

/// Another broker operation holds the lock: try again later. Its own type, so the CLI can say
/// "busy" (exit 75) without matching error text.
#[derive(Debug)]
pub struct Busy(pub String);

impl std::fmt::Display for Busy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "another broker operation is running ({})", self.0)
    }
}

impl std::error::Error for Busy {}

/// An apply or rollback that was interrupted and not yet reconciled.
#[derive(Debug, Clone, Serialize)]
pub struct Interrupted {
    pub op: String,
    pub target: u64,
}

/// How the machine compares with its ledger. See `Broker::status`.
#[derive(Debug)]
pub struct Status {
    pub ledger: std::result::Result<usize, String>,
    /// The config repo's HEAD; None if git cannot read it.
    pub head: Option<String>,
    /// Uncommitted changes in the config repo (only the broker writes it).
    pub dirty: bool,
    /// The commit the ledger says the config repo is at.
    pub ledgered_commit: Option<String>,
    pub running_system: Option<String>,
    /// What the next boot starts (the system profile), if there is one.
    pub next_boot: Option<String>,
    /// The system the ledger says is running: recorded by init and after every rebuild (None if
    /// the last record has none, e.g. written by an older broker).
    pub ledgered_system: Option<String>,
    /// The last rebuild failed and could not restore the previous system (its ledger entry).
    pub unrestored: Option<String>,
    /// An apply or rollback that was interrupted and not yet reconciled.
    pub interrupted: Option<Interrupted>,
    /// A broker operation is running (who holds the lock): mid-switch, the running system has
    /// changed before the ledger records it, so nothing is compared.
    pub busy: Option<String>,
}

impl Status {
    /// Everything that is not as the ledger says, worded for a person. Empty means all is well —
    /// or that it could not be checked because an operation is running (`busy`).
    pub fn problems(&self) -> Vec<String> {
        if self.busy.is_some() {
            return Vec::new();
        }
        let mut p = Vec::new();
        if let Err(e) = &self.ledger {
            p.push(format!("ledger: {e}"));
        }
        if let Some(i) = &self.interrupted {
            p.push(format!("the {} of #{} was interrupted: run `kestrel-broker reconcile --approved-by <name>`", i.op, i.target));
        }
        if let Some(e) = &self.unrestored {
            p.push(format!(
                "the last rebuild ({e}) failed and could not restore the previous system: the running system \
                 may not match the config; fix the cause, then `kestrel-broker repair --approved-by <name>`"
            ));
        }
        match &self.head {
            None if self.ledgered_commit.is_some() => {
                p.push("config repo cannot be read (git rev-parse HEAD failed)".to_string())
            }
            Some(head) if Some(head) != self.ledgered_commit.as_ref() => p.push(format!(
                "config repo is at {head}, not the last ledgered commit {}: it was changed outside the broker",
                self.ledgered_commit.as_deref().unwrap_or("(none)")
            )),
            _ => {}
        }
        if self.dirty {
            p.push("config repo has uncommitted changes, made outside the broker".to_string());
        }
        if let Some(want) = &self.ledgered_system {
            if self.running_system.as_ref() != Some(want) {
                p.push(format!(
                    "running system is {}, not the last ledgered build {want}: it was switched outside the \
                     broker (e.g. nixos-rebuild against another configuration, or an old boot entry)",
                    self.running_system.as_deref().unwrap_or("(unknown)")
                ));
            }
            if let Some(next) = self.next_boot.as_ref().filter(|n| *n != want) {
                p.push(format!(
                    "the next boot would start {next}, not the last ledgered build {want}: the boot default \
                     was changed outside the broker"
                ));
            }
        }
        p
    }
}

/// Exclusive lock so only one broker operation runs at a time. An OS file lock (flock), so the
/// kernel releases it when the last process holding it dies: a dead broker cannot wedge later ones.
struct Lock(fs::File);
impl Lock {
    /// Wait briefly for the lock: a status check holds it for milliseconds, and an operation that
    /// collides with one must not fail for it. A real operation holds it for minutes; after a few
    /// seconds, refuse and say who has it.
    fn acquire(dir: &Path) -> Result<Self> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            if let Some(l) = Self::try_acquire(dir)? {
                return Ok(l);
            }
            if std::time::Instant::now() >= deadline {
                return Err(Busy(Self::busy(dir)).into());
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    /// The lock, or None while another operation holds it.
    fn try_acquire(dir: &Path) -> Result<Option<Self>> {
        fs::create_dir_all(dir)?;
        let p = dir.join("broker.lock");
        OpenOptions::new().write(true).create(true).truncate(false).open(&p)?;
        // Read-only: the fd is handed to the rebuild as stdin, and nothing should write into it.
        let f = fs::File::open(&p)?;
        match f.try_lock() {
            Ok(()) => Ok(Some(Lock(f))),
            Err(fs::TryLockError::WouldBlock) => Ok(None),
            Err(fs::TryLockError::Error(e)) => Err(e).context("locking the broker"),
        }
    }

    /// Who holds the lock, for messages.
    fn busy(dir: &Path) -> String {
        let p = dir.join("broker.lock");
        let h = holders(&p);
        format!("{}; held by: {}", p.display(), if h.is_empty() { "unknown".to_string() } else { h.join(", ") })
    }

    /// The lock as a child's stdin, so the rebuild's whole process tree holds it too. Killing the
    /// broker does not stop `nixos-rebuild`, which may still switch the system; the next operation
    /// must wait for it rather than race it. A dup shares the open file description, which is what
    /// flock locks belong to, so the lock lasts until the last holder exits. The rebuild
    /// therefore has no terminal: anything that prompts (nix flake config, sudo, ssh host keys)
    /// reads EOF and fails, which is what a broker wants.
    fn for_child(&self) -> Result<Stdio> {
        Ok(Stdio::from(self.0.try_clone()?))
    }
}

/// Processes with `path` open. For the "operation is running" error (a process that inherited the
/// lock from a rebuild and outlived it would otherwise wedge the broker silently), and to tell a
/// stale git `index.lock` from a live one.
fn holders(path: &Path) -> Vec<String> {
    let path = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let me = std::process::id().to_string();
    let mut found = Vec::new();
    for proc in fs::read_dir("/proc").into_iter().flatten().flatten() {
        let name = proc.file_name();
        // Skip ourselves (we have the broker lock open too, having just failed to lock it) and the
        // non-pid entries `self`/`thread-self`, which are links back to us.
        if name == me.as_str() || !name.to_string_lossy().bytes().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let Ok(fds) = fs::read_dir(proc.path().join("fd")) else { continue };
        if fds.flatten().any(|fd| fs::read_link(fd.path()).is_ok_and(|t| t == path)) {
            let comm = fs::read_to_string(proc.path().join("comm")).unwrap_or_default();
            found.push(format!("{} ({})", proc.file_name().to_string_lossy(), comm.trim()));
        }
    }
    found
}

/// An apply or rollback in flight, written to `state/pending.json` before its commit and removed
/// once its outcome is in the ledger. The commit happens before the (minutes-long) rebuild and the
/// ledger entry after it, so a broker that dies in between leaves a commit nobody ledgered; the
/// next operation finds this record and reconciles (see `Broker::reconcile_locked`).
#[derive(Debug, Serialize, Deserialize)]
struct Pending {
    /// "apply" or "rollback".
    op: String,
    /// The proposal (apply) or applied entry (rollback) being acted on.
    target: u64,
    /// HEAD before the operation: the last ledgered config.
    before: String,
    /// Ledger length before the operation; an outcome entry would come after it.
    ledger_len: usize,
    /// The operation's commit, recorded as soon as it exists so it is never lost from the audit.
    commit: Option<String>,
    approved_by: String,
    tier: String,
    tainted: bool,
}

impl Pending {
    /// The ledger entry kinds that settle this operation, and the data key naming its target.
    fn outcome(&self) -> Result<(&'static [&'static str], &'static str)> {
        match self.op.as_str() {
            "apply" => Ok((&["applied", "apply_failed"], "proposal")),
            "rollback" => Ok((&["rolled_back", "rollback_failed"], "applied")),
            other => bail!("pending.json names an unknown operation '{other}'"),
        }
    }
}

/// fsync a directory, making a create, rename or unlink in it durable.
fn sync_dir(dir: &Path) -> Result<()> {
    fs::File::open(dir)?.sync_all()?;
    Ok(())
}

/// The commit the ledger says the config repo is at: that of the last entry that moved it.
/// (apply_failed / rollback_failed carry none: the config is back at the previous commit.)
fn ledgered_commit(entries: &[Entry]) -> Option<String> {
    entries
        .iter()
        .rev()
        .filter(|e| matches!(e.body.kind.as_str(), "init" | "applied" | "rolled_back" | "interrupted" | "repaired"))
        .find_map(|e| e.body.data.get("commit").and_then(|v| v.as_str()).map(String::from))
}

/// nixos-rebuild runs the switch in this transient unit (`systemd-run --pipe`). A broker rebuild's
/// switch inherits the lock through stdin, but a manual `nixos-rebuild` outside the broker does not.
const SWITCH_UNIT: &str = "nixos-rebuild-switch-to-configuration.service";

fn switch_running() -> bool {
    Command::new("systemctl")
        .args(["is-active", "--quiet", SWITCH_UNIT])
        .status()
        .is_ok_and(|s| s.success())
}

impl Broker {
    pub fn new(repo: impl Into<PathBuf>, state_dir: impl Into<PathBuf>, rebuild_cmd: impl Into<String>) -> Self {
        let state_dir = state_dir.into();
        Self {
            repo: Repo::new(repo),
            ledger: Ledger::open(state_dir.join("ledger.jsonl")),
            state_dir,
            rebuild_cmd: rebuild_cmd.into(),
            current_system: PathBuf::from("/run/current-system"),
            system_profile: PathBuf::from("/nix/var/nix/profiles/system"),
        }
    }

    pub fn with_current_system(mut self, link: impl Into<PathBuf>) -> Self {
        self.current_system = link.into();
        self
    }

    pub fn with_system_profile(mut self, link: impl Into<PathBuf>) -> Self {
        self.system_profile = link.into();
        self
    }

    fn running_system(&self) -> Option<String> {
        fs::read_link(&self.current_system).ok().map(|p| p.to_string_lossy().into_owned())
    }

    /// The system the next boot starts: the profile's chain of links (`system` → `system-N-link`
    /// → store path), followed without requiring the store path itself to exist.
    fn next_boot(&self) -> Option<String> {
        let mut p = self.system_profile.clone();
        for _ in 0..8 {
            match fs::read_link(&p) {
                Ok(t) if t.is_absolute() => p = t,
                Ok(t) => p = p.parent()?.join(t),
                Err(_) => break,
            }
        }
        (p != self.system_profile).then(|| p.to_string_lossy().into_owned())
    }

    /// Add the system the last rebuild switched to, so `status` can tell when the machine is later
    /// switched to something else outside the broker.
    fn with_system(&self, mut data: serde_json::Value) -> serde_json::Value {
        data["system"] = self.running_system().into();
        data
    }

    /// Compare the machine with its ledger. Read-only, but under the lock: during an operation the
    /// comparison would be meaningless, so it reports `busy` instead.
    pub fn status(&self) -> Result<Status> {
        let lock = Lock::try_acquire(&self.state_dir)?; // held until the comparison is done
        let busy = lock.is_none().then(|| Lock::busy(&self.state_dir));
        self.facts(busy)
    }

    /// The comparison itself, for a caller that already holds the lock (or knows it is busy).
    fn facts(&self, busy: Option<String>) -> Result<Status> {
        let ledger = self.ledger.verify().map_err(|e| format!("{e:#}"));
        let entries = self.ledger.read_all().unwrap_or_default();
        let interrupted = match self.read_pending()? {
            Some(p) if !self.outcome_recorded(&p)? => Some(Interrupted { op: p.op, target: p.target }),
            _ => None,
        };
        // The latest entry that recorded a system (init, or any rebuild). A null there (the link
        // could not be read) means unknown — not "fall back to an older one".
        let last_record = entries.iter().rev().find(|e| e.body.data.get("system").is_some());
        let unrestored = last_record
            .filter(|e| e.body.data.get("restored") == Some(&json!(false)))
            .map(|e| format!("#{} {}", e.body.seq, e.body.kind));
        Ok(Status {
            ledger,
            head: self.repo.head().ok(),
            dirty: self.repo.ensure_clean().is_err() && self.repo.head().is_ok(),
            ledgered_commit: ledgered_commit(&entries),
            running_system: self.running_system(),
            next_boot: self.next_boot(),
            ledgered_system: last_record.and_then(|e| e.body.data["system"].as_str().map(String::from)),
            unrestored,
            interrupted,
            busy,
        })
    }

    /// Return the machine to its last ledgered configuration (ADR-012): the person's explicit way
    /// back after drift — a system switched outside the broker, a changed boot default, a failed
    /// restore. Never run automatically. An interrupted operation is finished by reconciling it,
    /// which is the repair. A config repo changed outside the broker is refused: a rebuild would
    /// build that change, so a person must look at it first.
    pub fn repair(&self, by: &str) -> Result<Entry> {
        let lock = Lock::acquire(&self.state_dir)?;
        self.ledger.verify().context("cannot repair on a broken ledger")?;
        if let Some(e) = self.reconcile_locked(&lock, by)? {
            return Ok(e);
        }
        let head = self.repo.head()?;
        let entries = self.ledger.read_all()?;
        let Some(ledgered) = ledgered_commit(&entries) else {
            bail!("this machine's ledger has no configuration yet: initialise it first (kestrel-broker init)");
        };
        if ledgered != head || self.repo.ensure_clean().is_err() {
            bail!(
                "the config repo was changed outside the broker (it is at {head}, the ledger says {ledgered}); \
                 a rebuild would build that change, so look at {} first",
                self.repo.root().display()
            );
        }
        let before = self.facts(None)?;
        if before.problems().is_empty() {
            bail!("nothing to repair: the machine is as its ledger says");
        }
        // A manual nixos-rebuild does not take the broker lock: switching now would race it, and the
        // result could be its system recorded as ours.
        if switch_running() {
            bail!("a system switch ({SWITCH_UNIT}) is still running; wait for it to finish, then repair");
        }
        let rebuilt = self.rebuild(&lock).is_ok();
        // A switch sets the running system and the next boot alike; if they differ afterwards,
        // something else changed one of them, and this was not a repair.
        let (running, next) = (self.running_system(), self.next_boot());
        let restored = rebuilt && (next.is_none() || next == running);
        let e = self.ledger.append(NewEntry {
            actor: "user".into(),
            approved_by: Some(by.into()),
            kind: "repaired".into(),
            tier: Tier::System.to_string(),
            tainted: false,
            reason: if restored {
                "Returned the machine to its ledgered configuration".into()
            } else {
                "Tried to return the machine to its ledgered configuration, but it did not take".into()
            },
            // What was wrong is ledgered too: repairing must not erase the evidence of drift.
            data: self.with_system(json!({
                "commit": head,
                "restored": restored,
                "drift": {
                    "running_system": before.running_system,
                    "next_boot": before.next_boot,
                    "ledgered_system": before.ledgered_system,
                    "problems": before.problems(),
                },
            })),
        })?;
        if !restored {
            bail!("the repair did not take: the machine may still not match its ledger (recorded as #{})", e.body.seq);
        }
        Ok(e)
    }

    pub fn init(&self) -> Result<()> {
        let _l = Lock::acquire(&self.state_dir)?;
        self.refuse_if_interrupted()?;
        self.repo.init()?;
        if self.ledger.read_all()?.is_empty() {
            self.ledger.append(NewEntry {
                actor: "user".into(),
                approved_by: None,
                kind: "init".into(),
                tier: Tier::System.to_string(),
                tainted: false,
                reason: "Config repository initialised".into(),
                data: self.with_system(json!({ "commit": self.repo.head()? })), // the baseline for `status`
            })?;
        }
        Ok(())
    }

    pub fn propose(&self, diff_path: &Path, reason: &str, actor: Actor, tainted: bool) -> Result<Entry> {
        let _l = Lock::acquire(&self.state_dir)?;
        self.refuse_if_interrupted()?;
        let diff = fs::read_to_string(diff_path)
            .with_context(|| format!("reading diff {}", diff_path.display()))?;
        // Classify exactly the bytes that go into the ledger: the caller's file is read once and
        // could change between two reads.
        let copy = self.state_dir.join("proposing.diff");
        fs::write(&copy, &diff)?;
        let files = self.repo.diff_files(&copy);
        let _ = fs::remove_file(&copy);
        let files = files?;
        if files.is_empty() {
            bail!("diff changes no files");
        }
        let tier = policy::tier_for_paths(&files);
        policy::check_allowed(actor, tier)?;
        self.ledger.append(NewEntry {
            actor: actor.to_string(),
            approved_by: None,
            kind: "proposal".into(),
            tier: tier.to_string(),
            tainted,
            reason: reason.into(),
            data: json!({
                "files": files,
                "diff_sha256": hex::encode(Sha256::digest(diff.as_bytes())),
                "diff": diff,
            }),
        })
    }

    fn find_child(&self, kind: &str, key: &str, seq: u64) -> Result<Option<Entry>> {
        Ok(self
            .ledger
            .read_all()?
            .into_iter()
            .find(|e| e.body.kind == kind && e.body.data.get(key).and_then(|v| v.as_u64()) == Some(seq)))
    }

    fn pending_path(&self) -> PathBuf {
        self.state_dir.join("pending.json")
    }

    fn read_pending(&self) -> Result<Option<Pending>> {
        match fs::read(self.pending_path()) {
            Ok(b) => Ok(Some(serde_json::from_slice(&b).context("reading pending.json")?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Atomically replace pending.json: write, fsync, rename, fsync the directory.
    fn write_pending(&self, p: &Pending) -> Result<()> {
        let tmp = self.state_dir.join("pending.json.tmp");
        let mut f = fs::File::create(&tmp)?;
        f.write_all(&serde_json::to_vec_pretty(p)?)?;
        f.sync_all()?;
        fs::rename(&tmp, self.pending_path())?;
        sync_dir(&self.state_dir)
    }

    fn clear_pending(&self) -> Result<()> {
        fs::remove_file(self.pending_path())?;
        sync_dir(&self.state_dir)
    }

    /// Whether the ledger already holds this operation's outcome (the broker died after
    /// ledgering it but before removing pending.json).
    fn outcome_recorded(&self, p: &Pending) -> Result<bool> {
        let (kinds, key) = p.outcome()?;
        Ok(self.ledger.read_all()?.iter().skip(p.ledger_len).any(|e| {
            kinds.contains(&e.body.kind.as_str()) && e.body.data.get(key).and_then(|v| v.as_u64()) == Some(p.target)
        }))
    }

    /// For operations that must not rebuild (init, propose — the agent's entry point): settle a
    /// record whose outcome was ledgered, and refuse while one still needs reconciling.
    fn refuse_if_interrupted(&self) -> Result<()> {
        if let Some(p) = self.read_pending()? {
            // Only trust an outcome entry on an intact chain.
            self.ledger.verify().context("cannot settle an interrupted operation on a broken ledger")?;
            if !self.outcome_recorded(&p)? {
                bail!(
                    "the {} of #{} was interrupted and must be reconciled first: run `kestrel-broker \
                     reconcile --approved-by <name>` (it resets the config to the last ledgered commit \
                     and rebuilds)",
                    p.op, p.target
                );
            }
            self.clear_pending()?;
        }
        Ok(())
    }

    /// Finish an apply or rollback whose broker died: discard its unledgered commit (kept under
    /// `refs/kestrel/discarded/`), reset to the last ledgered commit, rebuild, and ledger it.
    /// `by` is who ran it (the approver of the operation that triggered it, or of `reconcile`).
    pub fn reconcile(&self, by: &str) -> Result<Option<Entry>> {
        let lock = Lock::acquire(&self.state_dir)?;
        self.reconcile_locked(&lock, by)
    }

    fn reconcile_locked(&self, lock: &Lock, by: &str) -> Result<Option<Entry>> {
        let Some(mut p) = self.read_pending()? else { return Ok(None) };
        // On a broken chain the final append fails, after a full reset and rebuild, on every retry
        // — and an outcome entry found on it is not trustworthy.
        self.ledger.verify().context("cannot reconcile on a broken ledger")?;
        if self.outcome_recorded(&p)? {
            self.clear_pending()?;
            return Ok(None);
        }
        // Fails open if systemctl is unavailable: broker rebuilds are covered by the lock anyway.
        if switch_running() {
            bail!("a system switch ({SWITCH_UNIT}) is still running; wait for it to finish, then retry");
        }
        let index_lock = self.repo.index_lock();
        if index_lock.exists() {
            let h = holders(&index_lock);
            if !h.is_empty() {
                bail!("git is still running on the config repo ({}); retry when it exits", h.join(", "));
            }
            self.repo.remove_stale_index_lock()?;
        }
        let head = self.repo.head()?;
        // Record the commit before resetting, so a crash in here cannot lose it from the audit.
        if p.commit.is_none() && head != p.before {
            p.commit = Some(head.clone());
            self.write_pending(&p)?;
        }
        if let Some(c) = &p.commit {
            self.repo.keep(c)?;
        }
        if !self.repo.is_ancestor(&p.before, &head)? {
            bail!(
                "config HEAD {head} does not descend from {}, the commit before the interrupted {}; \
                 refusing to reset",
                p.before, p.op
            );
        }
        // The reset destroys uncommitted changes — a half-applied diff, or someone's edit made
        // outside the broker. Keep them too.
        let uncommitted = self.repo.snapshot_dirty()?;
        if let Some(s) = &uncommitted {
            self.repo.keep(s)?;
        }
        // Always rebuild, even when nothing was committed: the broker may have died inside a
        // restore, after a failed rebuild had already activated the new system.
        let restored = self.restore(&p.before, lock)?;
        let e = self.ledger.append(NewEntry {
            actor: "broker".into(),
            approved_by: Some(p.approved_by.clone()),
            kind: "interrupted".into(),
            tier: p.tier.clone(),
            tainted: p.tainted,
            reason: if restored {
                format!("Interrupted {} of #{}: config reset to {} and rebuilt", p.op, p.target, p.before)
            } else {
                format!(
                    "Interrupted {} of #{}: config reset to {} but the restore rebuild ALSO failed — \
                     running system may not match the config",
                    p.op, p.target, p.before
                )
            },
            data: self.with_system(json!({
                "op": p.op,
                "target": p.target,
                "discarded_commit": p.commit,
                "discarded_uncommitted": uncommitted,
                "commit": p.before,
                "restored": restored,
                "reconciled_by": by,
            })),
        })?;
        self.clear_pending()?;
        if !restored {
            bail!(
                "reconciled the interrupted {} of #{}, but the restore rebuild failed; the running \
                 system may not match the config",
                p.op, p.target
            );
        }
        Ok(Some(e))
    }

    /// The commit step failed. Nothing was rebuilt or ledgered, but `git apply --index` may have
    /// left the tree dirty (or the commit landed and only its sync failed): put the repo back and
    /// drop the record. If even that fails, the record stays and the next operation reconciles.
    fn undo_failed_commit(&self, p: &Pending, r: Result<String>) -> Result<String> {
        r.or_else(|e| match self.repo.hard_reset(&p.before).and_then(|()| self.clear_pending()) {
            Ok(()) => Err(e),
            Err(u) => Err(e.context(format!("undoing it also failed ({u:#}); the next operation will reconcile"))),
        })
    }

    fn rebuild(&self, lock: &Lock) -> Result<()> {
        let cmd = self.rebuild_cmd.replace("{repo}", &self.repo.root().to_string_lossy());
        let status = Command::new("sh")
            .arg("-c")
            .arg(&cmd)
            .stdin(lock.for_child()?)
            // stdout carries the broker's result (one JSON line with --json): the rebuild's own output
            // (activation scripts print to stdout via systemd-run --pipe) goes to stderr.
            .stdout(Stdio::from(std::io::stderr().as_fd().try_clone_to_owned()?))
            .status()
            .context("running rebuild")?;
        if !status.success() {
            bail!("rebuild command failed ({status}): {cmd}");
        }
        Ok(())
    }

    /// Reset the config to `before` and rebuild it. A failed rebuild may already have activated
    /// the new system (switch-to-configuration exits non-zero when a unit fails), so resetting
    /// the repo alone would leave the running system out of step with it. Returns whether the
    /// restore rebuild succeeded.
    fn restore(&self, before: &str, lock: &Lock) -> Result<bool> {
        self.repo.hard_reset(before)?;
        Ok(self.rebuild(lock).is_ok())
    }

    fn failure_reason(what: &str, before: &str, restored: bool) -> String {
        if restored {
            format!("{what} failed; config reset to {before} and rebuilt")
        } else {
            format!("{what} failed; config reset to {before} but the restore rebuild ALSO failed — running system may not match the config")
        }
    }

    pub fn apply(&self, proposal_seq: u64, approved_by: &str, confirm_tainted: bool) -> Result<Entry> {
        let lock = Lock::acquire(&self.state_dir)?;
        self.reconcile_locked(&lock, approved_by)?;
        let p = self.ledger.get(proposal_seq)?;
        if p.body.kind != "proposal" {
            bail!("entry {proposal_seq} is a '{}', not a proposal", p.body.kind);
        }
        if self.find_child("applied", "proposal", proposal_seq)?.is_some() {
            bail!("proposal {proposal_seq} was already applied");
        }
        if p.body.tainted && !confirm_tainted {
            bail!(
                "proposal {proposal_seq} came from a session that read untrusted content; \
                 re-run with explicit per-step confirmation (--confirm-tainted)"
            );
        }
        let diff = p.body.data["diff"].as_str().context("proposal has no diff")?;

        // Re-validate against the current repo state (it may have changed since proposing).
        let tmp = self.state_dir.join(format!("proposal-{proposal_seq}.diff"));
        fs::write(&tmp, diff)?;
        let files = self.repo.diff_files(&tmp)?;
        let tier = policy::tier_for_paths(&files);
        let actor = if p.body.actor == "agent" { Actor::Agent } else { Actor::User };
        policy::check_allowed(actor, tier)?;

        let next_seq = self.ledger.read_all()?.len() as u64 + 1;
        let msg = format!(
            "{}\n\nActor: {}\nApproved-By: {approved_by}\nTier: {tier}\nProposal: {proposal_seq}\nLedger-Entry: {next_seq}",
            p.body.reason, p.body.actor
        );
        let before = self.repo.head()?;
        self.repo.ensure_clean()?; // before recording anything: a failed commit is undone by a reset
        let mut pending = Pending {
            op: "apply".into(),
            target: proposal_seq,
            before: before.clone(),
            ledger_len: (next_seq - 1) as usize,
            commit: None,
            approved_by: approved_by.into(),
            tier: tier.to_string(),
            tainted: p.body.tainted,
        };
        self.write_pending(&pending)?;
        let commit = self.undo_failed_commit(&pending, self.repo.apply_and_commit(&tmp, &msg))?;
        let _ = fs::remove_file(&tmp);
        pending.commit = Some(commit.clone());
        self.write_pending(&pending)?;

        if let Err(e) = self.rebuild(&lock) {
            let restored = self.restore(&before, &lock)?;
            self.ledger.append(NewEntry {
                actor: "broker".into(),
                approved_by: Some(approved_by.into()),
                kind: "apply_failed".into(),
                tier: tier.to_string(),
                tainted: p.body.tainted,
                reason: Self::failure_reason("Rebuild", &before, restored),
                data: self.with_system(json!({ "proposal": proposal_seq, "error": e.to_string(), "restored": restored })),
            })?;
            self.clear_pending()?;
            return Err(e.context("apply rolled back because the rebuild failed"));
        }

        let e = self.ledger.append(NewEntry {
            actor: p.body.actor.clone(),
            approved_by: Some(approved_by.into()),
            kind: "applied".into(),
            tier: tier.to_string(),
            tainted: p.body.tainted,
            reason: p.body.reason.clone(),
            data: self.with_system(json!({ "proposal": proposal_seq, "commit": commit, "files": files })),
        })?;
        self.clear_pending()?;
        Ok(e)
    }

    pub fn rollback(&self, applied_seq: u64, approved_by: &str) -> Result<Entry> {
        let lock = Lock::acquire(&self.state_dir)?;
        self.reconcile_locked(&lock, approved_by)?;
        let a = self.ledger.get(applied_seq)?;
        if a.body.kind != "applied" {
            bail!("entry {applied_seq} is a '{}', not an applied change", a.body.kind);
        }
        if self.find_child("rolled_back", "applied", applied_seq)?.is_some() {
            bail!("change {applied_seq} was already rolled back");
        }
        let commit = a.body.data["commit"].as_str().context("applied entry has no commit")?.to_string();
        let next_seq = self.ledger.read_all()?.len() as u64 + 1;
        let msg = format!(
            "Undo: {}\n\nReverts: {commit}\nApproved-By: {approved_by}\nLedger-Entry: {next_seq}",
            a.body.reason
        );
        let before = self.repo.head()?;
        self.repo.ensure_clean()?; // before recording anything: a failed commit is undone by a reset
        let mut pending = Pending {
            op: "rollback".into(),
            target: applied_seq,
            before: before.clone(),
            ledger_len: (next_seq - 1) as usize,
            commit: None,
            approved_by: approved_by.into(),
            tier: a.body.tier.clone(),
            tainted: false,
        };
        self.write_pending(&pending)?;
        let new_commit = self.undo_failed_commit(&pending, self.repo.revert_and_commit(&commit, &msg))?;
        pending.commit = Some(new_commit.clone());
        self.write_pending(&pending)?;
        if let Err(e) = self.rebuild(&lock) {
            let restored = self.restore(&before, &lock)?;
            self.ledger.append(NewEntry {
                actor: "broker".into(),
                approved_by: Some(approved_by.into()),
                kind: "rollback_failed".into(),
                tier: a.body.tier.clone(),
                tainted: false,
                reason: Self::failure_reason("Rollback rebuild", &before, restored),
                data: self.with_system(json!({ "applied": applied_seq, "error": e.to_string(), "restored": restored })),
            })?;
            self.clear_pending()?;
            return Err(e.context("rollback aborted because the rebuild failed"));
        }
        let e = self.ledger.append(NewEntry {
            actor: "user".into(),
            approved_by: Some(approved_by.into()),
            kind: "rolled_back".into(),
            tier: a.body.tier.clone(),
            tainted: false,
            reason: format!("Undo: {}", a.body.reason),
            data: self.with_system(json!({ "applied": applied_seq, "reverted_commit": commit, "commit": new_commit })),
        })?;
        self.clear_pending()?;
        Ok(e)
    }
}
