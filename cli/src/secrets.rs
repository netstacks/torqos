//! torqcli's side of the secrets (ADR-052): the rules, changed as commits of their files, and the list.
//! The values never pass through here except to the broker (`request system secret set`, typed at the
//! terminal) and back to the person who asked (`show`): the Machine does that part (shell.rs).
//! TorqOS Settings uses the same functions.

use crate::candidate::Repo;
use crate::session::Machine;
use anyhow::{bail, Context, Result};
use torq_wire::secrets::{self as w, Approve, Https, Kind, Rule, Rules, Ssh};

/// What a rule lets happen, given or taken back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Grant {
    /// An agent may use it. A person's own secret: their own agent (None, `torq-agent` in their rules). A system
    /// secret: the named person's agent, by its account (`torq-agent-<person>`): the approval design, part 1.
    Agent(Option<String>),
    /// Sent in an HTTPS header to this host.
    Https(String),
    /// Signs for SSH to this host.
    Ssh(String),
}

/// `request system secret <act> <name> … [user <person>]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Act {
    Set,
    /// The value from a file (an SSH key: several lines, not typed).
    SetFile(String),
    Delete,
    Show,
    Allow(Grant),
    Disallow(Grant),
    Approve(Approve),
}

impl Act {
    pub fn verb(&self) -> &'static str {
        match self {
            Act::Set | Act::SetFile(_) => "set",
            Act::Delete => "delete",
            Act::Show => "show",
            Act::Allow(_) => "allow",
            Act::Disallow(_) => "disallow",
            Act::Approve(_) => "approve",
        }
    }

    /// The words after the secret's name.
    pub fn rest(&self) -> String {
        let grant = |g: &Grant| match g {
            Grant::Agent(None) => " agent".to_string(),
            Grant::Agent(Some(p)) => format!(" agent {}", crate::syntax::quote(p)),
            Grant::Https(h) => format!(" https {h}"),
            Grant::Ssh(h) => format!(" ssh {h}"),
        };
        match self {
            Act::Allow(g) | Act::Disallow(g) => grant(g),
            Act::Approve(a) => format!(" {}", a.word()),
            Act::SetFile(p) => format!(" file {}", crate::syntax::quote(p)),
            _ => String::new(),
        }
    }
}

/// What a person lets their own agent do with secrets beyond their rules, and their answers to its uses that wait
/// (the approval design, parts 2 and 3). The broker's to keep and to judge, on its ledger: not a change of the rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Approval {
    /// `allow <name> session`, `allow <name> for <minutes> [tainted-too]` (`user <you>`: your own secret, else the
    /// system's), `allow all for <minutes>` (no name: all your own). No minutes: your agent's sessions running now.
    Grant { name: Option<String>, user: Option<String>, minutes: Option<u32>, tainted_too: bool },
    /// `allow revoke [<name>|all]`: end those grants of yours (no name: every one).
    Revoke { name: Option<String> },
    /// `approve <n>`, `deny <n> [session]`: your answer to a waiting use (`rest`: a no for the rest of its session).
    Answer { n: u64, approved: bool, rest: bool },
}

/// Where a person answers a use that waits: a terminal's command, or the panel's dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    Terminal,
    Dialog,
}

impl Place {
    /// As the broker takes it (`--where`), and as the use's entry then says it.
    fn word(self) -> &'static str {
        match self {
            Place::Terminal => "terminal",
            Place::Dialog => "dialog",
        }
    }
}

impl Approval {
    /// The command, after `request system secret `.
    pub fn line(&self) -> String {
        match self {
            Approval::Grant { name, user, minutes, tainted_too } => {
                let how_long = minutes.map_or(" session".to_string(), |m| format!(" for {m}"));
                let owner = user.as_ref().map(|u| format!(" user {}", crate::syntax::quote(u))).unwrap_or_default();
                format!("allow {}{how_long}{}{owner}", name.as_deref().unwrap_or("all"), if *tainted_too { " tainted-too" } else { "" })
            }
            Approval::Revoke { name: None } => "allow revoke".into(),
            Approval::Revoke { name: Some(n) } => format!("allow revoke {n}"),
            Approval::Answer { n, approved: true, .. } => format!("approve {n}"),
            Approval::Answer { n, approved: false, rest } => format!("deny {n}{}", if *rest { " session" } else { "" }),
        }
    }

