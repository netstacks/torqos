//! The agent's sessions, held by the broker, and a person's trust (ADR-042, amended; session.rs).
//! Each test fails if its guard is taken out.

#![forbid(unsafe_code)]
#![forbid(dead_code)]

use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use torq_broker::ledger::NewEntry;
use torq_broker::{agent::Limits, policy::Actor, session, Broker};

fn setup(settings: &[&str]) -> (tempfile::TempDir, Broker) {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("config");
    fs::create_dir_all(repo.join("users")).unwrap();
    fs::write(repo.join("desktop.nix"), "{ }\n").unwrap();
    let limits = Limits { users: vec!["casey".into()], settings: settings.iter().map(|s| s.to_string()).collect() };
    let b = Broker::new(&repo, dir.path().join("state"), "true").with_agent_limits(&limits);
    b.init().unwrap();
    (dir, b)
}

fn diff(dir: &Path, name: &str, path: &str, body: &str) -> PathBuf {
    let p = dir.join(name);
    fs::write(&p, format!("--- /dev/null\n+++ b/{path}\n@@ -0,0 +1 @@\n+{body}\n")).unwrap();
    p
}

fn htop(dir: &Path) -> PathBuf {
    diff(dir, "htop.diff", "users/casey/packages.json", "[\"htop\"]")
}

fn propose(b: &Broker, d: &Path, actor: Actor, tainted: bool, session: Option<&str>) -> anyhow::Result<torq_broker::ledger::Entry> {
    b.propose_in(Some(&b.repo.head().unwrap()), d, "install htop", actor, tainted, session)
}

fn kinds(b: &Broker, kind: &str) -> usize {
    b.ledger.read_all().unwrap().iter().filter(|e| e.body.kind == kind).count()
}

#[test]
fn an_agents_proposal_without_a_session_is_tainted() {
    let (dir, b) = setup(&[]);
    let p = propose(&b, &htop(dir.path()), Actor::Agent, false, None).unwrap();
    assert!(p.body.tainted, "no session: tainted, whatever the caller says");
    assert!(b.apply(p.body.seq, "casey", false).is_err(), "a tainted proposal needs --confirm-tainted");
    // A person's proposal is as they say.
    let p = propose(&b, &diff(dir.path(), "p.diff", "users/sam/packages.json", "[\"jq\"]"), Actor::User, false, None).unwrap();
    assert!(!p.body.tainted);
}

#[test]
fn the_broker_holds_the_taint_and_nothing_clears_it() {
    let (dir, b) = setup(&[]);
    let (s, e) = session::start(&b).unwrap();
    assert_eq!((e.body.kind.as_str(), e.body.data["session"].as_str()), ("session_started", Some(s.id.as_str())));
    // Its record is the broker's, in its state directory, private.
    use std::os::unix::fs::PermissionsExt;
    let rec = dir.path().join("state/sessions").join(format!("{}.json", s.id));
    assert_eq!(fs::metadata(&rec).unwrap().permissions().mode() & 0o777, 0o600);
    // A clean session's proposal is not tainted; the caller's --tainted still adds.
    let p = propose(&b, &htop(dir.path()), Actor::Agent, false, Some(&s.id)).unwrap();
    assert!(!p.body.tainted);
    assert_eq!(p.body.data["session"], json!(s.id));
    let d2 = diff(dir.path(), "two.diff", "users/casey/packages.json", "[\"jq\"]");
    assert!(propose(&b, &d2, Actor::Agent, true, Some(&s.id)).unwrap().body.tainted, "the caller's flag adds");
    // Tainted: ledgered once, sticky; a proposal saying otherwise is tainted all the same.
    session::taint(&b, &s.id, "read_journal").unwrap();
    session::taint(&b, &s.id, "show_ledger").unwrap();
    assert_eq!(kinds(&b, "session_tainted"), 1, "the first taint is ledgered, once");
    let r = session::read(&b, &s.id).unwrap().unwrap();
    assert!(r.tainted && r.tainted_by == ["read_journal", "show_ledger"]);
    let d3 = diff(dir.path(), "three.diff", "users/casey/packages.json", "[\"tree\"]");
    assert!(propose(&b, &d3, Actor::Agent, false, Some(&s.id)).unwrap().body.tainted, "from the broker's record, not the caller's word");
    // A source that is not a tool's name is refused, and nothing changes.
    assert!(session::taint(&b, &s.id, "x; rm").is_err());
}

