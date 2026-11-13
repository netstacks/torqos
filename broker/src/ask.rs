//! A use of a secret that waits for a person (the approval design, part 2; ADR-051, ADR-052 4).
//!
//! The broker is one process for each request, and none of them waits: a use that must ask is given a number and
//! its caller told so at once; the answer to it is another request, and the caller asking again about its number
//! (the secrets daemon, about once a second) is a third. They meet in a file.
//! The waiting use is kept here, in the broker's state directory (root's, 0700; each file 0600),
//! under a number: `secret-asks/<n>.json`. The person the agent serves, and no one else, answers it by that number
//! (`answer_ask`: the socket's gate lets only their uid ask, and this checks the name again). No connection to the
//! broker and no lock is held while a use waits, for at most two minutes.
//!
//! Every change of a waiting use is made under the secrets' short lock: it is opened, answered, ended at its time,
//! and taken away there. An unanswered file is a use whose outcome is not yet on the record; whoever ends it writes
//! the one entry and removes or marks the file in the same hold of the lock. So each asked use has exactly one of
//! `secret_use_approved`, `secret_use_denied` and `secret_use_timed_out`, whichever of an answer and the time came
//! first, and whether or not anyone still asks about it: one whose caller went away, or that a reboot left
//! behind, is ended as timed out by the next use, answer or listing.
//!
//! What is kept of a use is in two parts that never mix: what the broker knows for itself (`Facts`), and the
//! agent's own words (`agent_says`), one cleaned line. An answer acts on the number, so on the facts.

use crate::ledger::{Entry, NewEntry};
use crate::secrets::Owner;
use crate::{Broker, Lock};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// How long a use waits for its person.
pub const WAIT_SECONDS: u64 = torq_wire::secrets::ASK_WAIT_SECONDS;
/// The most uses of one agent's that wait at once: one more is refused at once.
pub const WAITING_EACH: usize = 3;
/// The most uses of one session a person is asked about: after them, a use that would ask is refused. Uses that
/// name no session are counted together, by the hour.
pub const ASKS_A_SESSION: u32 = 10;
/// The most of the agent's own words that are kept and shown: one line of this many characters.
pub const REASON_MAX: usize = 200;
/// The most uses of one agent's a person is asked about in an hour, whatever sessions they name: an agent starts a
/// new session when it likes, so the count by session alone bounds nothing. Three sessions' worth: room for
/// honest work, and an end to an agent that asks until someone says yes.
pub const ASKS_AN_HOUR: u32 = 30;
/// How long an answered use is kept past its time for the use to come back for its answer.
const COLLECT_SECONDS: i64 = 30;

/// What the broker knows of a waiting use for itself: nothing here is the agent's to word.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Facts {
    /// The person the agent serves: the only one who answers.
    pub person: String,
    /// The agent account the use is judged as, and the account that called (the agent's or the resident's).
    pub agent: String,
    pub caller: String,
    pub owner: Owner,
    pub secret: String,
    pub host: String,
    /// A signature for SSH to the host, not an HTTPS request to it.
    pub ssh: bool,
    pub session: Option<String>,
    /// Whether the session read untrusted text (a use that names none: yes).
    pub tainted: bool,
    /// Why a person is asked, in the broker's words.
    pub why: String,
}

/// A person's answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Answer {
    pub approved: bool,
    pub by: String,
    /// Where it was given, as the program that sent it says: `dialog` or `terminal`.
    pub place: String,
}

/// A use that waits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ask {
    pub n: u64,
    pub asked: String,
    /// When it is over, answered or not.
    pub until: String,
    pub facts: Facts,
    /// The agent's own words for why, cleaned to one line: shown as the agent's, never as a fact.
    pub agent_says: String,
    #[serde(default)]
    pub answer: Option<Answer>,
}

/// How a use that waited ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// As the person was shown it: whether its session had read untrusted text then.
    Approved { tainted: bool },
    Denied(String),
    TimedOut,
}

