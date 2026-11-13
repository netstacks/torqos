//! The values of a machine's secrets (ADR-052), kept by the broker as it keeps passwords and backup keys:
//! outside the repository, root's alone, each encrypted with `systemd-creds` (the TPM and the host's key where
//! there is a TPM). A system secret's are an administrator's; a person's own are theirs (`--user`), and the
//! socket's gate says who may ask what (socket.rs). Each set, deletion and showing is on the record first,
//! never the value: stopped between, the ledger says a little too much, never too little. What each secret
//! may be used for is not here: it is the configuration's (`torq_wire::secrets`), and the daemon's to apply.

use crate::ledger::{Entry, NewEntry};
use crate::policy::Tier;
use crate::{Broker, Lock};
use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::json;
use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::Stdio;

/// The largest value kept: an SSH key with room to spare, a token many times over.
pub const VALUE_MAX: usize = 16 * 1024;

/// Where the values are, and what encrypts them.
pub struct Store {
    pub(crate) dir: PathBuf,
    /// `systemd-creds` (tests: a stand-in that takes the same arguments).
    pub(crate) creds: String,
}

impl Default for Store {
    fn default() -> Self {
        Store { dir: "/var/lib/torq/secrets/store".into(), creds: "systemd-creds".into() }
    }
}

/// Whose a secret is.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Owner {
    System,
    Person(String),
}

impl Owner {
    /// The secret `name` of this owner, as the broker's own records name it: `system/<name>`, `user/<person>/<name>`.
    pub(crate) fn key(&self, name: &str) -> String {
        match self {
            Owner::System => format!("system/{name}"),
            Owner::Person(p) => format!("user/{p}/{name}"),
        }
    }

    /// `--user <name>`, or none: the system's.
    pub fn of(user: Option<&str>) -> Result<Owner> {
        match user {
            None => Ok(Owner::System),
            Some(u) if crate::policy::user_name_ok(u) => Ok(Owner::Person(u.into())),
            Some(u) => bail!("{u:?} is not a person's name"),
        }
    }

    /// A person's own value is set and read back by them alone, however the broker is reached (ADR-052): the
    /// socket's gate says so first, and this is where sudo and every other way in pass too.
    fn theirs_alone(&self, by: &str) -> Result<()> {
        match self {
            Owner::Person(p) if p != by => bail!("refused: {p}'s secrets are theirs alone: no one else sets or reads them, an administrator included"),
            _ => Ok(()),
        }
    }

    pub(crate) fn tier(&self) -> Tier {
        match self {
            Owner::System => Tier::System,
            Owner::Person(_) => Tier::User,
        }
    }

    /// How the ledger and the messages say it.
    pub(crate) fn whose(&self, name: &str) -> String {
        match self {
            Owner::System => format!("the system's secret {name}"),
            Owner::Person(p) => format!("{p}'s secret {name}"),
        }
    }

    pub(crate) fn data(&self, name: &str) -> serde_json::Value {
        match self {
            Owner::System => json!({ "secret": name, "owner": "system" }),
            Owner::Person(p) => json!({ "secret": name, "owner": "user", "user": p }),
        }
    }
}

/// A secret that has a value: whose, and its name. Never the value.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Kept {
    pub owner: Owner,
    pub name: String,
}

/// Why `value` is not one to keep, if it is not.
pub fn value_ok(value: &str) -> Result<()> {
    if value.is_empty() {
        bail!("not kept: no value");
    }
    if value.len() > VALUE_MAX {
        bail!("not kept: a value is at most {} KiB", VALUE_MAX / 1024);
    }
    if value.contains('\0') {
        bail!("not kept: a value holds no NUL");
    }
    Ok(())
}

impl Broker {
    /// Where the values are, and what encrypts them (tests: a scratch folder and a stand-in).
    pub fn with_secrets(mut self, dir: impl Into<PathBuf>, creds: &str) -> Self {
        self.secrets = Store { dir: dir.into(), creds: creds.into() };
        self
    }

    fn secret_dir(&self, owner: &Owner) -> PathBuf {
        match owner {
            Owner::System => self.secrets.dir.join("system"),
            Owner::Person(p) => self.secrets.dir.join("users").join(p),
        }
    }

    fn secret_file(&self, owner: &Owner, name: &str) -> Result<PathBuf> {
        if !torq_wire::secrets::name_ok(name) {
            bail!("{name:?} is not a secret's name (letters, digits, - and _)");
        }
        Ok(self.secret_dir(owner).join(name))
    }

    /// What `systemd-creds` binds a value to: its name, so a file moved to another name does not decrypt.
    fn cred_name(owner: &Owner, name: &str) -> String {
        match owner {
            Owner::System => format!("torq-secret.system.{name}"),
            Owner::Person(p) => format!("torq-secret.user.{p}.{name}"),
        }
    }

    fn record(&self, owner: &Owner, name: &str, by: &str, kind: &str, reason: String, extra: Option<serde_json::Value>) -> Result<Entry> {
        let mut data = owner.data(name);
        if let Some(serde_json::Value::Object(x)) = extra {
            for (k, v) in x {
                data[k] = v;
            }
        }
        self.ledger.append(NewEntry {
            actor: "user".into(),
            approved_by: Some(by.into()),
            kind: kind.into(),
            tier: owner.tier().to_string(),
            tainted: false,
            reason,
            data,
        })
    }