#[test]
fn a_session_must_be_the_brokers_and_the_agents() {
    let (dir, b) = setup(&[]);
    let d = htop(dir.path());
    let err = format!("{:#}", propose(&b, &d, Actor::Agent, false, Some(&"0".repeat(32))).unwrap_err());
    assert!(err.contains("no session the broker holds"), "{err}");
    assert!(kinds(&b, "refused") == 1, "an agent's refusal is ledgered");
    assert!(propose(&b, &d, Actor::Agent, false, Some("../../x")).is_err(), "not an id");
    let (s, _) = session::start(&b).unwrap();
    assert!(propose(&b, &d, Actor::User, false, Some(&s.id)).is_err(), "a person names no session");
    assert!(session::taint(&b, &"1".repeat(32), "read_journal").is_err(), "no such session");
}

#[test]
fn sessions_cannot_fill_the_ledger() {
    let (_dir, b) = setup(&[]);
    for _ in 0..session::SESSIONS_AN_HOUR {
        session::start(&b).unwrap();
    }
    assert!(session::start(&b).is_err());
    assert_eq!(kinds(&b, "session_started"), session::SESSIONS_AN_HOUR);
}

#[test]
fn under_trust_a_clean_proposal_in_the_persons_folder_applies() {
    let (dir, b) = setup(&[]);
    let g = session::grant(&b, "casey", 30).unwrap();
    assert_eq!((g.body.kind.as_str(), g.body.approved_by.as_deref()), ("trust_granted", Some("casey")));
    assert_eq!(session::active(&b.ledger.read_all().unwrap()).len(), 1);
    let (s, _) = session::start(&b).unwrap();
    let p = propose(&b, &htop(dir.path()), Actor::Agent, false, Some(&s.id)).unwrap();
    let a = session::apply_trusted(&b, &p).unwrap().expect("applied under trust");
    assert_eq!(a.body.kind, "applied");
    assert_eq!(a.body.approved_by.as_deref(), Some("casey"));
    assert_eq!(a.body.actor, "agent");
    assert_eq!(a.body.data["trust"], json!(g.body.seq), "the apply names the grant it was made under");
    // Revertible as any change.
    b.revert(a.body.seq, "casey").unwrap();
}

#[test]
fn trust_never_covers_a_tainted_or_sessionless_or_system_proposal() {
    let (dir, b) = setup(&["services.openssh", "networking"]);
    session::grant(&b, "casey", 30).unwrap();
    // No session: tainted, waits for a person.
    let p = propose(&b, &htop(dir.path()), Actor::Agent, false, None).unwrap();
    assert!(session::apply_trusted(&b, &p).unwrap().is_none());
    // A tainted session.
    let (s, _) = session::start(&b).unwrap();
    session::taint(&b, &s.id, "read_journal").unwrap();
    let p = propose(&b, &diff(dir.path(), "b.diff", "users/casey/packages.json", "[\"jq\"]"), Actor::Agent, false, Some(&s.id)).unwrap();
    assert!(session::apply_trusted(&b, &p).unwrap().is_none());
    // A clean session, but the proposal marked tainted by its caller: the mark counts.
    let (clean, _) = session::start(&b).unwrap();
    let p = propose(&b, &diff(dir.path(), "t.diff", "users/casey/packages.json", "[\"bat\"]"), Actor::Agent, true, Some(&clean.id)).unwrap();
    assert!(p.body.tainted && session::apply_trusted(&b, &p).unwrap().is_none());
    // System tier, from a clean session.
    let ssh = diff(dir.path(), "ssh.diff", "nixos/services.json", "{\"openssh\": {\"enable\": true}}");
    let p = propose(&b, &ssh, Actor::Agent, false, Some(&clean.id)).unwrap();
    assert_eq!(p.body.tier, "system");
    assert!(session::apply_trusted(&b, &p).unwrap().is_none());
    // A person's own proposal is theirs to apply: trust is for the agent's.
    let mine = propose(&b, &diff(dir.path(), "m.diff", "users/casey/packages.json", "[\"tree\"]"), Actor::User, false, None).unwrap();
    assert!(session::apply_trusted(&b, &mine).unwrap().is_none());
    assert_eq!(kinds(&b, "applied"), 0);
}

