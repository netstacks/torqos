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
use serde_json::json;
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Broker {
    pub repo: Repo,
    pub ledger: Ledger,
    state_dir: PathBuf,
    /// Shell command run after each commit. `{repo}` is replaced with the repo path.
    rebuild_cmd: String,
}

/// Exclusive lock so only one broker operation runs at a time. An OS file lock (flock), so the
/// kernel releases it when the process dies: a broker killed mid-rebuild cannot wedge later ones.
struct Lock(#[allow(dead_code)] fs::File);
impl Lock {
    fn acquire(dir: &Path) -> Result<Self> {
        fs::create_dir_all(dir)?;
        let p = dir.join("broker.lock");
        let f = OpenOptions::new().write(true).create(true).truncate(false).open(&p)?;
        f.try_lock()
            .with_context(|| format!("another broker operation is running ({})", p.display()))?;
        Ok(Lock(f))
    }
}

impl Broker {
    pub fn new(repo: impl Into<PathBuf>, state_dir: impl Into<PathBuf>, rebuild_cmd: impl Into<String>) -> Self {
        let state_dir = state_dir.into();
        Self {
            repo: Repo::new(repo),
            ledger: Ledger::open(state_dir.join("ledger.jsonl")),
            state_dir,
            rebuild_cmd: rebuild_cmd.into(),
        }
    }

    pub fn init(&self) -> Result<()> {
        let _l = Lock::acquire(&self.state_dir)?;
        self.repo.init()?;
        if self.ledger.read_all()?.is_empty() {
            self.ledger.append(NewEntry {
                actor: "user".into(),
                approved_by: None,
                kind: "init".into(),
                tier: Tier::System.to_string(),
                tainted: false,
                reason: "Config repository initialised".into(),
                data: json!({ "commit": self.repo.head()? }),
            })?;
        }
        Ok(())
    }

    pub fn propose(&self, diff_path: &Path, reason: &str, actor: Actor, tainted: bool) -> Result<Entry> {
        let _l = Lock::acquire(&self.state_dir)?;
        let diff = fs::read_to_string(diff_path)
            .with_context(|| format!("reading diff {}", diff_path.display()))?;
        let files = self.repo.diff_files(diff_path)?;
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

    fn rebuild(&self) -> Result<()> {
        let cmd = self.rebuild_cmd.replace("{repo}", &self.repo.root().to_string_lossy());
        let status = Command::new("sh").arg("-c").arg(&cmd).status().context("running rebuild")?;
        if !status.success() {
            bail!("rebuild command failed ({status}): {cmd}");
        }
        Ok(())
    }

    /// Reset the config to `before` and rebuild it. A failed rebuild may already have activated
    /// the new system (switch-to-configuration exits non-zero when a unit fails), so resetting
    /// the repo alone would leave the running system out of step with it. Returns whether the
    /// restore rebuild succeeded.
    fn restore(&self, before: &str) -> Result<bool> {
        self.repo.hard_reset(before)?;
        Ok(self.rebuild().is_ok())
    }

    fn failure_reason(what: &str, before: &str, restored: bool) -> String {
        if restored {
            format!("{what} failed; config reset to {before} and rebuilt")
        } else {
            format!("{what} failed; config reset to {before} but the restore rebuild ALSO failed — running system may not match the config")
        }
    }

    pub fn apply(&self, proposal_seq: u64, approved_by: &str, confirm_tainted: bool) -> Result<Entry> {
        let _l = Lock::acquire(&self.state_dir)?;
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
        let commit = self.repo.apply_and_commit(&tmp, &msg)?;
        let _ = fs::remove_file(&tmp);

        if let Err(e) = self.rebuild() {
            let restored = self.restore(&before)?;
            self.ledger.append(NewEntry {
                actor: "broker".into(),
                approved_by: Some(approved_by.into()),
                kind: "apply_failed".into(),
                tier: tier.to_string(),
                tainted: p.body.tainted,
                reason: Self::failure_reason("Rebuild", &before, restored),
                data: json!({ "proposal": proposal_seq, "error": e.to_string(), "restored": restored }),
            })?;
            return Err(e.context("apply rolled back because the rebuild failed"));
        }

        self.ledger.append(NewEntry {
            actor: p.body.actor.clone(),
            approved_by: Some(approved_by.into()),
            kind: "applied".into(),
            tier: tier.to_string(),
            tainted: p.body.tainted,
            reason: p.body.reason.clone(),
            data: json!({ "proposal": proposal_seq, "commit": commit, "files": files }),
        })
    }

    pub fn rollback(&self, applied_seq: u64, approved_by: &str) -> Result<Entry> {
        let _l = Lock::acquire(&self.state_dir)?;
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
        let new_commit = self.repo.revert_and_commit(&commit, &msg)?;
        if let Err(e) = self.rebuild() {
            let restored = self.restore(&before)?;
            self.ledger.append(NewEntry {
                actor: "broker".into(),
                approved_by: Some(approved_by.into()),
                kind: "rollback_failed".into(),
                tier: a.body.tier.clone(),
                tainted: false,
                reason: Self::failure_reason("Rollback rebuild", &before, restored),
                data: json!({ "applied": applied_seq, "error": e.to_string(), "restored": restored }),
            })?;
            return Err(e.context("rollback aborted because the rebuild failed"));
        }
        self.ledger.append(NewEntry {
            actor: "user".into(),
            approved_by: Some(approved_by.into()),
            kind: "rolled_back".into(),
            tier: a.body.tier.clone(),
            tainted: false,
            reason: format!("Undo: {}", a.body.reason),
            data: json!({ "applied": applied_seq, "reverted_commit": commit, "commit": new_commit }),
        })
    }
}