    /// `request system secret set <name> [user <person>]`: `value` kept, encrypted, in place of any before.
    pub fn set_secret(&self, owner: &Owner, name: &str, value: &str, by: &str) -> Result<Entry> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        let _lock = Lock::acquire(&self.state_dir)?;
        self.ledger.verify().context("cannot keep a secret on a broken ledger")?;
        owner.theirs_alone(by)?;
        let file = self.secret_file(owner, name)?;
        value_ok(value)?;
        let entry = self.record(owner, name, by, "secret_set", format!("A value given for {}", owner.whose(name)), None)?;
        let dir = self.secret_dir(owner);
        let part = dir.join(format!(".{name}.new"));
        let kept = (|| -> Result<()> {
            fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
            let _ = fs::remove_file(&part);
            // Made private before anything is in it: systemd-creds writes into what is there.
            fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&part)?;
            let mut child = crate::config::command(&self.secrets.creds)
                .args(["encrypt", "--with-key=auto", &format!("--name={}", Self::cred_name(owner, name)), "-"])
                .arg(&part)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .context("running systemd-creds")?;
            // A systemd-creds that fails early (no TPM, no host key) may exit before it reads the value: its own
            // word, then, not the broken pipe that writing to it gave.
            let written = child.stdin.take().context("systemd-creds' input")?.write_all(value.as_bytes());
            let out = child.wait_with_output()?;
            if !out.status.success() {
                bail!("systemd-creds could not encrypt it: {}", said(String::from_utf8_lossy(&out.stderr).trim()));
            }
            written.context("giving systemd-creds the value")?;
            // systemd-creds may write its output anew, with modes of its own: private again before it is in place.
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&part, fs::Permissions::from_mode(0o600))?;
            fs::File::open(&part)?.sync_all()?;
            // An SSH key's public half beside it (and a key it cannot read refused) before the value is in place.
            self.keep_public(owner, name, value)?;
            fs::rename(&part, &file)?;
            fs::File::open(&dir)?.sync_all()?;
            Ok(())
        })();
        let Err(e) = kept else { return Ok(entry) };
        let _ = fs::remove_file(&part);
        let noted = self.ledger.append(NewEntry {
            actor: "broker".into(),
            approved_by: Some(by.into()),
            kind: "secret_set_failed".into(),
            tier: owner.tier().to_string(),
            tainted: false,
            reason: format!("The value for {} was not kept, after all", owner.whose(name)),
            data: json!({ "entry": entry.body.seq, "error": said(&format!("{e:#}")) }),
        });
        Err(match noted {
            Ok(_) => e.context("the value was not kept"),
            Err(l) => e.context(format!("the value was not kept, and the ledger could not say so: {l:#}")),
        })
    }

    /// `request system secret delete <name> [user <person>]`: its value gone. Its rule, if any, stays: the
    /// configuration's to change.
    pub fn delete_secret(&self, owner: &Owner, name: &str, by: &str) -> Result<Entry> {
        let _lock = Lock::acquire(&self.state_dir)?;
        self.ledger.verify().context("cannot delete a secret on a broken ledger")?;
        let file = self.secret_file(owner, name)?;
        if fs::symlink_metadata(&file).is_err() {
            bail!("{} has no value", owner.whose(name));
        }
        let entry = self.record(owner, name, by, "secret_deleted", format!("The value of {} deleted", owner.whose(name)), None)?;
        fs::remove_file(&file).context("deleting it")?;
        let _ = fs::remove_file(self.secret_dir(owner).join(format!("{name}.pub")));
        fs::File::open(self.secret_dir(owner))?.sync_all()?;
        Ok(entry)
    }

    /// `request system secret show <name> [user <person>]`: the value, to the one who asked. On the record first.
    pub fn show_secret(&self, owner: &Owner, name: &str, by: &str) -> Result<(Entry, String)> {
        let _lock = Lock::acquire(&self.state_dir)?;
        self.ledger.verify().context("cannot show a secret on a broken ledger")?;
        owner.theirs_alone(by)?;
        let file = self.secret_file(owner, name)?;
        if !fs::symlink_metadata(&file).is_ok_and(|m| m.is_file()) {
            bail!("{} has no value", owner.whose(name));
        }
        let entry = self.record(owner, name, by, "secret_shown", format!("The value of {} shown to {by}", owner.whose(name)), None)?;
        Ok((entry, self.decrypt(owner, name)?))
    }

    /// The value of `owner`'s secret `name`, decrypted by systemd-creds under the name it was bound to.
    fn decrypt(&self, owner: &Owner, name: &str) -> Result<String> {
        let file = self.secret_file(owner, name)?;
        if !fs::symlink_metadata(&file).is_ok_and(|m| m.is_file()) {
            bail!("{} has no value", owner.whose(name));
        }
        let mut child = crate::config::command(&self.secrets.creds)
            .args(["decrypt", &format!("--name={}", Self::cred_name(owner, name))])
            .arg(&file)
            .arg("-")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("running systemd-creds")?;
        let mut value = Vec::new();
        child.stdout.take().context("systemd-creds' output")?.take(VALUE_MAX as u64 + 1).read_to_end(&mut value)?;
        let out = child.wait_with_output()?;
        if !out.status.success() {
            bail!("systemd-creds could not decrypt it: {}", said(String::from_utf8_lossy(&out.stderr).trim()));
        }
        String::from_utf8(value).context("the value is not text")
    }

    /// Every secret that has a value, the system's first: names only.
    pub fn kept_secrets(&self) -> Vec<Kept> {
        let names = |dir: PathBuf| -> Vec<String> {
            let mut n: Vec<String> = fs::read_dir(dir)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| torq_wire::secrets::name_ok(n))
                .collect();
            n.sort();
            n
        };
        let mut out: Vec<Kept> = names(self.secret_dir(&Owner::System)).into_iter().map(|name| Kept { owner: Owner::System, name }).collect();
        let mut people: Vec<String> = fs::read_dir(self.secrets.dir.join("users"))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|p| crate::policy::user_name_ok(p))
            .collect();
        people.sort();
        for p in people {
            let owner = Owner::Person(p);
            out.extend(names(self.secret_dir(&owner)).into_iter().map(|name| Kept { owner: owner.clone(), name }));
        }
        out
    }
}

