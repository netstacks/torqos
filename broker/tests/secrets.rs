//! The values of the secrets (ADR-052): kept encrypted outside the repository, root's alone, the system's and a
//! person's apart, and on the record without the value. systemd-creds is stood in for by a script that takes its
//! arguments and binds each value to its name as the real one does; tests/secrets.nix runs the real one.

// No unsafe code and no dead code, ever (docs/DECISIONS.md ADR-020).
#![forbid(unsafe_code)]
#![forbid(dead_code)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use torq_broker::secrets::{Kept, Owner};
use torq_broker::Broker;

const VALUE: &str = "ghp_correct-horse-battery-staple";

/// systemd-creds' arguments, and a value bound to its `--name`: one moved to another name does not decrypt.
const CREDS: &str = r#"#!/bin/sh
cmd=$1; shift
name=""; files=""
for a; do case $a in --name=*) name=${a#--name=};; --*) ;; *) files="$files $a";; esac; done
set -- $files
case $cmd in
  encrypt) [ -n "$FAIL" ] && { echo "no TPM" >&2; exit 1; }; { printf 'FAKE %s\n' "$name"; base64; } > "$2" ;;
  decrypt) [ "$(head -n 1 "$1")" = "FAKE $name" ] || { echo "name mismatch" >&2; exit 1; }; tail -n +2 "$1" | base64 -d ;;
  *) exit 2 ;;
esac
"#;

struct Machine {
    dir: tempfile::TempDir,
    b: Broker,
}

impl Machine {
    fn new() -> Self {
        Self::with_creds(CREDS)
    }
    fn with_creds(script: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("config");
        fs::create_dir_all(&repo).unwrap();
        fs::write(repo.join("desktop.nix"), "{ }\n").unwrap();
        let creds = dir.path().join("creds");
        fs::write(&creds, script).unwrap();
        fs::set_permissions(&creds, fs::Permissions::from_mode(0o755)).unwrap();
        let b = Broker::new(&repo, dir.path().join("state"), "true").with_secrets(dir.path().join("store"), &creds.to_string_lossy());
        b.init().unwrap();
        Machine { dir, b }
    }
    fn store(&self) -> PathBuf {
        self.dir.path().join("store")
    }
    fn ledger(&self) -> String {
        fs::read_to_string(self.b.ledger.path()).unwrap()
    }
}

fn alice() -> Owner {
    Owner::Person("alice".into())
}

#[test]
fn a_value_is_kept_encrypted_private_and_off_the_record() {
    let m = Machine::new();
    let e = m.b.set_secret(&Owner::System, "github-ci", VALUE, "casey").unwrap();
    assert_eq!(e.body.kind, "secret_set");
    assert_eq!((e.body.tier.as_str(), e.body.approved_by.as_deref()), ("system", Some("casey")));
    let file = m.store().join("system/github-ci");
    let kept = fs::read_to_string(&file).unwrap();
    assert!(!kept.contains(VALUE), "kept as systemd-creds made it, never as it was given");
    assert_eq!(fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(fs::metadata(m.store().join("system")).unwrap().permissions().mode() & 0o777, 0o700);
    let (shown, value) = m.b.show_secret(&Owner::System, "github-ci", "casey").unwrap();
    assert_eq!((shown.body.kind.as_str(), value.as_str()), ("secret_shown", VALUE));
    m.b.delete_secret(&Owner::System, "github-ci", "casey").unwrap();
    assert!(!file.exists());
    let ledger = m.ledger();
    assert!(!ledger.contains(VALUE), "no value on the record");
    assert!(ledger.contains("secret_set") && ledger.contains("secret_shown") && ledger.contains("secret_deleted"));
}

#[test]
fn a_persons_secret_is_kept_apart_and_bound_to_its_name() {
    let m = Machine::new();
    m.b.set_secret(&alice(), "github", "alice's", "alice").unwrap();
    m.b.set_secret(&Owner::System, "github", "the system's", "casey").unwrap();
    let e = m.b.set_secret(&alice(), "deploy", "key", "alice").unwrap();
    assert_eq!((e.body.tier.as_str(), e.body.data["user"].as_str()), ("user", Some("alice")));
    assert_eq!(m.b.show_secret(&alice(), "github", "alice").unwrap().1, "alice's");
    assert_eq!(m.b.show_secret(&Owner::System, "github", "casey").unwrap().1, "the system's");
    assert_eq!(
        m.b.kept_secrets(),
        vec![
            Kept { owner: Owner::System, name: "github".into() },
            Kept { owner: alice(), name: "deploy".into() },
            Kept { owner: alice(), name: "github".into() },
        ]
    );
    // A value put where another's would be is not that one's: it does not decrypt under another name.
    fs::copy(m.store().join("users/alice/github"), m.store().join("system/github")).unwrap();
    let err = format!("{:#}", m.b.show_secret(&Owner::System, "github", "casey").unwrap_err());
    assert!(err.contains("could not decrypt"), "{err}");
}

#[test]
fn a_new_value_takes_the_old_ones_place() {
    let m = Machine::new();
    m.b.set_secret(&alice(), "t", "one", "alice").unwrap();
    m.b.set_secret(&alice(), "t", "two", "alice").unwrap();
    assert_eq!(m.b.show_secret(&alice(), "t", "alice").unwrap().1, "two");
    let left: Vec<_> = fs::read_dir(m.store().join("users/alice")).unwrap().flatten().map(|e| e.file_name()).collect();
    assert_eq!(left, vec![std::ffi::OsString::from("t")], "no part-written file left beside it");
}

#[test]
fn what_is_refused_is_not_recorded() {
    let m = Machine::new();
    let before = m.ledger();
    for bad in ["", "../x", "a/b", "-x", ".x"] {
        assert!(m.b.set_secret(&Owner::System, bad, VALUE, "casey").is_err(), "{bad:?}");
    }
    assert!(m.b.set_secret(&Owner::System, "t", "", "casey").is_err(), "no value");
    assert!(m.b.set_secret(&Owner::System, "t", &"x".repeat(16 * 1024 + 1), "casey").is_err(), "too large");
    assert!(m.b.set_secret(&Owner::System, "t", "a\0b", "casey").is_err(), "a NUL");
    assert!(m.b.delete_secret(&Owner::System, "none", "casey").is_err(), "nothing to delete");
    assert!(m.b.show_secret(&alice(), "none", "alice").is_err(), "nothing to show");
    assert!(Owner::of(Some("../root")).is_err() && Owner::of(Some("Alice")).is_err());
    assert_eq!(m.ledger(), before);
    assert!(m.b.kept_secrets().is_empty());
}

#[test]
fn a_value_not_kept_says_so_and_leaves_nothing() {
    let m = Machine::with_creds(&CREDS.replace("[ -n \"$FAIL\" ]", "true"));
    let err = format!("{:#}", m.b.set_secret(&Owner::System, "t", VALUE, "casey").unwrap_err());
    assert!(err.contains("not kept") && err.contains("no TPM"), "{err}");
    let ledger = m.ledger();
    assert!(ledger.contains("secret_set_failed") && !ledger.contains(VALUE), "{ledger}");
    assert_eq!(fs::read_dir(m.store().join("system")).unwrap().count(), 0, "no part-written file");
    assert!(m.b.kept_secrets().is_empty());
}
