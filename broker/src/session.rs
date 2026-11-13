//! The agent's sessions, and a person's trust (ADR-042, 6 and 7, amended).
//!
//! **Sessions.** A program of the agent's (`torq-doctor`) starts a session at the broker over its
//! socket, as its agent account (`torq-agent-<person>`, ADR-051 3); the broker makes its record here,
//! in its own state directory (root's, 0600), naming that account: a session is its account's alone
//! — no other agent reports into it or cites it. Each read of untrusted text the program does, it
//! reports (`session-taint`) before the model
//! is shown the text; the broker marks the record tainted — sticky: nothing clears it, there is no
//! command that does. A proposal of the agent's names its session: it is tainted as this record says,
//! the caller's `--tainted` only adding to it; one that names none is tainted. The ledger holds a
//! session only where a proposal cites it: the proposal's entry carries the session's id, when it
//! started and what tainted it — so a session that proposed nothing costs the ledger nothing, and the
//! record of one that did outlives its file here.
//!
//! What the broker can vouch for is what was reported to it: it cannot see what a program of the
//! agent's reads. So a record is the agent's own word, kept where the agent cannot change it — a
//! report cannot be taken back. A program that would never report (any tool run as the agent other than
//! TorqOS's own) is why only the person's resident agent — its process in `torq-agent@<person>.service`'s
//! control group, as serve reads it from the peer's pidfd (ADR-051 step 5) — starts a session untainted
//! and has its proposals judged by it: every other caller of an agent account starts tainted
//! (`not-the-resident-agent`), and its proposals are tainted whatever session they cite. Hence the
//! defaults: no session, tainted; not resident, tainted.
//!
//! **Trust.** A person grants, with their password, "for the next N minutes (at most 60), my agent's
//! user-tier proposals in my own folder apply without asking": `trust_granted`, the ledger the only
//! record of it. While it holds, a proposal of that person's own agent that names a session, is not tainted, is
//! wholly in that person's folder (user tier, and on the agent's list) and could not cut the machine
//! off is applied as soon as it is made, as that person, its `applied` entry marked with the grant —
//! each its own change, revertible. Never a system-tier change, never a tainted one. Grants, revokes
//! and expiries are written under the sessions' own short lock, never the broker's: a revoke is on the
//! record at once, even while a trusted change is being built, and every trusted apply checks the
//! record again, under the broker's lock, just before it starts. Its time ends it (`trust_expired`,
//! ledgered by the next operation that writes trust or may apply under it — it is over at its time
//! whether or not that has been written).

use crate::ledger::{Entry, NewEntry};
use crate::policy::{self, Actor, Tier};
use crate::{socket, Broker, Lock};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::io::{Read, Write};
use std::path::PathBuf;

/// How many sessions may be started in an hour: past that, refused. Each agent its own count, and within it the
/// resident agent's own, apart from every other program's of the agent account (step 5's second review): a program
/// run as the agent account can use up its own, never the resident agent's.
pub const SESSIONS_AN_HOUR: usize = 600;
/// How many session records are kept for each agent: past that, its oldest go. A proposal naming a session whose
/// record is gone is refused, never taken as untainted; what tainted it is in its proposal's entry.
pub const SESSIONS_KEPT: usize = 2000;
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
    /// The agent account whose session it is (ADR-051 3). Empty (a record from before): no agent's,
    /// removed by the next session started,
    /// so no agent reports into it or cites it.
    #[serde(default)]
    pub agent: String,
    /// The proposals that cited it, as the broker ledgered them (what the agent is told it did).
    #[serde(default)]
    pub proposals: Vec<Cited>,
    /// Whether its person's resident agent started it (ADR-051 step 5): only the resident agent cites a session it
    /// started, and only another program of the agent's one that program's kind started — so no proposal of
    /// another program's is told to the resident agent as its own. False for a record from before.
    #[serde(default)]
    pub resident: bool,
    /// How many of its secret uses a person has been asked about (ask.rs): past the most, no more are asked.
    #[serde(default)]
    pub asks: u32,
    /// The secrets a person said not to ask about again in this session, each as `Owner::key` says it.
    #[serde(default)]
    pub denied: Vec<String>,
}

/// A proposal that cited a session, and the change it became if a person's trust applied it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cited {
    pub proposal: u64,
    #[serde(default)]
    pub applied: Option<u64>,
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

/// The sessions' and trust's own lock: short, never the broker's (which a rebuild holds for
/// minutes), so that a report or a revoke is never kept waiting by a change. Never taken twice in
/// one process at once; may be taken while the broker's is held, never the other way round.
fn lock(b: &Broker) -> Result<Lock> {
    Lock::acquire_named(&b.state_dir, "sessions.lock", 10)
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Every record kept, as (when it started, its id, whose, whether the resident agent started it).
fn kept(b: &Broker) -> Result<Vec<(String, String, String, bool)>> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir(b)) else { return Ok(out) };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(id) = name.strip_suffix(".json").filter(|i| id_ok(i)) else { continue };
        if let Some(r) = read(b, id)? {
            out.push((r.started, r.id, r.agent, r.resident));
        }
    }
    out.sort();
    Ok(out)
}

