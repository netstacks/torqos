//! A session of the agent's, and what it has read (AI-LAYER.md, taint tracking). Once a tool that
//! returns untrusted text has answered, the session is tainted for good: nothing clears it.
//!
//! Its record is written before the model is first asked and again after every tool result, so a
//! session that fails half-way (the model gone, a tool's error) still leaves what it read. The record
//! is in the agent's own home, which the agent can write: it is the agent's account of itself, not a
//! record against it — a program run as the agent (Claude Code under `torq agent run`) can rewrite
//! it. The taint that counts is the broker's (ADR-042, amended): each tainting read is reported there
//! first (doctor.rs), and a proposal is judged by the broker's record, never by this one.

use crate::tools::Tool;
use anyhow::{Context, Result};
use serde_json::json;
use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// How many task records are kept in their directory: past it, the oldest go as a session starts (the review of
/// the rebased branch, B1). Each holds a whole question and every call.
pub const RECORDS_KEPT: usize = 32;

/// Where a harness run with home `home` keeps its task records.
pub fn records_dir(home: &Path) -> PathBuf {
    home.join(".local/state/torq-agent/sessions")
}

/// The task records in `dir`, oldest first: only files this writes (`doctor-<32 hex>.json`, regular, no link).
fn records(dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e).with_context(|| format!("reading {}", dir.display())),
    };
    let mut found = Vec::new();
    for e in entries {
        let e = e.with_context(|| format!("reading {}", dir.display()))?;
        let name = e.file_name().to_string_lossy().into_owned();
        let ours = name.strip_prefix("doctor-").and_then(|n| n.strip_suffix(".json")).is_some_and(|h| h.len() == 32 && h.bytes().all(|b| b.is_ascii_hexdigit()));
        let m = std::fs::symlink_metadata(e.path()).with_context(|| format!("reading {}", e.path().display()))?;
        if ours && m.is_file() {
            found.push((m.modified().with_context(|| format!("reading {}", e.path().display()))?, e.path()));
        }
    }
    found.sort();
    Ok(found.into_iter().map(|(_, p)| p).collect())
}

/// Erase every task record in `dir` (`torq ask --forget`, with the memory): none there is none.
pub fn forget_all(dir: &Path) -> Result<()> {
    for r in records(dir)? {
        std::fs::remove_file(&r).with_context(|| format!("removing {}", r.display()))?;
    }
    Ok(())
}

#[derive(Debug)]
pub struct Session {
    /// Random: no two sessions share an id (inside the launcher's sandbox every pid is the same).
    pub id: String,
    /// Where its record is.
    pub record: PathBuf,
    /// Its id at the broker, which holds its taint.
    pub broker: String,
    /// What it proposed: torqcli's word for each.
    pub proposed: Vec<String>,
    question: String,
    model: String,
    /// The tools that returned untrusted text into it.
    pub tainted_by: BTreeSet<Tool>,
    /// What the broker said tainted it from its start (ADR-051 step 5: not the resident agent; a model others
    /// could stand in for): the broker's words, checked.
    pub held: Vec<String>,
    /// Whether the person attached text to the task: untrusted, as a tool's.
    attached: bool,
    /// Each tool call, as `name(arguments)`.
    pub calls: Vec<String>,
    /// Each call refused, and why.
    pub refused: Vec<String>,
}

impl Session {
    /// A new session, its record made at once in `dir` (the agent's own, 0700) — a new file, never
    /// one that is there already.
    pub fn start(dir: &Path, question: &str, model: &str, broker: &str) -> Result<Session> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).with_context(|| format!("creating {}", dir.display()))?;
        // Bounded: room for this one, the oldest gone.
        let old = records(dir)?;
        for r in &old[..old.len().saturating_sub(RECORDS_KEPT - 1)] {
            std::fs::remove_file(r).with_context(|| format!("removing {}", r.display()))?;
        }
        let id = format!("doctor-{}", random_hex()?);
        let record = dir.join(format!("{id}.json"));
        std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&record).with_context(|| format!("creating {}", record.display()))?;
        let s = Session { id, record, broker: broker.into(), proposed: vec![], question: question.into(), model: model.into(), tainted_by: BTreeSet::new(), held: vec![], attached: false, calls: vec![], refused: vec![] };
        s.save()?;
        Ok(s)
    }

    /// A tool answered into this session.
    pub fn read(&mut self, tool: Tool) {
        if tool.taints() {
            self.tainted_by.insert(tool);
        }
    }

    /// The person attached text to the task (an alarm, a crash's summary): untrusted, so tainting.
    pub fn attached(&mut self) {
        self.attached = true;
    }

    pub fn tainted(&self) -> bool {
        self.attached || !self.held.is_empty() || !self.tainted_by.is_empty()
    }

    pub fn taint_sources(&self) -> Vec<&str> {
        self.held.iter().map(String::as_str).chain(self.attached.then_some(crate::doctor::ATTACHED)).chain(self.tainted_by.iter().map(|t| t.name())).collect()
    }

    /// Write its record as it stands now (over the one it made; no link followed).
    pub fn save(&self) -> Result<()> {
        use std::os::unix::fs::OpenOptionsExt;
        let record = json!({
            "id": self.id,
            "brokerSession": self.broker,
            "question": self.question,
            "model": self.model,
            "tainted": self.tainted(),
            "taintedBy": self.taint_sources(),
            "calls": self.calls,
            "refused": self.refused,
            "proposed": self.proposed,
        });
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(&self.record)
            .with_context(|| format!("writing {}", self.record.display()))?;
        f.write_all(&serde_json::to_vec_pretty(&record)?).with_context(|| format!("writing {}", self.record.display()))
    }
}

/// 128 random bits, as hex, from the kernel.
fn random_hex() -> Result<String> {
    let mut b = [0u8; 16];
    std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b)).context("reading /dev/urandom")?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_session_its_own_new_record() {
        let d = tempfile::tempdir().unwrap();
        let a = Session::start(d.path(), "q", "m", "0").unwrap();
        let b = Session::start(d.path(), "q", "m", "0").unwrap();
        assert_ne!(a.id, b.id);
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 2);
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&a.record).unwrap()).unwrap();
        assert_eq!(v["tainted"], false, "written at once, before anything is read");
    }
    /// The records are bounded: past `RECORDS_KEPT` the oldest go when a session starts. And `forget_all` erases them,
    /// and nothing else in the directory (the review of the rebased branch, B1: each holds a whole question, and
    /// `torq ask --forget` left them all).
    #[test]
    fn records_are_bounded_and_forgotten() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("notes.txt"), "not a record").unwrap();
        let mut first = None;
        for i in 0..RECORDS_KEPT + 3 {
            let s = Session::start(d.path(), &format!("q{i}"), "m", "0").unwrap();
            first.get_or_insert(s.record);
        }
        let records = || std::fs::read_dir(d.path()).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with("doctor-")).count();
        assert_eq!(records(), RECORDS_KEPT);
        assert!(!first.unwrap().exists(), "the oldest went first");
        forget_all(d.path()).unwrap();
        assert_eq!(records(), 0);
        assert!(d.path().join("notes.txt").exists(), "only its records");
        forget_all(&d.path().join("none")).unwrap();
    }
}
