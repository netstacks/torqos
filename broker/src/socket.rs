//! The broker on a socket (ADR-040): who is asking is the kernel's answer, never the caller's.
//!
//! `torq-broker.socket` hands each connection to `torq-broker serve` (systemd, `Accept=yes`): the
//! connection is its standard input and output. The caller's uid comes from `SO_PEERCRED`; what it
//! asks for is one JSON line (`Request`); what it may do is decided here (`gate`), by that uid and
//! the change's tier; a system change asks the caller's password, checked through PAM. Then the
//! broker runs as it always has — as root, the request's arguments plus the identity this module
//! decided — and its output comes back as `Frame` lines, its exit code last.

use crate::policy::{self, Tier};
use anyhow::{bail, Context, Result};
// The socket's words, shared with its callers (ADR-044).
pub use torq_wire::socket::{Frame, Request, NEED_PASSWORD, SOCKET};

/// What every agent account's name begins with: `torq-agent-<person>` (ADR-051 3). A name is not
/// what makes an account an agent — the broker's map is (`caller`) — but no agent is named otherwise.
pub const AGENT_PREFIX: &str = "torq-agent-";
/// The PAM service a system change's password is checked against (modules/broker.nix).
pub const PAM_SERVICE: &str = "torq-approve";
/// The group whose members may approve a system change (with their password).
pub const ADMINS: &str = "wheel";
/// A request is a line, and no line is longer than this (a large diff, with room).
pub const MAX_REQUEST: u64 = 8 << 20;
/// The uids NixOS gives normal accounts (people): `isNormalUser` starts at 1000; Nix's build users
/// start at 30001, and are not people.
pub const PEOPLE_UIDS: std::ops::Range<u32> = 1000..30000;
/// Where faillock counts a password's failures: root's alone (modules/broker.nix), so that a program
/// running as the person cannot reset its own count between guesses.
pub const FAILLOCK_DIR: &str = "/run/torq/faillock";
/// How long a caller has to send its request, and to take each piece of the reply, in seconds.
pub const TIMEOUT_SECONDS: u64 = 10;


/// The account at the other end of the socket, as the kernel and the machine's own files say.
#[derive(Debug, Clone, PartialEq)]
pub struct Caller {
    pub uid: u32,
    pub name: String,
    /// The person this account is the agent of: it is in the broker's map of agents
    /// (/etc/torq/broker.json, `torq.agent.people`) and is not a person's uid. None: not an agent.
    pub agent_for: Option<String>,
    pub admin: bool,
    /// A normal account (NixOS gives people uids from 1000): not a service's, which proposes nothing.
    pub person: bool,
    /// An agent account calling from its person's resident agent, `torq-agent@<person>.service`: the person's
    /// resident account (`resident_account`, the map's word for the uid) *and* its process in that unit's
    /// control group (as `serve` reads it from the peer's pidfd: ADR-051 step 5, amended 2026-11-11). Never the
    /// caller's word; false unless serve found it so. Only then is its session untainted at its start, and its
    /// proposal judged by the session it cites.
    pub resident: bool,
    /// The agent account this caller acts as at the broker: its own, or — for a resident account — its
    /// person's agent account (`torq-agent-<person>`): both are the person's one agent (`data.agent`, the caps).
    pub acts_as: Option<String>,
    /// Whether this account is a person's resident agent's own (`torq-resident-<person>`, the map's
    /// `residents`): the only account whose requests can be resident.
    pub resident_account: bool,
}

/// What a session of an agent account's is tainted by from its start, and a proposal of one, when the
/// caller is not its person's resident agent (ADR-051 step 5): any other program run as the agent —
/// `torq agent run`'s, a coding tool, torq-mcp — whose reads the broker never sees.
pub const NOT_RESIDENT: &str = "not-the-resident-agent";

/// The control group of `person`'s resident agent, `torq-agent@<person>.service`, as systemd names it
/// (an instance of a template goes in `system-<its prefix, escaped>.slice`).
pub fn resident_cgroup(person: &str) -> String {
    format!("/system.slice/system-torq\\x2dagent.slice/torq-agent@{person}.service")
}

/// Whether a process's `/proc/<pid>/cgroup` puts it in `person`'s resident agent's own control group —
/// that group itself (the unit delegates nothing, so has no group below it), on the unified hierarchy.
pub fn in_resident(cgroup: &str, person: &str) -> bool {
    let want = resident_cgroup(person);
    let v2: Vec<&str> = cgroup.lines().filter_map(|l| l.strip_prefix("0::")).collect();
    policy::user_name_ok(person) && v2.len() == 1 && v2[0] == want
}

/// The pid a pidfd names, from its `/proc/self/fdinfo/<fd>` (`Pid:`); none for one that has exited (-1)
/// or is not in this namespace (0).
pub fn fdinfo_pid(fdinfo: &str) -> Option<i32> {
    fdinfo.lines().find_map(|l| l.strip_prefix("Pid:")).and_then(|v| v.trim().parse::<i32>().ok()).filter(|p| *p > 0)
}

/// Whether the peer of connection `conn` — an agent account serving `person`, its pid `pid` as SO_PEERCRED
/// says — is that person's resident agent, `torq-agent@<person>.service` (ADR-051 step 5): its process in
/// that unit's own control group, which no process of the agent's can move itself into (the group's
/// `cgroup.procs` is root's: the unit delegates nothing). Read without trusting the pid alone, which could
/// be another process's by now: the peer's pidfd (SO_PEERPIDFD, fixed when it connected) must name that
/// pid; its control group is read; then the pidfd must say the process has not exited — so the pid was
/// still its when the group was read. Anything that does not hold, a kernel without SO_PEERPIDFD included:
/// not resident, so tainted. Never an error: what cannot be read is a "no".
pub fn resident_peer(conn: &impl std::os::fd::AsFd, pid: i32, person: &str) -> bool {
    peer_in(conn, pid, |cgroup| in_resident(cgroup, person))
}

/// Whether `who`, at the other end of `conn` (its pid `pid`), is its person's resident agent: the resident
/// account (the map's word for the uid) first, then its process in the unit's control group (ADR-051 3 amended
/// 2026-11-11: the uid primary, the control group a second line). What serve sets `Caller.resident` by.
pub fn found_resident(who: &Caller, conn: &impl std::os::fd::AsFd, pid: i32) -> bool {
    found_resident_by(who, conn, pid, in_resident)
}

/// `found_resident`, the control group judged by `in_unit` (the cgroup text, the person).
fn found_resident_by(who: &Caller, conn: &impl std::os::fd::AsFd, pid: i32, in_unit: impl Fn(&str, &str) -> bool) -> bool {
    let Some(person) = who.agent_for.as_deref().filter(|_| who.resident_account) else { return false };
    peer_in(conn, pid, |cgroup| in_unit(cgroup, person))
}

/// `resident_peer`, its control group judged by `ok`.
fn peer_in(conn: &impl std::os::fd::AsFd, pid: i32, ok: impl Fn(&str) -> bool) -> bool {
    use std::os::fd::AsRawFd as _;
    let Ok(pidfd) = nix::sys::socket::getsockopt(conn, nix::sys::socket::sockopt::PeerPidfd) else { return false };
    let named = std::fs::read_to_string(format!("/proc/self/fdinfo/{}", pidfd.as_raw_fd())).ok().and_then(|i| fdinfo_pid(&i));
    if named != Some(pid) {
        return false;
    }
    let Ok(cgroup) = std::fs::read_to_string(format!("/proc/{pid}/cgroup")) else { return false };
    if !ok(&cgroup) {
        return false;
    }
    // A pidfd is readable once its process has exited: not readable now, it is still running.
    let mut fds = [rustix::event::PollFd::new(&pidfd, rustix::event::PollFlags::IN)];
    matches!(rustix::event::poll(&mut fds, Some(&rustix::event::Timespec { tv_sec: 0, tv_nsec: 0 })), Ok(0))
}