/// The agent's own words as they are kept and shown: its control and bidirectional characters gone, one line,
/// at most `REASON_MAX` characters.
pub fn agents_text(s: &str) -> String {
    torq_wire::clean::clean(s).chars().map(|c| if c == '\n' || c == '\t' { ' ' } else { c }).take(REASON_MAX).collect()
}

fn dir(state_dir: &Path) -> PathBuf {
    state_dir.join("secret-asks")
}

fn time(t: chrono::DateTime<chrono::Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn parsed(t: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(t).ok().map(|t| t.with_timezone(&chrono::Utc))
}

impl Ask {
    /// Whether it still waits at `now`: asked before, over after, and never for longer than a use waits — a file
    /// whose times say otherwise (a clock set back, a file from elsewhere) is not one a person answers.
    fn live(&self, now: chrono::DateTime<chrono::Utc>) -> bool {
        match (parsed(&self.asked), parsed(&self.until)) {
            (Some(a), Some(u)) => a <= now && now < u && u - a <= chrono::Duration::seconds(WAIT_SECONDS as i64),
            _ => false,
        }
    }
}

impl Ask {
    /// Whether, answered, it is still kept at `now` for its use to come back for the answer: its times a use's own,
    /// and its time over by no more than `COLLECT_SECONDS`.
    fn kept_for_its_answer(&self, now: chrono::DateTime<chrono::Utc>) -> bool {
        match (parsed(&self.asked), parsed(&self.until)) {
            (Some(a), Some(u)) => a <= now && u - a <= chrono::Duration::seconds(WAIT_SECONDS as i64) && now < u + chrono::Duration::seconds(COLLECT_SECONDS),
            _ => false,
        }
    }
}

/// The waiting use `n`, if its file is there and is one (never through a link).
fn read(state_dir: &Path, n: u64) -> Option<Ask> {
    use std::os::unix::fs::OpenOptionsExt;
    let f = std::fs::OpenOptions::new().read(true).custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32).open(dir(state_dir).join(format!("{n}.json"))).ok()?;
    let mut s = String::new();
    f.take(64 * 1024).read_to_string(&mut s).ok()?;
    serde_json::from_str::<Ask>(&s).ok().filter(|a| a.n == n)
}

/// A file of the store written whole: a new one beside it, then renamed over it.
fn put(state_dir: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let d = dir(state_dir);
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&d)?;
    let part = d.join(format!(".{name}.new"));
    // A part-written file of a broker that died, if there is one: nothing reads it, and that there is none is the
    // usual case, so a failure here says nothing; the create below refuses whatever could not be removed.
    let _ = std::fs::remove_file(&part);
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&part)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    std::fs::rename(&part, d.join(name))?;
    std::fs::File::open(&d)?.sync_all()?;
    Ok(())
}

/// Every waiting use kept, lowest number first.
fn all(state_dir: &Path) -> Vec<Ask> {
    let mut out: Vec<Ask> = std::fs::read_dir(dir(state_dir))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.strip_suffix(".json")?.parse::<u64>().ok())
        .filter_map(|n| read(state_dir, n))
        .collect();
    out.sort_by_key(|a| a.n);
    out
}

/// The last number given to a waiting use (`next`): none yet, 0.
fn last_given(state_dir: &Path) -> u64 {
    std::fs::read_to_string(dir(state_dir).join("next")).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0)
}

/// The person who answers waiting use `n`, for the socket's gate: read with no lock, which a rename makes safe (a
/// file is whole, or not there). None: no such use.
pub fn person_asked(state_dir: &Path, n: u64) -> Option<String> {
    read(state_dir, n).map(|a| a.facts.person)
}

/// How many of an agent's uses a person was asked about in an hour of the clock (`asked.json`): all of them, and
/// those that named no session.
#[derive(Default, Serialize, Deserialize)]
struct Asked {
    hour: String,
    asks: u32,
    no_session: u32,
}

