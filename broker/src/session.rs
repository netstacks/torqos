//! The agent's sessions, and a person's trust (ADR-042, 6 and 7, amended).
//!
//! **Sessions.** A program of the agent's (`torq-doctor`) starts a session at the broker over its
//! socket, as `torq-agent`; the broker makes its record here, in its own state directory (root's,
//! 0600), and ledgers `session_started`. Each read of untrusted text the program does, it reports
//! (`session-taint`) before the model is shown the text; the broker marks the record tainted —
//! sticky: nothing clears it, there is no command that does — and ledgers `session_tainted`, once. A
//! proposal of the agent's names its session: it is tainted as this record says, the caller's
//! `--tainted` only adding to it; one that names none is tainted.
//!
//! What the broker can vouch for is what was reported to it: it cannot see what a program of the
//! agent's reads. So a record is the agent's own word, kept where the agent cannot change it — a
//! report cannot be taken back, but a program that never reports (any tool run as the agent other than
//! TorqOS's own) has an untainted session. Hence the default: no session, tainted.
//!
//! **Trust.** A person grants, with their password, "for the next N minutes (at most 60), the agent's
//! user-tier proposals in my own folder apply without asking": `trust_granted`, the ledger the only
//! record of it. While it holds, a proposal of the agent's that names a session, is not tainted, is
//! wholly in that person's folder (user tier, and on the agent's list) and could not cut the machine
//! off is applied as soon as it is made, as that person, its `applied` entry marked with the grant —
//! each its own change, revertible. Never a system-tier change, never a tainted one. `trust-revoke`
//! ends it (`trust_revoked`); its time ends it (`trust_expired`, ledgered by the next operation that
//! writes the ledger — it is over at its time whether or not that has been written).

use crate::ledger::{Entry, NewEntry};
use crate::policy::{self, Actor, Tier};
use crate::{socket, Broker, Lock};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::io::{Read, Write};
use std::path::PathBuf;

/// How many sessions the agent may start in an hour: past that it is refused, without an entry each,
/// so that sessions cannot fill the ledger.
pub const SESSIONS_AN_HOUR: usize = 60;
/// The longest a person's trust lasts.
pub const TRUST_MAX_MINUTES: u32 = 60;

/// What the broker holds of a session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub id: String,
    pub started: String,
    pub tainted: bool,
    /// What tainted it, as reported: a tool's name.
    pub tainted_by: Vec<String>,
}

/// A session's id: 128 random bits, as 32 lowercase hex digits.
pub fn id_ok(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// What a taint's source may be called: a tool's name.
fn source_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn dir(b: &Broker) -> PathBuf {
    b.state_dir.join("sessions")
}

/// The record of session `id`, if there is one (never through a link).
pub fn read(b: &Broker, id: &str) -> Result<Option<Record>> {
    use std::os::unix::fs::OpenOptionsExt;
    if !id_ok(id) {
        bail!("{id:?} is not a session's id");
    }
    let p = dir(b).join(format!("{id}.json"));
    let f = match std::fs::OpenOptions::new().read(true).custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32).open(&p) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", p.display())),
    };
    let mut s = String::new();
    f.take(64 * 1024).read_to_string(&mut s)?;
    Ok(Some(serde_json::from_str(&s).with_context(|| format!("reading {}", p.display()))?))
}

/// Write a record: a new file beside it, then renamed over it — a record is whole or as it was.
fn write(b: &Broker, r: &Record) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let d = dir(b);
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&d)?;
    let part = d.join(format!(".{}.new", r.id));
    let _ = std::fs::remove_file(&part);
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&part)?;
    f.write_all(&serde_json::to_vec(r)?)?;
    f.sync_all()?;
    std::fs::rename(&part, d.join(format!("{}.json", r.id)))?;
    std::fs::File::open(&d)?.sync_all()?;
    Ok(())
}

