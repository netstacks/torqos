//! The declared backup (ADR-038): its key kept outside the repository and the ledger, its list read
//! only, and a restore that is a snapshot's copy — restic stubbed by a script that says what it was
//! asked and does what each test needs.

// No unsafe code and no dead code, ever (docs/DECISIONS.md ADR-020).
#![forbid(unsafe_code)]
#![forbid(dead_code)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use torq_broker::Broker;

const ID: &str = "4bd1d0c5b0a5e3a3e1d2f0b8c9a7e6d5c4b3a2918f7e6d5c4b3a2918f7e6d5c4";

/// A machine with one declared backup, `home`, whose restic is `script` (a shell script's body: `$@`
/// are restic's arguments, `$LOG` where they are written).
struct Machine {
    dir: tempfile::TempDir,
    b: Broker,
}

impl Machine {
    fn new(script: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("config");
        fs::create_dir_all(&repo).unwrap();
        fs::write(repo.join("desktop.nix"), "{ }\n").unwrap();
        let restic = dir.path().join("restic-home");
        fs::write(&restic, format!("#!/bin/sh\nLOG={}\necho \"$@\" >> \"$LOG\"\n{script}\n", dir.path().join("asked").display())).unwrap();
        fs::set_permissions(&restic, fs::Permissions::from_mode(0o755)).unwrap();
        let backups = BTreeMap::from([("home".to_string(), restic.to_string_lossy().into_owned())]);
        let b = Broker::new(&repo, dir.path().join("state"), "true").with_backups(&backups).with_backup_keys(dir.path().join("secrets/backup")).with_backup_rooms(dir.path().join("cache/restoring"));
        b.init().unwrap();
        Machine { dir, b }
    }
    fn keys(&self) -> PathBuf {
        self.dir.path().join("secrets/backup")
    }
    fn asked(&self) -> String {
        fs::read_to_string(self.dir.path().join("asked")).unwrap_or_default()
    }
    fn ledger(&self) -> String {
        fs::read_to_string(self.b.ledger.path()).unwrap()
    }
    fn rooms(&self) -> Vec<PathBuf> {
        fs::read_dir(self.dir.path().join("cache/restoring")).map(|d| d.flatten().map(|e| e.path()).collect()).unwrap_or_default()
    }
}

