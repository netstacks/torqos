//! torq-broker: the only writer of the TorqOS config repo.
//!
//! Lifecycle: propose (validated + ledgered, diff stored in the ledger) → apply (approved,
//! committed, rebuilt, ledgered) → revert (one change undone by a new commit, rebuilt, ledgered).
//! If a rebuild fails, the repo is reset to the previous commit, that commit is rebuilt (the
//! failed rebuild may already have activated), and the failure is ledgered.

pub mod config;
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

/// The largest user-tier data file the broker accepts (`Broker::check_user_data`).
const USER_DATA_MAX: u64 = 64 * 1024;
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
    /// The `commit confirmed` commands (see config.rs for each).
    test_cmd: String,
    activate_cmd: String,
    boot_cmd: String,
    schedule_cmd: String,
    /// This broker's own executable, which the scheduled rollback runs: the store path of the
    /// broker that made the change, not whatever `torq-broker` the new system has.
    exe: PathBuf,
    /// The kernel's boot ID (`/proc/sys/kernel/random/boot_id`), new on every boot.
    boot_id_file: PathBuf,
}

/// A `commit confirmed` waiting for `confirm`: an `applied` entry with a `confirm` record and no
/// `confirmed` or `rolled_back` for it. Derived from the ledger alone, never from a separate file.
#[derive(Debug, Clone, Serialize)]
pub struct PendingConfirm {
    pub applied: u64,
    pub deadline: String,
    /// The config and the system before the commit: what an unconfirmed commit goes back to.
    pub anchor_commit: String,
    pub anchor_system: String,
    /// The system the commit activated.
    pub system: Option<String>,
    /// The kernel's boot ID when it was applied: a rollback "by reboot" needs a different one.
    pub boot_id: Option<String>,
    #[serde(skip)]
    tier: String,
    #[serde(skip)]
    reason: String,
}

fn pending_confirm(entries: &[Entry]) -> Option<PendingConfirm> {
    let a = entries.iter().rev().find(|e| e.body.kind == "applied" && e.body.data.get("confirm").is_some())?;
    let seq = a.body.seq;
    let settled = entries
        .iter()
        .any(|e| matches!(e.body.kind.as_str(), "confirmed" | "rolled_back") && e.body.data["applied"] == json!(seq));
    if settled {
        return None;
    }
    let c = &a.body.data["confirm"];
    Some(PendingConfirm {
        applied: seq,
        deadline: c["deadline"].as_str()?.into(),
        anchor_commit: c["anchor_commit"].as_str()?.into(),
        anchor_system: c["anchor_system"].as_str()?.into(),
        system: a.body.data["system"].as_str().map(String::from),
        boot_id: c["boot_id"].as_str().map(String::from),
        tier: a.body.tier.clone(),
        reason: a.body.reason.clone(),
    })
}

/// The longest `commit confirmed` wait, in minutes (a day).
pub const MAX_CONFIRM_MINUTES: u32 = 24 * 60;

/// One shell word, for a placeholder's value.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
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

