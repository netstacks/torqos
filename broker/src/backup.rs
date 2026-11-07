//! The declared backup (ADR-038): NixOS's own `services.restic.backups`, set in the configuration tree
//! like any NixOS setting. Its key is the one secret a person keeps in their head, and it is kept as a
//! password is: outside the repository, root's alone (`/var/lib/torq/secrets/backup/<name>`, which
//! modules/backup.nix points the backup's `passwordFile` at), ledgered without it.
//!
//! A restore is as safe as a snapshot's, and is the same copy (snapshot.rs): restic writes what it
//! brings back into a room of the broker's that only root can reach, and from there the owner of the
//! folder writes it, root reading into a pipe. What is live is set aside, never overwritten, and changes
//! by a rename once all of it is there. Never into `/var/lib/torq`, nor the machine's own folders.

use crate::ledger::{Entry, NewEntry};
use crate::policy::Tier;
use crate::{config, snapshot, Broker, Lock};
use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// Where the keys are: each backup's in a file of its name, root's alone.
pub const KEYS: &str = "/var/lib/torq/secrets/backup";
/// Where a restore in progress is kept, root's alone: not in `/var/lib/torq`, a small file system of its
/// own so that no one can fill the ledger's room (the audit of M3, M2), nor in a snapshot or a backup.
pub const ROOMS: &str = "/var/cache/torq/restoring";
/// The longest key taken: a passphrase, not a file.
pub const KEY_MAX: usize = 1024;
/// How long `backups` waits for a repository to list its snapshots: a remote one may be out of reach.
const LIST_SECONDS: &str = "60";
/// How much of what restic says is kept, in root's memory.
const SAID: u64 = 4096;
/// How much of restic's answer is read: tens of thousands of snapshots.
const OUT_MAX: u64 = 64 << 20;
/// Characters restic reads as a pattern in `--include`: a path with one would bring back more than
/// was named.
const PATTERN: &[char] = &['*', '?', '[', ']', '\\'];

/// The backups the configuration declares, and where their keys are.
pub struct Backups {
    /// Each one's name, and the restic command with its repository and key already set.
    restic: BTreeMap<String, String>,
    keys: PathBuf,
    rooms: PathBuf,
}

impl Default for Backups {
    fn default() -> Self {
        Backups { restic: BTreeMap::new(), keys: KEYS.into(), rooms: ROOMS.into() }
    }
}

/// One backup, as `show system backups` says it.
#[derive(Debug, Serialize)]
pub struct Backup {
    pub name: String,
    /// Whether its key has been given (`request system backup key <name>`).
    pub key: bool,
    /// What its repository holds, newest last (restic's order).
    pub snapshots: Vec<Snap>,
    /// Why the snapshots could not be listed.
    pub error: Option<String>,
    /// How its unit last ran, since this boot (systemd's word): none if it has not.
    pub last_run: Option<Run>,
}

/// A snapshot in a backup's repository.
#[derive(Debug, Serialize)]
pub struct Snap {
    pub id: String,
    pub time: String,
    pub host: String,
    pub paths: Vec<String>,
}

/// How a backup's unit last ran.
#[derive(Debug, Serialize)]
pub struct Run {
    pub unit: String,
    /// `success`, or how it failed (`exit-code`, `timeout`, …).
    pub result: String,
    /// Its main process's exit status.
    pub status: String,
    pub at: String,
    /// Running now.
    pub running: bool,
}

