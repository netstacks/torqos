//! End-to-end: propose → apply → rollback against a real git repo, with rebuild stubbed.

use kestrel_broker::{policy::Actor, Broker};
use std::fs;
use std::path::Path;
use std::process::Command;

/// A config repo with a few files, not yet initialised.
fn config_dir() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("config");
    fs::create_dir_all(repo.join("users")).unwrap();
    fs::create_dir_all(repo.join("policy")).unwrap();
    fs::write(repo.join("desktop.nix"), "{ nightLight = false; }\n").unwrap();
    fs::write(repo.join("users/casey.nix"), "{ theme = \"dark\"; }\n").unwrap();
    fs::write(repo.join("policy/tiers.toml"), "agent_can_do_anything = false\n").unwrap();
    (dir, repo)
}

fn setup(rebuild: &str) -> (tempfile::TempDir, Broker) {
    let (dir, repo) = config_dir();
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
        "--- /dev/null\n+++ b/users/casey.packages.json\n@@ -0,0 +1,3 @@\n+[\n+  \"htop\"\n+]\n",
    );
    let p = b.propose(&d, "install htop", Actor::Agent, false).unwrap();
    assert_eq!(p.body.tier, "user");
}

/// `users/*.nix` is a full NixOS module (sudo rules, services, ...): changing it must never be
/// mistaken for a user-tier change just because it lives under `users/`.
#[test]
fn user_module_is_system_tier() {
    let (dir, b) = setup("true");
    let d = write_diff(dir.path(), "t.diff", THEME);
    let p = b.propose(&d, "light theme", Actor::Agent, false).unwrap();
    assert_eq!(p.body.tier, "system");
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
    // The orphan's trailer named the entry that now explains it, and it is kept, not lost to gc.
    assert_eq!(i.body.seq, 3);
    assert!(git(&b, &["log", "-1", "--format=%B", &orphan]).contains("Ledger-Entry: 3"));
    assert_eq!(git(&b, &["rev-parse", &format!("refs/kestrel/discarded/{orphan}")]).trim(), orphan);
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
fn crash_mid_rollback_is_reconciled_by_next_operation() {
    let tmp = tempfile::tempdir().unwrap();
    let calls = tmp.path().join("calls.log");
    let (dir, b) = setup(&format!("cat {{repo}}/desktop.nix >> {}", calls.display()));
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose(&d, "Night light", Actor::User, false).unwrap();
    let a = b.apply(p.body.seq, "casey", false).unwrap();
    let applied_head = b.repo.head().unwrap();

    assert_killed(&run_cli(dir.path(), CRASH, &["rollback", &a.body.seq.to_string(), "--approved-by", "casey"]));
    let orphan = b.repo.head().unwrap();
    assert_ne!(orphan, applied_head);

    // Reconcile puts the applied change back; the user can then undo it for real.
    b.rollback(a.body.seq, "casey").unwrap();
    let kinds: Vec<_> = b.ledger.read_all().unwrap().into_iter().map(|e| e.body.kind).collect();
    assert_eq!(kinds, ["init", "proposal", "applied", "interrupted", "rolled_back"]);
    let i = &b.ledger.read_all().unwrap()[3];
    assert_eq!(i.body.data["discarded_commit"], orphan.as_str());
    assert_eq!(i.body.data["commit"], applied_head.as_str());
    assert_eq!(git(&b, &["rev-parse", &format!("refs/kestrel/discarded/{orphan}")]).trim(), orphan);
    assert_eq!(i.body.data["restored"], true);
    assert_eq!(
        fs::read_to_string(&calls).unwrap(),
        "{ nightLight = true; }\n{ nightLight = true; }\n{ nightLight = false; }\n",
        "apply, then reconcile rebuilds the applied state, then the real rollback"
    );
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
        "kill -KILL $PPID; i=0; while [ ! -e {go} ] && [ $i -lt 600 ]; do sleep 0.05; i=$((i+1)); done; touch {done}",
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
    assert!(err.contains("(sh)"), "the error must name the surviving rebuild shell: {err}");
    fs::write(&go, "").unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while locked(&b) {
        assert!(std::time::Instant::now() < deadline, "lock never released");
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(done.exists(), "the lock must be held until the rebuild has finished");
}

fn git(b: &Broker, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(b.repo.root()).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn kinds(b: &Broker) -> Vec<String> {
    b.ledger.read_all().unwrap().into_iter().map(|e| e.body.kind).collect()
}

const THEME: &str = "--- a/users/casey.nix\n+++ b/users/casey.nix\n@@ -1 +1 @@\n-{ theme = \"dark\"; }\n+{ theme = \"light\"; }\n";

/// propose is the agent's entry point and must not start a rebuild nobody approved: it refuses
/// until the interruption is reconciled, which the `reconcile` subcommand does.
#[test]
fn propose_refuses_until_reconcile_runs() {
    let (dir, b) = setup("true");
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose(&d, "Night light", Actor::User, false).unwrap();
    assert_killed(&run_cli(dir.path(), CRASH, &["apply", &p.body.seq.to_string(), "--approved-by", "casey"]));

    let t = write_diff(dir.path(), "theme.diff", THEME);
    let err = format!("{:#}", b.propose(&t, "Light theme", Actor::Agent, false).unwrap_err());
    assert!(err.contains("kestrel-broker reconcile"), "{err}");

    let out = run_cli(dir.path(), "true", &["reconcile", "--approved-by", "casey"]);
    assert!(out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("Interrupted apply of #2"), "{out:?}");
    let out = run_cli(dir.path(), "true", &["reconcile", "--approved-by", "casey"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("nothing to reconcile"), "{out:?}");

    b.propose(&t, "Light theme", Actor::Agent, false).unwrap();
    assert_eq!(kinds(&b), ["init", "proposal", "interrupted", "proposal"]);
    b.ledger.verify().unwrap();
}

/// If reconcile's own rebuild fails, that is ledgered and reported — and not retried forever.
#[test]
fn failed_reconcile_rebuild_is_ledgered_and_reported() {
    let (dir, b) = setup("true");
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose(&d, "Night light", Actor::User, false).unwrap();
    assert_killed(&run_cli(dir.path(), CRASH, &["apply", &p.body.seq.to_string(), "--approved-by", "casey"]));

    let failing = Broker::new(dir.path().join("config"), dir.path().join("state"), "false");
    let err = format!("{:#}", failing.reconcile("casey").unwrap_err());
    assert!(err.contains("restore rebuild failed"), "{err}");
    let i = b.ledger.read_all().unwrap().pop().unwrap();
    assert_eq!(i.body.kind, "interrupted");
    assert_eq!(i.body.data["restored"], false);
    assert!(b.reconcile("casey").unwrap().is_none(), "already reconciled");
    assert!(fs::read_to_string(b.repo.root().join("desktop.nix")).unwrap().contains("false"));
}

fn write_pending(dir: &Path, pending: serde_json::Value) {
    fs::write(dir.join("state/pending.json"), pending.to_string()).unwrap();
}

/// The broker died after ledgering the outcome but before removing pending.json: the change was
/// applied for real and must not be undone.
#[test]
fn ledgered_outcome_is_not_undone() {
    let (dir, b) = setup("true");
    let before = b.repo.head().unwrap();
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose(&d, "Night light", Actor::User, false).unwrap();
    b.apply(p.body.seq, "casey", false).unwrap();
    let head = b.repo.head().unwrap();
    let stale = serde_json::json!({
        "op": "apply", "target": p.body.seq, "before": before, "ledger_len": 2, "commit": head,
        "approved_by": "casey", "tier": "system", "tainted": false,
    });

    // Both paths that settle a record: reconcile (apply, rollback, the CLI) and propose/init.
    write_pending(dir.path(), stale.clone());
    assert!(b.reconcile("casey").unwrap().is_none());
    assert_eq!(b.repo.head().unwrap(), head);
    assert!(!dir.path().join("state/pending.json").exists());

    write_pending(dir.path(), stale);
    let t = write_diff(dir.path(), "theme.diff", THEME);
    b.propose(&t, "Light theme", Actor::User, false).unwrap();
    assert_eq!(b.repo.head().unwrap(), head);
    assert_eq!(kinds(&b), ["init", "proposal", "applied", "proposal"]);
    assert!(!dir.path().join("state/pending.json").exists());
}

/// Died after recording the operation but before committing: nothing to discard, but it still
/// rebuilds (it cannot tell this apart from dying inside a restore).
#[test]
fn crash_before_commit_still_rebuilds() {
    let tmp = tempfile::tempdir().unwrap();
    let calls = tmp.path().join("calls.log");
    let (dir, b) = setup(&format!("cat {{repo}}/desktop.nix >> {}", calls.display()));
    let before = b.repo.head().unwrap();
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose(&d, "Night light", Actor::User, false).unwrap();
    write_pending(dir.path(), serde_json::json!({
        "op": "apply", "target": p.body.seq, "before": before, "ledger_len": 2, "commit": null,
        "approved_by": "casey", "tier": "system", "tainted": false,
    }));

    let i = b.reconcile("casey").unwrap().expect("reconciled");
    assert_eq!(i.body.data["discarded_commit"], serde_json::Value::Null);
    assert_eq!(i.body.data["restored"], true);
    assert_eq!(fs::read_to_string(&calls).unwrap(), "{ nightLight = false; }\n");
    assert_eq!(b.repo.head().unwrap(), before);
}

/// `git apply --index` succeeds but `git commit` fails: the repo must be put back, not left dirty
/// (which would fail every later operation's clean check).
#[test]
fn failed_commit_leaves_repo_clean() {
    let (dir, b) = setup("true");
    let before = b.repo.head().unwrap();
    let hook = b.repo.root().join(".git/hooks/pre-commit");
    fs::create_dir_all(hook.parent().unwrap()).unwrap();
    fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
    fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose(&d, "Night light", Actor::User, false).unwrap();
    assert!(b.apply(p.body.seq, "casey", false).is_err());
    assert_eq!(b.repo.head().unwrap(), before);
    b.repo.ensure_clean().unwrap();
    assert!(!dir.path().join("state/pending.json").exists());

    fs::remove_file(&hook).unwrap();
    b.apply(p.body.seq, "casey", false).unwrap();
    assert_eq!(kinds(&b), ["init", "proposal", "applied"]);
}

/// Crash after an apply, then hand-written state, for reconcile's own guards.
fn crashed_apply() -> (tempfile::TempDir, Broker, String) {
    let (dir, b) = setup("true");
    let before = b.repo.head().unwrap();
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose(&d, "Night light", Actor::User, false).unwrap();
    assert_killed(&run_cli(dir.path(), CRASH, &["apply", &p.body.seq.to_string(), "--approved-by", "casey"]));
    (dir, b, before)
}

/// The commit exists but the broker died before recording it: reconcile must still record and keep it.
#[test]
fn unrecorded_commit_is_still_kept() {
    let (dir, b) = setup("true");
    let before = b.repo.head().unwrap();
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose(&d, "Night light", Actor::User, false).unwrap();
    git(&b, &["apply", "--index", d.to_str().unwrap()]);
    git(&b, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "half-done apply"]);
    let orphan = b.repo.head().unwrap();
    write_pending(dir.path(), serde_json::json!({
        "op": "apply", "target": p.body.seq, "before": before, "ledger_len": 2, "commit": null,
        "approved_by": "casey", "tier": "system", "tainted": false,
    }));

    let i = b.reconcile("casey").unwrap().expect("reconciled");
    assert_eq!(i.body.data["discarded_commit"], orphan.as_str());
    assert_eq!(git(&b, &["rev-parse", &format!("refs/kestrel/discarded/{orphan}")]).trim(), orphan);
    assert_eq!(b.repo.head().unwrap(), before);
}

/// HEAD does not descend from the recorded commit: something else rewrote the repo. Refuse
/// rather than reset to a commit that is not in its history.
#[test]
fn reconcile_refuses_when_before_is_not_an_ancestor() {
    let (dir, b, _) = crashed_apply();
    let head = b.repo.head().unwrap();
    let tree = git(&b, &["rev-parse", "HEAD^{tree}"]);
    let unrelated = git(&b, &["-c", "user.name=t", "-c", "user.email=t@t", "commit-tree", tree.trim(), "-m", "unrelated"]);
    let mut p: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.path().join("state/pending.json")).unwrap()).unwrap();
    p["before"] = unrelated.trim().into();
    write_pending(dir.path(), p);

    let err = format!("{:#}", b.reconcile("casey").unwrap_err());
    assert!(err.contains("does not descend"), "{err}");
    assert_eq!(b.repo.head().unwrap(), head, "must not reset");
    assert!(dir.path().join("state/pending.json").exists());
}

#[test]
fn init_refuses_until_reconciled() {
    let (_dir, b, _) = crashed_apply();
    let err = format!("{:#}", b.init().unwrap_err());
    assert!(err.contains("kestrel-broker reconcile"), "{err}");
}

#[test]
fn failed_rollback_commit_leaves_repo_clean() {
    let (dir, b) = setup("true");
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose(&d, "Night light", Actor::User, false).unwrap();
    let a = b.apply(p.body.seq, "casey", false).unwrap();
    let applied = b.repo.head().unwrap();
    let hook = b.repo.root().join(".git/hooks/pre-commit");
    fs::create_dir_all(hook.parent().unwrap()).unwrap();
    fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
    fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

    assert!(b.rollback(a.body.seq, "casey").is_err());
    assert_eq!(b.repo.head().unwrap(), applied);
    b.repo.ensure_clean().unwrap();
    assert!(!dir.path().join("state/pending.json").exists());
}

/// An edit made in the repo outside the broker would be destroyed by the reset: keep it.
#[test]
fn reconcile_keeps_uncommitted_changes() {
    let (_dir, b, _) = crashed_apply();
    fs::write(b.repo.root().join("stray.nix"), "{ edited = \"by hand\"; }\n").unwrap();

    let i = b.reconcile("casey").unwrap().expect("reconciled");
    let kept = i.body.data["discarded_uncommitted"].as_str().expect("snapshot recorded").to_string();
    assert_eq!(git(&b, &["show", &format!("{kept}:stray.nix")]), "{ edited = \"by hand\"; }\n");
    assert_eq!(git(&b, &["rev-parse", &format!("refs/kestrel/discarded/{kept}")]).trim(), kept);
    assert_eq!(i.body.data["reconciled_by"], "casey");
    b.repo.ensure_clean().unwrap();
}

/// On a broken chain the final append would fail after a full reset and rebuild, on every retry.
#[test]
fn reconcile_refuses_on_a_broken_ledger_before_touching_anything() {
    let (dir, b, _) = crashed_apply();
    let head = b.repo.head().unwrap();
    let ledger = dir.path().join("state/ledger.jsonl");
    fs::write(&ledger, fs::read_to_string(&ledger).unwrap().replace("Night light", "Day light")).unwrap();
    let calls = dir.path().join("calls.log");
    let logging = Broker::new(dir.path().join("config"), dir.path().join("state"), format!("touch {}", calls.display()));

    let err = format!("{:#}", logging.reconcile("casey").unwrap_err());
    assert!(err.contains("broken ledger"), "{err}");
    assert_eq!(b.repo.head().unwrap(), head);
    assert!(!calls.exists(), "must not rebuild");
}

/// git children do not hold the broker lock, so an index.lock may belong to a git that is still
/// running: only a lock nobody has open may be removed.
#[test]
fn live_index_lock_is_refused_stale_one_removed() {
    let (_dir, b, _) = crashed_apply();
    let index_lock = b.repo.index_lock();
    fs::write(&index_lock, "").unwrap();
    let ready = b.repo.root().parent().unwrap().join("holder-ready");
    // Open the lock, say so, then become `sleep` (exec: one process, so killing it releases the fd).
    let mut holder = Command::new("sh")
        .arg("-c")
        .arg("exec 3<\"$0\"; : > \"$1\"; exec sleep 30")
        .arg(&index_lock)
        .arg(&ready)
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !ready.exists() {
        assert!(std::time::Instant::now() < deadline, "holder never opened the lock");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let err = format!("{:#}", b.reconcile("casey").unwrap_err());
    holder.kill().unwrap();
    holder.wait().unwrap();
    assert!(err.contains("git is still running") && err.contains("(sleep)"), "{err}");
    assert!(index_lock.exists());

    b.reconcile("casey").unwrap().expect("stale lock removed, reconciled");
    assert!(!index_lock.exists());
}

/// A broker whose "running system" is a symlink the test controls, like /run/current-system.
fn setup_with_system() -> (tempfile::TempDir, Broker, std::path::PathBuf) {
    let (dir, repo) = config_dir();
    let link = dir.path().join("current-system");
    std::os::unix::fs::symlink("/nix/store/aaa-system-1", &link).unwrap();
    let b = Broker::new(&repo, dir.path().join("state"), "true")
        .with_current_system(&link)
        .with_system_profile(dir.path().join("no-profile")); // not this machine's real one
    b.init().unwrap(); // records the fake system as the baseline, as on a real machine
    (dir, b, link)
}

fn switch_to(link: &Path, target: &str) {
    fs::remove_file(link).unwrap();
    std::os::unix::fs::symlink(target, link).unwrap();
}

/// Every change records the system it switched to, so drift from the ledger can be detected.
#[test]
fn changes_record_the_running_system() {
    let (dir, b, link) = setup_with_system();
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose(&d, "Night light", Actor::User, false).unwrap();
    let a = b.apply(p.body.seq, "casey", false).unwrap();
    assert_eq!(a.body.data["system"], "/nix/store/aaa-system-1");
    switch_to(&link, "/nix/store/bbb-system-2");
    let r = b.rollback(a.body.seq, "casey").unwrap();
    assert_eq!(r.body.data["system"], "/nix/store/bbb-system-2");
}

/// Something switched the machine outside the broker (today's incident): status says so.
#[test]
fn status_detects_a_system_switched_outside_the_broker() {
    let (dir, b, link) = setup_with_system();
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose(&d, "Night light", Actor::User, false).unwrap();
    b.apply(p.body.seq, "casey", false).unwrap();
    let s = b.status().unwrap();
    assert!(s.problems().is_empty(), "{:?}", s.problems());

    switch_to(&link, "/nix/store/zzz-nixos-system-nixos");
    let s = b.status().unwrap();
    let problems = s.problems().join("\n");
    assert!(problems.contains("running system") && problems.contains("zzz-nixos-system-nixos"), "{problems}");
}

/// A commit in the config repo that the ledger does not know about.
#[test]
fn status_detects_an_unledgered_commit() {
    let (dir, b, _) = setup_with_system();
    fs::write(b.repo.root().join("desktop.nix"), "{ nightLight = true; }\n").unwrap();
    git(&b, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qam", "edited behind the broker's back"]);
    let problems = b.status().unwrap().problems().join("\n");
    assert!(problems.contains("config repo"), "{problems}");
    let _ = dir;
}

#[test]
fn status_reports_an_interrupted_operation_and_a_broken_ledger() {
    let (dir, b, _) = crashed_apply();
    let problems = b.status().unwrap().problems().join("\n");
    assert!(problems.contains("interrupted"), "{problems}");
    let ledger = dir.path().join("state/ledger.jsonl");
    fs::write(&ledger, fs::read_to_string(&ledger).unwrap().replace("Night light", "Day light")).unwrap();
    let problems = b.status().unwrap().problems().join("\n");
    assert!(problems.contains("ledger"), "{problems}");
}

/// Mid-apply, the system has switched but the ledger entry is not written yet: comparing then would
/// report drift that is not there. status must say an operation is running instead.
#[test]
fn status_does_not_compare_during_an_operation() {
    let (dir, b, link) = setup_with_system();
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose(&d, "Night light", Actor::User, false).unwrap();
    b.apply(p.body.seq, "casey", false).unwrap();
    switch_to(&link, "/nix/store/ccc-being-switched-to");
    let held = fs::OpenOptions::new().write(true).open(dir.path().join("state/broker.lock")).unwrap();
    held.lock().unwrap();

    let s = b.status().unwrap();
    assert!(s.busy.is_some(), "{s:?}");
    assert!(s.problems().is_empty(), "{:?}", s.problems());
    drop(held);
    assert!(!b.status().unwrap().problems().is_empty(), "once the lock is free, the drift is reported");
}

/// A rebuild that failed and could not restore leaves the machine in an unknown state; status must
/// say so even though it records whatever is running then.
#[test]
fn status_reports_a_failed_restore() {
    let (dir, _, link) = setup_with_system();
    let b = Broker::new(dir.path().join("config"), dir.path().join("state"), "false")
        .with_current_system(&link)
        .with_system_profile(dir.path().join("no-profile"));
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose(&d, "Night light", Actor::User, false).unwrap();
    assert!(b.apply(p.body.seq, "casey", false).is_err());
    let problems = b.status().unwrap().problems().join("\n");
    assert!(problems.contains("could not restore"), "{problems}");
}

/// init records the running system, so drift is detectable from the first boot, not only after
/// the first change.
#[test]
fn init_records_the_running_system() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("config");
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("flake.nix"), "{ }\n").unwrap();
    let link = dir.path().join("current-system");
    std::os::unix::fs::symlink("/nix/store/aaa-system-1", &link).unwrap();
    let b = Broker::new(&repo, dir.path().join("state"), "true")
        .with_current_system(&link)
        .with_system_profile(dir.path().join("no-profile"));
    b.init().unwrap();
    assert_eq!(b.ledger.read_all().unwrap()[0].body.data["system"], "/nix/store/aaa-system-1");
    switch_to(&link, "/nix/store/zzz-elsewhere");
    assert!(b.status().unwrap().problems().join("\n").contains("running system"));
}

#[test]
fn status_reports_an_unreadable_or_dirty_repo() {
    let (_dir, b, _) = setup_with_system();
    fs::write(b.repo.root().join("stray.nix"), "{ }\n").unwrap();
    assert!(b.status().unwrap().problems().join("\n").contains("uncommitted"));
    fs::remove_dir_all(b.repo.root().join(".git")).unwrap();
    assert!(b.status().unwrap().problems().join("\n").contains("cannot be read"));
}

/// `nixos-rebuild boot` / `nix-env --set` outside the broker change only the next boot.
#[test]
fn status_reports_a_next_boot_changed_outside_the_broker() {
    let (dir, b, _) = setup_with_system();
    let profiles = dir.path().join("profiles");
    fs::create_dir_all(&profiles).unwrap();
    std::os::unix::fs::symlink("/nix/store/aaa-system-1", profiles.join("system-1-link")).unwrap();
    std::os::unix::fs::symlink("system-1-link", profiles.join("system")).unwrap(); // relative, like NixOS
    let b = b.with_system_profile(profiles.join("system"));
    assert!(b.status().unwrap().problems().is_empty(), "{:?}", b.status().unwrap().problems());
    std::os::unix::fs::symlink("/nix/store/bbb-stock-nixos", profiles.join("system-2-link")).unwrap();
    switch_to(&profiles.join("system"), "system-2-link");
    let problems = b.status().unwrap().problems().join("\n");
    assert!(problems.contains("next boot") && problems.contains("bbb-stock-nixos"), "{problems}");
}

/// A status check holds the lock for milliseconds; an operation that collides with it must wait,
/// not fail.
#[test]
fn operation_waits_briefly_for_the_lock() {
    let (dir, b) = setup("true");
    let held = fs::OpenOptions::new().write(true).open(dir.path().join("state/broker.lock")).unwrap();
    held.lock().unwrap();
    let release = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(500));
        drop(held);
    });
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    b.propose(&d, "waited", Actor::User, false).unwrap();
    release.join().unwrap();
}