/// An apply or revert that was interrupted and not yet reconciled.
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
    /// An apply or revert that was interrupted and not yet reconciled.
    pub interrupted: Option<Interrupted>,
    /// A broker operation is running (who holds the lock): mid-switch, the running system has
    /// changed before the ledger records it, so nothing is compared.
    pub busy: Option<String>,
    /// A commit confirmed waiting for `confirm`. Its boot default is, rightly, still the old system.
    pub pending_confirm: Option<PendingConfirm>,
    /// Its deadline passed more than two minutes ago and it has not been rolled back.
    pub overdue: bool,
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
            p.push(format!("the {} of #{} was interrupted: run `torq-broker reconcile --approved-by <name>`", i.op, i.target));
        }
        if let Some(e) = &self.unrestored {
            p.push(format!(
                "the last rebuild ({e}) failed and could not restore the previous system: the running system \
                 may not match the config; fix the cause, then `torq-broker repair --approved-by <name>`"
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
        if let Some(pc) = self.pending_confirm.as_ref().filter(|_| self.overdue) {
            p.push(format!(
                "change #{} (commit confirmed) was not confirmed by {} and has not been rolled back: \
                 `torq-broker expire-confirm` rolls it back",
                pc.applied, pc.deadline
            ));
        }
        if let Some(want) = &self.ledgered_system {
            if self.running_system.as_ref() != Some(want) {
                p.push(format!(
                    "running system is {}, not the last ledgered build {want}: it was switched outside the \
                     broker (e.g. nixos-rebuild against another configuration, or an old boot entry)",
                    self.running_system.as_deref().unwrap_or("(unknown)")
                ));
            }
            let waiting_anchor = self.pending_confirm.as_ref().map(|pc| &pc.anchor_system);
            if let Some(next) = self.next_boot.as_ref().filter(|n| *n != want && Some(*n) != waiting_anchor) {
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

/// An apply or revert in flight, written to `state/pending.json` before its commit and removed
/// once its outcome is in the ledger. The commit happens before the (minutes-long) rebuild and the
/// ledger entry after it, so a broker that dies in between leaves a commit nobody ledgered; the
/// next operation finds this record and reconciles (see `Broker::reconcile_locked`).
#[derive(Debug, Serialize, Deserialize)]
struct Pending {
    /// The record's format (`PENDING_VERSION`); absent in records written before it existed (0).
    /// A broker refuses a record newer than it understands rather than misreading it.
    #[serde(default)]
    version: u32,
    /// "apply", "revert", or "expire" (the rollback of an unconfirmed commit).
    op: String,
    /// The proposal (apply) or applied entry (revert, expire) being acted on.
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
            "revert" => Ok((&["reverted", "revert_failed"], "applied")),
            "expire" => Ok((&["rolled_back"], "applied")),
            other => bail!("pending.json names an unknown operation '{other}'"),
        }
    }
}

const PENDING_VERSION: u32 = 1;

/// fsync a directory, making a create, rename or unlink in it durable.
fn sync_dir(dir: &Path) -> Result<()> {
    fs::File::open(dir)?.sync_all()?;
    Ok(())
}

/// The commit the ledger says the config repo is at: that of the last entry that moved it.
/// (apply_failed / revert_failed carry none: the config is back at the previous commit.)
fn ledgered_commit(entries: &[Entry]) -> Option<String> {
    entries
        .iter()
        .rev()
        .filter(|e| matches!(e.body.kind.as_str(), "init" | "applied" | "reverted" | "interrupted" | "repaired" | "rolled_back"))
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
            test_cmd: config::TEST.into(),
            activate_cmd: config::ACTIVATE.into(),
            boot_cmd: config::BOOT.into(),
            schedule_cmd: config::SCHEDULE.into(),
            exe: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("torq-broker")),
            boot_id_file: PathBuf::from("/proc/sys/kernel/random/boot_id"),
        }
    }

    /// Where to read the boot ID (tests: a file they change to simulate a restart).
    pub fn with_boot_id_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.boot_id_file = path.into();
        self
    }

    fn boot_id(&self) -> Option<String> {
        fs::read_to_string(&self.boot_id_file).ok().map(|s| s.trim().to_string())
    }

    /// The `commit confirmed` commands: test activation, direct activation, boot default, schedule.
    pub fn with_confirm_commands(mut self, test: &str, activate: &str, boot: &str, schedule: &str) -> Self {
        (self.test_cmd, self.activate_cmd, self.boot_cmd) = (test.into(), activate.into(), boot.into());
        self.schedule_cmd = schedule.into();
        self
    }

    /// The broker the scheduled rollback runs (tests: the binary under test).
    pub fn with_exe(mut self, exe: impl Into<PathBuf>) -> Self {
        self.exe = exe.into();
        self
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
        let pending_confirm = pending_confirm(&entries);
        let overdue = pending_confirm.as_ref().is_some_and(|pc| {
            chrono::DateTime::parse_from_rfc3339(&pc.deadline)
                .is_ok_and(|d| chrono::Utc::now() > d + chrono::Duration::minutes(2))
        });
        Ok(Status {
            pending_confirm,
            overdue,
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
        // A waiting commit confirmed is not drift; its own rollback is what undoes it.
        self.refuse_if_confirm_pending()?;
        let head = self.repo.head()?;
        let entries = self.ledger.read_all()?;
        let Some(ledgered) = ledgered_commit(&entries) else {
            bail!("this machine's ledger has no configuration yet: initialise it first (torq-broker init)");
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
        // A switch started meanwhile would set both alike, so look for one too; and a profile that
        // vanished during the repair is not one we can vouch for.
        let (running, next) = (self.running_system(), self.next_boot());
        let profile_ok = if before.next_boot.is_some() { next.is_some() && next == running } else { next.is_none() || next == running };
        let restored = rebuilt && profile_ok && !switch_running();
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

    /// User-tier files are data by construction, whoever proposes them: a regular file (git mode
    /// 100644 — never a symlink) holding exactly the JSON shape its leaf takes
    /// (`policy::user_leaf_ok`), at most `USER_DATA_MAX` bytes. Deleting one is always fine. The
    /// NixOS module reading the file is still the authority on what the data means; this makes sure
    /// it is the right kind of data, refused now rather than failing the rebuild.
    fn check_user_data(&self, diff_path: &Path, files: &[String]) -> Result<()> {
        let user: Vec<String> = files.iter().filter(|f| policy::is_user_data(f)).cloned().collect();
        if user.is_empty() {
            return Ok(());
        }
        for (path, file) in self.repo.proposed_files(diff_path, &user, USER_DATA_MAX)? {
            let Some(f) = file else { continue };
            let leaf = path.rsplit('/').next().unwrap_or_default();
            let data = f.mode == "100644"
                && f.size <= USER_DATA_MAX
                && serde_json::from_slice::<serde_json::Value>(&f.content).is_ok_and(|v| policy::user_leaf_ok(leaf, &v));
            if !data {
                bail!(
                    "refused: {path} must be a regular file holding {} of at most {} KiB",
                    match leaf {
                        "packages.json" => format!("a JSON list of package names (at most {})", policy::MAX_USER_PACKAGES),
                        _ => "JSON data".to_string(),
                    },
                    USER_DATA_MAX / 1024
                );
            }
        }
        Ok(())
    }

    pub fn propose(&self, diff_path: &Path, reason: &str, actor: Actor, tainted: bool) -> Result<Entry> {
        self.propose_on(None, diff_path, reason, actor, tainted)
    }

    /// Propose a change prepared against the configuration at commit `base`. It is refused now if
    /// the configuration has moved on since, and again at `apply` if it moves before then: applied
    /// on top of a newer configuration, a change prepared against an older one can silently undo or
    /// mix with the commit in between. Without a base, the diff is applied wherever it applies.
    pub fn propose_on(&self, base: Option<&str>, diff_path: &Path, reason: &str, actor: Actor, tainted: bool) -> Result<Entry> {
        let _l = Lock::acquire(&self.state_dir)?;
        self.refuse_if_interrupted()?;
        if let Some(base) = base {
            // A full commit id, never a ref (`HEAD`, a branch) that would always "match".
            if base.len() != 40 || !base.bytes().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()) {
                bail!("--base must be a full commit id (40 lowercase hex digits), not {base:?}");
            }
            self.refuse_stale_base(base)?;
        }
        let diff = fs::read_to_string(diff_path)
            .with_context(|| format!("reading diff {}", diff_path.display()))?;
        // Classify exactly the bytes that go into the ledger: the caller's file is read once and
        // could change between two reads.
        let copy = self.state_dir.join("proposing.diff");
        fs::write(&copy, &diff)?;
        let files = self.repo.diff_files(&copy).and_then(|files| {
            self.check_user_data(&copy, &files)?;
            Ok(files)
        });
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
                "base": base,
            }),
        })
    }

    fn refuse_stale_base(&self, base: &str) -> Result<()> {
        let head = self.repo.head()?;
        if head != base {
            bail!(
                "refused: the configuration has changed since this change was prepared (prepared on \
                 {base}, now at {head}); prepare it again from the current configuration"
            );
        }
        Ok(())
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
            Ok(b) => {
                let p: Pending = serde_json::from_slice(&b).context("reading pending.json")?;
                if p.version > PENDING_VERSION {
                    bail!(
                        "pending.json was written by a newer broker (format {}, this one reads up to {PENDING_VERSION}); \
                         finish it with that broker",
                        p.version
                    );
                }
                Ok(Some(p))
            }
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
                    "the {} of #{} was interrupted and must be reconciled first: run `torq-broker \
                     reconcile --approved-by <name>` (it resets the config to the last ledgered commit \
                     and rebuilds)",
                    p.op, p.target
                );
            }
            self.clear_pending()?;
        }
        Ok(())
    }

    /// Finish an apply or revert whose broker died: discard its unledgered commit (kept under
    /// `refs/torq/discarded/`), reset to the last ledgered commit, rebuild, and ledger it.
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
        // An interrupted rollback of an unconfirmed commit is finished, not undone: it goes to the
        // anchor recorded in the ledger, which a generic reset-and-rebuild would not ledger as such.
        if p.op == "expire" {
            return self.expire_locked(lock, Some(p.target), false, Some(p));
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
        self.run_shell(lock, "rebuild", &self.rebuild_cmd.replace("{repo}", &self.repo.root().to_string_lossy()))
    }

    /// Run one of the configured commands with its placeholders replaced by shell-quoted values.
    fn run_command(&self, lock: &Lock, what: &str, template: &str, vars: &[(&str, &str)]) -> Result<()> {
        let mut cmd = template.to_string();
        for (k, v) in vars {
            cmd = cmd.replace(&format!("{{{k}}}"), &shell_quote(v));
        }
        self.run_shell(lock, what, &cmd)
    }

    fn run_shell(&self, lock: &Lock, what: &str, cmd: &str) -> Result<()> {
        let status = Command::new("sh")
            .arg("-c")
            .arg(&cmd)
            .stdin(lock.for_child()?)
            // stdout carries the broker's result (one JSON line with --json): the rebuild's own output
            // (activation scripts print to stdout via systemd-run --pipe) goes to stderr.
            .stdout(Stdio::from(std::io::stderr().as_fd().try_clone_to_owned()?))
            .status()
            .with_context(|| format!("running {what}"))?;
        if !status.success() {
            bail!("{what} command failed ({status}): {cmd}");
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
        self.apply_inner(proposal_seq, approved_by, confirm_tainted, None)
    }

    /// `commit confirmed`: apply, but activate the new system without making it the boot default,
    /// and roll it back unless `confirm` runs within `minutes`. The rollback is a transient timer
    /// running this broker's `expire-confirm`; a reboot rolls back by itself (the boot default is
    /// still the old system) and the boot-time check ledgers it.
    pub fn apply_confirmed(&self, proposal_seq: u64, approved_by: &str, confirm_tainted: bool, minutes: u32) -> Result<Entry> {
        if !(1..=MAX_CONFIRM_MINUTES).contains(&minutes) {
            bail!("commit confirmed takes 1 to {MAX_CONFIRM_MINUTES} minutes, not {minutes}");
        }
        self.apply_inner(proposal_seq, approved_by, confirm_tainted, Some(minutes))
    }

    /// While a `commit confirmed` waits, nothing else may change the machine: the rollback goes
    /// back to the configuration before it, and would silently take later changes with it.
    fn refuse_if_confirm_pending(&self) -> Result<()> {
        if let Some(pc) = pending_confirm(&self.ledger.read_all()?) {
            bail!(
                "refused: change #{} (commit confirmed) is waiting for confirmation until {}: \
                 `torq-broker confirm` keeps it, `torq-broker expire-confirm` rolls it back now",
                pc.applied, pc.deadline
            );
        }
        Ok(())
    }

    fn apply_inner(&self, proposal_seq: u64, approved_by: &str, confirm_tainted: bool, confirm: Option<u32>) -> Result<Entry> {
        let lock = Lock::acquire(&self.state_dir)?;
        self.reconcile_locked(&lock, approved_by)?;
        self.refuse_if_confirm_pending()?;
        // The rollback of an unconfirmed commit — and a reboot — go back to the running system, which
        // must therefore be the boot default too (that also keeps it from garbage collection).
        let anchor_system = match confirm {
            None => None,
            Some(_) => {
                let running = self.running_system().context("commit confirmed: cannot tell which system is running")?;
                if let Some(next) = self.next_boot().filter(|n| *n != running) {
                    bail!(
                        "commit confirmed: the next boot would start {next}, not the running system {running}, \
                         so a reboot would not roll back to it; `torq-broker repair` first"
                    );
                }
                Some(running)
            }
        };
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
        if let Some(base) = p.body.data["base"].as_str() {
            self.refuse_stale_base(base)?;
        }

        // Re-validate against the current repo state (it may have changed since proposing).
        let tmp = self.state_dir.join(format!("proposal-{proposal_seq}.diff"));
        fs::write(&tmp, diff)?;
        let files = self.repo.diff_files(&tmp)?;
        self.check_user_data(&tmp, &files)?;
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
            version: PENDING_VERSION,
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

        // Plain: rebuild and switch. Confirmed: build and activate (`test`), then schedule the
        // rollback; a failure in either goes straight back to the anchor system, not a rebuild.
        let activated = match (confirm, &anchor_system) {
            (Some(minutes), Some(anchor)) => self
                .run_command(&lock, "test activation", &self.test_cmd, &[("repo", &self.repo.root().to_string_lossy())])
                .and_then(|()| self.schedule_rollback(&lock, next_seq, minutes))
                .map(|deadline| Some(json!({
                    "minutes": minutes,
                    "deadline": deadline,
                    "anchor_commit": before,
                    "anchor_system": anchor,
                    "boot_id": self.boot_id(),
                })))
                .map_err(|e| (e, Some(anchor))),
            _ => self.rebuild(&lock).map(|()| None).map_err(|e| (e, None)),
        };
        let confirm_record = match activated {
            Ok(c) => c,
            Err((e, anchor)) => {
                let restored = match anchor {
                    Some(system) => self.restore_anchor(&lock, &before, system)?,
                    None => self.restore(&before, &lock)?,
                };
                self.ledger.append(NewEntry {
                    actor: "broker".into(),
                    approved_by: Some(approved_by.into()),
                    kind: "apply_failed".into(),
                    tier: tier.to_string(),
                    tainted: p.body.tainted,
                    reason: Self::failure_reason(if anchor.is_some() { "Test activation" } else { "Rebuild" }, &before, restored),
                    data: self.with_system(json!({ "proposal": proposal_seq, "error": e.to_string(), "restored": restored })),
                })?;
                self.clear_pending()?;
                return Err(e.context("apply undone because the rebuild failed"));
            }
        };

        let mut data = json!({ "proposal": proposal_seq, "commit": commit, "files": files });
        if let Some(c) = confirm_record {
            data["confirm"] = c;
        }
        let e = self.ledger.append(NewEntry {
            actor: p.body.actor.clone(),
            approved_by: Some(approved_by.into()),
            kind: "applied".into(),
            tier: tier.to_string(),
            tainted: p.body.tainted,
            reason: p.body.reason.clone(),
            data: self.with_system(data),
        })?;
        self.clear_pending()?;
        Ok(e)
    }

    /// Start the timer that rolls back applied entry `seq` after `minutes`; returns the deadline.
    /// It runs this broker by its own path (`exe`), so the rollback does not depend on the system
    /// being rolled back. If this broker dies before `seq` is ledgered, the timer finds nothing
    /// waiting and does nothing.
    fn schedule_rollback(&self, lock: &Lock, seq: u64, minutes: u32) -> Result<String> {
        let deadline = chrono::Utc::now() + chrono::Duration::minutes(minutes.into());
        self.run_command(
            lock,
            "rollback timer",
            &self.schedule_cmd,
            &[("seq", &seq.to_string()), ("seconds", &(u64::from(minutes) * 60).to_string()), ("broker", &self.exe.to_string_lossy())],
        )?;
        Ok(deadline.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
    }

    /// Go back to `commit` and the already-built `system` without rebuilding: reset the config,
    /// activate the system unless it is already running, and make it the boot default if something
    /// moved that. Returns whether the machine ended up running `system` with it as boot default.
    fn restore_anchor(&self, lock: &Lock, commit: &str, system: &str) -> Result<bool> {
        self.repo.hard_reset(commit)?;
        let activated = self.running_system().as_deref() == Some(system)
            || self.run_command(lock, "activation", &self.activate_cmd, &[("system", system)]).is_ok();
        let boot_ok = match self.next_boot() {
            Some(next) if next != system => self
                .run_command(lock, "boot default", &self.boot_cmd, &[("system", system), ("profile", &self.system_profile.to_string_lossy())])
                .is_ok(),
            _ => true,
        };
        Ok(activated && boot_ok && self.running_system().as_deref() == Some(system))
    }

    /// Keep the commit confirmed that is waiting: make the system it activated the boot default and
    /// ledger `confirmed`. Accepted until its rollback has started (the rollback takes the lock),
    /// not by the clock. It must be the running system: after a reboot or a switch elsewhere there
    /// is nothing to keep.
    pub fn confirm(&self, by: &str) -> Result<Entry> {
        let lock = Lock::acquire(&self.state_dir)?;
        self.reconcile_locked(&lock, by)?;
        let Some(pc) = pending_confirm(&self.ledger.read_all()?) else {
            bail!("nothing is waiting for confirmation");
        };
        let running = self.running_system();
        let Some(system) = pc.system.clone().filter(|s| running.as_ref() == Some(s)) else {
            bail!(
                "change #{} cannot be confirmed: the running system is {}, not the one it activated ({}); \
                 it will be rolled back",
                pc.applied,
                running.as_deref().unwrap_or("(unknown)"),
                pc.system.as_deref().unwrap_or("(unknown)")
            );
        };
        self.run_command(&lock, "boot default", &self.boot_cmd, &[("system", &system), ("profile", &self.system_profile.to_string_lossy())])
            .with_context(|| format!("change #{} is still unconfirmed and will be rolled back at {}", pc.applied, pc.deadline))?;
        self.ledger.append(NewEntry {
            actor: "user".into(),
            approved_by: Some(by.into()),
            kind: "confirmed".into(),
            tier: pc.tier.clone(),
            tainted: false,
            reason: format!("Confirmed: {}", pc.reason),
            data: self.with_system(json!({ "applied": pc.applied })),
        })
    }

    /// Roll back the commit confirmed that is waiting (`seq`, if given, must be it): what its timer
    /// runs at the deadline, the boot-time check after a reboot (`by_reboot`), or a person who
    /// wants it gone now. Nothing waiting is not an error: a confirmed commit's timer still fires.
    pub fn expire_confirm(&self, seq: Option<u64>, by_reboot: bool) -> Result<Option<Entry>> {
        let lock = Lock::acquire(&self.state_dir)?;
        if let Some(e) = self.reconcile_locked(&lock, "broker")? {
            if e.body.kind == "rolled_back" {
                return Ok(Some(e)); // it was this, interrupted: now finished
            }
        }
        self.expire_locked(&lock, seq, by_reboot, None)
    }

    /// Idempotent, so an interrupted rollback is finished by running it again (`resumed`).
    fn expire_locked(&self, lock: &Lock, seq: Option<u64>, by_reboot: bool, resumed: Option<Pending>) -> Result<Option<Entry>> {
        let entries = self.ledger.read_all()?;
        let pc = pending_confirm(&entries).filter(|pc| seq.is_none_or(|s| s == pc.applied));
        // "By reboot" only after one: the boot-time unit also starts on the switch that first adds it.
        let pc = pc.filter(|pc| !by_reboot || pc.boot_id.is_none() || pc.boot_id != self.boot_id());
        let Some(pc) = pc else {
            if resumed.is_some() {
                self.clear_pending()?;
            }
            return Ok(None);
        };
        self.ledger.verify().context("cannot roll back on a broken ledger")?;
        // A manual nixos-rebuild does not take the broker lock. Failing makes the timer retry.
        if switch_running() {
            bail!("a system switch ({SWITCH_UNIT}) is still running; the rollback waits for it");
        }
        let head = self.repo.head()?;
        let (running_before, next_before) = (self.running_system(), self.next_boot());
        let mut p = match resumed {
            Some(p) => p,
            None => Pending {
                version: PENDING_VERSION,
                op: "expire".into(),
                target: pc.applied,
                before: head.clone(),
                ledger_len: entries.len(),
                commit: None,
                approved_by: "broker".into(),
                tier: pc.tier.clone(),
                tainted: false,
            },
        };
        // What the reset discards is kept (refs/torq/discarded/): the unconfirmed commit, and anything
        // changed behind the broker's back since.
        if head != pc.anchor_commit {
            p.commit = Some(head.clone());
            self.repo.keep(&head)?;
        }
        self.write_pending(&p)?;
        let uncommitted = self.repo.snapshot_dirty()?;
        if let Some(s) = &uncommitted {
            self.repo.keep(s)?;
        }
        let restored = self.restore_anchor(lock, &pc.anchor_commit, &pc.anchor_system)?;
        let why = if by_reboot { "the machine restarted before it was confirmed" } else { "it was not confirmed in time" };
        let e = self.ledger.append(NewEntry {
            actor: "broker".into(),
            approved_by: None,
            kind: "rolled_back".into(),
            tier: pc.tier.clone(),
            tainted: false,
            reason: if restored {
                format!("Change #{} rolled back: {why}", pc.applied)
            } else {
                format!(
                    "Change #{} rolled back ({why}), but the restore did not fully take — running system may not \
                     match the config",
                    pc.applied
                )
            },
            data: self.with_system(json!({
                "applied": pc.applied,
                "unconfirmed": true,
                "by_reboot": by_reboot,
                "commit": pc.anchor_commit,
                "discarded_commit": p.commit,
                "discarded_uncommitted": uncommitted,
                "restored": restored,
                "found": { "running_system": running_before, "next_boot": next_before },
            })),
        })?;
        self.clear_pending()?;
        Ok(Some(e))
    }

    /// Undo one applied change by a new commit that reverts it. (Not `rollback`: that word is kept
    /// for loading a whole earlier configuration, and for an unconfirmed `commit confirmed`.)
    pub fn revert(&self, applied_seq: u64, approved_by: &str) -> Result<Entry> {
        let lock = Lock::acquire(&self.state_dir)?;
        self.reconcile_locked(&lock, approved_by)?;
        self.refuse_if_confirm_pending()?;
        let a = self.ledger.get(applied_seq)?;
        if a.body.kind != "applied" {
            bail!("entry {applied_seq} is a '{}', not an applied change", a.body.kind);
        }
        if self.find_child("reverted", "applied", applied_seq)?.is_some() {
            bail!("change {applied_seq} was already reverted");
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
            version: PENDING_VERSION,
            op: "revert".into(),
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
                kind: "revert_failed".into(),
                tier: a.body.tier.clone(),
                tainted: false,
                reason: Self::failure_reason("Revert rebuild", &before, restored),
                data: self.with_system(json!({ "applied": applied_seq, "error": e.to_string(), "restored": restored })),
            })?;
            self.clear_pending()?;
            return Err(e.context("revert aborted because the rebuild failed"));
        }
        let e = self.ledger.append(NewEntry {
            actor: "user".into(),
            approved_by: Some(approved_by.into()),
            kind: "reverted".into(),
            tier: a.body.tier.clone(),
            tainted: false,
            reason: format!("Undo: {}", a.body.reason),
            data: self.with_system(json!({ "applied": applied_seq, "reverted_commit": commit, "commit": new_commit })),
        })?;
        self.clear_pending()?;
        Ok(e)
    }
}