/// A backup's name, as it may be a file's in the keys' folder (modules/backup.nix asserts the same).
pub fn name_ok(name: &str) -> bool {
    (1..=64).contains(&name.len()) && name.starts_with(|c: char| c.is_ascii_alphanumeric()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// A snapshot as restic names one: `latest`, or its id (8 to 64 hex digits).
pub fn snapshot_ok(id: &str) -> bool {
    id == "latest" || ((8..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
}

/// A key: a passphrase on one line, as restic reads one (it trims spaces at either end, so none are taken).
pub fn key_ok(key: &str) -> Result<()> {
    if key.is_empty() || key.len() > KEY_MAX {
        bail!("a backup's key is 1 to {KEY_MAX} bytes");
    }
    if key.chars().any(char::is_control) || key.trim() != key {
        bail!("a backup's key is one line, with no space at either end");
    }
    Ok(())
}

/// Text from restic or systemd, safe to show and to ledger.
fn said(s: &str, n: usize) -> String {
    s.chars().filter(|c| !c.is_control()).take(n).collect()
}

/// The broker's room for a restore in progress: root's alone, its own for each run, gone when dropped.
/// What restic brings back sits here, as it was, until the owner's copy has read it.
struct Room(PathBuf);

impl Room {
    fn new(all: &Path) -> Result<Self> {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        fs::DirBuilder::new().recursive(true).mode(0o700).create(all).with_context(|| format!("creating {}", all.display()))?;
        // Root's own, and the folder itself: what restic writes here may be anyone's secrets.
        let m = fs::symlink_metadata(all)?;
        if !m.is_dir() || std::os::unix::fs::MetadataExt::uid(&m) != rustix::process::geteuid().as_raw() {
            bail!("{} is not the broker's own folder: not restored", all.display());
        }
        fs::set_permissions(all, fs::Permissions::from_mode(0o700))?;
        // What a restore cut short left (its broker gone): a copy of someone's data, kept from them.
        for e in fs::read_dir(all)?.flatten() {
            if e.file_name().to_str().and_then(|p| p.parse::<u32>().ok()).is_some_and(|p| !Path::new("/proc").join(p.to_string()).exists()) {
                let _ = fs::remove_dir_all(e.path());
            }
        }
        let dir = all.join(std::process::id().to_string());
        let _ = fs::remove_dir_all(&dir);
        fs::DirBuilder::new().mode(0o700).create(&dir).with_context(|| format!("creating {}", dir.display()))?;
        Ok(Room(dir))
    }
}

impl Drop for Room {
    fn drop(&mut self) {
        // `remove_dir_all` follows no link: what restic restored may hold any.
        let _ = fs::remove_dir_all(&self.0);
    }
}

impl Broker {
    /// The backups the configuration declares: broker.json's `backups`.
    pub fn with_backups(mut self, restic: &BTreeMap<String, String>) -> Self {
        self.backup.restic = restic.clone();
        self
    }

    /// Where a restore in progress is kept (tests: a scratch folder).
    pub fn with_backup_rooms(mut self, dir: impl Into<PathBuf>) -> Self {
        self.backup.rooms = dir.into();
        self
    }

    /// Where the keys are kept (tests: a scratch folder).
    pub fn with_backup_keys(mut self, dir: impl Into<PathBuf>) -> Self {
        self.backup.keys = dir.into();
        self
    }

    /// The restic command of the backup `name`, if the configuration declares it.
    fn restic_of(&self, name: &str) -> Result<&str> {
        if !name_ok(name) {
            bail!("{name:?} is not a backup's name");
        }
        self.backup.restic.get(name).map(String::as_str).with_context(|| format!("this machine's configuration declares no backup {name:?} (services restic backups {name})"))
    }

    /// Restic for backup `name`, with `args`, stopped after `seconds` if given: what it printed.
    fn restic(&self, name: &str, args: &[&str], seconds: Option<&str>) -> Result<Vec<u8>> {
        let cmd = self.restic_of(name)?;
        let mut c = match seconds {
            Some(s) => {
                let mut c = config::command("timeout");
                c.arg(s).arg(cmd);
                c
            }
            None => config::command(cmd),
        };
        let mut child = c.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().context("running restic")?;
        let mut err = child.stderr.take().context("restic's errors")?;
        let errs = std::thread::spawn(move || {
            let mut kept = Vec::new();
            let _ = (&mut err).take(SAID).read_to_end(&mut kept);
            let _ = std::io::copy(&mut err, &mut std::io::sink());
            kept
        });
        let mut out = Vec::new();
        // A list of snapshots, at most this large: the rest is read and dropped, never kept in root's memory.
        let mut stdout = child.stdout.take().context("restic's output")?;
        (&mut stdout).take(OUT_MAX).read_to_end(&mut out).context("reading restic")?;
        std::io::copy(&mut stdout, &mut std::io::sink()).context("reading restic")?;
        let status = child.wait().context("running restic")?;
        let errs = errs.join().unwrap_or_default();
        if !status.success() {
            let why = said(String::from_utf8_lossy(&errs).trim(), 300);
            bail!("restic failed ({status}){}", if why.is_empty() { String::new() } else { format!(": {why}") });
        }
        Ok(out)
    }

    /// The snapshots `wanted` names in `name`'s repository (`latest`, or an id): restic's JSON, read.
    fn snaps(&self, name: &str, wanted: Option<&str>) -> Result<Vec<Snap>> {
        let mut args = vec!["snapshots", "--json", "--no-lock"];
        args.extend(wanted);
        let out = self.restic(name, &args, Some(LIST_SECONDS))?;
        let v: serde_json::Value = serde_json::from_slice(&out).context("restic's list of snapshots is not JSON")?;
        let text = |s: &serde_json::Value, k: &str| said(s[k].as_str().unwrap_or_default(), 200);
        Ok(v.as_array()
            .into_iter()
            .flatten()
            .filter(|s| s["id"].as_str().is_some_and(|i| i.len() == 64 && snapshot_ok(i)))
            .map(|s| Snap {
                id: text(s, "id"),
                time: text(s, "time"),
                host: text(s, "hostname"),
                paths: s["paths"].as_array().into_iter().flatten().filter_map(|p| p.as_str()).map(|p| said(p, 200)).collect(),
            })
            .collect())
    }

    /// How `name`'s unit last ran, as systemd says (since this boot).
    fn last_run(name: &str) -> Option<Run> {
        let unit = format!("restic-backups-{name}.service");
        let out = config::command("systemctl")
            .args(["show", &unit, "--property=Result,ExecMainStatus,ExecMainExitTimestamp,ActiveState"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())?;
        let text = String::from_utf8_lossy(&out.stdout);
        let get = |k: &str| text.lines().find_map(|l| l.strip_prefix(k)?.strip_prefix('=')).map(|v| said(v, 100)).unwrap_or_default();
        let (at, running) = (get("ExecMainExitTimestamp"), get("ActiveState") == "activating");
        (!at.is_empty() || running).then(|| Run { unit, result: get("Result"), status: get("ExecMainStatus"), at, running })
    }

    /// `show system backups`: each declared backup, whether it has its key, what its repository holds,
    /// and how it last ran. Read only: nothing is locked, written or ledgered.
    pub fn backups(&self) -> Vec<Backup> {
        self.backup
            .restic
            .keys()
            .map(|name| {
                let key = self.backup.keys.join(name).is_file();
                let (snapshots, error) = if !key {
                    (vec![], Some(format!("no key yet: request system backup key {name}")))
                } else {
                    match self.snaps(name, None) {
                        Ok(s) => (s, None),
                        Err(e) => (vec![], Some(said(&format!("{e:#}"), 300))),
                    }
                };
                Backup { name: name.clone(), key, snapshots, error, last_run: Self::last_run(name) }
            })
            .collect()
    }

    /// `request system backup key <name>`: the passphrase of a declared backup, kept root's alone where
    /// its `passwordFile` points, outside the repository. On the record first, as a password is (never
    /// the key itself): stopped between, the ledger says a little too much, never too little.
    pub fn set_backup_key(&self, name: &str, key: &str, by: &str) -> Result<Entry> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        let _lock = Lock::acquire(&self.state_dir)?;
        self.ledger.verify().context("cannot change a backup's key on a broken ledger")?;
        self.restic_of(name)?;
        key_ok(key)?;
        let entry = self.ledger.append(NewEntry {
            actor: "user".into(),
            approved_by: Some(by.into()),
            kind: "backup_key".into(),
            tier: Tier::System.to_string(),
            tainted: false,
            reason: format!("The key of backup {name} given"),
            data: json!({ "backup": name }),
        })?;
        let part = self.backup.keys.join(format!(".{name}.new"));
        let kept = (|| -> Result<()> {
            fs::DirBuilder::new().recursive(true).mode(0o700).create(&self.backup.keys)?;
            let _ = fs::remove_file(&part);
            let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&part)?;
            f.write_all(key.as_bytes())?;
            f.sync_all()?;
            fs::rename(&part, self.backup.keys.join(name))?;
            // A rename is only as lasting as its directory.
            fs::File::open(&self.backup.keys)?.sync_all()?;
            Ok(())
        })();
        let Err(e) = kept else { return Ok(entry) };
        let _ = fs::remove_file(&part);
        let noted = self.ledger.append(NewEntry {
            actor: "broker".into(),
            approved_by: Some(by.into()),
            kind: "backup_key_failed".into(),
            tier: Tier::System.to_string(),
            tainted: false,
            reason: format!("The key of backup {name} was not kept, after all"),
            data: json!({ "backup": name, "entry": entry.body.seq, "error": said(&format!("{e:#}"), 300) }),
        });
        Err(match noted {
            Ok(_) => e.context("the key was not kept"),
            Err(l) => e.context(format!("the key was not kept, and the ledger could not say so: {l:#}")),
        })
    }

    /// `request system restore <name> [<snapshot>] <path>`: `path` back as backup `name`'s snapshot has
    /// it (`latest` if none is named). Restic restores it into the broker's room, root's alone; from
    /// there it is the snapshot's copy (`snapshot::plan`, `Copy::run`): written by the owner of the folder
    /// it lies in, what is there set aside, staged, then swapped. The lock is for the checks alone: a
    /// large restore takes hours. Ledgered, whether it worked or not, once the snapshot is found.
    pub fn restore_backup(&self, name: &str, wanted: Option<&str>, path: &str, by: &str) -> Result<Entry> {
        self.restic_of(name)?;
        let wanted = wanted.unwrap_or("latest");
        if !snapshot_ok(wanted) {
            bail!("{wanted:?} is not a snapshot: `latest`, or an id from show system backups");
        }
        let (tree, parts) = snapshot::data_path(path)?;
        let path = snapshot::written_out(tree, &parts);
        if path.contains(PATTERN) {
            bail!("{path:?}: restic would read its {PATTERN:?} as a pattern and bring back more than you named: restore the folder it is in");
        }
        {
            let _lock = Lock::acquire(&self.state_dir)?;
            // Not on top of a change still in doubt, as a snapshot's restore is not.
            if crate::pending_confirm(&self.ledger.read_all()?).is_some() {
                bail!("a commit confirmed is waiting: confirm it or let it roll back first");
            }
        }
        if !self.backup.keys.join(name).is_file() {
            bail!("backup {name} has no key here yet: request system backup key {name}");
        }
        // Its whole id, for the record: `latest` is a different snapshot tomorrow.
        let id = self.snaps(name, Some(wanted))?.into_iter().next().map(|s| s.id).with_context(|| format!("backup {name} has no snapshot {wanted}"))?;
        let short = &id[..8];
        let done = (|| -> Result<snapshot::Copy> {
            let room = Room::new(&self.backup.rooms)?;
            let target = room.0.to_string_lossy().into_owned();
            self.restic(name, &["restore", &id, "--target", &target, "--include", &path], None)?;
            // As restic wrote it: below the room, the path as it was backed up.
            let from = room.0.join(if tree == "home" { "home" } else { "var/lib" });
            let copy = {
                let _lock = Lock::acquire(&self.state_dir)?;
                snapshot::plan(&from, tree, &parts, &format!("backup-{name}-{short}"), &format!("in backup {name}'s snapshot {short}"))?
            };
            copy.run()?;
            Ok(copy)
        })();
        let error = done.as_ref().err().map(|e| said(&format!("{e:#}"), 300));
        // What happened, not what was expected: the swap decides, on what is there by then.
        let kept = done.as_ref().ok().map(|c| c.set_aside.clone()).filter(|a| fs::symlink_metadata(a).is_ok());
        let e = self.ledger.append(NewEntry {
            actor: "user".into(),
            approved_by: Some(by.into()),
            kind: "backup_restored".into(),
            tier: Tier::System.to_string(),
            tainted: false,
            reason: format!("{path} put back from backup {name}, snapshot {short}{}", if error.is_some() { ": it failed" } else { "" }),
            data: json!({ "backup": name, "backup_snapshot": id, "path": path, "set_aside": kept, "error": error }),
        })?;
        done.with_context(|| format!("{path} was not restored"))?;
        Ok(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_snapshots_and_keys() {
        for good in ["home", "nas-1", "off_site", "B2"] {
            assert!(name_ok(good), "{good}");
        }
        for bad in ["", ".", "..", "-x", "_x", "a/b", "a.b", "a b", "a\n", &"x".repeat(65)] {
            assert!(!name_ok(bad), "{bad:?}");
        }
        for good in ["latest", "0123abcd", &"f".repeat(64)] {
            assert!(snapshot_ok(good), "{good}");
        }
        for bad in ["", "LATEST", "0123abc", "0123ABCD", "0123abcg", "../x", &"f".repeat(65), "latest:/home"] {
            assert!(!snapshot_ok(bad), "{bad:?}");
        }
        assert!(key_ok("correct horse battery staple").is_ok());
        for bad in ["", " x", "x ", "a\nb", "a\tb", &"x".repeat(KEY_MAX + 1)] {
            assert!(key_ok(bad).is_err(), "{bad:?}");
        }
    }
}
