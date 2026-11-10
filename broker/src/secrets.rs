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
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Owner {
    System,
    Person(String),
}

impl Owner {
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

    fn tier(&self) -> Tier {
        match self {
            Owner::System => Tier::System,
            Owner::Person(_) => Tier::User,
        }
    }

    /// How the ledger and the messages say it.
    fn whose(&self, name: &str) -> String {
        match self {
            Owner::System => format!("the system's secret {name}"),
            Owner::Person(p) => format!("{p}'s secret {name}"),
        }
    }

    fn data(&self, name: &str) -> serde_json::Value {
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
            child.stdin.take().context("systemd-creds' input")?.write_all(value.as_bytes())?;
            let out = child.wait_with_output()?;
            if !out.status.success() {
                bail!("systemd-creds could not encrypt it: {}", said(String::from_utf8_lossy(&out.stderr).trim()));
            }
            // systemd-creds may write its output anew, with modes of its own: private again before it is in place.
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&part, fs::Permissions::from_mode(0o600))?;
            fs::File::open(&part)?.sync_all()?;
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
}

/// Why `u` may not happen by these rules (Err), or the rule that lets it. `tainted`: whether the agent's session
/// read untrusted text (None: no session, which is tainted too, as an agent's proposal naming none is).
/// Until a person can approve a use (step 4), the agent's uses are those its rule lets without asking, from a
/// session that read nothing untrusted; anything else is refused, never let through.
pub fn may_use<'a>(rules: &'a torq_wire::secrets::Rules, u: &Use, tainted: Option<bool>) -> Result<&'a torq_wire::secrets::Rule> {
    use torq_wire::secrets::{Approve, Kind, AGENT};
    let whose = u.owner.whose(&u.name);
    let r = rules.get(&u.name).with_context(|| format!("{whose} has no rule: it is used for nothing"))?;
    if r.kind != Kind::Token {
        bail!("{whose} is an SSH key: it is never sent");
    }
    if !r.https.as_ref().is_some_and(|h| h.hosts.contains(&u.host)) {
        bail!("{whose} is not sent to {}", u.host);
    }
    let named = r.actors.iter().any(|a| *a == u.actor);
    let allowed = match &u.owner {
        Owner::Person(p) => *p == u.actor || (u.actor == AGENT && named),
        Owner::System => named,
    };
    if !allowed {
        bail!("{} may not use {whose}", u.actor);
    }
    if u.actor == AGENT {
        match tainted {
            None => bail!("the agent's request names no session (X-Torq-Session): a person approves such a use, and approving is not built yet"),
            Some(true) => bail!("the agent's session read untrusted text: a person approves each of its uses, and approving is not built yet"),
            Some(false) if r.approve != Approve::Never => bail!("{whose} is used by the agent with a person's approval ({}), and approving is not built yet", r.approve.word()),
            Some(false) => {}
        }
    }
    Ok(r)
}

impl Broker {
    /// The rules `owner`'s secrets have now (the repository's HEAD): none is none; a file that is not one, refused.
    fn rules_of(&self, owner: &Owner) -> Result<torq_wire::secrets::Rules> {
        let (path, parse): (String, fn(&[u8]) -> std::result::Result<torq_wire::secrets::Rules, String>) = match owner {
            Owner::System => (torq_wire::secrets::SYSTEM_RULES.into(), torq_wire::secrets::parse),
            Owner::Person(p) => (format!("users/{p}/{}", torq_wire::secrets::PERSON_RULES), torq_wire::secrets::parse_person),
        };
        match self.repo.file_at_head(&path, torq_wire::secrets::MAX_FILE)? {
            None => Ok(Default::default()),
            Some(b) => parse(&b).map_err(|why| anyhow::anyhow!("{path}: {why}")),
        }
    }

