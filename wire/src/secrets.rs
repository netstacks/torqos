//! The rules of a machine's secrets (ADR-052): what each may be used for, by whom, and when a person is asked.
//! Two files of the configuration hold them, in this one shape: `secrets/system.json`, an administrator's, and
//! `users/<person>/secrets.json`, the person's own. The values are never here: the broker keeps them outside
//! the repository. The broker checks a file's shape before it is committed, torqcli writes it, and the daemon
//! reads it to decide each use. All three use this module, so a word one of them knows they all know.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Where the system's rules are, in the configuration repository.
pub const SYSTEM_RULES: &str = "secrets/system.json";
/// A person's own rules: `users/<person>/` and this.
pub const PERSON_RULES: &str = "secrets.json";
/// The agent's account (ADR-041): the only actor besides people.
pub const AGENT: &str = "torq-agent";

/// The most secrets one file names, and the most hosts one use names.
pub const MAX_SECRETS: usize = 64;
pub const MAX_HOSTS: usize = 16;
/// The most a rules file may hold, in bytes.
pub const MAX_FILE: u64 = 64 * 1024;

/// One file's rules: each secret by name.
pub type Rules = BTreeMap<String, Rule>;

/// What a secret is, which decides what it can be used for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// A token or a password sent in an HTTPS header.
    Token,
    /// An SSH private key: for SSH and for signing, never sent anywhere.
    SshKey,
}

/// When a person is asked before the agent uses the secret. A tainted session is always asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Approve {
    EachUse,
    #[default]
    FirstUse,
    Never,
}

impl Approve {
    pub fn word(self) -> &'static str {
        match self {
            Approve::EachUse => "each-use",
            Approve::FirstUse => "first-use",
            Approve::Never => "never",
        }
    }

    pub fn from_word(w: &str) -> Option<Approve> {
        [Approve::EachUse, Approve::FirstUse, Approve::Never].into_iter().find(|a| a.word() == w)
    }
}

/// An HTTPS use: the hosts it may be sent to, and the header it goes in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Https {
    pub hosts: Vec<String>,
    /// The header's name (`Authorization`).
    #[serde(default = "authorization")]
    pub header: String,
    /// What comes before the value in it (`Bearer `): may be empty.
    #[serde(default = "bearer")]
    pub prefix: String,
}

fn authorization() -> String {
    "Authorization".into()
}

fn bearer() -> String {
    "Bearer ".into()
}

/// An SSH use: the hosts the key may sign for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ssh {
    pub hosts: Vec<String>,
}

/// One secret's rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub https: Option<Https>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh: Option<Ssh>,
    /// Who may use it besides its owner: `torq-agent`, or people (a system secret's).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actors: Vec<String>,
    #[serde(default)]
    pub approve: Approve,
}

impl Rule {
    /// A new secret's rule: nobody's use yet.
    pub fn new(kind: Kind) -> Rule {
        Rule { kind, https: None, ssh: None, actors: Vec::new(), approve: Approve::default() }
    }
}