/// Start a session of the acting agent's (`Broker::as_agent`): its record, here, naming that account.
/// At most `SESSIONS_AN_HOUR` an hour for each agent; past
/// `SESSIONS_KEPT`, the oldest records go.
pub fn start(b: &Broker) -> Result<Record> {
    let (agent, _) = b.acting_agent()?;
    // Untainted at its start only from the resident agent (ADR-051 step 5), and only when its model is one no
    // account here can stand in for (its review: a TCP model on this machine, a socket in a place others write).
    // Judged before the sessions' lock: it may ask the resolver whether the
    // model's host is this machine, and a slow resolver then holds up this start alone, not every agent's. Not
    // bounded here: getaddrinfo's own timeouts (resolv.conf) are the wait, for this start only.
    let taint = if b.resident() { b.model_taint() } else { Some(socket::NOT_RESIDENT) };
    let _l = lock(b)?;
    let kept = kept(b)?;
    let hour_ago = chrono::Utc::now() - chrono::Duration::hours(1);
    // Each agent its own count: one person's agent cannot keep another's from starting.
    // And the resident agent its own, apart from any other program of the agent account's (step 5's second review):
    // a coding tool cannot keep the resident agent from starting, nor it a coding tool; together at most twice.
    let recent = kept.iter().filter(|(t, _, a, r)| a == agent && *r == b.resident() && chrono::DateTime::parse_from_rfc3339(t).is_ok_and(|t| t > hour_ago)).count();
    if recent >= SESSIONS_AN_HOUR {
        bail!("refused: {recent} of {agent}'s sessions were started in the last hour (at most {SESSIONS_AN_HOUR})");
    }
    // A record of no agent's (from before ADR-051 3) is no one's to report into or cite: it goes. Then its
    // own oldest go, never another agent's.
    for (_, id, _, _) in kept.iter().filter(|(_, _, a, _)| a.is_empty()) {
        std::fs::remove_file(dir(b).join(format!("{id}.json")))?;
    }
    let mine: Vec<&(String, String, String, bool)> = kept.iter().filter(|(_, _, a, r)| a == agent && *r == b.resident()).collect();
    for (_, id, _, _) in mine.iter().take((mine.len() + 1).saturating_sub(SESSIONS_KEPT)) {
        std::fs::remove_file(dir(b).join(format!("{id}.json")))?;
    }
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes)).context("reading /dev/urandom")?;
    let id: String = bytes.iter().map(|x| format!("{x:02x}")).collect();
    if read(b, &id)?.is_some() {
        bail!("a session {id} exists already");
    }
    let (tainted, tainted_by) = match taint {
        None => (false, vec![]),
        Some(why) => (true, vec![why.to_string()]),
    };
    let r = Record { id, started: now(), tainted, tainted_by, agent: agent.into(), proposals: vec![], resident: b.resident(), asks: 0, denied: vec![] };
    write(b, &r)?;
    Ok(r)
}

/// How many distinct sources a session's record names; past them it is tainted all the same.
pub const SOURCES_KEPT: usize = 32;

/// Session `id` read untrusted text (from `source`): tainted, for good. Only by the agent whose it is.
pub fn taint(b: &Broker, id: &str, source: &str) -> Result<Record> {
    if !source_ok(source) {
        bail!("{source:?} is not a source's name (a tool's: letters, digits, - and _)");
    }
    let _l = lock(b)?;
    let (agent, _) = b.acting_agent()?;
    let mut r = read(b, id)?.context("refused: no such session")?;
    if r.agent != agent {
        bail!("refused: session {id} is not {agent}'s");
    }
    // Reported into by its own kind of caller alone, as it is cited: another
    // program of the agent account reads the resident agent's session id in the ledger, and could otherwise taint it
    // on purpose, so that the resident agent's proposals wait and its trust never applies. Taint only adds, so this
    // is a denial it closes, not a way in; the resident agent never reports into another program's session either.
    if r.resident != b.resident() {
        let whose = if r.resident { "the resident agent's" } else { "another program's of the agent's" };
        bail!("refused: session {id} is {whose}: only that kind of caller reports into it");
    }
    // Bounded: a record any agent program can report into must not grow until no one can start a
    // session (the audit of 0.3.0). Tainted is tainted; past the bound, what tainted it is said as "more".
    if !r.tainted_by.iter().any(|s| s == source) && r.tainted_by.len() < SOURCES_KEPT {
        r.tainted_by.push(source.into());
    }
    r.tainted = true;
    write(b, &r)?;
    Ok(r)
}