/// Text from systemd-creds, safe to show and to ledger.
fn said(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(300).collect()
}

/// Who holds the client end of the loopback connection from `client` to `server` (ports), by the kernel's own
/// table of them (`/proc/net/tcp` or `tcp6`'s text): the uid of the socket whose local end is 127.0.0.1:`client`
/// and whose remote end is 127.0.0.1:`server`, established. The daemon is not taken at its word for who asks:
/// the broker reads it here itself, so even a daemon that lied could use a secret only for someone connected to it.
pub fn uid_of_connection(table: &str, client: u16, server: u16) -> Option<u32> {
    // 127.0.0.1 as the table writes it: in IPv4's, and as IPv6's maps it.
    const LOOPBACK: [&str; 2] = ["0100007F", "0000000000000000FFFF00000100007F"];
    let at = |field: &str| -> Option<u16> {
        let (addr, port) = field.split_once(':')?;
        LOOPBACK.contains(&addr).then(|| u16::from_str_radix(port, 16).ok()).flatten()
    };
    table.lines().skip(1).find_map(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        // sl local remote st tx:rx tr:tm retrnsmt uid …; 01 is ESTABLISHED.
        (f.len() > 7 && at(f[1]) == Some(client) && at(f[2]) == Some(server) && f[3] == "01").then(|| f[7].parse().ok()).flatten()
    })
}

/// What a use of a secret is for: one HTTPS request to `host`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Use {
    pub owner: Owner,
    pub name: String,
    pub host: String,
    /// The account using it (the broker's reading of the connection).
    pub actor: String,
    /// The agent's session, if its request named one (`X-Torq-Session`).
    pub session: Option<String>,
    /// An HTTPS request to `host` (a token), or a signature for SSH to it (a key).
    pub ssh: bool,
}

/// Whose agent an account is, by the broker's map (ADR-051 3): the person it serves, and the agent account it is
/// judged as — its own name, or for a resident account its person's agent account (one agent per person).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentOf {
    pub person: String,
    pub account: String,
    /// Whether the account is its person's resident agent's (`agent.residents`). Here that rests on the uid alone:
    /// the daemon vouches for the uid at the other end of its loopback connection, and the broker has no pidfd or
    /// control group of the caller as on its own socket. The resident account is the service's alone (no login,
    /// no home, ADR-051 3 amended), so its uid is the service.
    pub resident: bool,
}

/// Whether `actor` (the person's agent when `agent` says so) may use `owner`'s secret under rule `r`
/// (the approval design, part 1): a person's own secret by them, or by their own agent where the rule says
/// `torq-agent`, never another's agent; a system secret by each person and each agent account the rule names.
fn may(owner: &Owner, r: &torq_wire::secrets::Rule, actor: &str, agent: Option<&AgentOf>) -> bool {
    use torq_wire::secrets::AGENT;
    match (owner, agent) {
        (Owner::Person(p), None) => p == actor,
        (Owner::Person(p), Some(a)) => a.person == *p && r.actors.iter().any(|x| x == AGENT),
        (Owner::System, None) => r.actors.iter().any(|x| x == actor),
        (Owner::System, Some(a)) => r.actors.iter().any(|x| *x == a.account),
    }
}

/// Why `u` may not happen by these rules (Err), or the rule that lets it. `agent`: whose agent `u.actor` is, if
/// one. What is refused here no person is asked about and no grant lets through: a secret with no rule, a host or
/// an operation its rule does not name, an account its rule does not let. Whether a person is asked first is
/// `asks`.
pub fn may_use<'a>(rules: &'a torq_wire::secrets::Rules, u: &Use, agent: Option<&AgentOf>) -> Result<&'a torq_wire::secrets::Rule> {
    use torq_wire::secrets::Kind;
    let whose = u.owner.whose(&u.name);
    let r = rules.get(&u.name).with_context(|| format!("{whose} has no rule: it is used for nothing"))?;
    match (u.ssh, r.kind) {
        (false, Kind::SshKey) => bail!("{whose} is an SSH key: it is never sent"),
        (true, Kind::Token) => bail!("{whose} is a token: it signs for nothing"),
        (false, _) if !r.https.as_ref().is_some_and(|h| h.hosts.contains(&u.host)) => bail!("{whose} is not sent to {}", u.host),
        (true, _) if !r.ssh.as_ref().is_some_and(|s| s.hosts.contains(&u.host)) => bail!("{whose} does not sign for {}", u.host),
        _ => {}
    }
    if !may(&u.owner, r, &u.actor, agent) {
        bail!("{} may not use {whose}", u.actor);
    }
    Ok(r)
}