/// The sessions' own lock: short, never the broker's (which a rebuild holds for minutes), so that a
/// report is never kept waiting by a change.
fn lock(b: &Broker) -> Result<Lock> {
    Lock::acquire_named(&b.state_dir, "sessions.lock", 10)
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Start a session of the agent's: its record, and `session_started` in the ledger.
pub fn start(b: &Broker) -> Result<(Record, Entry)> {
    let _l = lock(b)?;
    let entries = b.ledger.read_all()?;
    if entries.first().is_none_or(|e| e.body.kind != "init") {
        bail!("refused: this machine's ledger has not begun");
    }
    let hour_ago = chrono::Utc::now() - chrono::Duration::hours(1);
    let recent = entries
        .iter()
        .filter(|e| e.body.kind == "session_started")
        .filter(|e| chrono::DateTime::parse_from_rfc3339(&e.body.ts).is_ok_and(|t| t > hour_ago))
        .count();
    if recent >= SESSIONS_AN_HOUR {
        bail!("refused: the agent has started {recent} sessions in the last hour (at most {SESSIONS_AN_HOUR})");
    }
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes)).context("reading /dev/urandom")?;
    let id: String = bytes.iter().map(|x| format!("{x:02x}")).collect();
    if read(b, &id)?.is_some() {
        bail!("a session {id} exists already");
    }
    let r = Record { id: id.clone(), started: now(), tainted: false, tainted_by: vec![] };
    write(b, &r)?;
    let e = b.ledger.append(NewEntry {
        actor: Actor::Agent.to_string(),
        approved_by: None,
        kind: "session_started".into(),
        tier: Tier::Observe.to_string(),
        tainted: false,
        reason: format!("The agent started session {id}"),
        data: json!({ "session": id }),
    })?;
    Ok((r, e))
}

/// Session `id` read untrusted text (from `source`): tainted, for good. The record first, so that a
/// ledger that cannot be written never leaves the session untainted; the ledger once, at the first.
pub fn taint(b: &Broker, id: &str, source: &str) -> Result<Record> {
    if !source_ok(source) {
        bail!("{source:?} is not a source's name (a tool's: letters, digits, - and _)");
    }
    let _l = lock(b)?;
    let mut r = read(b, id)?.context("refused: no such session")?;
    let first = !r.tainted;
    if !r.tainted_by.iter().any(|s| s == source) {
        r.tainted_by.push(source.into());
    }
    r.tainted = true;
    write(b, &r)?;
    if first {
        b.ledger.append(NewEntry {
            actor: Actor::Agent.to_string(),
            approved_by: None,
            kind: "session_tainted".into(),
            tier: Tier::Observe.to_string(),
            tainted: true,
            reason: format!("Session {id} read untrusted text ({source})"),
            data: json!({ "session": id, "source": source }),
        })?;
    }
    Ok(r)
}

/// Whether a proposal is tainted: by `actor`, the caller's `flag` (which only adds), and the
/// broker's record of `session`. A person's proposal names no session; an agent's that names none is
/// tainted; one that names a session there is not, is refused.
pub fn tainted(b: &Broker, actor: Actor, flag: bool, session: Option<&str>) -> Result<bool> {
    match (actor, session) {
        (Actor::User, Some(_)) => bail!("refused: a session is the agent's; a person's proposal names none"),
        (Actor::User, None) => Ok(flag),
        (Actor::Agent, None) => Ok(true),
        (Actor::Agent, Some(id)) => Ok(flag || read(b, id)?.context("refused: the proposal names no session the broker holds")?.tainted),
    }
}

/// A person's trust, granted and not yet revoked or expired on the record.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Grant {
    /// Its `trust_granted` entry.
    pub seq: u64,
    pub person: String,
    pub minutes: u64,
    pub until: String,
}

impl Grant {
    fn over(&self, at: chrono::DateTime<chrono::Utc>) -> bool {
        chrono::DateTime::parse_from_rfc3339(&self.until).map_or(true, |u| u <= at)
    }
}

/// Every grant not settled on the record (revoked or expired), oldest first.
pub fn unsettled(entries: &[Entry]) -> Vec<Grant> {
    let settled = |seq: u64| entries.iter().any(|e| matches!(e.body.kind.as_str(), "trust_revoked" | "trust_expired") && e.body.data["grant"] == json!(seq));
    entries
        .iter()
        .filter(|e| e.body.kind == "trust_granted" && !settled(e.body.seq))
        .filter_map(|e| {
            Some(Grant {
                seq: e.body.seq,
                person: e.body.data["person"].as_str()?.into(),
                minutes: e.body.data["minutes"].as_u64()?,
                until: e.body.data["until"].as_str()?.into(),
            })
        })
        .collect()
}

/// The grants that hold now: unsettled, and their time not over.
pub fn active(entries: &[Entry]) -> Vec<Grant> {
    let now = chrono::Utc::now();
    unsettled(entries).into_iter().filter(|g| !g.over(now)).collect()
}

/// Ledger `trust_expired` for every grant whose time is over and is not yet settled. Under the
/// broker's lock: by every operation that may apply under trust, and by a grant and a revoke.
pub fn settle_trust(b: &Broker) -> Result<()> {
    let now = chrono::Utc::now();
    for g in unsettled(&b.ledger.read_all()?).into_iter().filter(|g| g.over(now)) {
        b.ledger.append(NewEntry {
            actor: "broker".into(),
            approved_by: None,
            kind: "trust_expired".into(),
            tier: Tier::User.to_string(),
            tainted: false,
            reason: format!("{}'s trust in the agent ended at {}", g.person, g.until),
            data: json!({ "grant": g.seq, "person": g.person, "until": g.until }),
        })?;
    }
    Ok(())
}