/// Whether a proposal is tainted — by `actor`, the caller's `flag` (which only adds), and the
/// broker's record of `session` — and what of the session goes into its entry. A person's proposal
/// names no session; an agent's that names none is tainted; one that names a session not held here is
/// refused.
pub fn judge(b: &Broker, actor: Actor, flag: bool, session: Option<&str>) -> Result<(bool, Option<serde_json::Value>)> {
    match (actor, session) {
        (Actor::User, Some(_)) => bail!("refused: a session is the agent's; a person's proposal names none"),
        (Actor::User, None) => Ok((flag, None)),
        (Actor::Agent, None) => {
            b.acting_agent()?;
            Ok((true, None))
        }
        (Actor::Agent, Some(id)) => {
            let (agent, _) = b.acting_agent()?;
            let r = read(b, id)?.context("refused: the proposal names no session the broker holds")?;
            // Its own session, or none: one agent never cites another's (ADR-051 3).
            if r.agent != agent {
                bail!("refused: session {id} is not {agent}'s");
            }
            // A session is cited by its own kind of caller only (step 5's review): its id is in the ledger, and a
            // proposal of another program's cited by the resident agent's session would be told to the resident
            // agent as its own (`cite`, the record's `proposals`).
            if r.resident != b.resident() {
                let whose = if r.resident { "the resident agent's" } else { "another program's of the agent's" };
                bail!("refused: session {id} is {whose}: only that kind of caller cites it");
            }
            // Judged by its session only when it comes from the resident agent (ADR-051 step 5): a session's id
            // is in the ledger for anyone to read, so another program of the agent's could cite one.
            let mut by = r.tainted_by.clone();
            if !b.resident() && !by.iter().any(|s| s == socket::NOT_RESIDENT) {
                by.push(socket::NOT_RESIDENT.into());
            }
            Ok((flag || r.tainted || !b.resident(), Some(json!({ "id": r.id, "started": r.started, "tainted_by": by }))))
        }
    }
}

/// The proposal `seq` cited session `id` (and, `applied`, became that change under trust): noted in
/// its record, for what the agent is told.
pub fn cite(b: &Broker, id: &str, seq: u64, applied: Option<u64>) -> Result<()> {
    let _l = lock(b)?;
    let Some(mut r) = read(b, id)? else { return Ok(()) };
    match r.proposals.iter_mut().find(|c| c.proposal == seq) {
        Some(c) => c.applied = applied.or(c.applied),
        None => r.proposals.push(Cited { proposal: seq, applied }),
    }
    write(b, &r)
}

/// A person is asked about one more secret use of session `id`: counted in its record, and the count given.
pub(crate) fn count_ask(b: &Broker, id: &str) -> Result<u32> {
    let _l = lock(b)?;
    let mut r = read(b, id)?.context("refused: no such session")?;
    r.asks = r.asks.saturating_add(1);
    write(b, &r)?;
    Ok(r.asks)
}

/// A person said not to ask again in session `id` about the secret `key`: its uses are refused at once from now.
/// A session whose record is gone has nothing left to refuse.
pub(crate) fn deny_rest(b: &Broker, id: &str, key: &str) -> Result<()> {
    let _l = lock(b)?;
    let Some(mut r) = read(b, id)? else { return Ok(()) };
    if !r.denied.iter().any(|k| k == key) {
        r.denied.push(key.into());
        write(b, &r)?;
    }
    Ok(())
}

/// The longest a person's grant of secret uses lasts, and the longest a session it was given for is covered.
pub const SECRET_GRANT_MAX_MINUTES: u32 = 480;

/// What a grant covers: the one thing, and nothing else. A grant of secret uses applies no proposal, and trust in
/// proposals lets no secret be used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Covers {
    /// The agent's untainted proposals in the person's own folder (ADR-042 7).
    Proposals,
    /// The agent's uses of one secret: the person's own of that name, or (`system`) the system's.
    Secret { name: String, system: bool },
    /// The agent's uses of every secret of the person's own; never a system secret.
    AllSecrets,
}

impl Covers {
    /// As a `trust_*` entry says it (`data.covers`).
    fn word(&self) -> String {
        match self {
            Covers::Proposals => "proposals".into(),
            Covers::Secret { name, .. } => format!("secret {name}"),
            Covers::AllSecrets => "all-secrets".into(),
        }
    }

