//! End-to-end: propose → apply → rollback against a real git repo, with rebuild stubbed.

use kestrel_broker::{policy::Actor, Broker};
use std::fs;
use std::path::Path;
use std::process::Command;

fn setup(rebuild: &str) -> (tempfile::TempDir, Broker) {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("config");
    fs::create_dir_all(repo.join("users")).unwrap();
    fs::create_dir_all(repo.join("policy")).unwrap();
    fs::write(repo.join("desktop.nix"), "{ nightLight = false; }\n").unwrap();
    fs::write(repo.join("users/casey.nix"), "{ theme = \"dark\"; }\n").unwrap();
    fs::write(repo.join("policy/tiers.toml"), "agent_can_do_anything = false\n").unwrap();
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
        "--- /dev/null\n+++ b/policy/override.toml\n@@ -0,0 +1 @@\n+agent_can_do_anything = true\n",
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

/// A rebuild can fail *after* activating the new system (switch-to-configuration exit 4).
/// The broker must then rebuild the reset config so the running system matches the repo.
#[test]
fn failed_rebuild_restores_previous_system() {
    let dir = tempfile::tempdir().unwrap();
    let calls = dir.path().join("calls.log");
    // Record what each rebuild saw; fail whenever the new setting is present.
    let rebuild = format!(
        "cat {{repo}}/desktop.nix >> {c}; ! grep -q true {{repo}}/desktop.nix",
        c = calls.display()
    );
    let (dir2, b) = setup(&rebuild);
    let d = write_diff(dir2.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose(&d, "activates but fails", Actor::User, false).unwrap();
    let err = b.apply(p.body.seq, "casey", false).unwrap_err();

    let log = fs::read_to_string(&calls).unwrap_or_else(|e| panic!("{e}; apply error: {err:#}"));
    assert_eq!(log, "{ nightLight = true; }\n{ nightLight = false; }\n", "must rebuild the reset config");
    let last = b.ledger.read_all().unwrap().pop().unwrap();
    assert_eq!(last.body.kind, "apply_failed");
    assert_eq!(last.body.data["restored"], true);
}

#[test]
fn failed_rollback_is_ledgered_and_restored() {
    let dir = tempfile::tempdir().unwrap();
    let flag = dir.path().join("fail");
    let rebuild = format!("! test -e {}", flag.display());
    let (dir2, b) = setup(&rebuild);
    let d = write_diff(dir2.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose(&d, "night light", Actor::User, false).unwrap();
    let a = b.apply(p.body.seq, "casey", false).unwrap();
    let head = b.repo.head().unwrap();

    fs::write(&flag, "").unwrap();
    assert!(b.rollback(a.body.seq, "casey").is_err());
    assert_eq!(b.repo.head().unwrap(), head, "config must be reset to the applied state");
    let last = b.ledger.read_all().unwrap().pop().unwrap();
    assert_eq!(last.body.kind, "rollback_failed");
    assert_eq!(last.body.data["restored"], false, "restore rebuild also failed, and must say so");
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

fn assert_agent_refused(b: &Broker, diff: &Path) {
    let err = b.propose(diff, "sneaky", Actor::Agent, false).unwrap_err();
    assert!(err.to_string().contains("forbidden"), "{err:#}");
}

#[test]
fn agent_cannot_move_forbidden_file_away() {
    // numstat reports only the *new* name of a rename; the old (forbidden) path must count too.
    let (dir, b) = setup("true");
    let d = write_diff(
        dir.path(),
        "mv.diff",
        "diff --git a/policy/tiers.toml b/users/junk.nix\nsimilarity index 100%\nrename from policy/tiers.toml\nrename to users/junk.nix\n",
    );
    assert_agent_refused(&b, &d);
}

#[test]
fn agent_cannot_delete_forbidden_file() {
    let (dir, b) = setup("true");
    let d = write_diff(
        dir.path(),
        "rm.diff",
        "diff --git a/policy/tiers.toml b/policy/tiers.toml\ndeleted file mode 100644\n--- a/policy/tiers.toml\n+++ /dev/null\n@@ -1 +0,0 @@\n-agent_can_do_anything = false\n",
    );
    assert_agent_refused(&b, &d);
}

#[test]
fn agent_cannot_hide_forbidden_path_behind_quoting() {
    // git C-quotes unusual names ("policy/a\tb"), which defeats a plain prefix match.
    let (dir, b) = setup("true");
    let d = write_diff(dir.path(), "q.diff", "--- /dev/null\n+++ \"b/policy/a\\tb\"\n@@ -0,0 +1 @@\n+evil\n");
    assert_agent_refused(&b, &d);
}

#[test]
fn relative_diff_path_is_resolved_against_cwd() {
    let (_dir, b) = setup("true");
    let here = tempfile::tempdir_in(".").unwrap();
    write_diff(here.path(), "nl.diff", NIGHT_LIGHT);
    let d = Path::new(here.path().file_name().unwrap()).join("nl.diff"); // relative to the cwd
    b.propose(&d, "relative path", Actor::User, false).unwrap();
}

#[test]
fn conflicting_rollback_leaves_repo_usable() {
    let (dir, b) = setup("true");
    let d1 = write_diff(dir.path(), "a.diff", NIGHT_LIGHT);
    let p1 = b.propose(&d1, "night light", Actor::User, false).unwrap();
    let a1 = b.apply(p1.body.seq, "casey", false).unwrap();
    let d2 = write_diff(
        dir.path(),
        "b.diff",
        "--- a/desktop.nix\n+++ b/desktop.nix\n@@ -1 +1 @@\n-{ nightLight = true; }\n+{ nightLight = \"sunset\"; }\n",
    );
    let p2 = b.propose(&d2, "night light at sunset", Actor::User, false).unwrap();
    b.apply(p2.body.seq, "casey", false).unwrap();

    // Reverting the first change conflicts with the second.
    assert!(b.rollback(a1.body.seq, "casey").is_err());
    b.repo.ensure_clean().expect("a failed revert must not leave the repo dirty");
    assert!(!b.repo.root().join(".git/REVERT_HEAD").exists());
    // ...and the broker keeps working.
    let d3 = write_diff(dir.path(), "c.diff", "--- a/users/casey.nix\n+++ b/users/casey.nix\n@@ -1 +1 @@\n-{ theme = \"dark\"; }\n+{ theme = \"light\"; }\n");
    b.propose(&d3, "light theme", Actor::User, false).unwrap();
}

#[test]
fn stale_lock_file_does_not_block() {
    // A broker killed mid-rebuild (Ctrl-C, power loss) must not wedge every later operation.
    let (dir, b) = setup("true");
    fs::write(dir.path().join("state/broker.lock"), "").unwrap();
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    b.propose(&d, "after a crash", Actor::User, false).unwrap();
}

#[test]
fn concurrent_operation_is_refused() {
    let (dir, b) = setup("true");
    let held = fs::OpenOptions::new().write(true).open(dir.path().join("state/broker.lock")).unwrap();
    held.lock().unwrap();
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let err = b.propose(&d, "racing", Actor::User, false).unwrap_err();
    assert!(err.to_string().contains("another broker operation"), "{err:#}");
}

/// Run the real binary, so a crash is a real crash: the rebuild command SIGKILLs the broker.
fn run_cli(dir: &Path, rebuild: &str, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_kestrel-broker"))
        .arg("--repo").arg(dir.join("config"))
        .arg("--state-dir").arg(dir.join("state"))
        .arg("--rebuild-cmd").arg(rebuild)
        .args(args)
        .output()
        .unwrap()
}

const CRASH: &str = "kill -KILL $PPID";

fn assert_killed(out: &std::process::Output) {
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(out.status.signal(), Some(9), "broker should have been killed mid-rebuild: {out:?}");
}

fn trailers(b: &Broker) -> Vec<String> {
    let log = Command::new("git").arg("-C").arg(b.repo.root()).args(["log", "--format=%B", "main"]).output().unwrap();
    String::from_utf8_lossy(&log.stdout)
        .lines()
        .filter_map(|l| l.strip_prefix("Ledger-Entry: ").map(String::from))
        .collect()
}

/// A broker killed mid-rebuild leaves a commit that was never ledgered. The next operation must
/// discard it (it never completed), rebuild the last ledgered config, and ledger what happened.
#[test]
#[ignore = "needs reconcile (next commit)"]
fn crash_mid_apply_is_reconciled_by_next_operation() {
    let tmp = tempfile::tempdir().unwrap();
    let calls = tmp.path().join("calls.log");
    let (dir, b) = setup(&format!("cat {{repo}}/desktop.nix >> {}", calls.display()));
    let before = b.repo.head().unwrap();
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose(&d, "Night light", Actor::User, false).unwrap();

    assert_killed(&run_cli(dir.path(), CRASH, &["apply", &p.body.seq.to_string(), "--approved-by", "casey"]));
    let orphan = b.repo.head().unwrap();
    assert_ne!(orphan, before, "the crash window leaves an unledgered commit behind");

    let a = b.apply(p.body.seq, "casey", false).unwrap();

    let entries = b.ledger.read_all().unwrap();
    let kinds: Vec<_> = entries.iter().map(|e| e.body.kind.as_str()).collect();
    assert_eq!(kinds, ["init", "proposal", "interrupted", "applied"]);
    let i = &entries[2];
    assert_eq!(i.body.data["discarded_commit"], orphan.as_str());
    assert_eq!(i.body.data["restored"], true);
    // The orphan's trailer named the entry that now explains it.
    assert_eq!(i.body.seq, 3);
    assert_eq!(
        fs::read_to_string(&calls).unwrap(),
        "{ nightLight = false; }\n{ nightLight = true; }\n",
        "reconcile must rebuild the last ledgered config before the re-apply"
    );
    assert!(fs::read_to_string(b.repo.root().join("desktop.nix")).unwrap().contains("true"));
    assert_eq!(trailers(&b), [a.body.seq.to_string(), "init".to_string()], "one commit per trailer");
    b.ledger.verify().unwrap();
}

#[test]
#[ignore = "needs reconcile (next commit)"]
fn crash_mid_rollback_is_reconciled_by_next_operation() {
    let (dir, b) = setup("true");
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose(&d, "Night light", Actor::User, false).unwrap();
    let a = b.apply(p.body.seq, "casey", false).unwrap();
    let applied_head = b.repo.head().unwrap();

    assert_killed(&run_cli(dir.path(), CRASH, &["rollback", &a.body.seq.to_string(), "--approved-by", "casey"]));
    assert_ne!(b.repo.head().unwrap(), applied_head);

    // Reconcile puts the applied change back; the user can then undo it for real.
    b.rollback(a.body.seq, "casey").unwrap();
    let kinds: Vec<_> = b.ledger.read_all().unwrap().into_iter().map(|e| e.body.kind).collect();
    assert_eq!(kinds, ["init", "proposal", "applied", "interrupted", "rolled_back"]);
    assert!(fs::read_to_string(b.repo.root().join("desktop.nix")).unwrap().contains("false"));
    b.ledger.verify().unwrap();
}

/// Killing the broker does not kill its rebuild: `nixos-rebuild` carries on and may still switch
/// the system. The rebuild's process tree must therefore keep holding the broker lock, so the next
/// operation waits for it instead of racing it.
#[test]
fn rebuild_outliving_a_killed_broker_keeps_the_lock() {
    use std::os::unix::process::ExitStatusExt;
    let (dir, b) = setup("true");
    let go = dir.path().join("go");
    let done = dir.path().join("late-rebuild-finished");
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose(&d, "Night light", Actor::User, false).unwrap();

    // Kill the broker, then keep "rebuilding" (this shell survives its parent) until told to stop.
    let rebuild = format!(
        "kill -KILL $PPID; while [ ! -e {go} ]; do sleep 0.05; done; touch {done}",
        go = go.display(),
        done = done.display()
    );
    let status = Command::new(env!("CARGO_BIN_EXE_kestrel-broker"))
        .arg("--repo").arg(dir.path().join("config"))
        .arg("--state-dir").arg(dir.path().join("state"))
        .arg("--rebuild-cmd").arg(&rebuild)
        .args(["apply", &p.body.seq.to_string(), "--approved-by", "casey"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert_eq!(status.signal(), Some(9));
    assert!(!done.exists(), "the rebuild should still be running");

    // propose takes the lock but never rebuilds, so the probe is about the lock only.
    let lock_error = |b: &Broker| b.propose(&d, "probe", Actor::User, false).err().map(|e| format!("{e:#}"));
    let locked = |b: &Broker| lock_error(b).is_some_and(|e| e.contains("another broker operation is running"));
    let err = lock_error(&b).unwrap();
    assert!(err.contains("another broker operation is running"), "the surviving rebuild must still hold the broker lock: {err}");
    assert!(!err.contains("held by: unknown"), "the error must name the process holding the lock: {err}");
    fs::write(&go, "").unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while locked(&b) {
        assert!(std::time::Instant::now() < deadline, "lock never released");
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(done.exists(), "the lock must be held until the rebuild has finished");
}