/// A secret's name: letters, digits, `-` and `_`, as it names a file. At most 64.
pub fn name_ok(n: &str) -> bool {
    !n.is_empty() && n.len() <= 64 && !n.starts_with('-') && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// A host a secret may go to: a DNS name, lowercase, with at least one dot (no `localhost`, no bare name a
/// search domain could finish), no address and no wildcard. An address is refused: a rule names who the
/// certificate is for, not where it happens to be.
pub fn host_ok(h: &str) -> bool {
    let label = |l: &str| !l.is_empty() && l.len() <= 63 && !l.starts_with('-') && !l.ends_with('-') && l.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    let labels: Vec<&str> = h.split('.').collect();
    h.len() <= 253 && labels.len() >= 2 && labels.iter().all(|l| label(l)) && !labels.last().is_some_and(|l| l.chars().all(|c| c.is_ascii_digit()))
}

/// An HTTP header's name: a token (RFC 9110), never one HTTP itself decides with.
fn header_ok(h: &str) -> bool {
    const HTTP_S_OWN: &[&str] = &["host", "content-length", "transfer-encoding", "connection", "upgrade", "te", "trailer", "proxy-authorization", "cookie"];
    !h.is_empty() && h.len() <= 64 && h.chars().all(|c| c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(c)) && !HTTP_S_OWN.contains(&h.to_ascii_lowercase().as_str())
}

/// What precedes the value in its header: printable, no line break.
fn prefix_ok(p: &str) -> bool {
    p.len() <= 32 && p.chars().all(|c| c == ' ' || c.is_ascii_graphic())
}

/// An account that may use a secret: the agent, or a person's name.
fn actor_ok(a: &str) -> bool {
    a == AGENT || (!a.is_empty() && a.len() <= 32 && a.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c == '_') && a.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-'))
}

fn hosts_ok(hosts: &[String]) -> Result<(), String> {
    if hosts.is_empty() || hosts.len() > MAX_HOSTS {
        return Err(format!("a use names 1 to {MAX_HOSTS} hosts"));
    }
    if let Some(h) = hosts.iter().find(|h| !host_ok(h)) {
        return Err(format!("{h:?} is not a host name (lowercase, with a dot; no address, no wildcard)"));
    }
    Ok(())
}

/// Why `rules` is not a rules file, or None.
pub fn check(rules: &Rules) -> Result<(), String> {
    if rules.len() > MAX_SECRETS {
        return Err(format!("at most {MAX_SECRETS} secrets"));
    }
    for (name, r) in rules {
        let at = |why: String| format!("secret {name}: {why}");
        if !name_ok(name) {
            return Err(format!("{name:?} is not a secret's name (letters, digits, - and _)"));
        }
        match (r.kind, &r.https, &r.ssh) {
            (Kind::Token, _, Some(_)) => return Err(at("a token is not an SSH key: it has no ssh use".into())),
            (Kind::SshKey, Some(_), _) => return Err(at("an SSH key is never sent: it has no https use".into())),
            _ => {}
        }
        if let Some(h) = &r.https {
            hosts_ok(&h.hosts).map_err(at)?;
            if !header_ok(&h.header) {
                return Err(at(format!("{:?} is not a header it may go in", h.header)));
            }
            if !prefix_ok(&h.prefix) {
                return Err(at(format!("{:?} is not a header's prefix", h.prefix)));
            }
        }
        if let Some(s) = &r.ssh {
            hosts_ok(&s.hosts).map_err(at)?;
        }
        if r.actors.len() > MAX_SECRETS {
            return Err(at("too many actors".into()));
        }
        if let Some(a) = r.actors.iter().find(|a| !actor_ok(a)) {
            return Err(at(format!("{a:?} is not an account")));
        }
    }
    Ok(())
}

/// A rules file read and checked.
pub fn parse(bytes: &[u8]) -> Result<Rules, String> {
    if bytes.len() as u64 > MAX_FILE {
        return Err(format!("larger than {} KiB", MAX_FILE / 1024));
    }
    let rules: Rules = serde_json::from_slice(bytes).map_err(|e| format!("not a rules file: {e}"))?;
    check(&rules)?;
    Ok(rules)
}

/// A rules file as it is written: sorted, two-space indented, ending in a newline.
pub fn render(rules: &Rules) -> String {
    let mut s = serde_json::to_string_pretty(rules).unwrap_or_else(|_| "{}".into());
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(json: &str) -> Result<Rules, String> {
        parse(json.as_bytes())
    }

    #[test]
    fn a_rule_as_written() {
        let r = one(r#"{"github-ci":{"kind":"token","https":{"hosts":["api.github.com"]},"actors":["torq-agent"]}}"#).unwrap();
        let g = &r["github-ci"];
        assert_eq!(g.approve, Approve::FirstUse, "asked the first time, unless the rule says otherwise");
        let h = g.https.as_ref().unwrap();
        assert_eq!((h.header.as_str(), h.prefix.as_str()), ("Authorization", "Bearer "));
        assert_eq!(parse(render(&r).as_bytes()).unwrap(), r, "what is written reads back the same");
    }

    #[test]
    fn a_use_goes_only_where_it_can_be_named() {
        for bad in ["localhost", "github", "10.0.0.1", "1.2.3.4", "*.github.com", "API.github.com", "a..b", "-a.com", "a.com.", "exa mple.com", "evil.com/x", "a.com:443"] {
            assert!(!host_ok(bad), "{bad}");
        }
        for good in ["api.github.com", "git.example.org", "a1.b-c.io"] {
            assert!(host_ok(good), "{good}");
        }
        assert!(one(r#"{"t":{"kind":"token","https":{"hosts":[]}}}"#).is_err(), "a use names a host");
    }

    #[test]
    fn a_secret_is_used_only_as_what_it_is() {
        assert!(one(r#"{"t":{"kind":"token","ssh":{"hosts":["github.com"]}}}"#).is_err());
        assert!(one(r#"{"k":{"kind":"ssh-key","https":{"hosts":["api.github.com"]}}}"#).is_err());
        assert!(one(r#"{"k":{"kind":"ssh-key","ssh":{"hosts":["github.com"]}}}"#).is_ok());
    }

    #[test]
    fn nothing_odd_is_taken() {
        for bad in [
            r#"{"t":{"kind":"token","shell":true}}"#,
            r#"{"t":{"kind":"password"}}"#,
            r#"{"../x":{"kind":"token"}}"#,
            r#"{"t":{"kind":"token","https":{"hosts":["a.com"],"header":"Host"}}}"#,
            r#"{"t":{"kind":"token","https":{"hosts":["a.com"],"header":"X-A\r\nB"}}}"#,
            r#"{"t":{"kind":"token","https":{"hosts":["a.com"],"prefix":"a\nb"}}}"#,
            r#"{"t":{"kind":"token","actors":["Root"]}}"#,
            r#"{"t":{"kind":"token","approve":"sometimes"}}"#,
            r#"[]"#,
        ] {
            assert!(one(bad).is_err(), "{bad}");
        }
        assert!(parse(&vec![b' '; MAX_FILE as usize + 1]).is_err());
    }

    #[test]
    fn names() {
        assert!(name_ok("github-ci") && name_ok("a_1") && !name_ok("") && !name_ok("-x") && !name_ok("a/b") && !name_ok(".x") && !name_ok(&"a".repeat(65)));
        assert_eq!(Approve::from_word("each-use"), Some(Approve::EachUse));
        assert_eq!(Approve::from_word("x"), None);
    }
}
