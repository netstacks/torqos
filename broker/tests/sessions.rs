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
    let limits = Limits { agents: agents(&["casey"]), settings: settings.iter().map(|s| s.to_string()).collect() };
    let b = by_socket(Broker::new(&repo, dir.path().join("state"), "true").with_agent_limits(&limits).as_agent(Some("torq-agent-casey")), "torq-agent-casey");
    b.init().unwrap();
    (dir, b)
}

/// Each person's agent account (ADR-051 3): `torq-agent-<person>` → `<person>`.
fn agents(people: &[&str]) -> std::collections::BTreeMap<String, String> {
    people.iter().map(|p| (format!("torq-agent-{p}"), p.to_string())).collect()
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
    let before = b.ledger.read_all().unwrap().len();
    let s = session::start(&b).unwrap();
    // Its record is the broker's, in its state directory, private; the ledger has nothing of it yet.
    use std::os::unix::fs::PermissionsExt;
    let rec = dir.path().join("state/sessions").join(format!("{}.json", s.id));
    assert_eq!(fs::metadata(&rec).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(b.ledger.read_all().unwrap().len(), before, "a session that proposes nothing costs the ledger nothing");
    // A clean session's proposal is not tainted; the caller's --tainted still adds.
    let p = propose(&b, &htop(dir.path()), Actor::Agent, false, Some(&s.id)).unwrap();
    assert!(!p.body.tainted);
    assert_eq!(p.body.data["session"], json!({ "id": s.id, "started": s.started, "tainted_by": [] }));
    let d2 = diff(dir.path(), "two.diff", "users/casey/packages.json", "[\"jq\"]");
    assert!(propose(&b, &d2, Actor::Agent, true, Some(&s.id)).unwrap().body.tainted, "the caller's flag adds");
    // Tainted, sticky; a proposal saying otherwise is tainted all the same, and its entry says by what.
    session::taint(&b, &s.id, "read_journal").unwrap();
    session::taint(&b, &s.id, "show_ledger").unwrap();
    let r = session::read(&b, &s.id).unwrap().unwrap();
    assert!(r.tainted && r.tainted_by == ["read_journal", "show_ledger"]);
    let d3 = diff(dir.path(), "three.diff", "users/casey/packages.json", "[\"tree\"]");
    let p3 = propose(&b, &d3, Actor::Agent, false, Some(&s.id)).unwrap();
    assert!(p3.body.tainted, "from the broker's record, not the caller's word");
    assert_eq!(p3.body.data["session"]["tainted_by"], json!(["read_journal", "show_ledger"]), "the ledger keeps what tainted it");
    // The record says what cited it, for what the agent is told.
    let cited: Vec<u64> = session::read(&b, &s.id).unwrap().unwrap().proposals.iter().map(|c| c.proposal).collect();
    assert_eq!(cited.last(), Some(&p3.body.seq));
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
    for bad in ["../../x", &"A".repeat(32), &"0".repeat(31)] {
        let err = format!("{:#}", propose(&b, &d, Actor::Agent, false, Some(bad)).unwrap_err());
        assert!(err.contains("is not a session's id"), "{bad}: {err}");
        assert!(session::read(&b, bad).is_err(), "{bad}");
    }
    let s = session::start(&b).unwrap();
    assert!(propose(&b, &d, Actor::User, false, Some(&s.id)).is_err(), "a person names no session");
    assert!(session::taint(&b, &"1".repeat(32), "read_journal").is_err(), "no such session");
}

/// The review of w5: sessions are bounded in what they keep, and an old one's file going makes a
/// proposal naming it refused, never untainted.
#[test]
fn sessions_are_bounded() {
    let (dir, b) = setup(&[]);
    let sessions = dir.path().join("state/sessions");
    fs::create_dir_all(&sessions).unwrap();
    // As many as are kept, from long ago: one more start takes the oldest away.
    for i in 0..session::SESSIONS_KEPT {
        let id = format!("{i:032x}");
        fs::write(sessions.join(format!("{id}.json")), json!({"id": id, "started": format!("2020-01-01T00:{:02}:{:02}Z", i / 60 % 60, i % 60), "tainted": i == 0, "tainted_by": [], "agent": "torq-agent-casey"}).to_string()).unwrap();
    }
    let oldest = format!("{:032x}", 0);
    session::start(&b).unwrap();
    assert_eq!(fs::read_dir(&sessions).unwrap().count(), session::SESSIONS_KEPT);
    assert!(session::read(&b, &oldest).unwrap().is_none(), "the oldest went");
    assert!(propose(&b, &htop(dir.path()), Actor::Agent, false, Some(&oldest)).is_err(), "gone: refused, not untainted");
    // At most so many started in an hour.
    let now = chrono::Utc::now().to_rfc3339();
    for i in 0..session::SESSIONS_AN_HOUR {
        let id = format!("{:032x}", 1_000_000 + i);
        fs::write(sessions.join(format!("{id}.json")), json!({"id": id, "started": now, "tainted": false, "tainted_by": [], "agent": "torq-agent-casey"}).to_string()).unwrap();
    }
    assert!(format!("{:#}", session::start(&b).unwrap_err()).contains("in the last hour"));
    // Each agent its own count, and its own oldest (ADR-051 3): casey's agent used up, sam's starts, and its
    // start takes none of casey's records away.
    let before = fs::read_dir(&sessions).unwrap().count();
    let sam = as_agent(dir.path(), "torq-agent-sam", &["casey", "sam"]);
    assert_eq!(session::start(&sam).unwrap().agent, "torq-agent-sam");
    assert_eq!(fs::read_dir(&sessions).unwrap().count(), before + 1);
}

/// The review of ADR-051 step 1: a record from before agents had names is no one's (none may report
/// into it or cite it), so it is not kept: the next session started removes it.
#[test]
fn a_record_of_no_agents_goes() {
    let (dir, b) = setup(&[]);
    let sessions = dir.path().join("state/sessions");
    fs::create_dir_all(&sessions).unwrap();
    let old = format!("{:032x}", 7);
    fs::write(sessions.join(format!("{old}.json")), json!({"id": old, "started": "2026-11-01T00:00:00Z", "tainted": false, "tainted_by": []}).to_string()).unwrap();
    assert_eq!(session::read(&b, &old).unwrap().unwrap().agent, "");
    assert!(session::taint(&b, &old, "read_journal").is_err(), "no one's to report into");
    let s = session::start(&b).unwrap();
    assert!(session::read(&b, &old).unwrap().is_none(), "removed by the next start");
    assert!(session::read(&b, &s.id).unwrap().is_some());
}

#[test]
fn under_trust_a_clean_proposal_in_the_persons_folder_applies() {
    let (dir, b) = setup(&[]);
    let g = session::grant(&b, "casey", 30).unwrap();
    assert_eq!((g.body.kind.as_str(), g.body.approved_by.as_deref()), ("trust_granted", Some("casey")));
    assert_eq!(session::active(&b.ledger.read_all().unwrap()).len(), 1);
    let s = session::start(&b).unwrap();
    let p = propose(&b, &htop(dir.path()), Actor::Agent, false, Some(&s.id)).unwrap();
    let a = session::apply_trusted(&b, &p).unwrap().expect("applied under trust");
    assert_eq!(a.body.kind, "applied");
    assert_eq!(a.body.approved_by.as_deref(), Some("casey"));
    assert_eq!(a.body.actor, "agent");
    assert_eq!(a.body.data["trust"], json!(g.body.seq), "the apply names the grant it was made under");
    // Revertible as any change.
    b.revert(a.body.seq, "casey").unwrap();
}

/// ADR-053 (f): trust's automatic apply takes the same re-check as a person's. The configuration moved on
/// in another file: applied, re-checked; the same file made since, so the diff would do something else:
/// refused, nothing applied.
#[test]
fn under_trust_a_moved_configuration_is_rechecked() {
    let (dir, b) = setup(&[]);
    session::grant(&b, "casey", 30).unwrap();
    let s = session::start(&b).unwrap();
    let p = propose(&b, &htop(dir.path()), Actor::Agent, false, Some(&s.id)).unwrap();
    let night = dir.path().join("night.diff");
    fs::write(&night, "--- a/desktop.nix\n+++ b/desktop.nix\n@@ -1 +1 @@\n-{ }\n+{ nightLight = true; }\n").unwrap();
    let o = b.propose(&night, "night", Actor::User, false).unwrap();
    b.apply(o.body.seq, "casey", false).unwrap();
    let moved = b.repo.head().unwrap();
    let a = session::apply_trusted(&b, &p).unwrap().expect("applied under trust");
    assert_eq!(a.body.data["rechecked_on"], json!(moved));

    let (dir, b) = setup(&[]);
    session::grant(&b, "casey", 30).unwrap();
    let s = session::start(&b).unwrap();
    let q = propose(&b, &htop(dir.path()), Actor::Agent, false, Some(&s.id)).unwrap();
    // The same file, made by the person meanwhile: the agent's diff, which creates it, would now do
    // something else (here: nothing at all, it no longer applies).
    let mine = diff(dir.path(), "mine.diff", "users/casey/packages.json", "[\"git\"]");
    let m = b.propose(&mine, "mine", Actor::User, false).unwrap();
    b.apply(m.body.seq, "casey", false).unwrap();
    let err = format!("{:#}", session::apply_trusted(&b, &q).unwrap_err());
    assert!(err.contains("would now do something different"), "{err}");
    assert_eq!(fs::read_to_string(b.repo.root().join("users/casey/packages.json")).unwrap(), "[\"git\"]\n");
}

#[test]
fn trust_never_covers_a_tainted_or_sessionless_or_system_proposal() {
    let (dir, b) = setup(&["services.openssh", "networking"]);
    session::grant(&b, "casey", 30).unwrap();
    // No session: tainted, waits for a person.
    let p = propose(&b, &htop(dir.path()), Actor::Agent, false, None).unwrap();
    assert!(session::apply_trusted(&b, &p).unwrap().is_none());
    // A tainted session.
    let s = session::start(&b).unwrap();
    session::taint(&b, &s.id, "read_journal").unwrap();
    let p = propose(&b, &diff(dir.path(), "b.diff", "users/casey/packages.json", "[\"jq\"]"), Actor::Agent, false, Some(&s.id)).unwrap();
    assert!(session::apply_trusted(&b, &p).unwrap().is_none());
    // A clean session, but the proposal marked tainted by its caller: the mark counts.
    let clean = session::start(&b).unwrap();
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
    let b = b.with_agent_limits(&Limits { agents: agents(&["casey", "sam"]), settings: vec![] });
    session::grant(&b, "sam", 30).unwrap();
    let s = session::start(&b).unwrap();
    // casey's folder: sam's trust does not cover it.
    let p = propose(&b, &htop(dir.path()), Actor::Agent, false, Some(&s.id)).unwrap();
    assert!(session::apply_trusted(&b, &p).unwrap().is_none());
    // No agent of theirs: nothing to trust it with.
    assert!(format!("{:#}", session::grant(&b, "pat", 10).unwrap_err()).contains("has no agent"));
}

/// The same machine's state, as another agent account sees it: a second broker run for `agent`.
fn as_agent(dir: &Path, agent: &str, people: &[&str]) -> Broker {
    by_socket(Broker::new(dir.join("config"), dir.join("state"), "true").with_agent_limits(&Limits { agents: agents(people), settings: vec![] }).as_agent(Some(agent)), agent)
}

/// A model reached over TLS: nothing here can stand in for it (agent::model_taint).
const TLS_MODEL: &str = "https://api.example.org/v1";

/// What the ledger records of a call that came over the socket from `caller`, from its person's resident
/// agent (serve.rs sets both so: ADR-051 step 5).
fn by_socket(b: Broker, caller: &str) -> Broker {
    let mut b = b.from_resident(true).with_model(Some(TLS_MODEL));
    b.ledger.set_via(Some("socket".into()), Some(caller.into()));
    b
}

/// The review of ADR-051 step 1: trust applies only what the agent itself sent, over the socket. A
/// proposal that only says it is casey's agent's — root's `torq-broker propose --actor agent --agent
/// torq-agent-casey`, through sudo or at a root shell, or another account's over the socket — is not.
#[test]
fn trust_applies_only_what_the_agent_itself_sent() {
    let (dir, b) = setup(&[]);
    session::grant(&b, "casey", 30).unwrap();
    let s = session::start(&b).unwrap();
    for (via, caller) in [(None, None), (Some("sudo"), None), (Some("socket"), Some("dev")), (Some("socket"), Some("torq-agent-sam"))] {
        let mut root = Broker::new(dir.path().join("config"), dir.path().join("state"), "true").with_agent_limits(&Limits { agents: agents(&["casey"]), settings: vec![] }).as_agent(Some("torq-agent-casey")).from_resident(true);
        root.ledger.set_via(via.map(String::from), caller.map(String::from));
        let p = propose(&root, &diff(dir.path(), "r.diff", "users/casey/packages.json", "[\"nmap\"]"), Actor::Agent, false, Some(&s.id)).unwrap();
        assert!(session::apply_trusted(&root, &p).unwrap().is_none(), "{via:?} {caller:?}");
        assert_eq!(kinds(&b, "applied"), 0, "{via:?} {caller:?}");
    }
    // The agent itself, over the socket: applied.
    let p = propose(&b, &htop(dir.path()), Actor::Agent, false, Some(&s.id)).unwrap();
    assert!(session::apply_trusted(&b, &p).unwrap().is_some());
}

/// ADR-051 3: casey's trust covers casey's agent alone. sam's agent cannot propose in casey's folder
/// (refused, and ledgered as sam's agent's), and a proposal of casey's packages by an account that is
/// not casey's agent is never applied under casey's trust — judged by the broker's map as it is now.
#[test]
fn a_persons_trust_covers_their_own_agent_alone() {
    let (dir, b) = setup(&[]);
    let b = b.with_agent_limits(&Limits { agents: agents(&["casey", "sam"]), settings: vec![] });
    let sam = as_agent(dir.path(), "torq-agent-sam", &["casey", "sam"]);
    session::grant(&b, "casey", 30).unwrap();
    // sam's agent, in casey's folder: refused, on the record as sam's agent's.
    let s = session::start(&sam).unwrap();
    let e = format!("{:#}", propose(&sam, &htop(dir.path()), Actor::Agent, false, Some(&s.id)).unwrap_err());
    assert!(e.contains("only for sam"), "{e}");
    let last = b.ledger.read_all().unwrap().pop().unwrap();
    assert_eq!((last.body.kind.as_str(), last.body.data["agent"].as_str()), ("refused", Some("torq-agent-sam")));
    assert_eq!(kinds(&b, "applied"), 0);
    // sam's agent in sam's folder: a proposal, which casey's trust does not apply.
    let own = propose(&sam, &diff(dir.path(), "s.diff", "users/sam/packages.json", "[\"jq\"]"), Actor::Agent, false, Some(&s.id)).unwrap();
    assert_eq!((own.body.data["agent"].as_str(), own.body.data["agent_for"].as_str()), (Some("torq-agent-sam"), Some("sam")));
    assert!(session::apply_trusted(&sam, &own).unwrap().is_none());
    // casey's agent's proposal of casey's packages, while the map says that account is now sam's:
    // not casey's agent, so not under casey's trust.
    let c = session::start(&b).unwrap();
    let p = propose(&b, &htop(dir.path()), Actor::Agent, false, Some(&c.id)).unwrap();
    let moved = Broker::new(dir.path().join("config"), dir.path().join("state"), "true")
        .with_agent_limits(&Limits { agents: [("torq-agent-casey".to_string(), "sam".to_string())].into_iter().collect(), settings: vec![] });
    assert!(session::apply_trusted(&moved, &p).unwrap().is_none(), "not casey's agent now");
    assert_eq!(kinds(&b, "applied"), 0);
    // As the map has it: applied under casey's trust.
    assert!(session::apply_trusted(&b, &p).unwrap().is_some());
}

/// ADR-051 3: a session is its agent account's: no other agent reports into it or cites it, and an
/// account that is not an agent (none named, or one the map does not name) starts none.
#[test]
fn a_session_is_its_own_agents() {
    let (dir, b) = setup(&[]);
    let b = b.with_agent_limits(&Limits { agents: agents(&["casey", "sam"]), settings: vec![] });
    let sam = as_agent(dir.path(), "torq-agent-sam", &["casey", "sam"]);
    let s = session::start(&b).unwrap();
    assert_eq!(s.agent, "torq-agent-casey");
    assert!(format!("{:#}", session::taint(&sam, &s.id, "read_journal").unwrap_err()).contains("not torq-agent-sam's"));
    assert!(!session::read(&b, &s.id).unwrap().unwrap().tainted, "sam's agent reported nothing into it");
    let e = format!("{:#}", propose(&sam, &diff(dir.path(), "s.diff", "users/sam/packages.json", "[\"jq\"]"), Actor::Agent, false, Some(&s.id)).unwrap_err());
    assert!(e.contains("not torq-agent-sam's"), "{e}");
    // No agent named, or one the map does not name: no session, no proposal.
    let none = Broker::new(dir.path().join("config"), dir.path().join("state"), "true").with_agent_limits(&Limits { agents: agents(&["casey"]), settings: vec![] });
    assert!(session::start(&none).is_err());
    assert!(propose(&none, &htop(dir.path()), Actor::Agent, false, None).is_err());
    let stranger = as_agent(dir.path(), "torq-agent-x", &["casey"]);
    assert!(format!("{:#}", session::start(&stranger).unwrap_err()).contains("not an agent"));
    assert!(propose(&stranger, &htop(dir.path()), Actor::Agent, false, None).is_err());
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
    let s = session::start(&b).unwrap();
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
    let s = session::start(&b).unwrap();
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

/// The review of w5: a session tainted after its proposal, before the trusted apply, is not trusted
/// (the record is read again, under the lock).
#[test]
fn a_session_tainted_before_the_apply_is_not_trusted() {
    let (dir, b) = setup(&[]);
    session::grant(&b, "casey", 30).unwrap();
    let s = session::start(&b).unwrap();
    let p = propose(&b, &htop(dir.path()), Actor::Agent, false, Some(&s.id)).unwrap();
    assert!(!p.body.tainted);
    session::taint(&b, &s.id, "read_journal").unwrap();
    assert!(session::apply_trusted(&b, &p).unwrap().is_none());
    assert_eq!(kinds(&b, "applied"), 0);
}

/// The review of w5: a revoke never waits for the broker's lock — a trusted change being built
/// holds it — and no trusted apply starts after it.
#[test]
fn a_revoke_is_on_the_record_at_once_whatever_the_broker_is_doing() {
    let (dir, b) = setup(&[]);
    session::grant(&b, "casey", 30).unwrap();
    let s = session::start(&b).unwrap();
    let first = propose(&b, &htop(dir.path()), Actor::Agent, false, Some(&s.id)).unwrap();
    assert!(session::apply_trusted(&b, &first).unwrap().is_some(), "one applied under trust");
    let d = dir.path().join("r.diff");
    fs::write(&d, "--- a/users/casey/packages.json\n+++ b/users/casey/packages.json\n@@ -1 +1 @@\n-[\"htop\"]\n+[\"jq\"]\n").unwrap();
    let next = propose(&b, &d, Actor::Agent, false, Some(&s.id)).unwrap();
    // The broker's lock held, as a rebuild would: the revoke goes through, and quickly.
    let held = fs::File::open(dir.path().join("state/broker.lock")).unwrap();
    held.lock().unwrap();
    let t = std::time::Instant::now();
    let r = session::revoke(&b, "casey").unwrap().expect("revoked");
    assert!(t.elapsed() < std::time::Duration::from_secs(2), "it did not wait for the broker");
    assert_eq!(r.body.kind, "trust_revoked");
    drop(held);
    // The next apply checks the record again: revoked, nothing applied.
    assert!(session::apply_trusted(&b, &next).unwrap().is_none());
    assert_eq!(kinds(&b, "applied"), 1);
}

/// The review of w5: a commit at never takes what could cut the machine off — no one is there to confirm.
#[test]
fn a_commit_at_refuses_what_could_cut_the_machine_off() {
    let (dir, b) = setup(&["networking"]);
    let s = session::start(&b).unwrap();
    let wifi = diff(dir.path(), "w.diff", "nixos/networking.json", "{\"wireless\": {\"enable\": true}}");
    let p = propose(&b, &wifi, Actor::Agent, false, Some(&s.id)).unwrap();
    let at = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
    let err = format!("{:#}", torq_broker::at::Scheduler::new(&b, "true", "true").schedule(p.body.seq, "casey", &at).unwrap_err());
    assert!(err.contains("could lose the network or the screen"), "{err}");
    assert_eq!(kinds(&b, "apply_scheduled"), 0);
}

/// Decline (ADR-051, amended): session trust's apply refuses a declined proposal too — the one path
/// with no person there.
#[test]
fn trust_never_applies_a_declined_proposal() {
    let (dir, b) = setup(&[]);
    session::grant(&b, "casey", 30).unwrap();
    let s = session::start(&b).unwrap();
    // Such a proposal is covered by the trust: this one applies at once.
    let covered = propose(&b, &htop(dir.path()), Actor::Agent, false, Some(&s.id)).unwrap();
    assert!(session::apply_trusted(&b, &covered).unwrap().is_some(), "covered by casey's trust");
    // The same kind of proposal, declined before trust's apply reaches it: not applied.
    let jq = dir.path().join("jq.diff");
    fs::write(&jq, "--- a/users/casey/packages.json\n+++ b/users/casey/packages.json\n@@ -1 +1 @@\n-[\"htop\"]\n+[\"htop\", \"jq\"]\n").unwrap();
    let p = propose(&b, &jq, Actor::Agent, false, Some(&s.id)).unwrap();
    torq_broker::at::Scheduler::new(&b, "true", "true").decline(p.body.seq, "casey").unwrap();
    let r = session::apply_trusted(&b, &p);
    assert!(r.as_ref().map_or_else(|e| format!("{e:#}").contains("declined"), |a| a.is_none()), "{r:?}");
    assert_eq!(kinds(&b, "applied"), 1, "only the one before");
}

/// ADR-051 step 5 (KNOWN-GAPS 6zq): only the person's resident agent starts a session untainted and has
/// its proposals judged by it. Any other program of the agent account's — Claude Code under `torq agent run`,
/// torq-mcp, `torq doctor` — starts tainted (`not-the-resident-agent`), and its proposal is tainted whatever
/// session it cites, the resident agent's own clean one included (its id is in the ledger); under the
/// person's trust, none of it applies.
#[test]
fn only_the_resident_agent_is_untainted() {
    let (dir, b) = setup(&[]);
    session::grant(&b, "casey", 30).unwrap();
    // The same account, over the socket, from outside the resident agent's control group.
    let mut other = Broker::new(dir.path().join("config"), dir.path().join("state"), "true").with_agent_limits(&Limits { agents: agents(&["casey"]), settings: vec![] }).as_agent(Some("torq-agent-casey"));
    other.ledger.set_via(Some("socket".into()), Some("torq-agent-casey".into()));
    assert!(!other.resident());
    let s = session::start(&other).unwrap();
    assert!(s.tainted && s.tainted_by == ["not-the-resident-agent"], "{s:?}");
    let p = propose(&other, &htop(dir.path()), Actor::Agent, false, Some(&s.id)).unwrap();
    assert!(p.body.tainted && p.body.data["session"]["tainted_by"] == json!(["not-the-resident-agent"]), "{:?}", p.body);
    assert!(session::apply_trusted(&other, &p).unwrap().is_none(), "not applied under trust");
    // Citing the resident agent's session (its id is in the ledger): refused, and nothing of it lands in that
    // session's record — so the resident agent is never told another program's proposal as its own.
    let clean = session::start(&b).unwrap();
    assert!(!clean.tainted && clean.tainted_by.is_empty() && clean.resident);
    let e = format!("{:#}", propose(&other, &diff(dir.path(), "j.diff", "users/casey/packages.json", "[\"jq\"]"), Actor::Agent, false, Some(&clean.id)).unwrap_err());
    assert!(e.contains("the resident agent's"), "{e}");
    let held = session::read(&b, &clean.id).unwrap().unwrap();
    assert!(!held.tainted && held.proposals.is_empty(), "its record untouched: {held:?}");
    // Nor the other way: the resident agent does not cite another program's session.
    let e = format!("{:#}", propose(&b, &diff(dir.path(), "k.diff", "users/casey/packages.json", "[\"jq\"]"), Actor::Agent, false, Some(&s.id)).unwrap_err());
    assert!(e.contains("another program's"), "{e}");
    assert_eq!(kinds(&b, "applied"), 0);
    // The resident agent's own, on that session: applied under trust.
    let p = propose(&b, &diff(dir.path(), "t.diff", "users/casey/packages.json", "[\"tree\"]"), Actor::Agent, false, Some(&clean.id)).unwrap();
    assert!(!p.body.tainted && p.body.data["session"]["tainted_by"] == json!([]), "{:?}", p.body);
    assert!(session::apply_trusted(&b, &p).unwrap().is_some());
}

/// ADR-051 step 5's review: the resident agent's session starts clean only with a model no account here can
/// stand in for. A TCP model on this machine (an impostor on `[::1]` beside the real one on `127.0.0.1`), a
/// socket in a place others can write, or no model named: tainted from its start, and not applied under trust.
#[test]
fn the_resident_agent_with_a_model_others_can_stand_in_for_is_tainted() {
    let (dir, b) = setup(&[]);
    session::grant(&b, "casey", 30).unwrap();
    let open = tempfile::tempdir().unwrap();
    let sock = format!("unix:{}/m.sock", open.path().display());
    for (model, why) in [(Some("http://localhost:11434/v1"), "local-tcp-model"), (Some("http://127.0.0.1:8080/v1"), "local-tcp-model"), (Some(sock.as_str()), "model-socket-open"), (None, "model-unknown")] {
        let mut r = Broker::new(dir.path().join("config"), dir.path().join("state"), "true").with_agent_limits(&Limits { agents: agents(&["casey"]), settings: vec![] }).as_agent(Some("torq-agent-casey")).from_resident(true).with_model(model);
        r.ledger.set_via(Some("socket".into()), Some("torq-agent-casey".into()));
        let s = session::start(&r).unwrap();
        assert!(s.resident && s.tainted && s.tainted_by == [why], "{model:?}: {s:?}");
        let p = propose(&r, &htop(dir.path()), Actor::Agent, false, Some(&s.id)).unwrap();
        assert!(p.body.tainted, "{model:?}");
        assert!(session::apply_trusted(&r, &p).unwrap().is_none(), "{model:?}");
    }
    assert_eq!(kinds(&b, "applied"), 0);
    // Over TLS: clean, and applied (the control).
    let s = session::start(&b).unwrap();
    assert!(!s.tainted, "{s:?}");
}