    /// One use of a secret, for the daemon (ADR-052, step 2): judged by its rule, the agent's first use in each
    /// session on the record (and every refusal of the agent's, once per session, secret and reason), and its
    /// value. Every use the daemon logs itself. No broker lock: a rebuild does not hold a use up.
    pub fn use_secret(&self, u: &Use) -> Result<Granted> {
        let _l = Lock::acquire_named(&self.state_dir, "secrets.lock", 10)?;
        let agent = u.actor == torq_wire::secrets::AGENT;
        let tainted = match &u.session {
            Some(id) if agent => Some(crate::session::read(self, id)?.context("refused: the request names no session the broker holds")?.tainted),
            Some(_) => bail!("refused: a session is the agent's; a person's use names none"),
            None => None,
        };
        let rules = self.rules_of(&u.owner)?;
        let judged = may_use(&rules, u, tainted).map(|r| r.https.clone().unwrap_or_else(|| torq_wire::secrets::Https { hosts: vec![], header: "Authorization".into(), prefix: "Bearer ".into() }));
        let note = |kind: &str, reason: String, extra: serde_json::Value| -> Result<Entry> {
            let mut data = u.owner.data(&u.name);
            data["session"] = json!(u.session);
            data["host"] = json!(u.host);
            if let serde_json::Value::Object(x) = extra {
                for (k, v) in x {
                    data[k] = v;
                }
            }
            self.ledger.append(NewEntry { actor: if agent { "agent".into() } else { "user".into() }, approved_by: None, kind: kind.into(), tier: u.owner.tier().to_string(), tainted: tainted.unwrap_or(agent), reason, data })
        };
        let whose = match &u.owner {
            Owner::System => "system".to_string(),
            Owner::Person(p) => format!("user/{p}"),
        };
        let key = format!("{whose}/{}|{}", u.name, u.session.as_deref().unwrap_or("-"));
        let mut seen = Seen::read(&self.state_dir);
        let https = match judged {
            Ok(h) => h,
            Err(e) => {
                // On the record: the agent's first refusal in a session for a secret that has a rule, and no more
                // than REFUSALS_AN_HOUR of them: names it made up, or a refusal it repeats, are the daemon's log's.
                let why: String = said(&format!("{e:#}"));
                if agent && rules.contains_key(&u.name) && seen.first(&format!("refused|{key}")) && seen.refusal_allowed() {
                    note("secret_use_refused", format!("The agent's use of {} refused", u.owner.whose(&u.name)), json!({ "error": why }))?;
                }
                seen.write(&self.state_dir)?;
                return Err(e);
            }
        };
        if agent && seen.first(&format!("first|{key}")) {
            note("secret_first_use", format!("The agent used {} for the first time in its session", u.owner.whose(&u.name)), json!({}))?;
            seen.write(&self.state_dir)?;
        }
        Ok(Granted { value: self.decrypt(&u.owner, &u.name)?, header: https.header, prefix: https.prefix })
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

/// A use granted: the value, and where in the request it goes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Granted {
    pub value: String,
    pub header: String,
    pub prefix: String,
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

    fn rules() -> torq_wire::secrets::Rules {
        parse(br#"{"gh":{"kind":"token","https":{"hosts":["api.github.com"]},"actors":["torq-agent"],"approve":"never"},
                   "asks":{"kind":"token","https":{"hosts":["api.github.com"]},"actors":["torq-agent"]},
                   "mine":{"kind":"token","https":{"hosts":["api.github.com"]}},
                   "key":{"kind":"ssh-key","ssh":{"hosts":["github.com"]},"actors":["torq-agent"],"approve":"never"}}"#).unwrap()
    }

    fn u(owner: Owner, name: &str, host: &str, actor: &str, session: Option<&str>) -> Use {
        Use { owner, name: name.into(), host: host.into(), actor: actor.into(), session: session.map(String::from) }
    }

    #[test]
    fn a_use_is_what_its_rule_says_and_nothing_else() {
        let r = rules();
        let sam = || Owner::Person("sam".into());
        assert!(may_use(&r, &u(sam(), "mine", "api.github.com", "sam", None), None).is_ok(), "its owner's own");
        assert!(may_use(&r, &u(sam(), "gh", "api.github.com", "torq-agent", Some("s")), Some(false)).is_ok(), "the agent, as the rule lets it");
        for (use_, tainted, why) in [
            (u(sam(), "mine", "api.github.com", "torq-agent", Some("s")), Some(false), "not the agent's"),
            (u(sam(), "mine", "api.github.com", "viper", None), None, "not another person's"),
            (u(sam(), "gh", "evil.example.com", "sam", None), None, "not to another host"),
            (u(sam(), "key", "github.com", "torq-agent", Some("s")), Some(false), "an SSH key is never sent"),
            (u(sam(), "none", "api.github.com", "sam", None), None, "no rule"),
            (u(sam(), "gh", "api.github.com", "torq-agent", None), None, "no session"),
            (u(sam(), "gh", "api.github.com", "torq-agent", Some("s")), Some(true), "a tainted session"),
            (u(sam(), "asks", "api.github.com", "torq-agent", Some("s")), Some(false), "approval needed"),
            (u(Owner::System, "mine", "api.github.com", "sam", None), None, "a system secret is used by the actors it names"),
        ] {
            assert!(may_use(&r, &use_, tainted).is_err(), "{why}");
        }
        assert!(may_use(&r, &u(Owner::System, "gh", "api.github.com", "torq-agent", Some("s")), Some(false)).is_ok());
    }
}