    /// What the broker is asked (who asks is the kernel's word, added at its socket). `place`: where the person
    /// gives an answer to a waiting use, which its entry records.
    pub fn args(&self, place: Place) -> Vec<String> {
        match self {
            Approval::Grant { name, user, minutes, tainted_too } => {
                let mut a = vec!["secret-grant".to_string()];
                match name {
                    Some(n) => a.extend(["--name".into(), n.clone()]),
                    None => a.push("--all".into()),
                }
                if let Some(u) = user {
                    a.extend(["--user".into(), u.clone()]);
                }
                match minutes {
                    Some(m) => a.extend(["--minutes".into(), m.to_string()]),
                    None => a.push("--session".into()),
                }
                if *tainted_too {
                    a.push("--tainted-too".into());
                }
                a
            }
            Approval::Revoke { name } => {
                let mut a = vec!["secret-revoke".to_string()];
                if let Some(n) = name {
                    a.extend(["--name".into(), n.clone()]);
                }
                a
            }
            Approval::Answer { n, approved, rest } => {
                let mut a = vec![if *approved { "secret-approve" } else { "secret-deny" }.to_string(), n.to_string(), "--where".into(), place.word().into()];
                if *rest && !*approved {
                    a.push("--rest-of-session".into());
                }
                a
            }
        }
    }

    /// What the broker answered (its entry, or the grants it ended), said.
    pub fn said(&self, v: &serde_json::Value) -> String {
        let entry = |e: &serde_json::Value| format!("{} (ledger #{})\n", crate::history::clean(e["reason"].as_str().unwrap_or("done")), e["seq"].as_u64().unwrap_or(0));
        match self {
            Approval::Grant { .. } => format!("{}Until {}. `request system secret allow revoke` ends it.\n", entry(v), crate::history::clean(v["data"]["until"].as_str().unwrap_or("?"))),
            Approval::Answer { .. } => entry(v),
            Approval::Revoke { .. } => {
                let ended: String = match v["revoked"].as_array().map(Vec::as_slice) {
                    None | Some([]) => "You had no such grant to end.\n".into(),
                    Some(ended) => ended.iter().map(entry).collect(),
                };
                // Said, so that no one thinks the secret asks again when it does not.
                let still = match v["all_until"].as_str() {
                    Some(until) => format!("Your grant of all your own secrets still covers a secret of your own by that name, until {}: `request system secret allow revoke all` ends it too.\n", crate::history::clean(until)),
                    None => String::new(),
                };
                ended + &still
            }
        }
    }
}

/// `show system secrets waiting`: the uses of your agent's that wait for your answer (the broker's `secret-waiting`).
/// What the broker knows first, each on its own line; then, marked as the agent's, what the agent says: never
/// among the facts, and shown as text.
pub fn waiting_shown(v: &serde_json::Value) -> String {
    let clean = |s: &serde_json::Value| crate::history::clean(s.as_str().unwrap_or("?"));
    let asks = v["waiting"].as_array().map(Vec::as_slice).unwrap_or_default();
    if asks.is_empty() {
        return "(no use of a secret is waiting for you)\n".into();
    }
    let mut out = String::new();
    for a in asks {
        let (f, n) = (&a["facts"], a["n"].as_u64().unwrap_or(0));
        let whose = match f["owner"]["person"].as_str() {
            Some(p) => format!("{}'s secret {}", crate::history::clean(p), clean(&f["secret"])),
            None => format!("the system's secret {}", clean(&f["secret"])),
        };
        let what = if f["ssh"] == true { "to sign in over SSH to" } else { "in an HTTPS request to" };
        out.push_str(&format!("{n}  {whose}, {what} {}\n", clean(&f["host"])));
        let caller = clean(&f["caller"]);
        let kind = if caller == clean(&f["agent"]) { "a program run as your agent, such as a coding tool" } else { "your agent itself" };
        out.push_str(&format!("   asked by {caller} ({kind}); {}\n", if f["tainted"] == false { "its session read nothing untrusted" } else { "ITS SESSION READ UNTRUSTED TEXT, or it names none" }));
        out.push_str(&format!("   why you are asked: {}\n", clean(&f["why"])));
        if a["agent_says"].as_str().is_some_and(|s| !s.is_empty()) {
            // Last on its line, to the line's end, and one line by its cleaning: nothing in it can end it, where
            // a quote of its own could have seemed to close one put round it.
            out.push_str(&format!("   the agent's own words, to the end of this line (not checked by anything): {}\n", clean(&a["agent_says"])));
        }
        out.push_str(&format!("   until {}: request system secret approve {n}, or deny {n}\n", clean(&a["until"])));
    }
    out
}

/// `show system secrets allowed`: each grant of secret uses that holds, what it covers and its end (`secret-grants`).
pub fn allowed_shown(v: &serde_json::Value) -> String {
    let clean = |s: &serde_json::Value| crate::history::clean(s.as_str().unwrap_or("?"));
    let grants = v["grants"].as_array().map(Vec::as_slice).unwrap_or_default();
    if grants.is_empty() {
        return "(no agent is let use a secret without asking)\n".into();
    }
    let mut out = String::new();
    for g in grants {
        let covers = match &g["covers"]["secret"] {
            serde_json::Value::Null => "all their own secrets".to_string(),
            s if s["system"] == true => format!("the system's secret {}", clean(&s["name"])),
            s => format!("their secret {}", clean(&s["name"])),
        };
        let scope = if g["session"] == true { ", in the agent's sessions running when it was given" } else { "" };
        let taint = if g["tainted_too"] == true { " — EVEN FROM A SESSION THAT READ UNTRUSTED TEXT" } else { "" };
        out.push_str(&format!("#{}  {}'s agent: {covers}, until {}{scope}{taint}\n", g["seq"].as_u64().unwrap_or(0), clean(&g["person"]), clean(&g["until"])));
    }
    out
}