/// `person` trusts the agent's user-tier proposals in their own folder for `minutes`. (Over the
/// socket: only that person, with their password.)
pub fn grant(b: &Broker, person: &str, minutes: u32) -> Result<Entry> {
    if !policy::user_name_ok(person) {
        bail!("{person:?} is not a person's name");
    }
    if !(1..=TRUST_MAX_MINUTES).contains(&minutes) {
        bail!("refused: trust lasts 1 to {TRUST_MAX_MINUTES} minutes, not {minutes}");
    }
    if !b.agent_limits.users.iter().any(|u| u == person) {
        bail!("refused: the agent may not propose in {person}'s folder (torq.agent.mayPropose.users), so there is nothing to trust it with");
    }
    let _l = Lock::acquire(&b.state_dir)?;
    settle_trust(b)?;
    if let Some(g) = active(&b.ledger.read_all()?).into_iter().find(|g| g.person == person) {
        bail!("refused: {person} trusts the agent already, until {} (#{}): revoke it first", g.until, g.seq);
    }
    let until = (chrono::Utc::now() + chrono::Duration::minutes(minutes.into())).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    b.ledger.append(NewEntry {
        actor: Actor::User.to_string(),
        approved_by: Some(person.into()),
        kind: "trust_granted".into(),
        tier: Tier::User.to_string(),
        tainted: false,
        reason: format!("{person} lets the agent's proposals in their own folder apply without asking, for {minutes} minutes"),
        data: json!({ "person": person, "minutes": minutes, "until": until }),
    })
}

/// End `person`'s trust now, if they have granted one.
pub fn revoke(b: &Broker, person: &str) -> Result<Option<Entry>> {
    let _l = Lock::acquire(&b.state_dir)?;
    settle_trust(b)?;
    let Some(g) = active(&b.ledger.read_all()?).into_iter().find(|g| g.person == person) else { return Ok(None) };
    Ok(Some(b.ledger.append(NewEntry {
        actor: Actor::User.to_string(),
        approved_by: Some(person.into()),
        kind: "trust_revoked".into(),
        tier: Tier::User.to_string(),
        tainted: false,
        reason: format!("{person} revoked their trust in the agent"),
        data: json!({ "grant": g.seq, "person": person }),
    })?))
}

/// What of proposal `p` could cut the machine off, if anything (`cuts_off`, recorded when proposed).
pub fn cuts_off(p: &Entry) -> Option<Vec<String>> {
    let c: Vec<String> = p.body.data["cuts_off"].as_array()?.iter().filter_map(|s| s.as_str().map(String::from)).collect();
    (!c.is_empty()).then_some(c)
}

/// Whose trust `p` could be applied under: an agent's proposal, naming a session, not tainted, user
/// tier, wholly one person's folder, nothing that could cut the machine off. None: not one.
fn trusted_by(p: &Entry) -> Option<String> {
    let b = &p.body;
    if b.kind != "proposal" || b.actor != Actor::Agent.to_string() || b.tainted || b.tier != Tier::User.to_string() || b.data["session"].as_str().is_none() || cuts_off(p).is_some() {
        return None;
    }
    let files: Vec<String> = b.data["files"].as_array()?.iter().filter_map(|f| f.as_str().map(String::from)).collect();
    let person = files.first()?.strip_prefix("users/")?.split_once('/')?.0.to_string();
    socket::theirs(&person, &socket::Change { tier: Tier::User, files }).then_some(person)
}

/// Apply proposal `p` now if a person's trust covers it (`trusted_by`, and their grant holding): as
/// that person, its entry marked with the grant. None: not covered; it waits for a person.
pub fn apply_trusted(b: &Broker, p: &Entry) -> Result<Option<Entry>> {
    if trusted_by(p).is_none() {
        return Ok(None);
    }
    let lock = Lock::acquire(&b.state_dir)?;
    settle_trust(b)?;
    // The proposal as the ledger has it, judged again under the lock.
    let p = b.ledger.get(p.body.seq)?;
    let Some(person) = trusted_by(&p) else { return Ok(None) };
    // Its session as the broker holds it now: tainted since the proposal, it is not trusted.
    if p.body.data["session"].as_str().map(|s| read(b, s)).transpose()?.flatten().is_none_or(|r| r.tainted) {
        return Ok(None);
    }
    let Some(g) = active(&b.ledger.read_all()?).into_iter().find(|g| g.person == person) else { return Ok(None) };
    b.apply_locked(&lock, p.body.seq, &person, false, None, Some(("trust", g.seq))).map(Some)
}