#[test]
fn trust_is_the_persons_own_folder_only() {
    let (dir, b) = setup(&[]);
    let b = b.with_agent_limits(&Limits { users: vec!["casey".into(), "sam".into()], settings: vec![] });
    session::grant(&b, "sam", 30).unwrap();
    let (s, _) = session::start(&b).unwrap();
    // casey's folder: sam's trust does not cover it.
    let p = propose(&b, &htop(dir.path()), Actor::Agent, false, Some(&s.id)).unwrap();
    assert!(session::apply_trusted(&b, &p).unwrap().is_none());
    // Not on the agent's list: nothing to trust it with.
    assert!(format!("{:#}", session::grant(&b, "pat", 10).unwrap_err()).contains("mayPropose"));
}

#[test]
fn trust_is_bounded_revocable_and_expires_on_the_record() {
    let (dir, b) = setup(&[]);
    for bad in [0, 61] {
        assert!(session::grant(&b, "casey", bad).is_err(), "{bad}");
    }
    let g = session::grant(&b, "casey", 60).unwrap();
    assert!(session::grant(&b, "casey", 5).is_err(), "one at a time");
    let r = session::revoke(&b, "casey").unwrap().unwrap();
    assert_eq!((r.body.kind.as_str(), r.body.data["grant"].as_u64()), ("trust_revoked", Some(g.body.seq)));
    assert!(session::revoke(&b, "casey").unwrap().is_none());
    let (s, _) = session::start(&b).unwrap();
    let p = propose(&b, &htop(dir.path()), Actor::Agent, false, Some(&s.id)).unwrap();
    assert!(session::apply_trusted(&b, &p).unwrap().is_none(), "revoked");
    // A grant whose time is over: settled by the next operation, and covers nothing.
    let past = (chrono::Utc::now() - chrono::Duration::minutes(1)).to_rfc3339();
    let old = b.ledger.append(NewEntry {
        actor: "user".into(),
        approved_by: Some("casey".into()),
        kind: "trust_granted".into(),
        tier: "user".into(),
        tainted: false,
        reason: "old".into(),
        data: json!({ "person": "casey", "minutes": 5, "until": past }),
    }).unwrap();
    assert!(session::active(&b.ledger.read_all().unwrap()).is_empty(), "over at its time, entry or not");
    let p2 = propose(&b, &diff(dir.path(), "j.diff", "users/casey/packages.json", "[\"jq\"]"), Actor::Agent, false, Some(&s.id)).unwrap();
    let expired = b.ledger.read_all().unwrap().into_iter().find(|e| e.body.kind == "trust_expired").expect("ledgered");
    assert_eq!(expired.body.data["grant"], json!(old.body.seq));
    assert!(session::apply_trusted(&b, &p2).unwrap().is_none());
}

/// ADR-042, 6: an agent's change that could lose the network is applied only as a commit confirmed.
#[test]
fn what_could_cut_the_machine_off_is_applied_confirmed() {
    let (dir, b) = setup(&["networking"]);
    let (s, _) = session::start(&b).unwrap();
    let wifi = diff(dir.path(), "w.diff", "nixos/networking.json", "{\"wireless\": {\"enable\": true}}");
    let p = propose(&b, &wifi, Actor::Agent, false, Some(&s.id)).unwrap();
    assert_eq!(p.body.data["cuts_off"], json!(["networking.wireless.enable"]));
    let err = format!("{:#}", b.apply(p.body.seq, "casey", false).unwrap_err());
    assert!(err.contains("commit confirmed"), "{err}");
    assert_eq!(kinds(&b, "applied"), 0);
    // A person's own change is theirs to make as they like.
    let mine = propose(&b, &diff(dir.path(), "m.diff", "nixos/time.json", "{\"timeZone\": \"UTC\"}"), Actor::User, false, None).unwrap();
    assert!(mine.body.data.get("cuts_off").is_none());
}