/// Where `user`'s rules are in the repository (None: the system's).
pub fn rules_path(user: Option<&str>) -> String {
    match user {
        None => w::SYSTEM_RULES.to_string(),
        Some(u) => format!("users/{u}/{}", w::PERSON_RULES),
    }
}

fn whose(name: &str, user: Option<&str>) -> String {
    match user {
        None => format!("the system's secret {name}"),
        Some(u) => format!("{u}'s secret {name}"),
    }
}

/// `user`'s rules at `rev`: none yet is none at all; a file that is not a rules file is said, not taken.
pub fn read(repo: &Repo, rev: &str, user: Option<&str>) -> Result<Rules> {
    let path = rules_path(user);
    match repo.file_at(rev, &path)? {
        None => Ok(Rules::new()),
        Some(bytes) => if user.is_some() { w::parse_person(&bytes) } else { w::parse(&bytes) }.map_err(|why| anyhow::anyhow!("{path}: {why}")),
    }
}

/// The actor a grant to an agent writes: `torq-agent` in a person's own rules (their own agent), the named
/// person's agent account in the system's.
fn agent_word(of: &Option<String>) -> String {
    match of {
        None => w::AGENT.to_string(),
        Some(p) => format!("{}-{p}", w::AGENT),
    }
}

/// The same, as said to the person.
fn agent_said(of: &Option<String>) -> String {
    match of {
        None => "the agent".to_string(),
        Some(p) => format!("{p}'s agent ({})", agent_word(of)),
    }
}

/// A grant to an agent fits whose secret it is: a person's own, their own agent alone; a system secret, the
/// agent of a person it names (the approval design, part 1).
fn agent_fits(name: &str, user: Option<&str>, act: &Act) -> Result<()> {
    match (act, user) {
        (Act::Allow(Grant::Agent(None)) | Act::Disallow(Grant::Agent(None)), None) => {
            bail!("a system secret names whose agent may use it: request system secret {} {name} agent <person>", act.verb())
        }
        (Act::Allow(Grant::Agent(Some(p))) | Act::Disallow(Grant::Agent(Some(p))), Some(u)) => {
            bail!("{u}'s own secret {name} is used by {u}'s own agent alone, not {p}'s: request system secret {} {name} agent user {u}", act.verb())
        }
        _ => Ok(()),
    }
}

/// Whether `person` has an agent account on this machine, by /etc/passwd's text (`torq agent people` makes one each):
/// a system secret is given only to an agent that exists (the broker checks it
/// again against its own map).
fn agent_here(passwd: &str, person: &str) -> bool {
    let account = format!("{}-{person}", w::AGENT);
    passwd.lines().any(|l| l.split(':').next() == Some(account.as_str()))
}

/// `act` done to `rules`: what it did, in a few words, for the change's reason. Refused, it changes nothing.
pub fn edit(rules: &mut Rules, name: &str, act: &Act) -> Result<String> {
    edit_with(rules, name, act, &[])
}

/// `edit`, with the keys `known` of the host an `allow … ssh <host>` names (known_hosts lines, from the person's
/// own known_hosts): what OpenSSH's agent checks that host against before it signs.
pub fn edit_with(rules: &mut Rules, name: &str, act: &Act, known: &[String]) -> Result<String> {
    if !w::name_ok(name) {
        bail!("'{name}' is not a secret's name (letters, digits, - and _)");
    }
    let before = rules.clone();
    let what = edit_unchecked(rules, name, act, known);
    let done = what.and_then(|what| w::check(rules).map(|()| what).map_err(|why| anyhow::anyhow!("{why}")));
    if done.is_err() {
        *rules = before;
    }
    done
}