/// What a command needs from its caller.
#[derive(Debug, PartialEq)]
pub enum Gate {
    Open,
    Password,
    Refused(String),
}

/// The change a command acts on: its tier and the files it touches (a proposal's, for `apply`; an
/// applied change's, for `revert`).
#[derive(Debug)]
pub struct Change {
    pub tier: Tier,
    pub files: Vec<String>,
}

/// Flags no caller may send: the broker's settings (they are /etc/torq/broker.json's), who is
/// asking and who approves (the kernel's), and files (they come in the request).
const NOT_FROM_CALLERS: &[&str] = &[
    "--repo", "--state-dir", "--rebuild-cmd", "--build-cmd", "--activate-cmd", "--boot-cmd", "--schedule-cmd",
    "--commit-at-cmd", "--commit-at-cancel-cmd", "--current-system", "--system-profile", "--actor", "--agent", "--resident", "--approved-by", "--diff", "--hash-file", "--key-file", "--value-file",
];

/// Refuse arguments that name the caller, a file, or the broker's settings.
pub fn check_args(args: &[String]) -> Result<()> {
    for a in args {
        let flag = a.split_once('=').map_or(a.as_str(), |(f, _)| f);
        if NOT_FROM_CALLERS.contains(&flag) {
            bail!("{flag} is not taken from a caller: who is asking is the kernel's answer, and a file comes in the request");
        }
    }
    Ok(())
}

/// The account with `uid`, from /etc/passwd's text; an admin if /etc/group's `wheel` lists it; an
/// agent if `agents` (the broker's map, agent account → person) names it, or `residents` (resident account →
/// person, ADR-051 3 amended 2026-11-11) names it for a person who has an agent, and its uid is no person's —
/// a person's account is never an agent, whatever it is called.
pub fn caller(uid: u32, passwd: &str, group: &str, agents: &crate::agent::Limits, residents: &std::collections::BTreeMap<String, String>) -> Result<Caller> {
    let name = passwd
        .lines()
        .filter_map(|l| {
            let mut f = l.split(':');
            let name = f.next()?;
            let id = f.nth(1)?.parse::<u32>().ok()?;
            (id == uid).then(|| name.to_string())
        })
        .next()
        .with_context(|| format!("no account has uid {uid}"))?;
    if !policy::user_name_ok(&name) {
        bail!("the account {name:?} is not one TorqOS acts for");
    }
    let admin = group.lines().any(|l| {
        let f: Vec<&str> = l.split(':').collect();
        f.len() == 4 && f[0] == ADMINS && f[3].split(',').any(|m| m == name)
    });
    let person = PEOPLE_UIDS.contains(&uid);
    let (agent_for, acts_as, resident_account) = if person {
        (None, None, false)
    } else if let Some(p) = agents.serves(&name) {
        (Some(p.to_string()), Some(name.clone()), false)
    } else if let Some(p) = residents.get(&name) {
        // A resident account acts as its person's one agent account, which the map must name.
        match agents.agents.iter().find(|(_, q)| *q == p) {
            Some((a, _)) => (Some(p.clone()), Some(a.clone()), true),
            None => (None, None, false),
        }
    } else {
        (None, None, false)
    };
    Ok(Caller { uid, agent_for, person, name, admin, resident: false, acts_as, resident_account })
}

/// Read-only commands: open to every account (the ledger says what changed, never a secret: a
/// password's hash is kept outside it).
const OBSERVE: &[&str] = &["log", "verify", "status", "show", "remote", "snapshots", "backups", "scheduled", "session", "trust", "secrets"];

/// What `cmd` needs from `who`: `change` is what it acts on (`apply`, `revert`), `user` whose
/// password it sets (`password`). An `apply --confirm` is `apply-confirmed`: while it waits, no
/// one else's change can be made, and only an administrator may hold the machine so.
pub fn gate(who: &Caller, cmd: &str, change: Option<&Change>, user: Option<&str>) -> Gate {
    let admins_only = || {
        if who.admin {
            Gate::Password
        } else {
            Gate::Refused(format!("{} is not an administrator ({ADMINS}): a system change is theirs to approve", who.name))
        }
    };
    // A use of a secret is the secrets daemon's to ask, and it asks nothing else (ADR-052): who the use is for,
    // the broker reads from the kernel itself.
    if who.name == torq_wire::secrets::DAEMON {
        return if matches!(cmd, "secret-use" | "secret-agent") { Gate::Open } else { Gate::Refused("the secrets daemon asks for uses of secrets, and nothing else".into()) };
    }
    if matches!(cmd, "secret-use" | "secret-agent") {
        return Gate::Refused("a use of a secret is asked by the secrets daemon, for the one connected to it".into());
    }
    if OBSERVE.contains(&cmd) {
        return Gate::Open;
    }
    if who.agent_for.is_some() {
        // It proposes, and keeps the broker told of what its sessions read (session.rs).
        return if matches!(cmd, "propose" | "session-start" | "session-taint") {
            Gate::Open
        } else {
            Gate::Refused("the agent proposes; a person applies".into())
        };
    }
    // A service's account (nobody, a daemon's) proposes nothing: the ledger is people's, and the
    // agent's, and every proposal stays in it for good.
    if !who.person {
        return Gate::Refused(format!("{} is not a person's account: only people, and the agent, propose changes", who.name));
    }
    // A person's secrets' rules are theirs alone (ADR-052): an administrator's password applies, schedules or
    // undoes no change to anyone else's. What the agent may do with a secret is its owner's to say.
    if let (Some(c), "apply" | "apply-confirmed" | "revert" | "schedule") = (change, cmd) {
        if let Some(f) = c.files.iter().find(|f| others_secret_rules(&who.name, f)) {
            return Gate::Refused(format!("{f}: a person's secrets are theirs alone to change"));
        }
    }
    match cmd {
        "propose" => Gate::Open,
        // A session is the agent's: its record is its reports, and no one else's.
        "session-start" | "session-taint" => Gate::Refused("a session is the agent's: a person's reads are their own".into()),
        // Trust in the agent with their own folder: theirs alone to give, with their password; ending it, at any time.
        "trust-grant" => Gate::Password,
        "trust-revoke" => Gate::Open,
        // A person's, not everyone's: it goes to the network, as root (the review of pull).
        "fetch-incoming" => Gate::Open,
        "apply-confirmed" => match change {
            Some(c) if c.tier == Tier::Forbidden => Gate::Refused("that change touches what no one may change".into()),
            Some(_) => admins_only(),
            None => Gate::Refused("no such change".into()),
        },
        // A commit at is approved now, as an apply is: the same person, the same password.
        "apply" | "revert" | "schedule" => match change {
            None => Gate::Refused("no such change".into()),
            Some(c) if c.tier == Tier::Forbidden => Gate::Refused("that change touches what no one may change".into()),
            // A person's own settings are theirs: no password (KNOWN-GAPS 6a — someone else's are not).
            Some(c) if theirs(&who.name, c) => Gate::Open,
            Some(_) => admins_only(),
        },
        // Their own password: the current one first, as passwd asks.
        "password" if user == Some(who.name.as_str()) => Gate::Password,
        // Declining a waiting proposal (ADR-051, amended): the person whose own folder it wholly is, or
        // whose agent proposed it (`user`: the ledger's word for whom that agent serves), as their own
        // apply is — no password; anyone else's, an administrator's, with their password.
        "decline" => match change {
            None => Gate::Refused("that is not a proposal: only a waiting proposal is declined".into()),
            Some(c) if theirs(&who.name, c) || user == Some(who.name.as_str()) => Gate::Open,
            Some(_) => admins_only(),
        },
        // A commit at, by the one who approved it (`user`: theirs); anyone else's, an administrator's.
        "cancel-scheduled" if user == Some(who.name.as_str()) => Gate::Open,
        "cancel-scheduled" => admins_only(),
        // A secret's value (ADR-052): a person's own is theirs, as their own settings are; reading it back
        // takes their password, so no program of their session reads it unseen. No one else sets or reads
        // it, an administrator included, who may only delete it. The system's are an administrator's.
        "secret-set" | "secret-delete" if user == Some(who.name.as_str()) => Gate::Open,
        "secret-show" if user == Some(who.name.as_str()) => Gate::Password,
        "secret-set" | "secret-show" if user.is_some() => Gate::Refused("a person's secret is theirs alone: no one else sets or reads it, an administrator included".into()),
        "secret-set" | "secret-delete" | "secret-show" => admins_only(),
        "password" | "confirm" | "reconcile" | "repair" | "remote-key" | "remote-clear" | "restore-data" | "ledger-acknowledge" | "snapshot-take" | "snapshot-delete" | "backup-key" | "backup-restore" => admins_only(),
        other => Gate::Refused(format!("`{other}` is not run over the socket (it is the machine's own, as root)")),
    }
}

