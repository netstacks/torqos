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
        let value = String::from_utf8(value).context("the value is not text")?;
        Ok((entry, value))
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