/// Why a person is asked before the agent's use of `whose` under rule `r`, unless a grant covers it (the approval
/// design, part 2), or None: it goes without asking. `tainted`: whether the agent's session read untrusted text
/// (None: the use names no session, which is asked as a tainted one is). `first`: no use of the secret in this
/// session has gone through yet. Only the person's own standing word, `never-even-tainted`, lets a tainted use
/// through unasked.
pub fn asks(whose: &str, r: &torq_wire::secrets::Rule, tainted: Option<bool>, first: bool) -> Option<String> {
    use torq_wire::secrets::Approve;
    match (r.approve, tainted) {
        (Approve::NeverEvenTainted, _) => None,
        (_, None) => Some("the agent's request names no session (X-Torq-Session): a person approves such a use".into()),
        (_, Some(true)) => Some("the agent's session read untrusted text: a person approves each of its uses".into()),
        (Approve::EachUse, Some(false)) => Some(format!("{whose} is used by the agent with a person's approval ({})", r.approve.word())),
        (Approve::FirstUse, Some(false)) if first => Some(format!("{whose} is used by the agent with a person's approval ({})", r.approve.word())),
        (Approve::FirstUse | Approve::Never, Some(false)) => None,
    }
}

impl Broker {
    /// Whose agent `account` is, by the broker's map: an agent account it names (`agent.agents`), or a resident
    /// account whose person has one (`agent.residents`). None: a person, or an account that is no one's agent.
    pub fn agent_of(&self, account: &str) -> Option<AgentOf> {
        if let Some(p) = self.agent_limits.serves(account) {
            return Some(AgentOf { person: p.to_string(), account: account.to_string(), resident: false });
        }
        let p = self.residents().get(account)?;
        let (a, _) = self.agent_limits.agents.iter().find(|(_, q)| *q == p)?;
        Some(AgentOf { person: p.clone(), account: a.clone(), resident: true })
    }

    /// Session `id`, if it is agent `a`'s own and of its own kind: a session is its agent account's (ADR-051 3), and
    /// another agent's is none of this one's to cite; the resident agent's is cited by the resident account alone, and
    /// another program's of the agent account by the agent account alone. A
    /// session's id is in the ledger for every account to read: without this, a coding tool would cite the resident
    /// agent's untainted session and use a secret no person was asked about. As `session::judge` does for proposals.
    fn agents_session(&self, a: &AgentOf, id: &str) -> Result<crate::session::Record> {
        let r = crate::session::read(self, id)?.context("refused: the request names no session the broker holds")?;
        if r.agent != a.account {
            bail!("refused: session {id} is not {}'s", a.account);
        }
        if r.resident != a.resident {
            // Said as what it is: the resident account citing a session its service did not start (another program's
            // of the agent account, or its own from outside its service, as root's su makes it) is not "another
            // program's" in its own eyes.
            if r.resident {
                bail!("refused: session {id} is the resident agent's: only it cites it");
            }
            bail!("refused: session {id} was not started inside the resident agent's service: the resident account cites only its service's sessions");
        }
        Ok(r)
    }

    /// The rules `owner`'s secrets have now (the repository's HEAD): none is none; a file that is not one, refused.
    pub(crate) fn rules_of(&self, owner: &Owner) -> Result<torq_wire::secrets::Rules> {
        let (path, parse): (String, fn(&[u8]) -> std::result::Result<torq_wire::secrets::Rules, String>) = match owner {
            Owner::System => (torq_wire::secrets::SYSTEM_RULES.into(), torq_wire::secrets::parse),
            Owner::Person(p) => (format!("users/{p}/{}", torq_wire::secrets::PERSON_RULES), torq_wire::secrets::parse_person),
        };
        match self.repo.file_at_head(&path, torq_wire::secrets::MAX_FILE)? {
            None => Ok(Default::default()),
            Some(b) => parse(&b).map_err(|why| anyhow::anyhow!("{path}: {why}")),
        }
    }

    /// The session an agent's use names, as the broker holds it now; none for a use that names none.
    fn session_of(&self, u: &Use, of: Option<&AgentOf>) -> Result<Option<crate::session::Record>> {
        match (&u.session, of) {
            (Some(id), Some(a)) => Ok(Some(self.agents_session(a, id)?)),
            (Some(_), None) => bail!("refused: a session is the agent's; a person's use names none"),
            (None, _) => Ok(None),
        }
    }