/// The CLI signals "busy, not checked" with its own exit code (75), not just text.
#[test]
fn status_cli_exits_75_while_busy() {
    let (dir, _b) = setup("true");
    let held = fs::OpenOptions::new().write(true).open(dir.path().join("state/broker.lock")).unwrap();
    held.lock().unwrap();
    let out = run_cli(dir.path(), "true", &["status"]);
    assert_eq!(out.status.code(), Some(75), "{out:?}");
}

/// The broker's own `log` must not let a proposal's reason drive the terminal; `--brief` drops
/// the diffs.
#[test]
fn log_neutralises_control_characters_and_brief_drops_diffs() {
    let (dir, b) = setup("true");
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    b.propose(&d, "x\r\u{1b}[2K#99 applied fake\nsecond line", Actor::Agent, false).unwrap();
    let out = run_cli(dir.path(), "true", &["log"]);
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(!text.contains('\r') && !text.contains('\u{1b}'), "{text:?}");
    assert_eq!(text.lines().count(), 2, "one line per entry: {text}");
    let brief = String::from_utf8(run_cli(dir.path(), "true", &["log", "--json", "--brief"]).stdout).unwrap();
    assert!(!brief.contains("\"diff\":"), "{brief}");
    let full = String::from_utf8(run_cli(dir.path(), "true", &["log", "--json"]).stdout).unwrap();
    assert!(full.contains("\"diff\":"));
}