    /// From a `trust_granted` entry's data. One that says nothing is from before grants covered anything else:
    /// proposals. One that says what no torq knows covers nothing (None).
    fn read(data: &serde_json::Value) -> Option<Covers> {
        match data.get("covers") {
            None => Some(Covers::Proposals),
            Some(c) => match c.as_str()? {
                "proposals" => Some(Covers::Proposals),
                "all-secrets" => Some(Covers::AllSecrets),
                w => {
                    let name = w.strip_prefix("secret ").filter(|n| torq_wire::secrets::name_ok(n))?;
                    Some(Covers::Secret { name: name.into(), system: data["owner"] == json!("system") })
                }
            },
        }
    }

    /// The most minutes such a grant lasts.
    fn max_minutes(&self) -> u32 {
        match self {
            Covers::Proposals => TRUST_MAX_MINUTES,
            Covers::Secret { .. } | Covers::AllSecrets => SECRET_GRANT_MAX_MINUTES,
        }
    }
}

/// A person's grant to their own agent, not yet revoked or expired on the record: trust in its proposals, or leave
/// to use a secret without asking (the approval design, parts 3 and 4).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Grant {
    /// Its `trust_granted` entry.
    pub seq: u64,
    pub person: String,
    pub minutes: u64,
    pub until: String,
    /// When it was granted: its entry's time. A grant lasts no longer than its minutes from then,
    /// whatever its `until` says (a ledger from elsewhere is someone else's text: the audit of 0.3.0).
    pub at: String,
    pub covers: Covers,
    /// Whether its entry says it holds in a session that read untrusted text. Honoured only of one named secret
    /// (`covers_use`), whatever an entry says.
    pub tainted_too: bool,
    /// Given for "this session": only the agent's sessions already started when it was granted.
    pub session: bool,
}

impl Grant {
    fn over(&self, at: chrono::DateTime<chrono::Utc>) -> bool {
        let minutes = i64::try_from(self.minutes.min(u64::from(self.covers.max_minutes()))).unwrap_or(0);
        let ends = chrono::DateTime::parse_from_rfc3339(&self.at).map(|t| t + chrono::Duration::minutes(minutes));
        let until = chrono::DateTime::parse_from_rfc3339(&self.until);
        match (until, ends) {
            (Ok(u), Ok(e)) => u.min(e) <= at,
            _ => true,
        }
    }

    /// Whether it lets the agent use the secret `name` (the system's, or its person's own) without asking, at `at`.
    /// `tainted`: the use's session read untrusted text, or the use names none. `started`: when its session did.
    pub fn covers_use(&self, system: bool, name: &str, tainted: bool, started: Option<&str>, at: chrono::DateTime<chrono::Utc>) -> bool {
        let what = match &self.covers {
            Covers::Secret { name: n, system: s } => n == name && *s == system,
            Covers::AllSecrets => !system,
            Covers::Proposals => false,
        };
        // Under taint, only a grant that says so, and only of one named secret: never of all.
        let taint = !tainted || (self.tainted_too && matches!(self.covers, Covers::Secret { .. }));
        // "This session": one that was running when the person said so, and for no longer than a grant may last.
        let scope = !self.session
            || started.and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()).is_some_and(|s| {
                chrono::DateTime::parse_from_rfc3339(&self.at).is_ok_and(|g| s <= g) && at < s + chrono::Duration::minutes(SECRET_GRANT_MAX_MINUTES.into())
            });
        what && taint && scope && !self.over(at)
    }
}

/// Every grant not settled on the record (revoked or expired), oldest first.
pub fn unsettled(entries: &[Entry]) -> Vec<Grant> {
    let settled = |seq: u64| entries.iter().any(|e| matches!(e.body.kind.as_str(), "trust_revoked" | "trust_expired") && e.body.data["grant"] == json!(seq));
    entries
        .iter()
        .filter(|e| e.body.kind == "trust_granted" && !settled(e.body.seq))
        .filter_map(|e| {
            let covers = Covers::read(&e.body.data)?;
            Some(Grant {
                seq: e.body.seq,
                person: e.body.data["person"].as_str()?.into(),
                minutes: e.body.data["minutes"].as_u64()?,
                until: e.body.data["until"].as_str()?.into(),
                at: e.body.ts.clone(),
                tainted_too: e.body.data["tainted_too"] == json!(true),
                session: e.body.data["scope"] == json!("session"),
                covers,
            })
        })
        .collect()
}

/// The grants that hold now: unsettled, and their time not over.
pub fn active(entries: &[Entry]) -> Vec<Grant> {
    let now = chrono::Utc::now();
    unsettled(entries).into_iter().filter(|g| !g.over(now)).collect()
}