impl Broker {
    /// How long a use waits (tests: a moment).
    pub fn with_ask_wait(mut self, wait: std::time::Duration) -> Self {
        self.ask_wait = wait;
        self
    }

    /// The secrets' short lock: every use is judged under it, and every change of a waiting use is made under it.
    pub(crate) fn asks_lock(&self) -> Result<Lock> {
        Lock::acquire_named(&self.state_dir, "secrets.lock", 10)
    }

    /// The one entry of an asked use: how it ended, who answered and where. Never the value, and not the agent's words.
    fn record_ask(&self, a: &Ask, kind: &str, answer: Option<&Answer>, rest: bool) -> Result<Entry> {
        let f = &a.facts;
        let whose = f.owner.whose(&f.secret);
        let mut data = f.owner.data(&f.secret);
        data["ask"] = json!(a.n);
        data["session"] = json!(f.session);
        data["host"] = json!(f.host);
        data["agent"] = json!(f.agent);
        data["asked_by"] = json!(f.caller);
        let reason = match answer {
            Some(w) => {
                data["by"] = json!(w.by);
                data["where"] = json!(w.place);
                if rest {
                    data["for"] = json!("session");
                }
                let rest = if rest { ", and every other in its session" } else { "" };
                format!("{} {} the agent's use of {whose} for {}{rest}", w.by, if w.approved { "approved" } else { "denied" }, f.host)
            }
            None => format!("No one answered the agent's use of {whose} for {}: refused", f.host),
        };
        self.ledger.append(NewEntry {
            actor: if answer.is_some() { "user".into() } else { "broker".into() },
            approved_by: answer.map(|w| w.by.clone()),
            kind: kind.into(),
            tier: f.owner.tier().to_string(),
            tainted: f.tainted,
            reason,
            data,
        })
    }

    /// Every use whose time is over, ended: one never answered is on the record as timed out, and its file goes;
    /// one answered goes once the use it was for has had `COLLECT_SECONDS` more to come back for the answer (an
    /// answer given in the last moment is still one). Under the secrets' lock (held).
    fn end_stale(&self, _held: &Lock) -> Result<()> {
        let now = chrono::Utc::now();
        for a in all(&self.state_dir).into_iter().filter(|a| !a.live(now)) {
            if a.answer.is_some() && a.kept_for_its_answer(now) {
                continue;
            }
            // Its number is not given again, even should the count of them be lost: the count is kept past it.
            if a.n > last_given(&self.state_dir) {
                put(&self.state_dir, "next", a.n.to_string().as_bytes())?;
            }
            // On the record, then gone. Should the file not go (the store's directory damaged), this fails, and so
            // does every later use, answer and listing until it is mended: each try would record the use as timed
            // out again. Left so: taking the file away first would lose the entry instead, and a store that cannot
            // be written is the machine's fault to mend, not a state to run on.
            if a.answer.is_none() {
                self.record_ask(&a, "secret_use_timed_out", None, false)?;
            }
            std::fs::remove_file(dir(&self.state_dir).join(format!("{}.json", a.n)))?;
        }
        Ok(())
    }