/// Whether change `c` is wholly within `name`'s own folder of the configuration: theirs to make.
pub fn theirs(name: &str, c: &Change) -> bool {
    c.tier <= Tier::User && !c.files.is_empty() && c.files.iter().all(|f| own(name, f))
}

/// The account named `name`, as `caller` reads it for its uid.
pub fn account(name: &str, passwd: &str, group: &str, agents: &crate::agent::Limits, residents: &std::collections::BTreeMap<String, String>) -> Result<Caller> {
    let uid = passwd
        .lines()
        .find_map(|l| {
            let mut f = l.split(':');
            (f.next()? == name).then(|| f.nth(1)?.parse::<u32>().ok()).flatten()
        })
        .with_context(|| format!("no account is named {name:?}"))?;
    let c = caller(uid, passwd, group, agents, residents)?;
    if c.name != name {
        bail!("{name:?} shares its uid with {:?}: not one account", c.name);
    }
    Ok(c)
}

/// Whether `file` is someone other than `name`'s secrets' rules (`users/<other>/secrets.json`).
fn others_secret_rules(name: &str, file: &str) -> bool {
    file.strip_prefix("users/").and_then(|r| r.split_once('/')).is_some_and(|(n, leaf)| leaf == torq_wire::secrets::PERSON_RULES && n != name)
}

/// Whether `file` is in `name`'s own folder of the configuration.
fn own(name: &str, file: &str) -> bool {
    policy::is_user_data(file) && file.strip_prefix("users/").and_then(|r| r.split_once('/')).is_some_and(|(n, _)| n == name)
}

/// The arguments the broker runs with: the caller's, then who it is, as this module decided.
/// `diff`, `hash` and `key` are the private files the request's own came to.
pub fn with_identity(args: &[String], cmd: &str, who: &Caller, diff: Option<&str>, hash: Option<&str>, key: Option<&str>) -> Result<Vec<String>> {
    let mut a = args.to_vec();
    match cmd {
        "propose" => {
            // What the gate sees of a proposal is what it does on the commit it was judged on: without one,
            // a diff can be nothing at that commit and something at the next (the audit of M3, H1).
            if !args.iter().any(|x| x == "--base" || x.starts_with("--base=")) {
                bail!("a proposal needs --base, the commit it was prepared against");
            }
            match &who.agent_for {
                // Which agent: the broker judges its proposal by the person it serves (ADR-051 3).
                Some(_) => a.extend(["--actor".into(), "agent".into(), "--agent".into(), who.acts_as.clone().context("an agent acts as its agent account")?]),
                None => a.extend(["--actor".into(), "user".into()]),
            }
            // From the resident agent alone is it judged by its session (ADR-051 step 5); else tainted.
            if who.agent_for.is_some() && who.resident && who.resident_account {
                a.push("--resident".into());
            }
            a.extend(["--diff".into(), diff.context("a proposal needs its diff, in the request")?.into()]);
        }
        // A session is its agent account's own: no other agent starts, reports into or cites it.
        // (Anyone else's is refused by the gate: a session is an agent's.) Untainted at its start only
        // from the resident agent (ADR-051 step 5).
        "session-start" if who.agent_for.is_some() => {
            a.extend(["--agent".into(), who.acts_as.clone().context("an agent acts as its agent account")?]);
            if who.resident && who.resident_account {
                a.push("--resident".into());
            }
        }
        "session-taint" if who.agent_for.is_some() => a.extend(["--agent".into(), who.acts_as.clone().context("an agent acts as its agent account")?]),
        "password" => {
            a.extend(["--hash-file".into(), hash.context("a password needs its hash, in the request")?.into()]);
            a.extend(["--approved-by".into(), who.name.clone()]);
        }
        "backup-key" => {
            a.extend(["--key-file".into(), key.context("a backup's key comes in the request")?.into()]);
            a.extend(["--approved-by".into(), who.name.clone()]);
        }
        // A secret's value comes as a backup's key does: in the request's `key`.
        "secret-set" => {
            a.extend(["--value-file".into(), key.context("a secret's value comes in the request")?.into()]);
            a.extend(["--approved-by".into(), who.name.clone()]);
        }
        "secret-delete" | "secret-show" => a.extend(["--approved-by".into(), who.name.clone()]),
        "apply" | "revert" | "schedule" | "cancel-scheduled" | "decline" | "trust-grant" | "trust-revoke" | "confirm" | "reconcile" | "repair" | "remote-clear" | "restore-data" | "ledger-acknowledge" | "snapshot-take" | "snapshot-delete" | "backup-restore" => a.extend(["--approved-by".into(), who.name.clone()]),
        _ => {}
    }
    Ok(a)
}

/// Check `name`'s password through PAM (`torq-approve`: pam_unix, with faillock counting the
/// failures), as the broker — root — can. Nothing is kept: the password goes when this returns.
pub fn password_ok(name: &str, password: &str) -> bool {
    use nonstick::items::Items;
    use nonstick::{AuthnFlags, ConversationAdapter, PamShared, Transaction, TransactionBuilder};
    use std::ffi::{OsStr, OsString};
    struct Answer<'a> {
        name: &'a str,
        password: &'a str,
    }
    impl ConversationAdapter for Answer<'_> {
        fn prompt(&self, _: impl AsRef<OsStr>) -> nonstick::Result<OsString> {
            Ok(self.name.into())
        }
        fn masked_prompt(&self, _: impl AsRef<OsStr>) -> nonstick::Result<OsString> {
            Ok(self.password.into())
        }
        fn error_msg(&self, _: impl AsRef<OsStr>) {}
        fn info_msg(&self, _: impl AsRef<OsStr>) {}
    }
    let Ok(mut t) = TransactionBuilder::new_with_service(PAM_SERVICE).username(name).build(Answer { name, password }.into_conversation()) else {
        return false;
    };
    // The account as PAM ended up with: a module may change it, and then it is not this person.
    t.authenticate(AuthnFlags::DISALLOW_NULL_AUTHTOK).is_ok()
        && t.account_management(AuthnFlags::DISALLOW_NULL_AUTHTOK).is_ok()
        && t.items().user().is_ok_and(|u| u.as_deref() == Some(OsStr::new(name)))
}

/// The polkit action a desktop's approval of a system change is (modules/desktop.nix; `auth_self`: the
/// caller's own password, asked by COSMIC's agent, every time).
pub const APPROVE_ACTION: &str = "org.torqos.approve";