/// Ledger `trust_expired` for every grant whose time is over and is not yet settled, under the
/// sessions' lock (held).
fn settle_locked(b: &Broker) -> Result<()> {
    let now = chrono::Utc::now();
    for g in unsettled(&b.ledger.read_all()?).into_iter().filter(|g| g.over(now)) {
        let what = match g.covers {
            Covers::Proposals => "trust in the agent".to_string(),
            _ => format!("leave for the agent to use {} without asking", said(&g.covers)),
        };
        b.ledger.append(NewEntry {
            actor: "broker".into(),
            approved_by: None,
            kind: "trust_expired".into(),
            tier: Tier::User.to_string(),
            tainted: false,
            reason: format!("{}'s {what} ended at {}", g.person, g.until),
            data: json!({ "grant": g.seq, "person": g.person, "until": g.until, "covers": g.covers.word() }),
        })?;
    }
    Ok(())
}

/// `settle_locked`, taking the sessions' lock: by every operation that may apply under trust.
pub fn settle_trust(b: &Broker) -> Result<()> {
    let _l = lock(b)?;
    settle_locked(b)
}

/// What a grant of secret uses covers, as a person reads it.
fn said(c: &Covers) -> String {
    match c {
        Covers::Proposals => "its proposals".into(),
        Covers::Secret { name, system: true } => format!("the system's secret {name}"),
        Covers::Secret { name, system: false } => format!("their secret {name}"),
        Covers::AllSecrets => "all their own secrets".into(),
    }
}

/// A grant on the record: `person`'s, to their own agent, of `covers`, for `minutes`. One of each at a time.
/// `extra`: what else its entry says. Under the sessions' lock, never the broker's.
fn grant_of(b: &Broker, person: &str, covers: Covers, minutes: u32, reason: String, extra: serde_json::Value) -> Result<Entry> {
    if !policy::user_name_ok(person) {
        bail!("{person:?} is not a person's name");
    }
    if !b.agent_limits.has_agent(person) {
        bail!("refused: {person} has no agent (torq.agent.people), so there is nothing to trust it with");
    }
    let _l = lock(b)?;
    settle_locked(b)?;
    if let Some(g) = active(&b.ledger.read_all()?).into_iter().find(|g| g.person == person && g.covers == covers) {
        let what = if covers == Covers::Proposals { "trusts the agent already".to_string() } else { format!("lets the agent use {} already", said(&covers)) };
        bail!("refused: {person} {what}, until {} (#{}): revoke it first", g.until, g.seq);
    }
    let until = (chrono::Utc::now() + chrono::Duration::minutes(minutes.into())).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut data = json!({ "person": person, "minutes": minutes, "until": until, "covers": covers.word() });
    if let serde_json::Value::Object(x) = extra {
        for (k, v) in x {
            data[k] = v;
        }
    }
    b.ledger.append(NewEntry { actor: Actor::User.to_string(), approved_by: Some(person.into()), kind: "trust_granted".into(), tier: Tier::User.to_string(), tainted: false, reason, data })
}

/// `person` trusts the agent's user-tier proposals in their own folder for `minutes`. (Over the
/// socket: only that person, with their password.)
pub fn grant(b: &Broker, person: &str, minutes: u32) -> Result<Entry> {
    if !(1..=TRUST_MAX_MINUTES).contains(&minutes) {
        bail!("refused: trust lasts 1 to {TRUST_MAX_MINUTES} minutes, not {minutes}");
    }
    grant_of(b, person, Covers::Proposals, minutes, format!("{person} lets the agent's proposals in their own folder apply without asking, for {minutes} minutes"), json!({}))
}

/// How long a grant of secret uses is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// 1 to `SECRET_GRANT_MAX_MINUTES`.
    Minutes(u32),
    /// The agent's sessions running now, for as long as a grant may last.
    Session,
}