/// What restic says of the repository's snapshots: one, `ID`.
fn listing() -> String {
    format!(r#"case "$1" in snapshots) echo '[{{"id":"{ID}","short_id":"4bd1d0c5","time":"2026-11-07T01:00:00Z","hostname":"torq5","paths":["/home","/var/lib"]}}]' ;; esac"#)
}

#[test]
fn the_key_is_kept_root_s_alone_and_never_in_the_ledger() {
    let m = Machine::new("true");
    let secret = "correct horse battery staple";
    let e = m.b.set_backup_key("home", secret, "casey").unwrap();
    assert_eq!((e.body.kind.as_str(), e.body.approved_by.as_deref()), ("backup_key", Some("casey")));
    let file = m.keys().join("home");
    assert_eq!(fs::read_to_string(&file).unwrap(), secret, "as restic reads it: no newline");
    assert_eq!(fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(fs::metadata(m.keys()).unwrap().permissions().mode() & 0o777, 0o700);
    assert!(!m.ledger().contains("horse"), "the key is never in the ledger");
    assert!(!fs::read_dir(m.keys()).unwrap().flatten().any(|f| f.file_name().to_string_lossy().ends_with(".new")), "nothing staged is left");
    // A new key replaces it, on the record too.
    m.b.set_backup_key("home", "another one", "casey").unwrap();
    assert_eq!(fs::read_to_string(&file).unwrap(), "another one");
    // A backup the configuration does not declare, or a key that is not one: refused, and nothing written.
    let n = m.b.ledger.read_all().unwrap().len();
    for (name, key) in [("work", "a key"), ("../passwords/casey", "a key"), ("home", ""), ("home", " spaced "), ("home", "two\nlines")] {
        assert!(m.b.set_backup_key(name, key, "casey").is_err(), "{name:?} {key:?}");
    }
    assert_eq!(m.b.ledger.read_all().unwrap().len(), n);
    assert!(!m.keys().join("work").exists());
    m.b.ledger.verify().unwrap();
}

#[test]
fn the_list_is_read_only_and_says_what_is_missing() {
    let m = Machine::new(&listing());
    let n = m.b.ledger.read_all().unwrap().len();
    let l = m.b.backups();
    assert_eq!((l.len(), l[0].key), (1, false));
    assert!(l[0].error.as_deref().unwrap().contains("request system backup key home"), "{:?}", l[0].error);
    assert!(m.asked().is_empty(), "no key: restic is not asked");
    m.b.set_backup_key("home", "the key", "casey").unwrap();
    let l = m.b.backups();
    assert_eq!((l[0].key, l[0].snapshots.len(), l[0].error.as_deref()), (true, 1, None));
    assert_eq!((l[0].snapshots[0].id.as_str(), l[0].snapshots[0].host.as_str()), (ID, "torq5"));
    assert_eq!(m.asked().trim(), "snapshots --json --no-lock");
    assert_eq!(m.b.ledger.read_all().unwrap().len(), n + 1, "the list writes nothing (the one entry is the key's)");
    // A repository that will not open: said, not a failure of the whole list.
    let bad = Machine::new("echo 'Fatal: wrong password or no key found' >&2; exit 1");
    bad.b.set_backup_key("home", "wrong", "casey").unwrap();
    let l = bad.b.backups();
    assert!(l[0].snapshots.is_empty() && l[0].error.as_deref().unwrap().contains("wrong password"), "{:?}", l[0].error);
}

#[test]
fn a_restore_names_what_it_may_and_asks_for_no_more() {
    let m = Machine::new(&listing());
    // Before restic is asked anything: a backup not declared, a snapshot that is not one, a path outside
    // the data or in the machine's own (the ledger, the configuration, the keys), or one restic would read
    // as a pattern.
    for (name, snap, path, why) in [
        ("work", None, "/home/alice", "declares no backup"),
        ("home", Some("HEAD~1"), "/home/alice", "not a snapshot"),
        ("home", None, "/etc/shadow", "below /home and /var/lib"),
        ("home", None, "/var/lib/torq/secrets/backup/home", "the machine's own"),
        ("home", None, "/var/lib/torq", "the machine's own"),
        ("home", None, "/home/alice/../../etc", "no `..`"),
        ("home", None, "/home/alice/*", "as a pattern"),
        ("home", None, "/home/alice/[ab]", "as a pattern"),
        ("home", None, "/home/alice", "no key here yet"),
    ] {
        let e = format!("{:#}", m.b.restore_backup(name, snap, path, "casey").unwrap_err());
        assert!(e.contains(why), "{path}: {e}");
    }
    assert!(m.asked().is_empty(), "restic was asked nothing: {}", m.asked());
    assert_eq!(m.b.ledger.read_all().unwrap().len(), 1, "refusals are not entries");
    m.b.set_backup_key("home", "the key", "casey").unwrap();
    let none = Machine::new("case \"$1\" in snapshots) echo '[]' ;; esac");
    none.b.set_backup_key("home", "k", "casey").unwrap();
    assert!(format!("{:#}", none.b.restore_backup("home", None, "/home/alice", "casey").unwrap_err()).contains("has no snapshot latest"));
}

/// restic fails: nothing live was touched, the room is gone, and the ledger says which snapshot, whole.
#[test]
fn a_failed_restore_is_ledgered_and_leaves_nothing() {
    let m = Machine::new(&format!("{}\n[ \"$1\" = restore ] && {{ mkdir -p \"$4/home\"; echo 'Fatal: unable to open repository' >&2; exit 1; }}\nexit 0", listing()));
    m.b.set_backup_key("home", "the key", "casey").unwrap();
    let e = format!("{:#}", m.b.restore_backup("home", None, "/home/alice/docs", "casey").unwrap_err());
    assert!(e.contains("was not restored") && e.contains("unable to open repository"), "{e}");
    let asked = m.asked();
    let mut lines = asked.lines();
    assert_eq!(lines.next(), Some("snapshots --json --no-lock latest"), "latest, resolved first: {asked}");
    let restore = lines.next().unwrap();
    assert!(restore.starts_with(&format!("restore {ID} --target ")) && restore.ends_with(" --include /home/alice/docs"), "{restore}");
    // Into the broker's own room, root's alone, not anywhere a person can reach.
    let target = restore.split(' ').nth(3).unwrap();
    assert!(Path::new(target).starts_with(m.dir.path().join("cache/restoring")), "{target}");
    assert!(m.rooms().is_empty(), "the room is gone: {:?}", m.rooms());
    let last = m.b.ledger.read_all().unwrap().pop().unwrap();
    assert_eq!((last.body.kind.as_str(), last.body.approved_by.as_deref(), last.body.tier.as_str()), ("backup_restored", Some("casey"), "system"));
    assert_eq!((last.body.data["backup_snapshot"].as_str(), last.body.data["path"].as_str()), (Some(ID), Some("/home/alice/docs")));
    assert!(last.body.reason.contains("it failed") && last.body.data["error"].as_str().unwrap().contains("unable to open"), "{:?}", last.body);
    assert!(!m.ledger().contains("the key"));
    m.b.ledger.verify().unwrap();
}

/// What restic brought back is checked as a snapshot is: a link on the way is refused, and a folder
/// whose live parent is not there is not made by root. Nothing live is touched; the room goes.
#[test]
fn what_restic_brought_back_is_checked_as_a_snapshot_is() {
    let link = format!("{}\n[ \"$1\" = restore ] && {{ mkdir -p \"$4/home/torq-test-nobody\"; ln -s /etc \"$4/home/torq-test-nobody/docs\"; }}\nexit 0", listing());
    let m = Machine::new(&link);
    m.b.set_backup_key("home", "the key", "casey").unwrap();
    let e = format!("{:#}", m.b.restore_backup("home", Some("4bd1d0c5"), "/home/torq-test-nobody/docs/passwd", "casey").unwrap_err());
    assert!(e.contains("is a link"), "{e}");
    assert!(m.asked().lines().next().unwrap().ends_with("4bd1d0c5"), "the snapshot named: {}", m.asked());
    assert!(m.rooms().is_empty());
    let file = format!("{}\n[ \"$1\" = restore ] && {{ mkdir -p \"$4/home/torq-test-nobody\"; echo back > \"$4/home/torq-test-nobody/a\"; }}\nexit 0", listing());
    let m = Machine::new(&file);
    m.b.set_backup_key("home", "the key", "casey").unwrap();
    let e = format!("{:#}", m.b.restore_backup("home", None, "/home/torq-test-nobody/a", "casey").unwrap_err());
    assert!(e.contains("is not there: restore that first"), "{e}");
    assert!(!Path::new("/home/torq-test-nobody").exists());
    assert!(m.rooms().is_empty());
    assert_eq!(m.b.ledger.read_all().unwrap().pop().unwrap().body.kind, "backup_restored");
}

/// The room is the broker's own folder: one that is a link (or anyone else's) is not written into.
#[test]
fn the_room_is_the_broker_s_own() {
    let m = Machine::new(&format!("{}\n[ \"$1\" = restore ] && mkdir -p \"$4/home\"\nexit 0", listing()));
    m.b.set_backup_key("home", "the key", "casey").unwrap();
    let elsewhere = m.dir.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::create_dir_all(m.dir.path().join("cache")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, m.dir.path().join("cache/restoring")).unwrap();
    let e = format!("{:#}", m.b.restore_backup("home", None, "/home/torq-test-nobody", "casey").unwrap_err());
    assert!(e.contains("not the broker's own folder"), "{e}");
    assert!(!m.asked().contains("restore"), "restic was not asked to write: {}", m.asked());
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
}