    /// One use of a secret, for the daemon (ADR-052, step 2): judged by its rule, the agent's first use in each
    /// session on the record (and every refusal of the agent's, once per session, secret and reason), and its
    /// value. Every use the daemon logs itself. No broker lock: a rebuild does not hold a use up.
    ///
    /// An agent's use that a person is asked about (`asks`, and no grant of theirs covering it) is not waited for
    /// here: it is given a number (ask.rs) and the caller told `Waiting`, at once. The caller asks again about
    /// that number (`resume_use`) until the person has answered or its time is over. `agent_says`: the agent's own
    /// words for what the use is for, shown to the person as the agent's.
    pub fn begin_use(&self, u: &Use, agent_says: &str) -> Result<Used> {
        let lock = self.asks_lock()?;
        let of = self.agent_of(&u.actor);
        let agent = of.is_some();
        let session = self.session_of(u, of.as_ref())?;
        let tainted = session.as_ref().map(|s| s.tainted);
        let rules = self.rules_of(&u.owner)?;
        let whose = u.owner.whose(&u.name);
        let key = Self::use_key(u);
        let mut seen = Seen::read(&self.state_dir);
        // On the record: the agent's first refusal in a session for a secret that has a rule, and no more
        // than REFUSALS_AN_HOUR of them: names it made up, or a refusal it repeats, are the daemon's log's.
        let refused = |seen: &mut Seen, e: anyhow::Error| -> anyhow::Error {
            let why: String = said(&format!("{e:#}"));
            let recorded = (|| -> Result<()> {
                if agent && rules.contains_key(&u.name) && seen.first(&format!("refused|{key}")) && seen.refusal_allowed() {
                    self.note_use(u, tainted, "secret_use_refused", format!("The agent's use of {whose} refused"), json!({ "error": why }))?;
                }
                seen.write(&self.state_dir)
            })();
            recorded.err().unwrap_or(e)
        };
        let rule = match may_use(&rules, u, of.as_ref()) {
            Ok(r) => r.clone(),
            Err(e) => return Err(refused(&mut seen, e)),
        };
        // The grant the use goes under, if a person's lets it go unasked.
        let mut under = None;
        if let Some(why) = of.as_ref().and_then(|_| asks(&whose, &rule, tainted, !seen.has(&format!("first|{key}")))) {
            let a = of.as_ref().context("an agent's use")?;
            // Said for this session, it holds whatever is granted after: the person's no outlasts a later yes
            // to the secret at large, until the session is over.
            if session.as_ref().is_some_and(|s| s.denied.contains(&u.owner.key(&u.name))) {
                return Err(refused(&mut seen, anyhow::anyhow!("refused: {} said not to be asked again in this session about {whose}", a.person)));
            }
            under = crate::session::covering(self, &a.person, u.owner == Owner::System, &u.name, tainted.unwrap_or(true), session.as_ref().map(|s| s.started.as_str()))?;
            if under.is_none() {
                let facts = crate::ask::Facts {
                    person: a.person.clone(),
                    agent: a.account.clone(),
                    caller: u.actor.clone(),
                    owner: u.owner.clone(),
                    secret: u.name.clone(),
                    host: u.host.clone(),
                    ssh: u.ssh,
                    session: u.session.clone(),
                    tainted: tainted.unwrap_or(true),
                    why,
                };
                return match self.open_ask(&lock, facts, agent_says) {
                    Ok(n) => Ok(Used::Waiting(n)),
                    Err(e) => Err(refused(&mut seen, e.context("refused"))),
                };
            }
        }
        let used = self.granted(u, tainted, rule, seen, under, false).map(Used::Granted);
        drop(lock);
        used
    }

    /// `u` asks again about the number it waits under: still `Waiting`, or how it ended. Approved, it is judged
    /// again as things stand now (what a person approved is the use they were shown: a rule changed meanwhile,
    /// or a session that has since read untrusted text, is not it), and granted, once: the answer is spent.
    /// `u` must be the very use that waits as `n`, by the broker's own facts of it.
    pub fn resume_use(&self, u: &Use, n: u64) -> Result<Used> {
        let lock = self.asks_lock()?;
        let of = self.agent_of(&u.actor).context("refused: only an agent's use waits for a person")?;
        let same = |f: &crate::ask::Facts| f.agent == of.account && f.caller == u.actor && f.owner == u.owner && f.secret == u.name && f.host == u.host && f.ssh == u.ssh && f.session == u.session;
        let whose = u.owner.whose(&u.name);
        let tainted = self.session_of(u, Some(&of))?.map(|s| s.tainted);
        let shown = match self.look_at_ask(&lock, n, same)? {
            None => return Ok(Used::Waiting(n)),
            Some(crate::ask::Outcome::Denied(by)) => bail!("refused: {by} denied this use of {whose}"),
            Some(crate::ask::Outcome::TimedOut) => {
                // Why it asked, as it would be said now: the use that waited is gone with its own word for it.
                let why = self.rules_of(&u.owner).ok().and_then(|r| r.get(&u.name).and_then(|r| asks(&whose, r, tainted, true))).unwrap_or_else(|| "a person approves this use".into());
                bail!("refused: no one answered in time ({why})")
            }
            Some(crate::ask::Outcome::Approved { tainted }) => tainted,
        };
        if tainted.unwrap_or(true) != shown {
            bail!("refused: the agent's session read untrusted text while this use waited: it is asked again");
        }
        let rule = may_use(&self.rules_of(&u.owner)?, u, Some(&of))?.clone();
        let used = self.granted(u, tainted, rule, Seen::read(&self.state_dir), None, true).map(Used::Granted);
        drop(lock);
        used
    }

    /// How the broker's own records name a use's secret and session.
    fn use_key(u: &Use) -> String {
        format!("{}|{}", u.owner.key(&u.name), u.session.as_deref().unwrap_or("-"))
    }

    /// An entry of a use: the agent's first in a session, or its refusal.
    fn note_use(&self, u: &Use, tainted: Option<bool>, kind: &str, reason: String, extra: serde_json::Value) -> Result<Entry> {
        let agent = self.agent_of(&u.actor).is_some();
        let mut data = u.owner.data(&u.name);
        data["session"] = json!(u.session);
        data["host"] = json!(u.host);
        if let serde_json::Value::Object(x) = extra {
            for (k, v) in x {
                data[k] = v;
            }
        }
        self.ledger.append(NewEntry { actor: if agent { "agent".into() } else { "user".into() }, approved_by: None, kind: kind.into(), tier: u.owner.tier().to_string(), tainted: tainted.unwrap_or(agent), reason, data })
    }