/// `person` lets their own agent use a secret (`covers`: one, or all their own) without asking. (Over the socket:
/// only that person, with their password.) What it never does: it gives the agent no secret its rule does not —
/// a use is still judged by its rule first, its host and its actors (`secrets::may_use`) — and under taint it
/// holds only as `tainted_too`, which is of one named secret, for minutes.
pub fn grant_secret(b: &Broker, person: &str, covers: Covers, scope: Scope, tainted_too: bool) -> Result<Entry> {
    use torq_wire::secrets::AGENT;
    let minutes = match scope {
        Scope::Minutes(m) if (1..=SECRET_GRANT_MAX_MINUTES).contains(&m) => m,
        Scope::Minutes(m) => bail!("refused: such a grant lasts 1 to {SECRET_GRANT_MAX_MINUTES} minutes, not {m}"),
        Scope::Session => SECRET_GRANT_MAX_MINUTES,
    };
    let mut extra = json!({});
    match &covers {
        Covers::Proposals => bail!("refused: trust in proposals is `trust-grant`'s"),
        Covers::AllSecrets if tainted_too => bail!("refused: all secrets at once are never used unasked from a session that read untrusted text: tainted-too is of one named secret"),
        Covers::AllSecrets if scope == Scope::Session => bail!("refused: all secrets at once are let for minutes, not for a session"),
        Covers::AllSecrets => {}
        Covers::Secret { .. } if tainted_too && scope == Scope::Session => bail!("refused: tainted-too is for minutes, not for a session"),
        Covers::Secret { name, system } => {
            // A grant widens no rule: said here so that a person is not left thinking it did.
            let owner = if *system { crate::secrets::Owner::System } else { crate::secrets::Owner::Person(person.into()) };
            let rules = b.rules_of(&owner)?;
            let rule = rules.get(name).with_context(|| format!("refused: no rule for a secret {name} of {}", if *system { "the system's" } else { "theirs" }))?;
            let named = if *system { b.agent_limits.agents.iter().any(|(a, p)| p == person && rule.actors.contains(a)) } else { rule.actors.iter().any(|a| a == AGENT) };
            if !named {
                bail!("refused: the rule of secret {name} does not let {person}'s agent use it: a grant widens no rule");
            }
            extra["owner"] = if *system { "system".into() } else { "user".into() };
        }
    }
    if scope == Scope::Session {
        extra["scope"] = "session".into();
    }
    if tainted_too {
        extra["tainted_too"] = true.into();
    }
    let how_long = match scope {
        Scope::Minutes(m) => format!("for {m} minutes"),
        Scope::Session => "in its sessions running now".into(),
    };
    let even = if tainted_too { ", even from a session that read untrusted text" } else { "" };
    grant_of(b, person, covers.clone(), minutes, format!("{person} lets the agent use {} without asking, {how_long}{even}", said(&covers)), extra)
}

/// The grant that lets `person`'s agent make this use without asking, if one holds (`Grant::covers_use`): its entry.
pub fn covering(b: &Broker, person: &str, system: bool, name: &str, tainted: bool, started: Option<&str>) -> Result<Option<u64>> {
    let _l = lock(b)?;
    settle_locked(b)?;
    let now = chrono::Utc::now();
    Ok(active(&b.ledger.read_all()?).into_iter().find(|g| g.person == person && g.covers_use(system, name, tainted, started, now)).map(|g| g.seq))
}

/// `trust_revoked` for grant `g`, by its person. Under the sessions' lock (held).
fn revoke_locked(b: &Broker, g: &Grant) -> Result<Entry> {
    let what = match g.covers {
        Covers::Proposals => "trust in the agent".to_string(),
        _ => format!("leave for the agent to use {} without asking", said(&g.covers)),
    };
    b.ledger.append(NewEntry {
        actor: Actor::User.to_string(),
        approved_by: Some(g.person.clone()),
        kind: "trust_revoked".into(),
        tier: Tier::User.to_string(),
        tainted: false,
        reason: format!("{} revoked their {what}", g.person),
        data: json!({ "grant": g.seq, "person": g.person, "covers": g.covers.word() }),
    })
}

/// End `person`'s trust now, if they have granted one: on the record at once, whatever the broker is
/// doing (the sessions' lock, never the broker's). A trusted apply already started finishes; none
/// starts after.
pub fn revoke(b: &Broker, person: &str) -> Result<Option<Entry>> {
    let _l = lock(b)?;
    settle_locked(b)?;
    let Some(g) = active(&b.ledger.read_all()?).into_iter().find(|g| g.person == person && g.covers == Covers::Proposals) else { return Ok(None) };
    Ok(Some(revoke_locked(b, &g)?))
}

/// End `person`'s grants of secret uses now: those of the secret `name` (their own or the system's), or with none
/// named, every one. No use starts under them after.
pub fn revoke_secrets(b: &Broker, person: &str, name: Option<&str>) -> Result<Vec<Entry>> {
    let _l = lock(b)?;
    settle_locked(b)?;
    let theirs = |g: &Grant| match (&g.covers, name) {
        (Covers::Proposals, _) => false,
        (_, None) => true,
        (Covers::Secret { name: n, .. }, Some(name)) => n == name,
        (Covers::AllSecrets, Some(_)) => false,
    };
    active(&b.ledger.read_all()?).iter().filter(|g| g.person == person && theirs(g)).map(|g| revoke_locked(b, g)).collect()
}

/// What of proposal `p` could cut the machine off, if anything (`cuts_off`, recorded when proposed).
pub fn cuts_off(p: &Entry) -> Option<Vec<String>> {
    let c: Vec<String> = p.body.data["cuts_off"].as_array()?.iter().filter_map(|s| s.as_str().map(String::from)).collect();
    (!c.is_empty()).then_some(c)
}

/// The session a proposal cites, by its id (`data.session.id`).
pub fn cited(p: &Entry) -> Option<&str> {
    p.body.data["session"]["id"].as_str()
}