fn edit_unchecked(rules: &mut Rules, name: &str, act: &Act, known: &[String]) -> Result<String> {
    let first = || anyhow::anyhow!("secret {name} is not used for anything yet: say what it is for first (request system secret allow {name} https <host>, or ssh <host>)");
    match act {
        Act::Allow(Grant::Https(h)) | Act::Allow(Grant::Ssh(h)) if !w::host_ok(h) => {
            bail!("'{h}' is not a host name (lowercase, with a dot; no address, no wildcard)")
        }
        Act::Allow(Grant::Https(h)) => {
            let r = rules.entry(name.into()).or_insert_with(|| Rule::new(Kind::Token));
            if r.kind != Kind::Token {
                bail!("secret {name} is an SSH key: it is never sent, so it has no https use");
            }
            let https = r.https.get_or_insert_with(|| Https { hosts: Vec::new(), header: "Authorization".into(), prefix: "Bearer ".into() });
            if https.hosts.contains(h) {
                bail!("secret {name} is already sent to {h}");
            }
            https.hosts.push(h.clone());
            https.hosts.sort();
            Ok(format!("may be sent to {h}"))
        }
        Act::Allow(Grant::Ssh(h)) => {
            let r = rules.entry(name.into()).or_insert_with(|| Rule::new(Kind::SshKey));
            if r.kind != Kind::SshKey {
                bail!("secret {name} is a token: it has no ssh use");
            }
            let ssh = r.ssh.get_or_insert_with(|| Ssh { hosts: Vec::new(), known: Vec::new() });
            if ssh.hosts.contains(h) {
                bail!("secret {name} already signs for {h}");
            }
            ssh.hosts.push(h.clone());
            // Its keys as the person knows them, for that host alone.
            ssh.known.extend(known.iter().filter(|l| l.split(' ').next() == Some(h.as_str()) && !ssh.known.contains(l)).cloned().collect::<Vec<_>>());
            ssh.hosts.sort();
            Ok(format!("may sign for {h}"))
        }
        Act::Allow(Grant::Agent(of)) => {
            let r = rules.get_mut(name).ok_or_else(first)?;
            let a = agent_word(of);
            if r.actors.contains(&a) {
                bail!("{} may already use secret {name}", agent_said(of));
            }
            r.actors.push(a);
            r.actors.sort();
            Ok(format!("{} may use it", agent_said(of)))
        }
        Act::Disallow(g) => {
            let r = rules.get_mut(name).with_context(|| format!("secret {name} has no rule"))?;
            let (list, what) = match g {
                Grant::Agent(of) => (&mut r.actors, agent_word(of)),
                Grant::Https(h) => (r.https.as_mut().map(|x| &mut x.hosts).with_context(|| format!("secret {name} is sent nowhere"))?, h.clone()),
                Grant::Ssh(h) => (r.ssh.as_mut().map(|x| &mut x.hosts).with_context(|| format!("secret {name} signs for nothing"))?, h.clone()),
            };
            if !list.contains(&what) {
                bail!("{what} is not allowed it now");
            }
            list.retain(|x| x != &what);
            // A host taken back takes its keys with it.
            if let (Grant::Ssh(h), Some(s)) = (g, r.ssh.as_mut()) {
                s.known.retain(|l| l.split(' ').next() != Some(h.as_str()));
            }
            // A use with no host left is no use.
            if r.https.as_ref().is_some_and(|x| x.hosts.is_empty()) {
                r.https = None;
            }
            if r.ssh.as_ref().is_some_and(|x| x.hosts.is_empty()) {
                r.ssh = None;
            }
            Ok(match g {
                Grant::Agent(of) => format!("{} may no longer use it", agent_said(of)),
                Grant::Https(h) => format!("no longer sent to {h}"),
                Grant::Ssh(h) => format!("no longer signs for {h}"),
            })
        }
        Act::Approve(a) => {
            let r = rules.get_mut(name).ok_or_else(first)?;
            r.approve = *a;
            Ok(match a {
                Approve::EachUse => "a person approves each use by the agent".into(),
                Approve::FirstUse => "a person approves the agent's first use in each session".into(),
                Approve::Never => "the agent uses it without asking (a tainted session still asks)".into(),
                Approve::NeverEvenTainted => "the agent uses it without ever asking, EVEN FROM A SESSION THAT READ UNTRUSTED TEXT (it asks your password)".into(),
            })
        }
        Act::Delete => {
            rules.remove(name).context("no rule")?;
            Ok("its rule removed".into())
        }
        Act::Set | Act::SetFile(_) | Act::Show => bail!("not a change of the rules"),
    }
}

/// `act` on `user`'s rules, committed: no question for a person's own, an administrator's approval for the
/// system's (the broker asks). The change's number.
pub fn change(machine: &dyn Machine, repo: &Repo, name: &str, user: Option<&str>, act: &Act) -> Result<u64> {
    let head = repo.head()?;
    let path = rules_path(user);
    let old = repo.file_at(&head, &path)?;
    let mut rules = read(repo, &head, user)?;
    // An SSH host's keys, as the person asking knows them (their own known_hosts): never taken on first sight.
    let known = match act {
        Act::Allow(Grant::Ssh(h)) => machine.known_host_keys(h)?,
        _ => Vec::new(),
    };
    agent_fits(name, user, act)?;
    let what = edit_with(&mut rules, name, act, &known)?;
    if let Act::Allow(Grant::Agent(Some(p))) = act {
        let passwd = std::fs::read_to_string("/etc/passwd").context("reading /etc/passwd")?;
        if !agent_here(&passwd, p) {
            bail!("{p} has no agent on this machine ({}-{p}): `torq agent people` names who has one", w::AGENT);
        }
    }
    let new = (!rules.is_empty()).then(|| w::render(&rules));
    let scratch = crate::scratch_dir()?;
    let diff = crate::candidate::unified_diff_bytes(scratch.path(), &[(path, old.as_deref(), new.as_deref().map(str::as_bytes))])?;
    let tier = if user.is_some() { "user" } else { "system" };
    let mut reason = format!("Secret {name}: {what}");
    if user.is_none() {
        reason = format!("The system's secret {name}: {what}");
    }
    machine.commit(&diff, &head, tier, &reason, None)
}