    /// A use that goes through, its caller holding the secrets' lock: on the record where it is the agent's first in its
    /// session, and its value. `under`: the grant it goes under. `asked`: a person approved it, which is on the
    /// record as that already.
    fn granted(&self, u: &Use, tainted: Option<bool>, rule: torq_wire::secrets::Rule, mut seen: Seen, under: Option<u64>, asked: bool) -> Result<Granted> {
        if self.agent_of(&u.actor).is_some() {
            // The first use in a session is on the record once; under a grant, once for that grant, naming it.
            let key = Self::use_key(u);
            let first = match under {
                Some(g) => format!("first|{key}|grant {g}"),
                None => format!("first|{key}"),
            };
            if seen.first(&first) {
                if !asked {
                    let (how, extra) = match under {
                        Some(g) => (format!(" under grant #{g}"), json!({ "grant": g })),
                        None => (String::new(), json!({})),
                    };
                    self.note_use(u, tainted, "secret_first_use", format!("The agent used {} for the first time in its session{how}", u.owner.whose(&u.name)), extra)?;
                }
                seen.write(&self.state_dir)?;
            }
        }
        let (header, prefix) = rule.https.map(|h| (h.header, h.prefix)).unwrap_or_default();
        let known = rule.ssh.map(|s| s.known.into_iter().filter(|l| l.split(' ').next() == Some(u.host.as_str())).collect()).unwrap_or_default();
        Ok(Granted { value: self.decrypt(&u.owner, &u.name)?, header, prefix, known })
    }
}


/// The most of the agent's refusals that go on the record in an hour; past them, its daemon's log has them.
pub const REFUSALS_AN_HOUR: u32 = 60;
/// How many of what was recorded are remembered, so as not to record them again: the oldest go first.
const SEEN_KEPT: usize = 20_000;

/// What of the agent's uses is on the record already (first uses, refusals, each by secret and session), so that
/// a use does not read the whole ledger to know: the broker's state, `secret-uses.json`. Lost, it costs only an
/// entry said twice.
#[derive(Debug, Default, Serialize, serde::Deserialize)]
struct Seen {
    keys: std::collections::VecDeque<String>,
    hour: String,
    refused_in_hour: u32,
}

impl Seen {
    fn path(dir: &std::path::Path) -> PathBuf {
        dir.join("secret-uses.json")
    }

    fn read(dir: &std::path::Path) -> Seen {
        fs::read(Self::path(dir)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    fn write(&self, dir: &std::path::Path) -> Result<()> {
        use std::os::unix::fs::OpenOptionsExt;
        let part = dir.join(".secret-uses.json.new");
        let _ = fs::remove_file(&part);
        fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&part)?.write_all(&serde_json::to_vec(self)?)?;
        fs::rename(&part, Self::path(dir))?;
        Ok(())
    }

    /// Whether `key` is remembered.
    fn has(&self, key: &str) -> bool {
        self.keys.iter().any(|k| k == key)
    }

    /// Whether `key` is new (and now remembered).
    fn first(&mut self, key: &str) -> bool {
        if self.keys.iter().any(|k| k == key) {
            return false;
        }
        self.keys.push_back(key.into());
        while self.keys.len() > SEEN_KEPT {
            self.keys.pop_front();
        }
        true
    }

    /// Whether another refusal may go on the record this hour (and now counts).
    fn refusal_allowed(&mut self) -> bool {
        let hour = chrono::Utc::now().format("%Y-%m-%dT%H").to_string();
        if self.hour != hour {
            self.hour = hour;
            self.refused_in_hour = 0;
        }
        self.refused_in_hour += 1;
        self.refused_in_hour <= REFUSALS_AN_HOUR
    }
}

/// How a use stands, once the broker was asked: granted, or waiting for a person under a number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Used {
    Granted(Granted),
    Waiting(u64),
}

/// A use granted: the value, and where in the request it goes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Granted {
    pub value: String,
    pub header: String,
    pub prefix: String,
    /// For SSH: the host's keys, which OpenSSH's agent checks the server against before it signs.
    pub known: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use torq_wire::secrets::parse;

    /// `/proc/net/tcp` as the kernel writes it, the two ends of one loopback connection and one other.
    const TABLE: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:1E78 00000000:0000 0A 00000000:00000000 00:00000000 00000000   990        0 1 1 0000000000000000 100 0 0 10 0
   1: 0100007F:C350 0100007F:1E78 01 00000000:00000000 00:00000000 00000000  4343        0 2 1 0000000000000000 20 4 30 10 -1
   2: 0100007F:1E78 0100007F:C350 01 00000000:00000000 00:00000000 00000000   990        0 3 1 0000000000000000 20 4 30 10 -1
   3: 0A00000F:C351 0100007F:1E78 01 00000000:00000000 00:00000000 00000000  1000        0 4 1 0000000000000000 20 4 30 10 -1