/// Whose trust `p` could be applied under: an agent's proposal, naming a session, not tainted, user
/// tier, wholly one person's folder, nothing that could cut the machine off, and made by that person's
/// own agent, as the broker's map says now (ADR-051 3: a person's trust covers their agent alone).
/// None: not one.
fn trusted_by(b: &Broker, p: &Entry) -> Option<String> {
    let e = &p.body;
    // Nor one that makes a secret `never-even-tainted`: that asks its person's password, which trust never stands in for.
    if e.kind != "proposal" || e.actor != Actor::Agent.to_string() || e.tainted || e.tier != Tier::User.to_string() || cited(p).is_none() || cuts_off(p).is_some() || e.data.get("standing").is_some() {
        return None;
    }
    let files: Vec<String> = e.data["files"].as_array()?.iter().filter_map(|f| f.as_str().map(String::from)).collect();
    let person = files.first()?.strip_prefix("users/")?.split_once('/')?.0.to_string();
    let agent = e.data["agent"].as_str()?;
    if b.agent_limits.serves(agent) != Some(person.as_str()) {
        return None;
    }
    // Made by that person's resident agent itself, over the socket (the kernel's word for who called: its own account,
    // ADR-051 3 amended 2026-11-11) — never a proposal that only says it is the agent's (root's `torq-broker propose
    // --actor agent --agent …`), nor the agent account's (any program run as it: tainted, but said here too).
    if e.data["via"].as_str() != Some("socket") || e.data["caller"].as_str().is_none_or(|c| Some(c) != b.resident_of(&person)) {
        return None;
    }
    socket::theirs(&person, &socket::Change { tier: Tier::User, files, standing: false }).then_some(person)
}