/// A value of a logind record (`KEY=value` lines).
fn record<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
}

fn graphical(session: &str) -> bool {
    matches!(record(session, "TYPE"), Some("wayland" | "x11"))
}

/// Whether a process belongs to a person's graphical session (ADR-045 5): in the session itself, or among
/// the applications of a user manager whose person has one logged in. Anything else (a text console, SSH, a
/// system service) is as it was: its own record says so. `cgroup` is /proc/<pid>/cgroup, `session` reads
/// logind's record of a session, and `user` is logind's record of the caller's account.
pub fn on_the_desktop(cgroup: &str, session: impl Fn(&str) -> Option<String>, user: Option<&str>) -> bool {
    let Some(path) = cgroup.lines().find_map(|l| l.strip_prefix("0::")) else { return false };
    for part in path.split('/') {
        if let Some(id) = part.strip_prefix("session-").and_then(|s| s.strip_suffix(".scope")) {
            return session(id).is_some_and(|s| graphical(&s));
        }
        if part.starts_with("user@") && part.ends_with(".service") {
            return user.and_then(|u| record(u, "SESSIONS")).is_some_and(|ids| ids.split_whitespace().any(|id| session(id).is_some_and(|s| graphical(&s))));
        }
    }
    false
}

/// When a process started, from its /proc/<pid>/stat: with its pid and uid, what polkit names it by, so
/// that a pid used again is not taken for it. (After the name, which may hold anything: the last `)`.)
pub fn start_time(stat: &str) -> Option<u64> {
    stat.rsplit_once(')')?.1.split_whitespace().nth(19)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWD: &str = "root:x:0:0::/root:/bin/sh\nviper:x:1000:100::/home/viper:/bin/zsh\nsam:x:1001:100::/home/sam:/bin/zsh\ntorq-agent-viper:x:990:990::/var/lib/torq-agent-viper:/run/current-system/sw/bin/nologin\ntorq-agent-sam:x:991:991::/var/lib/torq-agent-sam:/run/current-system/sw/bin/nologin\ntorq-agent-x:x:992:992::/var/empty:/run/current-system/sw/bin/nologin\ntorq-agent-pat:x:1002:100::/home/pat:/bin/zsh\nnobody:x:65534:65534::/var/empty:/run/current-system/sw/bin/nologin\nsshd:x:998:998::/var/empty:/run/current-system/sw/bin/nologin\ntorq-resident-viper:x:980:980::/var/empty:/run/current-system/sw/bin/nologin\ntorq-resident-dave:x:981:981::/var/empty:/run/current-system/sw/bin/nologin\ntorq-resident-pat:x:1003:100::/home/pat2:/bin/zsh\ntorq-resident-sam:x:982:982::/var/empty:/run/current-system/sw/bin/nologin\ntorq-secrets:x:989:989::/var/empty:/run/current-system/sw/bin/nologin\n";
    /// The broker's map: viper's and sam's agents, and one (`torq-agent-pat`) that names a person's
    /// account — which is a person, never an agent.
    fn agents() -> crate::agent::Limits {
        let agents = [("torq-agent-viper", "viper"), ("torq-agent-sam", "sam"), ("torq-agent-pat", "pat")].iter().map(|(a, p)| (a.to_string(), p.to_string())).collect();
        crate::agent::Limits { agents, settings: vec![] }
    }
    /// The residents' map (ADR-051 3 amended 2026-11-11): viper's, one for dave (who has no agent), one with a
    /// person's uid; sam's account exists but the map does not name it.
    fn residents() -> std::collections::BTreeMap<String, String> {
        [("torq-resident-viper", "viper"), ("torq-resident-dave", "dave"), ("torq-resident-pat", "pat")].iter().map(|(a, p)| (a.to_string(), p.to_string())).collect()
    }
    const GROUP: &str = "root:x:0:\nwheel:x:1:viper\nusers:x:100:sam\n";

    fn who(uid: u32) -> Caller {
        caller(uid, PASSWD, GROUP, &agents(), &residents()).unwrap()
    }
    fn change(tier: Tier, files: &[&str]) -> Change {
        Change { tier, files: files.iter().map(|f| f.to_string()).collect() }
    }

    #[test]
    fn the_caller_is_the_uid() {
        assert_eq!(who(1000), Caller { uid: 1000, name: "viper".into(), agent_for: None, admin: true, person: true, resident: false, acts_as: None, resident_account: false });
        assert_eq!(who(1001), Caller { uid: 1001, name: "sam".into(), agent_for: None, admin: false, person: true, resident: false, acts_as: None, resident_account: false });
        assert!(!who(65534).person && !who(998).person && !who(990).person);
        assert!(who(990).agent_for.as_deref() == Some("viper") && !who(990).admin);
        assert!(caller(4242, PASSWD, GROUP, &agents(), &residents()).is_err(), "no account");
        // A name that is only a prefix of a wheel member's is not one.
        assert!(!caller(1, "vipe:x:1:1::/:/bin/sh\n", GROUP, &agents(), &residents()).unwrap().admin);
    }

    /// ADR-051 3: an agent is an account the broker's map names, and whose uid is no person's — the
    /// name alone makes nothing an agent, and each agent is told apart by the person it serves.
    #[test]
    fn an_agent_is_the_maps_and_never_a_person() {
        assert_eq!(who(990).agent_for.as_deref(), Some("viper"));
        assert_eq!(who(991).agent_for.as_deref(), Some("sam"));
        // Named like one, not in the map: no agent (and no person: it proposes nothing).
        assert_eq!(who(992).agent_for, None);
        assert!(matches!(gate(&who(992), "propose", None, None), Gate::Refused(_)));
        assert!(matches!(gate(&who(992), "session-start", None, None), Gate::Refused(_)));
        // In the map, with a person's uid: a person, never an agent.
        let pat = who(1002);
        assert!(pat.person && pat.agent_for.is_none(), "{pat:?}");
        assert!(matches!(gate(&pat, "session-start", None, None), Gate::Refused(_)));
        let p = with_identity(&["propose".into(), "--base=c".into()], "propose", &pat, Some("/run/d"), None, None).unwrap();
        assert!(p.contains(&"user".to_string()) && !p.contains(&"--agent".to_string()), "{p:?}");
        // With no map at all (an older broker.json), nothing is an agent.
        assert_eq!(caller(990, PASSWD, GROUP, &crate::agent::Limits::default(), &residents()).unwrap().agent_for, None);
        // Each agent's identity is its own account, as the kernel says.
        let s = with_identity(&["session-start".into()], "session-start", &who(991), None, None, None).unwrap();
        assert_eq!(s, ["session-start", "--agent", "torq-agent-sam"]);
        assert_eq!(with_identity(&["session-start".into()], "session-start", &who(1000), None, None, None).unwrap(), ["session-start"], "a person names no agent");
        assert!(matches!(gate(&who(1000), "session-start", None, None), Gate::Refused(_)));
        assert!(check_args(&["propose".into(), "--agent=torq-agent-viper".into()]).is_err(), "no caller names its agent");
    }

    /// ADR-051 step 5: the resident agent is a process in its person's `torq-agent@<person>.service`'s own
    /// control group — no other group, no group below it, no look-alike name, nothing on the old hierarchy.
    #[test]
    fn the_resident_agent_is_its_units_control_group() {
        let cg = |p: &str| format!("0::{p}\n");
        let unit = "/system.slice/system-torq\\x2dagent.slice/torq-agent@viper.service";
        assert_eq!(resident_cgroup("viper"), unit);
        assert!(in_resident(&cg(unit), "viper"));
        for not in [
            "/system.slice/system-torq\\x2dagent.slice/torq-agent@sam.service",
            "/system.slice/system-torq\\x2dagent.slice/torq-agent@viper.service/sub",
            "/system.slice/system-torq\\x2dagent.slice/torq-agent@viper.service.d",
            "/system.slice/torq-agent@viper.service",
            "/user.slice/user-1000.slice/session-3.scope",
            "/user.slice/user-1000.slice/user@1000.service/app.slice/torq-agent@viper.service",
            "/system.slice/run-u12.service",
            "",
        ] {
            assert!(!in_resident(&cg(not), "viper"), "{not}");
        }
        assert!(!in_resident(&cg(unit), "sam"), "another person's agent");
        assert!(!in_resident(&format!("1:name=systemd:{unit}\n"), "viper"), "only the unified hierarchy says");
        assert!(!in_resident(&format!("0::{unit}\n0::/other\n"), "viper"), "one group, or none");
        assert!(!in_resident("", "viper"));
        assert!(!in_resident(&cg(&resident_cgroup("../x")), "../x"), "not a person's name");
    }

    /// The peer read through its pidfd: a live peer whose pid SO_PEERCRED named, its group as judged; a pid
    /// that is not the pidfd's, or a peer that has exited, is not resident — and a connection with no peer
    /// (not a socket) never is.
    #[test]
    fn the_peer_is_read_by_its_pidfd() {
        let me = std::process::id() as i32;
        let (a, _b) = std::os::unix::net::UnixStream::pair().unwrap();
        // This process is the peer of its own pair.
        assert!(peer_in(&a, me, |_| true), "a live peer, as SO_PEERCRED named it");
        assert!(!peer_in(&a, me, |_| false), "its group judged");
        assert!(!peer_in(&a, me + 1, |_| true), "a pid that is not the pidfd's");
        assert!(!resident_peer(&a, me, "viper"), "a test is no unit's");
        let f = tempfile::tempfile().unwrap();
        assert!(!peer_in(&f, me, |_| true), "not a socket: no peer");
        // A peer that has gone: a child connects, and exits.
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("s");
        let l = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "socket::tests::helper_connects", "--ignored", "--test-threads=1"])
            .env("TORQ_TEST_CONNECT", &path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id() as i32;
        let (c, _) = l.accept().unwrap();
        // Exited, not yet reaped (a zombie): its pid is still its own and named, but the pidfd says it ended.
        let t = std::time::Instant::now();
        while !std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap().rsplit_once(") ").is_some_and(|(_, r)| r.starts_with('Z')) {
            assert!(t.elapsed() < std::time::Duration::from_secs(20), "the helper never ended");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(!peer_in(&c, pid, |_| true), "it exited: its pid could soon be anyone's");
        child.wait().unwrap();
        assert!(!peer_in(&c, pid, |_| true), "reaped: gone");
    }

    /// What serve sets `resident` by: the resident account, *and* its process in the unit — each needed. A test's
    /// process is in no unit, so the unit is said to be found here; the agent account in it is still no resident.
    #[test]
    fn residency_takes_the_uid_and_the_unit() {
        let me = std::process::id() as i32;
        let (a, _b) = std::os::unix::net::UnixStream::pair().unwrap();
        let in_any = |_: &str, _: &str| true;
        assert!(found_resident_by(&who(980), &a, me, in_any), "the resident account, in its unit");
        assert!(!found_resident_by(&who(980), &a, me, |_, _| false), "the resident account, outside its unit");
        assert!(!found_resident_by(&who(990), &a, me, in_any), "the agent account, even in the unit");
        assert!(!found_resident_by(&who(1000), &a, me, in_any), "a person");
        assert!(!found_resident_by(&who(982), &a, me, in_any), "named like a resident, not in the map");
        assert!(!found_resident(&who(980), &a, me), "a test is no unit's");
    }

    /// Not a test: the peer of `the_peer_is_read_by_its_pidfd`, which connects and ends.
    #[test]
    #[ignore]
    fn helper_connects() {
        if let Some(p) = std::env::var_os("TORQ_TEST_CONNECT") {
            let s = std::os::unix::net::UnixStream::connect(p).unwrap();
            std::mem::forget(s);
        }
    }

    /// ADR-051 3, amended 2026-11-11: a person's resident agent has its own account, which the residents' map
    /// names; at the broker it is that person's one agent (it acts as `torq-agent-<person>`), and it alone can be
    /// resident. The agent account never is, wherever it runs; a name alone makes nothing a resident.
    #[test]
    fn the_resident_account_is_the_persons_one_agent() {
        let r = who(980);
        assert_eq!((r.agent_for.as_deref(), r.acts_as.as_deref(), r.resident_account, r.person, r.admin), (Some("viper"), Some("torq-agent-viper"), true, false, false));
        let a = who(990);
        assert_eq!((a.acts_as.as_deref(), a.resident_account), (Some("torq-agent-viper"), false));
        // Named like one but not in the map (sam's), in the map for a person with no agent (dave), in the map with
        // a person's uid (pat): none is a resident, nor an agent.
        for uid in [982, 981, 1003] {
            let c = who(uid);
            assert!(c.agent_for.is_none() && !c.resident_account, "{c:?}");
            assert!(c.person || matches!(gate(&c, "propose", None, None), Gate::Refused(_)), "a service proposes nothing: {c:?}");
        }
        // As the person's agent account, both: the broker judges and counts them as one agent.
        let p = |c: &Caller| with_identity(&["propose".into(), "--base=c".into()], "propose", c, Some("/run/d"), None, None).unwrap();
        let s = |c: &Caller| with_identity(&["session-start".into()], "session-start", c, None, None, None).unwrap();
        let mut res = who(980);
        assert_eq!(s(&res), ["session-start", "--agent", "torq-agent-viper"], "its uid alone is not enough: serve says resident");
        res.resident = true;
        assert_eq!(p(&res), ["propose", "--base=c", "--actor", "agent", "--agent", "torq-agent-viper", "--resident", "--diff", "/run/d"]);
        assert_eq!(s(&res), ["session-start", "--agent", "torq-agent-viper", "--resident"]);
        // The agent account, even were it found in the unit's control group: never resident.
        let mut agent = who(990);
        agent.resident = true;
        assert!(!p(&agent).contains(&"--resident".to_string()) && !s(&agent).contains(&"--resident".to_string()));
        // It proposes, and nothing more, as the agent does.
        for cmd in ["apply", "revert", "decline", "trust-grant", "password", "fetch-incoming"] {
            assert!(matches!(gate(&who(980), cmd, Some(&change(Tier::User, &["users/viper/packages.json"])), Some("viper")), Gate::Refused(_)), "{cmd}");
        }
        assert_eq!(account("torq-resident-viper", PASSWD, GROUP, &agents(), &residents()).unwrap(), who(980));
    }

    #[test]
    fn a_pidfds_pid() {
        assert_eq!(fdinfo_pid("pos:\t0\nflags:\t02000002\nmnt_id:\t15\nino:\t1057\nPid:\t4242\nNSpid:\t4242\n"), Some(4242));
        assert_eq!(fdinfo_pid("Pid:\t-1\nNSpid:\t-1\n"), None, "exited");
        assert_eq!(fdinfo_pid("Pid:\t0\n"), None, "not in this namespace");
        assert_eq!(fdinfo_pid("pos:\t0\n"), None);
    }

    /// The broker is told the caller is the resident agent only by serve, never by the caller; and only for
    /// an agent's proposal and session start.
    #[test]
    fn resident_is_added_never_taken() {
        assert!(check_args(&["session-start".into(), "--resident".into()]).is_err());
        assert!(check_args(&["propose".into(), "--resident=true".into()]).is_err());
        // The person's resident account (ADR-051 3 amended 2026-11-11): the only one that can be resident.
        let mut a = who(980);
        let p = |a: &Caller| with_identity(&["propose".into(), "--base=c".into()], "propose", a, Some("/run/d"), None, None).unwrap();
        let s = |a: &Caller| with_identity(&["session-start".into()], "session-start", a, None, None, None).unwrap();
        assert!(!p(&a).contains(&"--resident".to_string()) && !s(&a).contains(&"--resident".to_string()), "not found so: not resident");
        a.resident = true;
        assert_eq!(p(&a), ["propose", "--base=c", "--actor", "agent", "--agent", "torq-agent-viper", "--resident", "--diff", "/run/d"]);
        assert_eq!(s(&a), ["session-start", "--agent", "torq-agent-viper", "--resident"]);
        let t = with_identity(&["session-taint".into(), "x".into()], "session-taint", &a, None, None, None).unwrap();
        assert_eq!(t, ["session-taint", "x", "--agent", "torq-agent-viper"]);
        // A person is never resident, whatever were set.
        let mut v = who(1000);
        v.resident = true;
        assert!(!p(&v).contains(&"--resident".to_string()));
    }

    #[test]
    fn reading_is_open_to_everyone() {
        for uid in [1000, 1001, 990] {
            for cmd in OBSERVE {
                assert_eq!(gate(&who(uid), cmd, None, None), Gate::Open, "{uid} {cmd}");
            }
        }
    }

    #[test]
    fn a_persons_own_settings_are_theirs() {
        let mine = change(Tier::User, &["users/sam/packages.json"]);
        // No root shell over the socket, for anyone: break-glass is sudo's, with who sudo says it is.
        for uid in [1000, 1001] {
            assert!(matches!(gate(&who(uid), "break-glass", None, None), Gate::Refused(_)), "{uid}");
            // A person without the group is refused; an administrator gives their password.
            for cmd in ["snapshot-take", "snapshot-delete", "restore-data"] {
                assert_eq!(gate(&who(1000), cmd, None, None), Gate::Password, "{cmd}");
                assert!(matches!(gate(&who(1001), cmd, None, None), Gate::Refused(_)), "{cmd}");
            }
        }
        assert_eq!(gate(&who(1001), "apply", Some(&mine), None), Gate::Open);
        assert_eq!(gate(&who(1001), "revert", Some(&mine), None), Gate::Open);
        // Someone else's are not: an administrator, with their password; anyone else, refused (6a).
        let theirs = change(Tier::User, &["users/viper/packages.json"]);
        assert!(matches!(gate(&who(1001), "apply", Some(&theirs), None), Gate::Refused(_)));
        assert_eq!(gate(&who(1000), "apply", Some(&mine), None), Gate::Password);
        // Their own and someone else's at once: someone else's.
        let both = change(Tier::User, &["users/sam/packages.json", "users/viper/packages.json"]);
        assert!(matches!(gate(&who(1001), "apply", Some(&both), None), Gate::Refused(_)));
    }

    #[test]
    fn a_system_change_asks_an_administrators_password() {
        let sys = change(Tier::System, &["nixos/services.json"]);
        assert_eq!(gate(&who(1000), "apply", Some(&sys), None), Gate::Password);
        assert!(matches!(gate(&who(1001), "apply", Some(&sys), None), Gate::Refused(_)));
        for cmd in ["confirm", "reconcile", "repair"] {
            assert_eq!(gate(&who(1000), cmd, None, None), Gate::Password, "{cmd}");
            assert!(matches!(gate(&who(1001), cmd, None, None), Gate::Refused(_)), "{cmd}");
        }
        assert!(matches!(gate(&who(1000), "apply", Some(&change(Tier::Forbidden, &[".git/config"])), None), Gate::Refused(_)));
        assert!(matches!(gate(&who(1000), "apply", None, None), Gate::Refused(_)), "no such change");
    }

    /// ADR-052: a person's own secret is theirs alone; the system's an administrator's; none of it the agent's.
    #[test]
    fn a_secret_is_its_owners() {
        assert_eq!(gate(&who(1001), "secret-set", None, Some("sam")), Gate::Open);
        assert_eq!(gate(&who(1001), "secret-delete", None, Some("sam")), Gate::Open);
        assert_eq!(gate(&who(1001), "secret-show", None, Some("sam")), Gate::Password, "read back with their password");
        // No one else sets or reads it, an administrator included, who may only delete it.
        for cmd in ["secret-set", "secret-show"] {
            assert!(matches!(gate(&who(1000), cmd, None, Some("sam")), Gate::Refused(_)), "{cmd}");
            assert!(matches!(gate(&who(1001), cmd, None, Some("viper")), Gate::Refused(_)), "{cmd}");
        }
        assert_eq!(gate(&who(1000), "secret-delete", None, Some("sam")), Gate::Password);
        assert!(matches!(gate(&who(1001), "secret-delete", None, Some("viper")), Gate::Refused(_)));
        for cmd in ["secret-set", "secret-delete", "secret-show"] {
            assert_eq!(gate(&who(1000), cmd, None, None), Gate::Password, "{cmd}: the system's");
            assert!(matches!(gate(&who(1001), cmd, None, None), Gate::Refused(_)), "{cmd}");
            for user in [None, Some("torq-agent"), Some("sam")] {
                assert!(matches!(gate(&who(990), cmd, None, user), Gate::Refused(_)), "{cmd} {user:?}: never the agent's");
            }
            assert!(matches!(gate(&who(65534), cmd, None, Some("nobody")), Gate::Refused(_)), "{cmd}: a service's account has none");
        }
        // Their rules too: an administrator's password changes no one else's, whatever else the change holds.
        let sams = change(Tier::User, &["users/sam/secrets.json"]);
        assert_eq!(gate(&who(1001), "apply", Some(&sams), None), Gate::Open);
        for cmd in ["apply", "apply-confirmed", "revert", "schedule"] {
            assert!(matches!(gate(&who(1000), cmd, Some(&sams), None), Gate::Refused(_)), "{cmd}");
        }
        let mixed = change(Tier::System, &["nixos/services.json", "users/sam/secrets.json"]);
        assert!(matches!(gate(&who(1000), "apply", Some(&mixed), None), Gate::Refused(_)));
        assert_eq!(gate(&who(1000), "apply", Some(&change(Tier::User, &["users/viper/secrets.json"])), None), Gate::Open, "their own, as any of theirs");
        // A use is the daemon's to ask, and it asks nothing else; no one else asks one, a person nor the agent.
        assert_eq!(gate(&who(989), "secret-use", None, None), Gate::Open);
        assert_eq!(gate(&who(989), "secret-agent", None, None), Gate::Open);
        for cmd in ["log", "secret-show", "secret-set", "propose", "apply"] {
            assert!(matches!(gate(&who(989), cmd, None, Some("sam")), Gate::Refused(_)), "{cmd}");
        }
        for uid in [1000, 1001, 990, 65534] {
            assert!(matches!(gate(&who(uid), "secret-use", None, None), Gate::Refused(_)), "{uid}");
            assert!(matches!(gate(&who(uid), "secret-agent", None, None), Gate::Refused(_)), "{uid}");
        }
        let s = with_identity(&["secret-set".into(), "--name=t".into(), "--user=sam".into()], "secret-set", &who(1001), None, None, Some("/run/v")).unwrap();
        assert_eq!(s, ["secret-set", "--name=t", "--user=sam", "--value-file", "/run/v", "--approved-by", "sam"]);
        assert!(with_identity(&["secret-set".into()], "secret-set", &who(1001), None, None, None).is_err(), "a value comes in the request");
        assert!(check_args(&["secret-set".into(), "--value-file=/etc/shadow".into()]).is_err());
    }

    #[test]
    fn a_password_is_the_persons_own_or_an_administrators() {
        assert_eq!(gate(&who(1001), "password", None, Some("sam")), Gate::Password);
        assert!(matches!(gate(&who(1001), "password", None, Some("viper")), Gate::Refused(_)));
        assert_eq!(gate(&who(1000), "password", None, Some("sam")), Gate::Password);
    }


    #[test]
    fn who_is_on_the_desktop() {
        let session = |id: &str| match id {
            "7" => Some("TYPE=wayland\nCLASS=user\n".to_string()),
            "4" => Some("TYPE=unspecified\nCLASS=manager\n".to_string()),
            "9" => Some("TYPE=tty\nREMOTE=1\n".to_string()),
            _ => None,
        };
        let cg = |p: &str| format!("0::{p}\n");
        let logged_in = "NAME=viper\nSESSIONS=7 4\n";
        // In the graphical session, and among its applications.
        assert!(on_the_desktop(&cg("/user.slice/user-1000.slice/session-7.scope"), session, Some(logged_in)));
        assert!(on_the_desktop(&cg("/user.slice/user-1000.slice/user@1000.service/app.slice/app-cosmic.scope"), session, Some(logged_in)));
        // SSH and the text console stay as they were, even with a desktop logged in beside them.
        assert!(!on_the_desktop(&cg("/user.slice/user-1000.slice/session-9.scope"), session, Some(logged_in)));
        // A user manager with no desktop logged in: a server's.
        assert!(!on_the_desktop(&cg("/user.slice/user-1000.slice/user@1000.service/app.slice/x.service"), session, Some("NAME=viper\nSESSIONS=9 4\n")));
        assert!(!on_the_desktop(&cg("/user.slice/user-1000.slice/user@1000.service/app.slice/x.service"), session, None));
        // A system service; a session no record names; nothing to read.
        assert!(!on_the_desktop(&cg("/system.slice/backdoor.service"), session, Some(logged_in)));
        assert!(!on_the_desktop(&cg("/user.slice/user-1000.slice/session-77.scope"), session, Some(logged_in)));
        assert!(!on_the_desktop("", session, Some(logged_in)));
    }

    #[test]
    fn a_process_is_named_by_when_it_started() {
        let rest: Vec<String> = (3..=30).map(|n| n.to_string()).collect();
        // A name with spaces and a `)` in it does not move the fields.
        assert_eq!(start_time(&format!("42 (ev)il na me) {}", rest.join(" "))), Some(22));
        assert_eq!(start_time("nonsense"), None);
    }
    #[test]
    fn the_agent_proposes_and_nothing_more() {
        let a = who(990);
        assert_eq!(gate(&a, "propose", None, None), Gate::Open);
        let own = change(Tier::User, &["users/torq-agent-viper/packages.json"]);
        for cmd in ["apply", "revert", "confirm", "reconcile", "repair", "password", "init", "setup", "expire-confirm", "serve"] {
            assert!(matches!(gate(&a, cmd, Some(&own), Some("torq-agent-viper")), Gate::Refused(_)), "{cmd}");
        }
    }

    #[test]
    fn the_machines_own_commands_are_not_on_the_socket() {
        for cmd in ["init", "setup", "expire-confirm", "serve", "push", "nonsense"] {
            assert!(matches!(gate(&who(1000), cmd, None, None), Gate::Refused(_)), "{cmd}");
        }
    }

    #[test]
    fn no_caller_names_itself_a_file_or_a_setting() {
        for bad in ["--actor", "--actor=agent", "--approved-by=viper", "--diff", "--hash-file=/etc/shadow", "--repo=/tmp/x", "--rebuild-cmd", "--state-dir"] {
            assert!(check_args(&["apply".into(), "3".into(), bad.into()]).is_err(), "{bad}");
        }
        assert!(check_args(&["--json".into(), "apply".into(), "3".into(), "--confirm".into(), "5".into(), "--confirm-tainted".into()]).is_ok());
        // A value that only looks like a flag is a value: `--reason=--actor` names nothing.
        assert!(check_args(&["propose".into(), "--reason=--actor x".into()]).is_ok());
    }

    #[test]
    fn identity_is_added_not_taken() {
        let a = with_identity(&["apply".into(), "3".into()], "apply", &who(1001), None, None, None).unwrap();
        assert_eq!(a, ["apply", "3", "--approved-by", "sam"]);
        assert!(with_identity(&["propose".into(), "--reason".into(), "x".into()], "propose", &who(1000), Some("/run/d"), None, None).is_err(), "no base");
        let p = with_identity(&["propose".into(), "--base=c".into(), "--reason".into(), "x".into()], "propose", &who(990), Some("/run/d"), None, None).unwrap();
        assert_eq!(p, ["propose", "--base=c", "--reason", "x", "--actor", "agent", "--agent", "torq-agent-viper", "--diff", "/run/d"]);
        assert!(with_identity(&["propose".into()], "propose", &who(1000), None, None, None).is_err(), "a proposal brings its diff");
    }


    /// Decline (ADR-051, amended): the person whose folder a proposal wholly is, or whose agent proposed
    /// it, without a password; anyone else's an administrator's, with theirs; never an agent, nor a
    /// service; only a proposal. Who declined is the kernel's answer.
    #[test]
    fn who_may_decline() {
        let sams = change(Tier::User, &["users/sam/packages.json"]);
        let vipers = change(Tier::User, &["users/viper/packages.json"]);
        let sys = change(Tier::System, &["nixos/services.json"]);
        assert_eq!(gate(&who(1001), "decline", Some(&sams), None), Gate::Open, "sam's own folder");
        assert_eq!(gate(&who(1001), "decline", Some(&sys), Some("sam")), Gate::Open, "sam's agent proposed it");
        assert!(matches!(gate(&who(1001), "decline", Some(&vipers), None), Gate::Refused(_)), "another's, not an administrator");
        assert!(matches!(gate(&who(1001), "decline", Some(&vipers), Some("viper")), Gate::Refused(_)), "another's agent's");
        assert!(matches!(gate(&who(1001), "decline", Some(&sys), None), Gate::Refused(_)));
        assert_eq!(gate(&who(1000), "decline", Some(&sams), Some("sam")), Gate::Password, "an administrator, with their password");
        assert_eq!(gate(&who(1000), "decline", Some(&vipers), None), Gate::Open, "viper's own folder");
        for agent in [990, 991] {
            for c in [&sams, &vipers, &sys] {
                assert!(matches!(gate(&who(agent), "decline", Some(c), Some("sam")), Gate::Refused(_)), "an agent declines nothing");
            }
        }
        assert!(matches!(gate(&who(65534), "decline", Some(&sams), None), Gate::Refused(_)), "a service");
        assert!(matches!(gate(&who(1000), "decline", None, None), Gate::Refused(_)), "not a proposal");
        let a = with_identity(&["decline".into(), "7".into()], "decline", &who(1001), None, None, None).unwrap();
        assert_eq!(a, ["decline", "7", "--approved-by", "sam"]);
    }
    /// The audit of ADR-040 (2026-11-04, its 2): `apply --confirm` holds every other change back
    /// until it is confirmed or expires — an administrator's to do, own folder or not.
    #[test]
    fn holding_the_machine_is_an_administrators() {
        let mine = change(Tier::User, &["users/sam/packages.json"]);
        assert!(matches!(gate(&who(1001), "apply-confirmed", Some(&mine), None), Gate::Refused(_)));
        assert_eq!(gate(&who(1000), "apply-confirmed", Some(&mine), None), Gate::Password);
        assert!(matches!(gate(&who(990), "apply-confirmed", Some(&mine), None), Gate::Refused(_)));
    }

    /// `commit at`: approved now, as an apply is (a person's own settings theirs, a system change an
    /// administrator's with their password); cancelled by an administrator; run only by its timer.
    #[test]
    fn a_commit_at_is_approved_as_an_apply_is() {
        let sys = change(Tier::System, &["nixos/services.json"]);
        let mine = change(Tier::User, &["users/sam/packages.json"]);
        assert_eq!(gate(&who(1000), "schedule", Some(&sys), None), Gate::Password);
        assert!(matches!(gate(&who(1001), "schedule", Some(&sys), None), Gate::Refused(_)));
        assert_eq!(gate(&who(1001), "schedule", Some(&mine), None), Gate::Open);
        assert!(matches!(gate(&who(1000), "schedule", None, None), Gate::Refused(_)), "no such proposal");
        assert!(matches!(gate(&who(1000), "schedule", Some(&change(Tier::Forbidden, &[".git/config"])), None), Gate::Refused(_)));
        assert!(matches!(gate(&who(990), "schedule", Some(&mine), None), Gate::Refused(_)), "the agent");
        assert_eq!(gate(&who(1000), "cancel-scheduled", None, None), Gate::Password);
        assert!(matches!(gate(&who(1001), "cancel-scheduled", None, None), Gate::Refused(_)));
        // Its approver cancels their own without a password (`user`: who approved it, as the ledger says).
        assert_eq!(gate(&who(1001), "cancel-scheduled", None, Some("sam")), Gate::Open);
        assert!(matches!(gate(&who(1001), "cancel-scheduled", None, Some("viper")), Gate::Refused(_)));
        assert!(matches!(gate(&who(990), "cancel-scheduled", None, Some("torq-agent-viper")), Gate::Refused(_)), "the agent cancels nothing");
        assert_eq!(account("sam", PASSWD, GROUP, &agents(), &residents()).unwrap(), who(1001));
        assert!(account("nobody-here", PASSWD, GROUP, &agents(), &residents()).is_err());
        assert!(account("sam", "sam:x:1000:100::/:/bin/sh\nviper:x:1000:100::/:/bin/sh\n", GROUP, &agents(), &residents()).is_ok());
        assert!(account("viper", "sam:x:1000:100::/:/bin/sh\nviper:x:1000:100::/:/bin/sh\n", GROUP, &agents(), &residents()).is_err(), "a uid shared is not one account");
        for uid in [1000, 1001, 990] {
            assert!(matches!(gate(&who(uid), "run-scheduled", None, None), Gate::Refused(_)), "{uid}");
            assert!(matches!(gate(&who(uid), "resume-scheduled", None, None), Gate::Refused(_)), "{uid}");
            assert_eq!(gate(&who(uid), "scheduled", None, None), Gate::Open, "{uid}");
        }
        let a = with_identity(&["schedule".into(), "3".into(), "--at=x".into()], "schedule", &who(1001), None, None, None).unwrap();
        assert_eq!(a, ["schedule", "3", "--at=x", "--approved-by", "sam"]);
        assert!(check_args(&["schedule".into(), "--commit-at-cmd=true".into()]).is_err());
        assert!(check_args(&["schedule".into(), "--commit-at-cancel-cmd".into()]).is_err());
    }

    /// ADR-042 (amended): the agent's sessions are the agent's; trust in it is a person's, given with
    /// their own password, for their own folder (the approver, as the kernel says), ended without one.
    #[test]
    fn sessions_are_the_agents_and_trust_a_persons() {
        let a = who(990);
        for cmd in ["session-start", "session-taint"] {
            assert_eq!(gate(&a, cmd, None, None), Gate::Open, "{cmd}");
            for uid in [1000, 1001, 65534] {
                assert!(matches!(gate(&who(uid), cmd, None, None), Gate::Refused(_)), "{cmd} {uid}");
            }
        }
        for cmd in ["trust-grant", "trust-revoke"] {
            assert!(matches!(gate(&a, cmd, None, None), Gate::Refused(_)), "the agent trusts itself with nothing: {cmd}");
            assert!(matches!(gate(&who(65534), cmd, None, None), Gate::Refused(_)), "{cmd}");
        }
        // Not only an administrator's: any person's, for their own folder, with their password.
        assert_eq!(gate(&who(1001), "trust-grant", None, None), Gate::Password);
        assert_eq!(gate(&who(1000), "trust-grant", None, None), Gate::Password);
        assert_eq!(gate(&who(1001), "trust-revoke", None, None), Gate::Open);
        for uid in [1000, 1001, 990] {
            assert_eq!(gate(&who(uid), "trust", None, None), Gate::Open);
            assert_eq!(gate(&who(uid), "session", None, None), Gate::Open);
        }
        // Whose trust it is, is the kernel's answer, never the caller's.
        let g = with_identity(&["trust-grant".into(), "--minutes=30".into()], "trust-grant", &who(1001), None, None, None).unwrap();
        assert_eq!(g, ["trust-grant", "--minutes=30", "--approved-by", "sam"]);
        assert!(check_args(&["trust-grant".into(), "--minutes=30".into(), "--approved-by=viper".into()]).is_err());
        let r = with_identity(&["trust-revoke".into()], "trust-revoke", &who(1000), None, None, None).unwrap();
        assert_eq!(r, ["trust-revoke", "--approved-by", "viper"]);
    }

    /// The review of pull: a fetch goes to the network, as root — a person's, not everyone's.
    #[test]
    fn only_people_fetch_from_the_remote() {
        assert_eq!(gate(&who(1001), "fetch-incoming", None, None), Gate::Open);
        assert!(matches!(gate(&who(990), "fetch-incoming", None, None), Gate::Refused(_)), "the agent");
        assert!(matches!(gate(&who(65534), "fetch-incoming", None, None), Gate::Refused(_)), "nobody");
    }

    /// Its 5: a service's account (nobody, sshd) proposes nothing — every proposal stays in the
    /// ledger for good; reading is still everyone's.
    #[test]
    fn services_propose_nothing() {
        for uid in [65534, 998] {
            assert!(matches!(gate(&who(uid), "propose", None, None), Gate::Refused(_)), "{uid}");
            assert_eq!(gate(&who(uid), "log", None, None), Gate::Open, "{uid}");
        }
    }

    /// ADR-038: a backup's key and a restore from it are an administrator's, with their password; the
    /// list is everyone's. The key comes in the request, never as a path, and never from the agent.
    #[test]
    fn a_backup_is_an_administrators() {
        for cmd in ["backup-key", "backup-restore"] {
            assert_eq!(gate(&who(1000), cmd, None, None), Gate::Password, "{cmd}");
            for uid in [1001, 990, 65534] {
                assert!(matches!(gate(&who(uid), cmd, None, None), Gate::Refused(_)), "{cmd} {uid}");
            }
        }
        for uid in [1000, 1001, 990] {
            assert_eq!(gate(&who(uid), "backups", None, None), Gate::Open, "{uid}");
        }
        assert!(check_args(&["backup-key".into(), "--name=home".into(), "--key-file=/etc/shadow".into()]).is_err());
        let k = with_identity(&["backup-key".into(), "--name".into(), "home".into()], "backup-key", &who(1000), None, None, Some("/run/k")).unwrap();
        assert_eq!(k, ["backup-key", "--name", "home", "--key-file", "/run/k", "--approved-by", "viper"]);
        assert!(with_identity(&["backup-key".into()], "backup-key", &who(1000), None, None, None).is_err(), "a key comes in the request");
        let r = with_identity(&["backup-restore".into(), "--name=home".into(), "--path=/home/sam".into()], "backup-restore", &who(1000), None, None, None).unwrap();
        assert_eq!(r.last().map(String::as_str), Some("viper"));
    }
}