";

    #[test]
    fn the_caller_is_the_kernels_answer() {
        assert_eq!(uid_of_connection(TABLE, 0xC350, 0x1E78), Some(4343), "the client's end, not the daemon's");
        assert_eq!(uid_of_connection(TABLE, 0x1E78, 0xC350), Some(990), "read the other way, the daemon's own");
        assert_eq!(uid_of_connection(TABLE, 0xC351, 0x1E78), None, "not from loopback");
        assert_eq!(uid_of_connection(TABLE, 0xC352, 0x1E78), None, "no such connection");
        let six = "  sl  local_address                         remote_address                        st\n   0: 0000000000000000FFFF00000100007F:C350 0000000000000000FFFF00000100007F:1E78 01 00000000:00000000 00:00000000 00000000  4343 0 9\n";
        assert_eq!(uid_of_connection(six, 0xC350, 0x1E78), Some(4343));
        let listening = TABLE.replace(" 01 00000000:00000000 00:00000000 00000000  4343", " 06 00000000:00000000 00:00000000 00000000  4343");
        assert_eq!(uid_of_connection(&listening, 0xC350, 0x1E78), None, "established only");
    }

    /// Sam's own rules: `torq-agent` there is sam's own agent.
    fn rules() -> torq_wire::secrets::Rules {
        torq_wire::secrets::parse_person(br#"{"gh":{"kind":"token","https":{"hosts":["api.github.com"]},"actors":["torq-agent"],"approve":"never"},
                   "asks":{"kind":"token","https":{"hosts":["api.github.com"]},"actors":["torq-agent"]},
                   "mine":{"kind":"token","https":{"hosts":["api.github.com"]}},
                   "key":{"kind":"ssh-key","ssh":{"hosts":["github.com"],"known":["github.com ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl"]},"actors":["torq-agent"],"approve":"never"}}"#).unwrap()
    }

    fn u(owner: Owner, name: &str, host: &str, actor: &str, session: Option<&str>) -> Use {
        Use { owner, name: name.into(), host: host.into(), actor: actor.into(), session: session.map(String::from), ssh: false }
    }

    fn agent(person: &str) -> AgentOf {
        AgentOf { person: person.into(), account: format!("torq-agent-{person}"), resident: false }
    }

    #[test]
    fn a_use_is_what_its_rule_says_and_nothing_else() {
        let r = rules();
        let sam = || Owner::Person("sam".into());
        let sams = agent("sam");
        assert!(may_use(&r, &u(sam(), "mine", "api.github.com", "sam", None), None).is_ok(), "its owner's own");
        assert!(may_use(&r, &u(sam(), "gh", "api.github.com", "torq-agent-sam", Some("s")), Some(&sams)).is_ok(), "sam's agent, as the rule lets it");
        assert!(may_use(&r, &u(sam(), "gh", "api.github.com", "torq-resident-sam", Some("s")), Some(&sams)).is_ok(), "sam's resident agent: the same agent");
        for (use_, of, why) in [
            (u(sam(), "gh", "api.github.com", "torq-agent-viper", Some("s")), Some(agent("viper")), "not another person's agent"),
            (u(sam(), "gh", "api.github.com", "torq-agent", Some("s")), None, "an account that is no one's agent"),
            (u(sam(), "mine", "api.github.com", "torq-agent-sam", Some("s")), Some(sams.clone()), "not the agent's"),
            (u(sam(), "mine", "api.github.com", "viper", None), None, "not another person's"),
            (u(sam(), "gh", "evil.example.com", "sam", None), None, "not to another host"),
            (u(sam(), "key", "github.com", "torq-agent-sam", Some("s")), Some(sams.clone()), "an SSH key is never sent"),
            (u(sam(), "none", "api.github.com", "sam", None), None, "no rule"),
        ] {
            assert!(may_use(&r, &use_, of.as_ref()).is_err(), "{why}");
        }
        // The system's: each agent by its account, each person by name, and no one else.
        let system = parse(br#"{"gh":{"kind":"token","https":{"hosts":["api.github.com"]},"actors":["torq-agent-sam","viper"],"approve":"never"}}"#).unwrap();
        let sys = |actor: &str, of: Option<&AgentOf>| may_use(&system, &u(Owner::System, "gh", "api.github.com", actor, of.map(|_| "s")), of).is_ok();
        assert!(sys("torq-agent-sam", Some(&sams)) && sys("torq-resident-sam", Some(&sams)), "sam's agent, named");
        assert!(sys("viper", None), "viper, named");
        assert!(!sys("torq-agent-viper", Some(&agent("viper"))), "viper's agent is not named");
        assert!(!sys("sam", None), "nor is sam");
        // SSH: a key, for the hosts its rule names; a token signs for nothing.
        let ssh = |name: &str, host: &str| Use { ssh: true, ..u(sam(), name, host, "torq-agent-sam", Some("s")) };
        assert!(may_use(&r, &ssh("key", "github.com"), Some(&sams)).is_ok());
        assert!(may_use(&r, &ssh("key", "gitlab.com"), Some(&sams)).is_err());
        assert!(may_use(&r, &ssh("gh", "api.github.com"), Some(&sams)).is_err());
    }

    /// When a person is asked (the approval design, part 2): the rule says each use; or the first, and it is; or
    /// the use names no session; or its session is tainted, whatever the rule says, but for the person's own
    /// standing word. What was refused while approving was not built is asked now.
    #[test]
    fn a_person_is_asked_in_these_cases_and_no_others() {
        use torq_wire::secrets::{Approve, Kind, Rule};
        let rule = |approve| Rule { approve, ..Rule::new(Kind::Token) };
        let asked = |approve, tainted, first| asks("sam's secret gh", &rule(approve), tainted, first).is_some();
        assert!(asked(Approve::EachUse, Some(false), true) && asked(Approve::EachUse, Some(false), false), "each use");
        assert!(asked(Approve::FirstUse, Some(false), true), "the first use in the session");
        assert!(!asked(Approve::FirstUse, Some(false), false), "not the ones after");
        assert!(!asked(Approve::Never, Some(false), true) && !asked(Approve::Never, Some(false), false));
        for approve in [Approve::EachUse, Approve::FirstUse, Approve::Never] {
            assert!(asked(approve, None, false), "no session: {approve:?}");
            assert!(asked(approve, Some(true), false), "a tainted session: {approve:?}");
        }
        for (tainted, first) in [(Some(false), true), (Some(true), true), (None, true), (Some(true), false)] {
            assert!(!asked(Approve::NeverEvenTainted, tainted, first), "never-even-tainted: {tainted:?}");
        }
        assert!(asks("x", &rule(Approve::Never), Some(true), false).unwrap().contains("untrusted text"));
    }
}

/// How long a grant of an agent socket lasts (ADR-052, SSH).
pub const GRANT_SECONDS: i64 = 3600;
/// The most agent sockets one account holds at once.
pub const GRANTS_EACH: usize = 8;