/// Apply proposal `p` now if a person's trust covers it (`trusted_by`, and their grant holding): as
/// that person, its entry marked with the grant. None: not covered; it waits for a person. Judged
/// under the broker's lock, with the sessions' lock held only while the record is read — so a revoke
/// is never kept waiting by the build.
pub fn apply_trusted(b: &Broker, p: &Entry) -> Result<Option<Entry>> {
    if trusted_by(b, p).is_none() {
        return Ok(None);
    }
    // The model judged again, as the broker's settings stand at this apply (step 5's second review): a session that
    // started clean with one model is not trusted once the machine asks one that is not believed. Before any lock:
    // it may ask the resolver.
    if b.model_taint().is_some() {
        return Ok(None);
    }
    let lock = Lock::acquire(&b.state_dir)?;
    let (p, person, grant) = {
        let _s = self::lock(b)?;
        settle_locked(b)?;
        // The proposal as the ledger has it, and its session as the broker holds it now: tainted since
        // the proposal, it is not trusted; its grant revoked or over, nor is it.
        let p = b.ledger.get(p.body.seq)?;
        let Some(person) = trusted_by(b, &p) else { return Ok(None) };
        if cited(&p).map(|s| read(b, s)).transpose()?.flatten().is_none_or(|r| r.tainted) {
            return Ok(None);
        }
        let Some(g) = active(&b.ledger.read_all()?).into_iter().find(|g| g.person == person && g.covers == Covers::Proposals) else { return Ok(None) };
        (p, person, g.seq)
    };
    let e = b.apply_locked(&lock, p.body.seq, &person, false, None, Some(("trust", grant)))?;
    if let Some(id) = cited(&p) {
        cite(b, id, p.body.seq, Some(e.body.seq))?;
    }
    Ok(Some(e))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A grant lasts its minutes from when it was made, whatever its `until` says: a ledger from
    /// elsewhere is someone else's text (the audit of 0.3.0).
    #[test]
    fn a_grant_lasts_its_minutes_whatever_its_until_says() {
        let g = |at: &str, minutes: u64, until: &str| Grant { seq: 1, person: "casey".into(), minutes, until: until.into(), at: at.into(), covers: Covers::Proposals, tainted_too: false, session: false };
        let now = chrono::DateTime::parse_from_rfc3339("2026-11-08T12:00:00Z").unwrap().with_timezone(&chrono::Utc);
        assert!(g("2026-11-08T10:00:00Z", 60, "2099-01-01T00:00:00Z").over(now), "60 minutes from 10:00 is over at 12:00");
        assert!(!g("2026-11-08T11:30:00Z", 60, "2099-01-01T00:00:00Z").over(now));
        assert!(g("2026-11-08T11:30:00Z", 6000, "2099-01-01T00:00:00Z").over(now + chrono::Duration::minutes(61)), "never more than the most a grant may be");
        assert!(g("2026-11-08T11:30:00Z", 60, "2026-11-08T11:45:00Z").over(now), "nor past its until");
        assert!(g("not a time", 60, "2099-01-01T00:00:00Z").over(now), "unreadable is over");
    }

    fn secret_grant(covers: Covers, tainted_too: bool, session: bool) -> Grant {
        Grant { seq: 1, person: "casey".into(), minutes: 480, until: "2026-11-08T20:00:00Z".into(), at: "2026-11-08T12:00:00Z".into(), covers, tainted_too, session }
    }

    fn at(t: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(t).unwrap().with_timezone(&chrono::Utc)
    }

    /// A grant covers the one thing it says: one secret is not another, a person's own of a name is not the system's
    /// of that name, all is the person's own and never a system secret, and trust in proposals lets no secret be used.
    #[test]
    fn a_grant_covers_what_it_says_and_nothing_else() {
        let now = at("2026-11-08T12:30:00Z");
        let own = secret_grant(Covers::Secret { name: "gh".into(), system: false }, false, false);
        assert!(own.covers_use(false, "gh", false, None, now));
        assert!(!own.covers_use(false, "other", false, None, now), "another secret");
        assert!(!own.covers_use(true, "gh", false, None, now), "the system's of the same name");
        let all = secret_grant(Covers::AllSecrets, false, false);
        assert!(all.covers_use(false, "gh", false, None, now) && all.covers_use(false, "other", false, None, now));
        assert!(!all.covers_use(true, "gh", false, None, now), "all is their own, never the system's");
        let system = secret_grant(Covers::Secret { name: "gh".into(), system: true }, false, false);
        assert!(system.covers_use(true, "gh", false, None, now) && !system.covers_use(false, "gh", false, None, now));
        assert!(!secret_grant(Covers::Proposals, false, false).covers_use(false, "gh", false, None, now), "trust in proposals is of proposals");
        assert!(!own.covers_use(false, "gh", false, None, at("2026-11-08T20:00:00Z")), "over at its time");
    }

    /// Under taint a grant holds only if it says so, and only of one named secret: `all` never does, whatever its
    /// entry says.
    #[test]
    fn taint_is_waived_only_by_a_named_secrets_grant_that_says_so() {
        let now = at("2026-11-08T12:30:00Z");
        let named = |too| secret_grant(Covers::Secret { name: "gh".into(), system: false }, too, false);
        assert!(named(true).covers_use(false, "gh", true, None, now));
        assert!(!named(false).covers_use(false, "gh", true, None, now), "it does not say so");
        assert!(!secret_grant(Covers::AllSecrets, false, false).covers_use(false, "gh", true, None, now));
        assert!(!secret_grant(Covers::AllSecrets, true, false).covers_use(false, "gh", true, None, now), "all is never tainted-too");
    }

    /// "This session" is a session that was running when the person said so: not one started after, not a use that
    /// names none, and not past the longest a grant lasts from the session's start.
    #[test]
    fn a_session_grant_is_of_the_sessions_running_then() {
        let g = secret_grant(Covers::Secret { name: "gh".into(), system: false }, false, true);
        let now = at("2026-11-08T12:30:00Z");
        assert!(g.covers_use(false, "gh", false, Some("2026-11-08T11:00:00Z"), now));
        assert!(!g.covers_use(false, "gh", false, Some("2026-11-08T12:10:00Z"), now), "started after the grant");
        assert!(!g.covers_use(false, "gh", false, None, now), "no session");
        assert!(!g.covers_use(false, "gh", false, Some("not a time"), now));
        assert!(!g.covers_use(false, "gh", false, Some("2026-11-08T11:00:00Z"), at("2026-11-08T19:00:00Z")), "eight hours from the session's start");
    }

    /// What an entry says it covers. One from before grants covered anything else says nothing, and is trust in
    /// proposals; one that says what no torq knows is no grant; tainted-too on `all` is not read as one.
    #[test]
    fn what_an_entry_covers() {
        assert_eq!(Covers::read(&json!({ "person": "casey", "minutes": 30 })), Some(Covers::Proposals));
        assert_eq!(Covers::read(&json!({ "covers": "proposals" })), Some(Covers::Proposals));
        assert_eq!(Covers::read(&json!({ "covers": "all-secrets" })), Some(Covers::AllSecrets));
        assert_eq!(Covers::read(&json!({ "covers": "secret gh", "owner": "user" })), Some(Covers::Secret { name: "gh".into(), system: false }));
        assert_eq!(Covers::read(&json!({ "covers": "secret gh", "owner": "system" })), Some(Covers::Secret { name: "gh".into(), system: true }));
        for unknown in [json!({ "covers": "everything" }), json!({ "covers": "secret ../x" }), json!({ "covers": 7 }), json!({ "covers": "secret " })] {
            assert_eq!(Covers::read(&unknown), None, "{unknown}");
        }
        for c in [Covers::Proposals, Covers::AllSecrets, Covers::Secret { name: "gh".into(), system: false }] {
            assert_eq!(Covers::read(&json!({ "covers": c.word(), "owner": "user" })), Some(c));
        }
    }
}
