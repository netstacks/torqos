//! End-to-end: propose → apply → rollback against a real git repo, with rebuild stubbed.

use kestrel_broker::{policy::Actor, Broker};
use std::fs;
use std::path::Path;
use std::process::Command;

fn setup(rebuild: &str) -> (tempfile::TempDir, Broker) {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("config");
    fs::create_dir_all(repo.join("users")).unwrap();
    fs::write(repo.join("desktop.nix"), "{ nightLight = false; }\n").unwrap();
    fs::write(repo.join("users/casey.nix"), "{ theme = \"dark\"; }\n").unwrap();
    let b = Broker::new(&repo, dir.path().join("state"), rebuild);
    b.init().unwrap();
    (dir, b)
}

fn write_diff(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
    let p = dir.join(name);
    fs::write(&p, body).unwrap();
    p
}

const NIGHT_LIGHT: &str = "--- a/desktop.nix\n+++ b/desktop.nix\n@@ -1 +1 @@\n-{ nightLight = false; }\n+{ nightLight = true; }\n";

#[test]
fn propose_apply_rollback() {
    let (dir, b) = setup("true");
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose(&d, "Night light at sunset", Actor::Agent, false).unwrap();
    assert_eq!(p.body.tier, "system");

    let a = b.apply(p.body.seq, "casey", false).unwrap();
    let cfg = b.repo.root().join("desktop.nix");
    assert!(fs::read_to_string(&cfg).unwrap().contains("true"));

    // Commit message carries the ledger trailer pointing at the applied entry.
    let log = Command::new("git").arg("-C").arg(b.repo.root()).args(["log", "-1", "--format=%B"]).output().unwrap();
    let msg = String::from_utf8_lossy(&log.stdout);
    assert!(msg.contains(&format!("Ledger-Entry: {}", a.body.seq)), "{msg}");

    assert!(b.apply(p.body.seq, "casey", false).is_err(), "double apply must fail");

    b.rollback(a.body.seq, "casey").unwrap();
    assert!(fs::read_to_string(&cfg).unwrap().contains("false"));
    assert!(b.rollback(a.body.seq, "casey").is_err(), "double rollback must fail");

    assert_eq!(b.ledger.verify().unwrap(), 4); // init, proposal, applied, rolled_back
}

#[test]
fn agent_cannot_touch_policy() {
    let (dir, b) = setup("true");
    let d = write_diff(
        dir.path(),
        "p.diff",
        "--- /dev/null\n+++ b/policy/tiers.toml\n@@ -0,0 +1 @@\n+agent_can_do_anything = true\n",
    );
    let err = b.propose(&d, "loosen my own permissions", Actor::Agent, false).unwrap_err();
    assert!(err.to_string().contains("forbidden"), "{err}");
}

#[test]
fn tainted_needs_explicit_confirmation() {
    let (dir, b) = setup("true");
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose(&d, "a web page suggested this", Actor::Agent, true).unwrap();
    assert!(b.apply(p.body.seq, "casey", false).is_err());
    b.apply(p.body.seq, "casey", true).unwrap();
}

#[test]
fn failed_rebuild_resets_config() {
    let (dir, b) = setup("false");
    let before = b.repo.head().unwrap();
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose(&d, "will fail to build", Actor::User, false).unwrap();
    assert!(b.apply(p.body.seq, "casey", false).is_err());
    assert_eq!(b.repo.head().unwrap(), before);
    assert!(fs::read_to_string(b.repo.root().join("desktop.nix")).unwrap().contains("false"));
    let last = b.ledger.read_all().unwrap().pop().unwrap();
    assert_eq!(last.body.kind, "apply_failed");
    b.ledger.verify().unwrap();
}

#[test]
fn user_tier_detected() {
    let (dir, b) = setup("true");
    let d = write_diff(
        dir.path(),
        "t.diff",
        "--- a/users/casey.nix\n+++ b/users/casey.nix\n@@ -1 +1 @@\n-{ theme = \"dark\"; }\n+{ theme = \"light\"; }\n",
    );
    let p = b.propose(&d, "light theme", Actor::Agent, false).unwrap();
    assert_eq!(p.body.tier, "user");
}
