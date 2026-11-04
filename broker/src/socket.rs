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
use serde::{Deserialize, Serialize};

/// Where the socket is (modules/broker.nix sets the unit to it).
pub const SOCKET: &str = "/run/torq/broker.sock";
/// The agent's account (ADR-035): whatever arrives from it is the agent.
pub const AGENT: &str = "torq-agent";
/// The PAM service a system change's password is checked against (modules/broker.nix).
pub const PAM_SERVICE: &str = "torq-approve";
/// The group whose members may approve a system change (with their password).
pub const ADMINS: &str = "wheel";
/// The exit code that says: this needs your password (none was sent, or it was wrong).
pub const NEED_PASSWORD: i32 = 77;
/// A request is a line, and no line is longer than this (a large diff, with room).
pub const MAX_REQUEST: u64 = 8 << 20;

/// What a caller sends: the broker's arguments, without who it is or any file. A diff or a
/// password hash comes in the request itself, never as a path for root to open (N16).
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
}

/// What comes back, one JSON line at a time: the broker's standard output and error as they come,
/// and how it ended, last.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Frame {
    Out(String),
    Err(String),
    Exit(i32),
}

/// The account at the other end of the socket, as the kernel and the machine's own files say.
#[derive(Debug, Clone, PartialEq)]
pub struct Caller {
    pub uid: u32,
    pub name: String,
    pub agent: bool,
    pub admin: bool,
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
    "--current-system", "--system-profile", "--actor", "--approved-by", "--diff", "--hash-file",
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

/// The account with `uid`, from /etc/passwd's text; an admin if /etc/group's `wheel` lists it.
pub fn caller(uid: u32, passwd: &str, group: &str) -> Result<Caller> {
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
    Ok(Caller { uid, agent: name == AGENT, name, admin })
}

/// Read-only commands: open to every account (the ledger says what changed, never a secret: a
/// password's hash is kept outside it).
const OBSERVE: &[&str] = &["log", "verify", "status", "show"];

/// What `cmd` needs from `who`: `change` is what it acts on (`apply`, `revert`), `user` whose
/// password it sets (`password`).
pub fn gate(who: &Caller, cmd: &str, change: Option<&Change>, user: Option<&str>) -> Gate {
    let admins_only = || {
        if who.admin {
            Gate::Password
        } else {
            Gate::Refused(format!("{} is not an administrator ({ADMINS}): a system change is theirs to approve", who.name))
        }
    };
    if OBSERVE.contains(&cmd) {
        return Gate::Open;
    }
    if who.agent {
        return if cmd == "propose" {
            Gate::Open
        } else {
            Gate::Refused("the agent proposes; a person applies".into())
        };
    }
    match cmd {
        "propose" => Gate::Open,
        "apply" | "revert" => match change {
            None => Gate::Refused("no such change".into()),
            Some(c) if c.tier == Tier::Forbidden => Gate::Refused("that change touches what no one may change".into()),
            // A person's own settings are theirs: no password (KNOWN-GAPS 6a — someone else's are not).
            Some(c) if c.tier <= Tier::User && !c.files.is_empty() && c.files.iter().all(|f| own(&who.name, f)) => Gate::Open,
            Some(_) => admins_only(),
        },
        // Their own password: the current one first, as passwd asks.
        "password" if user == Some(who.name.as_str()) => Gate::Password,
        "password" | "confirm" | "reconcile" | "repair" => admins_only(),
        other => Gate::Refused(format!("`{other}` is not run over the socket (it is the machine's own, as root)")),
    }
}

/// Whether `file` is in `name`'s own folder of the configuration.
fn own(name: &str, file: &str) -> bool {
    policy::is_user_data(file) && file.strip_prefix("users/").and_then(|r| r.split_once('/')).is_some_and(|(n, _)| n == name)
}

/// The arguments the broker runs with: the caller's, then who it is, as this module decided.
/// `diff` and `hash` are the private files the request's own came to.
pub fn with_identity(args: &[String], cmd: &str, who: &Caller, diff: Option<&str>, hash: Option<&str>) -> Result<Vec<String>> {
    let mut a = args.to_vec();
    match cmd {
        "propose" => {
            a.extend(["--actor".into(), if who.agent { "agent".into() } else { "user".into() }]);
            a.extend(["--diff".into(), diff.context("a proposal needs its diff, in the request")?.into()]);
        }
        "password" => {
            a.extend(["--hash-file".into(), hash.context("a password needs its hash, in the request")?.into()]);
            a.extend(["--approved-by".into(), who.name.clone()]);
        }
        "apply" | "revert" | "confirm" | "reconcile" | "repair" => a.extend(["--approved-by".into(), who.name.clone()]),
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

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWD: &str = "root:x:0:0::/root:/bin/sh\nviper:x:1000:100::/home/viper:/bin/zsh\nsam:x:1001:100::/home/sam:/bin/zsh\ntorq-agent:x:990:990::/var/empty:/run/current-system/sw/bin/nologin\n";
    const GROUP: &str = "root:x:0:\nwheel:x:1:viper\nusers:x:100:sam\n";

    fn who(uid: u32) -> Caller {
        caller(uid, PASSWD, GROUP).unwrap()
    }
    fn change(tier: Tier, files: &[&str]) -> Change {
        Change { tier, files: files.iter().map(|f| f.to_string()).collect() }
    }

    #[test]
    fn the_caller_is_the_uid() {
        assert_eq!(who(1000), Caller { uid: 1000, name: "viper".into(), agent: false, admin: true });
        assert_eq!(who(1001), Caller { uid: 1001, name: "sam".into(), agent: false, admin: false });
        assert!(who(990).agent && !who(990).admin);
        assert!(caller(4242, PASSWD, GROUP).is_err(), "no account");
        // A name that is only a prefix of a wheel member's is not one.
        assert!(!caller(1, "vipe:x:1:1::/:/bin/sh\n", GROUP).unwrap().admin);
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

    #[test]
    fn a_password_is_the_persons_own_or_an_administrators() {
        assert_eq!(gate(&who(1001), "password", None, Some("sam")), Gate::Password);
        assert!(matches!(gate(&who(1001), "password", None, Some("viper")), Gate::Refused(_)));
        assert_eq!(gate(&who(1000), "password", None, Some("sam")), Gate::Password);
    }

    #[test]
    fn the_agent_proposes_and_nothing_more() {
        let a = who(990);
        assert_eq!(gate(&a, "propose", None, None), Gate::Open);
        let own = change(Tier::User, &["users/torq-agent/packages.json"]);
        for cmd in ["apply", "revert", "confirm", "reconcile", "repair", "password", "init", "setup", "expire-confirm", "serve"] {
            assert!(matches!(gate(&a, cmd, Some(&own), Some(AGENT)), Gate::Refused(_)), "{cmd}");
        }
    }

    #[test]
    fn the_machines_own_commands_are_not_on_the_socket() {
        for cmd in ["init", "setup", "expire-confirm", "serve", "nonsense"] {
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
        let a = with_identity(&["apply".into(), "3".into()], "apply", &who(1001), None, None).unwrap();
        assert_eq!(a, ["apply", "3", "--approved-by", "sam"]);
        let p = with_identity(&["propose".into(), "--reason".into(), "x".into()], "propose", &who(990), Some("/run/d"), None).unwrap();
        assert_eq!(p, ["propose", "--reason", "x", "--actor", "agent", "--diff", "/run/d"]);
        assert!(with_identity(&["propose".into()], "propose", &who(1000), None, None).is_err(), "a proposal brings its diff");
    }
}