    /// A use begins to wait, under the secrets' lock (held): its number. Refused, in the design's words, when
    /// `WAITING_EACH` of this agent's wait already, when a person was asked `ASKS_AN_HOUR` times about this agent
    /// this hour, or `ASKS_A_SESSION` times about its session.
    pub(crate) fn open_ask(&self, held: &Lock, facts: Facts, agent_says: &str) -> Result<u64> {
        self.end_stale(held)?;
        let kept = all(&self.state_dir);
        if kept.iter().filter(|a| a.facts.agent == facts.agent && a.answer.is_none()).count() >= WAITING_EACH {
            bail!("{WAITING_EACH} uses are waiting for you already");
        }
        // By the agent and the hour of the clock (so twice the most can fall either side of an hour's turn).
        let file = dir(&self.state_dir).join("asked.json");
        let mut every: std::collections::BTreeMap<String, Asked> = std::fs::read(&file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let hour = chrono::Utc::now().format("%Y-%m-%dT%H").to_string();
        let count = every.entry(facts.agent.clone()).or_default();
        if count.hour != hour {
            *count = Asked { hour, asks: 0, no_session: 0 };
        }
        count.asks = count.asks.saturating_add(1);
        if facts.session.is_none() {
            count.no_session = count.no_session.saturating_add(1);
        }
        let (asks, no_session) = (count.asks, count.no_session);
        put(&self.state_dir, "asked.json", &serde_json::to_vec(&every)?)?;
        if asks > ASKS_AN_HOUR {
            bail!("a person was asked {ASKS_AN_HOUR} times about this agent's uses this hour: no more are asked until the next, and a use that would ask is refused");
        }
        let (in_session, of) = match &facts.session {
            Some(id) => (crate::session::count_ask(self, id)?, "this session"),
            None => (no_session, "uses that name no session, this hour"),
        };
        if in_session > ASKS_A_SESSION {
            bail!("a person was asked {ASKS_A_SESSION} times about {of}: no more are asked, and a use that would ask is refused");
        }
        // A number is never given twice, whatever was left behind: past the last given (`end_stale` keeps that past
        // every use it takes away), and past every use still here.
        let n = last_given(&self.state_dir).max(kept.iter().map(|a| a.n).max().unwrap_or(0)).checked_add(1).context("no number is left for a waiting use")?;
        put(&self.state_dir, "next", n.to_string().as_bytes())?;
        let now = chrono::Utc::now();
        let wait = chrono::Duration::from_std(self.ask_wait.min(std::time::Duration::from_secs(WAIT_SECONDS))).context("the wait")?;
        let ask = Ask { n, asked: time(now), until: time(now + wait), facts, agent_says: agents_text(agent_says), answer: None };
        put(&self.state_dir, &format!("{n}.json"), &serde_json::to_vec(&ask)?)?;
        Ok(n)
    }

    /// How waiting use `n` stands, for the use that asks again about it (`same`: whether the facts kept of `n` are
    /// that use's own: a number is no key to another use's answer). Under the secrets' lock (held). None: it
    /// still waits. Answered: as answered, and its file gone, so an answer is spent once. Gone, or its time over
    /// unanswered: timed out, and on the record as that, here or by whoever ended it first.
    pub(crate) fn look_at_ask(&self, held: &Lock, n: u64, same: impl Fn(&Facts) -> bool) -> Result<Option<Outcome>> {
        self.end_stale(held)?;
        let Some(a) = read(&self.state_dir, n) else { return Ok(Some(Outcome::TimedOut)) };
        if !same(&a.facts) {
            bail!("refused: the use that waits as {n} is not this use");
        }
        let outcome = match &a.answer {
            None => return Ok(None),
            Some(w) if w.approved => Outcome::Approved { tainted: a.facts.tainted },
            Some(w) => Outcome::Denied(w.by.clone()),
        };
        std::fs::remove_file(dir(&self.state_dir).join(format!("{n}.json")))?;
        Ok(Some(outcome))
    }

    /// `by` answers waiting use `n`: approved, or denied (`rest`: and its secret not asked about again in its
    /// session). Only the person the agent serves; the first answer stands; a use whose time is over has none.
    /// (Over the socket, `by` is the kernel's word for who asked, and the gate refuses anyone else first.)
    pub fn answer_ask(&self, n: u64, by: &str, approved: bool, rest: bool, place: &str) -> Result<Entry> {
        if !matches!(place, "dialog" | "terminal") {
            bail!("{place:?} is not where an answer is given (dialog, terminal)");
        }
        if approved && rest {
            bail!("refused: only a denial is for the rest of a session; a grant lets later uses through (secret-grant)");
        }
        let held = self.asks_lock()?;
        self.end_stale(&held)?;
        let Some(mut a) = read(&self.state_dir, n) else { bail!("no use waits as {n}: it was answered, or its time is over") };
        if a.facts.person != by {
            bail!("refused: use {n} is asked of {}, whose agent it is: no one else answers it", a.facts.person);
        }
        if a.answer.is_some() {
            bail!("use {n} is answered already: the first answer stands");
        }
        if rest && a.facts.session.is_none() {
            bail!("refused: use {n} names no session, so there is no rest of one to deny");
        }
        let answer = Answer { approved, by: by.into(), place: place.into() };
        // On the record first, as every answer the broker acts on is: stopped between, the ledger says a little
        // too much (an approval no use came of), never too little.
        let e = self.record_ask(&a, if approved { "secret_use_approved" } else { "secret_use_denied" }, Some(&answer), rest)?;
        let file = dir(&self.state_dir).join(format!("{n}.json"));
        let given = (|| -> Result<()> {
            if let (true, Some(id)) = (rest, &a.facts.session) {
                crate::session::deny_rest(self, id, &a.facts.owner.key(&a.facts.secret))?;
            }
            a.answer = Some(answer);
            put(&self.state_dir, &format!("{n}.json"), &serde_json::to_vec(&a)?)
        })();
        if let Err(why) = given {
            // The answer is on the record and could not be carried out whole. The use is taken away, so that it
            // ends refused: left waiting, it would get a second entry (timed out, or approved by another answer).
            std::fs::remove_file(&file).context("taking away a use whose answer could not be given")?;
            return Err(why.context("the answer was recorded and could not be given: the use is refused"));
        }
        Ok(e)
    }

    /// The uses that wait for `person`'s answer now, lowest number first.
    pub fn waiting_asks(&self, person: &str) -> Result<Vec<Ask>> {
        let held = self.asks_lock()?;
        self.end_stale(&held)?;
        Ok(all(&self.state_dir).into_iter().filter(|a| a.facts.person == person && a.answer.is_none()).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The agent's words are kept as one cleaned line of at most 200 characters: no line of its own under the
    /// broker's facts, no escape, no bidirectional override, nothing hidden.
    #[test]
    fn the_agents_words_are_one_cleaned_line() {
        assert_eq!(agents_text("deploy the site\nsecret: other\thost: evil.example.com"), "deploy the site secret: other host: evil.example.com");
        assert_eq!(agents_text("\x1b[2Jplain \u{202e}desrever\u{202c}\r\u{200b} text\0"), "plain desrever text");
        assert_eq!(agents_text(&"x".repeat(500)).chars().count(), REASON_MAX);
        assert_eq!(agents_text(&"é".repeat(500)).chars().count(), REASON_MAX, "characters, not bytes");
        assert_eq!(agents_text(""), "");
    }

    /// A waiting use is one only within its own two minutes: a file whose times say more, or that cannot be read,
    /// is not answered.
    #[test]
    fn a_use_waits_only_within_its_time() {
        let facts = Facts { person: "alice".into(), agent: "torq-agent-alice".into(), caller: "torq-agent-alice".into(), owner: Owner::System, secret: "gh".into(), host: "api.github.com".into(), ssh: false, session: None, tainted: true, why: String::new() };
        let ask = |asked: &str, until: &str| Ask { n: 1, asked: asked.into(), until: until.into(), facts: facts.clone(), agent_says: String::new(), answer: None };
        let now = parsed("2026-11-13T12:01:00Z").unwrap();
        assert!(ask("2026-11-13T12:00:00Z", "2026-11-13T12:02:00Z").live(now));
        assert!(!ask("2026-11-13T12:00:00Z", "2026-11-13T12:01:00Z").live(now), "over at its time");
        assert!(!ask("2026-11-13T12:00:30Z", "2026-11-13T12:09:00Z").live(now), "no use waits longer than two minutes");
        assert!(!ask("2026-11-13T12:01:30Z", "2026-11-13T12:02:00Z").live(now), "asked in the future: a clock set back");
        assert!(!ask("then", "2026-11-13T12:02:00Z").live(now));
    }
}