/// A program's SSH agent socket, granted: who it is for, read from the kernel when it asked (over a loopback
/// connection, as an HTTPS use is), and the session it named. The daemon names it in each use of a key through that
/// socket: a Unix socket's caller is not in the kernel's table of TCP connections, so this is how the broker still
/// knows who, and not by the daemon's word.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct Grant {
    pub actor: String,
    pub uid: u32,
    pub session: Option<String>,
    pub until: String,
}

/// An SSH key an account may use: whose, its name, its public half, and the hosts it signs for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SshKey {
    pub owner: Owner,
    pub name: String,
    pub public: String,
    pub hosts: Vec<String>,
    pub known: Vec<String>,
}

fn grant_id_ok(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Whether `value` is meant as an SSH private key (any form: one not OpenSSH's is refused, saying why).
fn is_ssh_key(value: &str) -> bool {
    value.trim_start().starts_with("-----BEGIN ") && value.contains("PRIVATE KEY-----")
}

impl Broker {
    fn grants_dir(&self) -> PathBuf {
        self.state_dir.join("secret-grants")
    }

    /// An agent socket granted to `actor` (`uid`), for `session` (the agent's: it must be one the broker holds).
    pub fn grant_agent(&self, actor: &str, uid: u32, session: Option<&str>) -> Result<String> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        if let Some(id) = session {
            let a = self.agent_of(actor).context("refused: a session is the agent's; a person's names none")?;
            self.agents_session(&a, id)?;
        }
        let mut bytes = [0u8; 16];
        fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes)).context("reading /dev/urandom")?;
        let id: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let until = (chrono::Utc::now() + chrono::Duration::seconds(GRANT_SECONDS)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let g = Grant { actor: actor.into(), uid, session: session.map(String::from), until };
        let dir = self.grants_dir();
        fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
        // The old ones go: a grant past its time is no grant.
        for e in fs::read_dir(&dir)?.flatten() {
            let old = fs::read(e.path()).ok().and_then(|b| serde_json::from_slice::<Grant>(&b).ok());
            if old.is_none_or(|o| chrono::DateTime::parse_from_rfc3339(&o.until).is_ok_and(|t| t < chrono::Utc::now())) {
                let _ = fs::remove_file(e.path());
            }
        }
        // A few at once for each account: grants nobody uses cannot pile up.
        let live = fs::read_dir(&dir)?.flatten().filter(|e| fs::read(e.path()).ok().and_then(|b| serde_json::from_slice::<Grant>(&b).ok()).is_some_and(|o| o.actor == actor)).count();
        if live >= GRANTS_EACH {
            bail!("refused: {actor} has {live} agent sockets already (at most {GRANTS_EACH} at once, each for an hour)");
        }
        fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(dir.join(&id))?.write_all(&serde_json::to_vec(&g)?)?;
        Ok(id)
    }

    /// The grant `id`, if it is one and its time is not over.
    pub fn grant(&self, id: &str) -> Result<Grant> {
        if !grant_id_ok(id) {
            bail!("refused: {id:?} is not a grant");
        }
        let g: Grant = fs::read(self.grants_dir().join(id)).ok().and_then(|b| serde_json::from_slice(&b).ok()).context("refused: no such grant")?;
        if chrono::DateTime::parse_from_rfc3339(&g.until).map_or(true, |t| t < chrono::Utc::now()) {
            bail!("refused: the grant is over: ask for another agent socket");
        }
        Ok(g)
    }

    /// The SSH keys `actor` may use, by their rules: their public halves (no value, nothing on the record). Which of
    /// them a use may sign with, and for which host, is judged at each use.
    pub fn ssh_keys_for(&self, actor: &str) -> Vec<SshKey> {
        use torq_wire::secrets::Kind;
        let agent = self.agent_of(actor);
        let mut out = Vec::new();
        let mut owners: Vec<Owner> = self.kept_secrets().into_iter().map(|k| k.owner).collect();
        owners.sort();
        owners.dedup();
        for owner in owners {
            let Ok(rules) = self.rules_of(&owner) else { continue };
            for (name, r) in rules {
                let Some(ssh) = r.ssh.clone() else { continue };
                let theirs = may(&owner, &r, actor, agent.as_ref());
                let Ok(public) = fs::read_to_string(self.secret_dir(&owner).join(format!("{name}.pub"))) else { continue };
                if r.kind == Kind::SshKey && theirs {
                    out.push(SshKey { owner: owner.clone(), name, public: public.trim().to_string(), hosts: ssh.hosts, known: ssh.known });
                }
            }
        }
        out
    }

    /// The public half of an SSH key just kept, beside it: for listing the key without its value.
    /// Not a key: nothing, and any public half of an earlier value goes.
    fn keep_public(&self, owner: &Owner, name: &str, value: &str) -> Result<()> {
        use std::os::unix::fs::OpenOptionsExt;
        let public = self.secret_dir(owner).join(format!("{name}.pub"));
        if !is_ssh_key(value) {
            let _ = fs::remove_file(&public);
            return Ok(());
        }
        // From the key's own header, where it is in the clear: the private key is given to no program, and is in
        // no file but its encrypted one. A key that cannot be read refuses the value, and the old one stays listed.
        let line = torq_wire::secrets::openssh_public(value).map_err(|why| anyhow::anyhow!("not kept: {why}"))?;
        let part = self.secret_dir(owner).join(format!(".{name}.pub.new"));
        let _ = fs::remove_file(&part);
        fs::OpenOptions::new().write(true).create_new(true).mode(0o644).open(&part)?.write_all(format!("{line}\n").as_bytes())?;
        fs::rename(&part, &public)?;
        Ok(())
    }
}