/// `request system secret <act> …` for the rules: done, and said.
pub fn changed(machine: &dyn Machine, repo: &Repo, name: &str, user: Option<&str>, act: &Act) -> Result<String> {
    let seq = change(machine, repo, name, user, act)?;
    Ok(format!("Changed what {} may be used for (change #{seq}).\n", whose(name, user)))
}

/// The largest value the broker keeps (broker/src/secrets.rs): a file larger is not read.
pub const VALUE_MAX: usize = 16 * 1024;

/// Why `value` is not one to send to the broker, if it is not. What was given is never repeated in the refusal:
/// a refusal is shown, and may be logged.
pub fn value_ok(value: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("not changed: no value");
    }
    if value.len() > VALUE_MAX {
        bail!("not changed: a value is at most {} KiB", VALUE_MAX / 1024);
    }
    Ok(())
}

/// `value_ok`, for a value typed twice where it cannot be seen (TorqOS Settings' masked fields).
pub fn typed_ok(value: &str, again: &str) -> Result<()> {
    if value != again {
        bail!("not changed: the two differ");
    }
    value_ok(value)
}

/// A secret that has a value, as the broker lists them (None: the system's).
pub type Kept = (Option<String>, String);

/// Every person with a rules file at `rev`.
pub fn people_with_rules(repo: &Repo, rev: &str) -> Result<Vec<String>> {
    let mut people: Vec<String> = repo
        .paths(rev)?
        .into_iter()
        .filter_map(|p| p.strip_prefix("users/").and_then(|r| r.strip_suffix(&format!("/{}", w::PERSON_RULES))).map(String::from))
        .filter(|u| !u.contains('/'))
        .collect();
    people.sort();
    Ok(people)
}

/// A secret as `show system secrets` lists it: whose, whether it has a value, and its rule. Never the value:
/// nothing that makes one of these has read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    /// Whose it is (None: the system's).
    pub owner: Option<String>,
    pub name: String,
    pub has_value: bool,
    /// What it may be used for, and by whom (None: nothing yet).
    pub rule: Option<Rule>,
}

impl Listed {
    /// What it is and what it may be used for, in the list's words.
    pub fn said(&self) -> String {
        let value = if self.has_value { "has a value" } else { "no value yet" };
        let Some(r) = &self.rule else { return format!("{value}; used for nothing") };
        let kind = match r.kind {
            Kind::Token => "token",
            Kind::SshKey => "SSH key",
        };
        let mut uses = Vec::new();
        if let Some(h) = &r.https {
            uses.push(format!("https to {}", h.hosts.join(", ")));
        }
        if let Some(s) = &r.ssh {
            uses.push(format!("ssh to {}", s.hosts.join(", ")));
        }
        // Which agents: a person's own secret, their own (`torq-agent`); a system secret, each by its account.
        let agents: Vec<&str> = r.actors.iter().map(String::as_str).filter(|a| *a == w::AGENT || a.starts_with(&format!("{}-", w::AGENT))).collect();
        // Marked wherever the secret is listed: this one asks no one, whatever its session read.
        let standing = if r.approve == Approve::NeverEvenTainted { " [!! NEVER ASKS, EVEN TAINTED]" } else { "" };
        let who = match agents.as_slice() {
            [] => "not the agent".to_string(),
            [a] if *a == w::AGENT => format!("the agent too, approved {}{standing}", r.approve.word()),
            some => format!("{} too, approved {}{standing}", some.join(", "), r.approve.word()),
        };
        let uses = if uses.is_empty() { "used for nothing".to_string() } else { uses.join("; ") };
        format!("{kind}, {value}; {uses}; {who}")
    }
}

/// Every secret, the system's first, then each person's by name. A rules file that cannot be read is said (Err)
/// where its secrets would come, and those of its secrets that have a value are still listed.
pub fn listed(repo: &Repo, kept: &[Kept]) -> Result<Vec<Result<Listed, String>>> {
    let head = repo.head()?;
    let mut owners: Vec<Option<String>> = vec![None];
    let mut people = people_with_rules(repo, &head)?;
    people.extend(kept.iter().filter_map(|(u, _)| u.clone()));
    people.sort();
    people.dedup();
    owners.extend(people.into_iter().map(Some));
    let mut out = Vec::new();
    for owner in owners {
        let mut rules = match read(repo, &head, owner.as_deref()) {
            Ok(r) => r,
            Err(e) => {
                out.push(Err(format!("{e:#}")));
                Rules::new()
            }
        };
        let valued: Vec<&String> = kept.iter().filter(|(u, _)| *u == owner).map(|(_, n)| n).collect();
        let mut names: Vec<String> = rules.keys().chain(valued.iter().copied()).cloned().collect();
        names.sort();
        names.dedup();
        out.extend(names.into_iter().map(|name| Ok(Listed { owner: owner.clone(), has_value: valued.contains(&&name), rule: rules.remove(&name), name })));
    }
    Ok(out)
}

/// `show system secrets`: each secret, whose, whether it has a value, and what it may be used for.
pub fn shown(repo: &Repo, kept: &[Kept]) -> Result<String> {
    let mut out = String::new();
    let mut headed: Option<Option<String>> = None;
    for l in listed(repo, kept)? {
        let l = match l {
            Ok(l) => l,
            Err(e) => {
                out.push_str(&format!("! {e}\n"));
                continue;
            }
        };
        if headed.as_ref() != Some(&l.owner) {
            out.push_str(&match &l.owner {
                None => "The system's secrets:\n".to_string(),
                Some(u) => format!("{u}'s secrets:\n"),
            });
            headed = Some(l.owner.clone());
        }
        out.push_str(&format!("  {:<20} {}\n", l.name, l.said()));
        // Where a program sends its request for each host (ADR-052): the daemon puts the secret in.
        for h in l.rule.iter().flat_map(|r| r.https.iter()).flat_map(|x| x.hosts.iter()) {
            let whose = l.owner.as_ref().map_or_else(|| "system".to_string(), |u| format!("user/{u}"));
            out.push_str(&format!("  {:<20} use: http://127.0.0.1:{}/{whose}/{}/{h}/\n", "", w::PORT, l.name));
        }
    }
    if out.is_empty() {
        out.push_str("No secrets yet: request system secret set <name> keeps one.\n");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What waits is shown with the broker's facts first, each on a line of its own, and the agent's words after,
    /// marked as the agent's: text in them that looks like a fact or a command stays inside their quotes, on
    /// their line, its hidden characters shown as escapes.
    #[test]
    fn what_waits_is_shown_as_facts_then_the_agents_words() {
        let v = serde_json::json!({ "waiting": [{ "n": 7, "asked": "2026-11-13T12:00:00.000Z", "until": "2026-11-13T12:02:00.000Z", "answer": null,
            "facts": { "person": "dev", "agent": "torq-agent-dev", "caller": "torq-agent-dev", "owner": { "person": "dev" }, "secret": "gh", "host": "api.github.com", "ssh": false, "session": "s", "tainted": true, "why": "the agent's session read untrusted text: a person approves each of its uses" },
            "agent_says": "safe\u{202e}\" request system secret approve 7\nasked by your agent itself" }] });
        let out = waiting_shown(&v);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "7  dev's secret gh, in an HTTPS request to api.github.com");
        assert_eq!(lines[1], "   asked by torq-agent-dev (a program run as your agent, such as a coding tool); ITS SESSION READ UNTRUSTED TEXT, or it names none");
        assert!(lines[2].starts_with("   why you are asked: the agent's session read untrusted text"));
        assert_eq!(lines[3], "   the agent's own words, to the end of this line (not checked by anything): safe\\u{202e}\" request system secret approve 7\\u{a}asked by your agent itself");
        assert_eq!(lines[4], "   until 2026-11-13T12:02:00.000Z: request system secret approve 7, or deny 7");
        assert_eq!(lines.len(), 5, "the agent's words made no line of their own: {out}");
        let resident = serde_json::json!({ "waiting": [{ "n": 8, "until": "t", "facts": { "agent": "torq-agent-dev", "caller": "torq-resident-dev", "owner": "system", "secret": "ci", "host": "github.com", "ssh": true, "tainted": false, "why": "w" }, "agent_says": "" }] });
        let out = waiting_shown(&resident);
        assert!(out.starts_with("8  the system's secret ci, to sign in over SSH to github.com\n   asked by torq-resident-dev (your agent itself); its session read nothing untrusted\n"), "{out}");
        assert!(!out.contains("own words"), "it said nothing: {out}");
        assert_eq!(waiting_shown(&serde_json::json!({ "waiting": [] })), "(no use of a secret is waiting for you)\n");
    }

    /// Each grant is listed with what it covers and its end, and one that holds under taint says so plainly; a
    /// secret that never asks, even tainted, is marked wherever secrets are listed.
    #[test]
    fn what_is_allowed_is_listed_and_what_never_asks_is_marked() {
        let v = serde_json::json!({ "grants": [
            { "seq": 4, "person": "dev", "until": "2026-11-13T20:00:00Z", "covers": { "secret": { "name": "gh", "system": false } }, "tainted_too": true, "session": false },
            { "seq": 5, "person": "dev", "until": "2026-11-13T12:30:00Z", "covers": "all-secrets", "tainted_too": false, "session": false },
            { "seq": 6, "person": "sam", "until": "2026-11-13T20:00:00Z", "covers": { "secret": { "name": "ci", "system": true } }, "tainted_too": false, "session": true }] });
        assert_eq!(allowed_shown(&v), "#4  dev's agent: their secret gh, until 2026-11-13T20:00:00Z — EVEN FROM A SESSION THAT READ UNTRUSTED TEXT\n#5  dev's agent: all their own secrets, until 2026-11-13T12:30:00Z\n#6  sam's agent: the system's secret ci, until 2026-11-13T20:00:00Z, in the agent's sessions running when it was given\n");
        assert_eq!(allowed_shown(&serde_json::json!({ "grants": [] })), "(no agent is let use a secret without asking)\n");
        let mut rule = Rule::new(Kind::Token);
        rule.actors = vec![w::AGENT.to_string()];
        rule.approve = Approve::NeverEvenTainted;
        let listed = |rule: &Rule| Listed { owner: Some("dev".into()), name: "gh".into(), has_value: true, rule: Some(rule.clone()) }.said();
        assert!(listed(&rule).ends_with("the agent too, approved never-even-tainted [!! NEVER ASKS, EVEN TAINTED]"), "{}", listed(&rule));
        rule.approve = Approve::Never;
        assert!(!listed(&rule).contains("!!"), "{}", listed(&rule));
        // What the broker is asked, word for word: all is never tainted-too or someone's from here either.
        assert_eq!(Approval::Grant { name: None, user: None, minutes: Some(30), tainted_too: false }.args(Place::Terminal), ["secret-grant", "--all", "--minutes", "30"]);
        assert_eq!(Approval::Grant { name: Some("gh".into()), user: Some("dev".into()), minutes: Some(5), tainted_too: true }.args(Place::Terminal), ["secret-grant", "--name", "gh", "--user", "dev", "--minutes", "5", "--tainted-too"]);
        assert_eq!(Approval::Grant { name: Some("ci".into()), user: None, minutes: None, tainted_too: false }.args(Place::Terminal), ["secret-grant", "--name", "ci", "--session"]);
        assert_eq!(Approval::Revoke { name: None }.args(Place::Terminal), ["secret-revoke"]);
        assert_eq!(Approval::Answer { n: 7, approved: false, rest: false }.args(Place::Terminal), ["secret-deny", "7", "--where", "terminal"]);
        assert_eq!(Approval::Answer { n: 7, approved: false, rest: true }.args(Place::Terminal), ["secret-deny", "7", "--where", "terminal", "--rest-of-session"]);
        // An answer says where it was given; nothing else of a request does.
        assert_eq!(Approval::Answer { n: 7, approved: true, rest: false }.args(Place::Dialog), ["secret-approve", "7", "--where", "dialog"]);
        assert_eq!(Approval::Revoke { name: Some("gh".into()) }.args(Place::Dialog), ["secret-revoke", "--name", "gh"]);
        // Ending one secret's grants says when a grant of all still covers it.
        let ended = serde_json::json!({ "revoked": [{ "seq": 9, "reason": "dev revoked their leave" }], "all_until": "2026-11-13T20:00:00Z" });
        assert!(Approval::Revoke { name: Some("gh".into()) }.said(&ended).ends_with("still covers a secret of your own by that name, until 2026-11-13T20:00:00Z: `request system secret allow revoke all` ends it too.\n"));
        assert_eq!(Approval::Revoke { name: Some("gh".into()) }.said(&serde_json::json!({ "revoked": [{ "seq": 9, "reason": "r" }] })), "r (ledger #9)\n");
    }

    #[test]
    fn a_secret_is_said_what_it_is_for_then_who() {
        let mut r = Rules::new();
        assert!(edit(&mut r, "gh", &Act::Allow(Grant::Agent(None))).is_err(), "what it is for first");
        assert!(edit(&mut r, "gh", &Act::Approve(Approve::Never)).is_err());
        assert_eq!(edit(&mut r, "gh", &Act::Allow(Grant::Https("api.github.com".into()))).unwrap(), "may be sent to api.github.com");
        edit(&mut r, "gh", &Act::Allow(Grant::Agent(None))).unwrap();
        edit(&mut r, "gh", &Act::Approve(Approve::EachUse)).unwrap();
        let g = &r["gh"];
        assert_eq!((g.kind, g.actors.clone(), g.approve), (Kind::Token, vec![w::AGENT.to_string()], Approve::EachUse));
        assert!(edit(&mut r, "gh", &Act::Allow(Grant::Agent(None))).is_err(), "already");
        assert!(edit(&mut r, "gh", &Act::Allow(Grant::Ssh("github.com".into()))).is_err(), "a token signs for nothing");
        assert!(edit(&mut r, "gh", &Act::Allow(Grant::Https("api.github.com".into()))).is_err(), "already");
    }

    /// Whose agent (the approval design, part 1): a person's own secret, their own agent (`torq-agent`); a system
    /// secret, a named person's agent by its account. Either way round is refused before anything changes.
    #[test]
    fn a_grant_to_an_agent_says_whose() {
        let mut r = Rules::new();
        edit(&mut r, "ci", &Act::Allow(Grant::Https("api.github.com".into()))).unwrap();
        assert_eq!(edit(&mut r, "ci", &Act::Allow(Grant::Agent(Some("dev".into())))).unwrap(), "dev's agent (torq-agent-dev) may use it");
        assert_eq!(r["ci"].actors, ["torq-agent-dev"]);
        assert!(edit(&mut r, "ci", &Act::Allow(Grant::Agent(Some("dev".into())))).is_err(), "already");
        assert_eq!(Act::Allow(Grant::Agent(Some("dev".into()))).rest(), " agent dev");
        let mine = Act::Allow(Grant::Agent(None));
        let theirs = Act::Allow(Grant::Agent(Some("sam".into())));
        assert!(agent_fits("ci", None, &theirs).is_ok() && agent_fits("gh", Some("dev"), &mine).is_ok());
        let e = format!("{:#}", agent_fits("ci", None, &mine).unwrap_err());
        assert!(e.contains("agent <person>"), "{e}");
        let e = format!("{:#}", agent_fits("gh", Some("dev"), &theirs).unwrap_err());
        assert!(e.contains("dev's own agent alone"), "{e}");
    }

    #[test]
    fn what_is_refused_changes_nothing() {
        let mut r = Rules::new();
        edit_with(&mut r, "k", &Act::Allow(Grant::Ssh("github.com".into())), &["github.com ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl".into()]).unwrap();
        let before = r.clone();
        for (name, act) in [
            ("k", Act::Allow(Grant::Https("api.github.com".into()))),
            ("k", Act::Allow(Grant::Ssh("10.0.0.1".into()))),
            ("k", Act::Allow(Grant::Ssh("*.github.com".into()))),
            ("k", Act::Disallow(Grant::Ssh("gitlab.com".into()))),
            ("k", Act::Disallow(Grant::Agent(None))),
            ("../k", Act::Allow(Grant::Ssh("github.com".into()))),
            ("none", Act::Delete),
        ] {
            assert!(edit(&mut r, name, &act).is_err(), "{name} {act:?}");
            assert_eq!(r, before, "{name} {act:?}");
        }
    }

    #[test]
    fn taken_back_a_use_with_no_host_is_no_use() {
        let mut r = Rules::new();
        edit(&mut r, "gh", &Act::Allow(Grant::Https("api.github.com".into()))).unwrap();
        edit(&mut r, "gh", &Act::Allow(Grant::Https("uploads.github.com".into()))).unwrap();
        edit(&mut r, "gh", &Act::Allow(Grant::Agent(None))).unwrap();
        edit(&mut r, "gh", &Act::Disallow(Grant::Https("api.github.com".into()))).unwrap();
        assert_eq!(r["gh"].https.as_ref().unwrap().hosts, ["uploads.github.com"]);
        edit(&mut r, "gh", &Act::Disallow(Grant::Https("uploads.github.com".into()))).unwrap();
        assert_eq!(r["gh"].https, None);
        edit(&mut r, "gh", &Act::Disallow(Grant::Agent(None))).unwrap();
        assert!(r["gh"].actors.is_empty());
        edit(&mut r, "gh", &Act::Delete).unwrap();
        assert!(r.is_empty());
    }

    /// What is refused before anything is sent, and that no refusal repeats what was typed.
    #[test]
    fn a_value_typed_twice_is_refused_in_words_that_do_not_repeat_it() {
        let long = "k".repeat(VALUE_MAX + 1);
        for (value, again, why) in [
            ("", "", "not changed: no value"),
            (" \t", " \t", "not changed: no value"),
            ("hunter2-token", "hunter2-tokem", "not changed: the two differ"),
            ("hunter2-token", "", "not changed: the two differ"),
            (long.as_str(), long.as_str(), "not changed: a value is at most 16 KiB"),
        ] {
            assert_eq!(typed_ok(value, again).unwrap_err().to_string(), why);
        }
        typed_ok("hunter2-token", "hunter2-token").unwrap();
        typed_ok(&long[1..], &long[1..]).unwrap();
    }

    #[test]
    fn the_words_read_back() {
        assert_eq!(Act::Allow(Grant::Https("a.com".into())).rest(), " https a.com");
        assert_eq!(Act::Disallow(Grant::Agent(None)).rest(), " agent");
        assert_eq!(Act::Approve(Approve::FirstUse).rest(), " first-use");
        assert_eq!(rules_path(None), "secrets/system.json");
        assert_eq!(rules_path(Some("casey")), "users/casey/secrets.json");
    }

    /// A system secret is given only to an agent that exists.
    #[test]
    fn only_an_agent_that_exists_is_given_one() {
        let passwd = "root:x:0:0::/root:/bin/sh\nalice:x:1000:100::/home/alice:/bin/sh\ntorq-agent-alice:x:990:990::/var/lib/torq-agent-alice:/sbin/nologin\n";
        assert!(agent_here(passwd, "alice"));
        assert!(!agent_here(passwd, "alicee") && !agent_here(passwd, "bob") && !agent_here(passwd, "agent-alice"));
    }
}
