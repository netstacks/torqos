//! TorqOS's releases (ADR-028): which repositories are them, one rule for torqcli and the broker, so the
//! two cannot drift. A source there must be a commit the release key signed, at lock (torqcli) and at
//! apply (the broker). The machine's setting (/etc/torq/releases.json, modules/releases.nix) names the
//! releases' address now, and the addresses they had before (`former`): a machine installed from an older
//! release still names one in its flake.nix, and it is the same repository, under its old name, so it is
//! checked the same (ADR-028, amended 2026-11-12).
//!
//! The releases are taken as git only: a git lock names a commit, and what is built is that commit's tree,
//! which is what the signature covers. A tarball of them (`github:`, an archive URL) is not bound to the
//! commit the same way, so it is refused, never taken as checked. An address is compared as these spellings
//! of it: any scheme, without user information, the scheme's default port, `www.`, `.git` or a final `/`,
//! percent-escapes decoded, in any case. Another spelling is another address.

use serde::Deserialize;
use serde_json::Value;

/// Where the machine's setting is.
pub const FILE: &str = "/etc/torq/releases.json";

/// The machine's releases. Fields a newer TorqOS adds are passed by (an older broker may read its setting
/// during an upgrade or after a rollback); the two it reads must be there and sound.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Releases {
    /// The releases now, as a flake reference: `git+https://…/torqos-releases?ref=stable`.
    pub url: String,
    /// Their earlier addresses (`https://…`), still the releases.
    #[serde(default)]
    pub former: Vec<String>,
}

/// What a locked input is, as the releases go.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Not the releases: nothing of the release key's to check.
    Other,
    /// The releases as git, at this commit: its signature is what is checked.
    Release(String),
    /// The releases in a form whose content is not bound to a commit the key signed: refused, and why.
    Refused(String),
}

impl Releases {
    /// The setting's text, read. A git repository (`git+…`): over HTTPS on a machine (modules/releases.nix
    /// allows nothing else), a folder in a test.
    pub fn parse(text: &str) -> Result<Releases, String> {
        let r: Releases = serde_json::from_str(text).map_err(|e| format!("not the releases' setting: {e}"))?;
        let at = base(&r.url).strip_prefix("git+").unwrap_or_default();
        if !at.contains("://") || at.ends_with("://") {
            return Err(format!("the releases are a git repository (git+https://…), not {}", r.url));
        }
        Ok(r)
    }

    /// The releases' repository as git fetches it now: `https://…`, as the setting writes it.
    pub fn fetch_url(&self) -> String {
        base(&self.url).trim_start_matches("git+").to_string()
    }

    /// Their addresses, as compared: now, then each earlier one.
    fn places(&self) -> impl Iterator<Item = String> + '_ {
        std::iter::once(place(&self.url)).chain(self.former.iter().map(|f| place(f)))
    }

    /// Whether `repo` (any of the spellings above) is the releases, under their name now or an earlier one.
    pub fn is_repo(&self, repo: &str) -> bool {
        let at = place(repo);
        self.places().any(|p| p == at)
    }

    /// What a locked input is: the releases as git (at a commit), the releases in another form (refused),
    /// or something else.
    pub fn verdict(&self, locked: &Value) -> Verdict {
        let rev = || locked["rev"].as_str().map(String::from);
        match locked["type"].as_str() {
            Some("git") => match locked["url"].as_str() {
                Some(url) if self.is_repo(url) => match rev() {
                    Some(r) => Verdict::Release(r),
                    None => Verdict::Refused("the releases' lock names no commit".into()),
                },
                _ => Verdict::Other,
            },
            // A tarball named by owner and repository (`github:`; its `host` attribute or none): if it names the
            // releases, by any host, it is refused, not checked.
            Some("github" | "gitlab" | "sourcehut") => {
                let named = locked["owner"].as_str().zip(locked["repo"].as_str()).map(|(o, r)| format!("https://github.com/{o}/{r}"));
                if named.is_some_and(|n| self.is_repo(&n)) {
                    Verdict::Refused(TARBALL.into())
                } else {
                    Verdict::Other
                }
            }
            // A tarball or file by URL: the releases' own address or anything under it (`…/archive/<rev>.tar.gz`,
            // `….git/archive/…`), and for a GitHub address, GitHub's tarball hosts for it (`codeload.github.com/<o>/<r>/…`,
            // `api.github.com/repos/<o>/<r>/…`). These exactly; another host serving them is another address.
            Some("tarball" | "file") => match locked["url"].as_str() {
                Some(url) if self.places().any(|p| tarball_of(&place(url), &p)) => Verdict::Refused(TARBALL.into()),
                _ => Verdict::Other,
            },
            _ => Verdict::Other,
        }
    }

    /// Whether a lock's attribute set (its `original`, or its `locked`) names the releases at all, in any form.
    pub fn names(&self, attrs: &Value) -> bool {
        self.verdict(attrs) != Verdict::Other
    }

    /// A source (`git+https://…?ref=…`) at an earlier address of the releases, as the same source at their
    /// address now (its `?ref=…` kept). None: not at an earlier address (a `github:` source is not moved: it is
    /// refused as a tarball at its lock).
    pub fn moved(&self, source: &str) -> Option<String> {
        let at = base(source).strip_prefix("git+")?;
        let p = place(at);
        if p == place(&self.url) || !self.former.iter().any(|f| place(f) == p) {
            return None;
        }
        Some(format!("{}{}", base(&self.url), &source[base(source).len()..]))
    }
}

const TARBALL: &str = "the releases are taken as git (git+https://…/torqos-releases?ref=…), so that what is built is the commit whose signature is checked; a tarball of them (github:, an archive) is not bound to that commit";

/// A flake reference without its query or fragment, and without a `/` at its end.
fn base(url: &str) -> &str {
    url.split(['?', '#']).next().unwrap_or(url).trim_end_matches('/')
}

/// An address as compared: host and path, without scheme, user information, the scheme's default port or
/// `www.`; the path percent-decoded, without `.git` or a final `/`; all in lowercase (GitHub's are case-blind).
pub fn place(url: &str) -> String {
    let url = base(url.strip_prefix("git+").unwrap_or(url));
    let (scheme, rest) = url.split_once("://").unwrap_or(("", url));
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let host_port = authority.rsplit('@').next().unwrap_or(authority).to_ascii_lowercase();
    let default = match scheme.to_ascii_lowercase().as_str() {
        "https" => ":443",
        "http" => ":80",
        "ssh" => ":22",
        "git" => ":9418",
        _ => "",
    };
    let host = host_port.strip_suffix(default).filter(|_| !default.is_empty()).unwrap_or(&host_port);
    let host = host.strip_prefix("www.").unwrap_or(host);
    let path = decoded(path).to_ascii_lowercase();
    let path = path.trim_end_matches('/');
    format!("{host}{}", path.strip_suffix(".git").unwrap_or(path).trim_end_matches('/'))
}

/// `%XX` escapes decoded; anything else as it is.
fn decoded(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        match (b[i], b.get(i + 1).copied().and_then(hex), b.get(i + 2).copied().and_then(hex)) {
            (b'%', Some(h), Some(l)) => {
                out.push((h * 16 + l) as u8);
                i += 3;
            }
            (c, _, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Whether address `a` is `repo` or a path under it (`repo/…`, or `repo.git/…`).
fn under(a: &str, repo: &str) -> bool {
    a == repo || [format!("{repo}/"), format!("{repo}.git/")].iter().any(|p| a.starts_with(p.as_str()))
}

/// Whether address `a` is a tarball of `repo`: under it, or for a GitHub repository under GitHub's tarball hosts.
fn tarball_of(a: &str, repo: &str) -> bool {
    under(a, repo)
        || repo.strip_prefix("github.com/").is_some_and(|or| under(a, &format!("codeload.github.com/{or}")) || under(a, &format!("api.github.com/repos/{or}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn releases() -> Releases {
        Releases::parse(r#"{"url":"git+https://github.com/torqosorg/torqos-releases?ref=stable","former":["https://github.com/netstacks/torqos-releases"]}"#).unwrap()
    }

    #[test]
    fn the_releases_are_their_address_now_and_their_earlier_ones_in_these_spellings() {
        let r = releases();
        assert_eq!(r.fetch_url(), "https://github.com/torqosorg/torqos-releases");
        for at in [
            "https://github.com/torqosorg/torqos-releases",
            "https://github.com/netstacks/torqos-releases",
            "https://GitHub.com/NetStacks/torqos-releases.git/",
            "git+https://github.com/netstacks/torqos-releases?ref=stable",
            "https://x@github.com/torqosorg/torqos-releases",
            "https://user:pw@github.com:443/torqosorg/torqos-releases",
            "https://www.github.com/torqosorg/torqos-releases",
            "https://github.com/torqosorg/torqos%2Dreleases",
            "ssh://git@github.com/netstacks/torqos-releases.git",
        ] {
            assert!(r.is_repo(at), "{at}");
        }
        for not in [
            "https://github.com/torqosorg/torqos",
            "https://github.com/netstacks/torqos-releases-evil",
            "https://example.com/torqos-releases",
            "https://github.com/someone/torqos-releases",
            "https://github.com:8443/torqosorg/torqos-releases",
            "https://github.com.evil.example/torqosorg/torqos-releases",
        ] {
            assert!(!r.is_repo(not), "{not}");
        }
        // Without earlier addresses (a setting written before them): only the address now.
        let only = Releases::parse(r#"{"url":"git+https://github.com/torqosorg/torqos-releases?ref=stable"}"#).unwrap();
        assert!(only.is_repo("https://github.com/torqosorg/torqos-releases") && !only.is_repo("https://github.com/netstacks/torqos-releases"));
    }

    #[test]
    fn the_releases_as_git_are_checked_and_as_a_tarball_refused() {
        let r = releases();
        let rev = "a".repeat(40);
        let release = |l: Value| assert_eq!(r.verdict(&l), Verdict::Release(rev.clone()), "{l}");
        release(json!({"type": "git", "url": "https://github.com/torqosorg/torqos-releases", "rev": rev}));
        release(json!({"type": "git", "url": "https://github.com/netstacks/torqos-releases.git", "rev": rev}));
        release(json!({"type": "git", "url": "https://x@www.github.com:443/NetStacks/torqos-releases", "rev": rev}));
        let refused = |l: Value| assert!(matches!(r.verdict(&l), Verdict::Refused(_)), "{l}");
        refused(json!({"type": "git", "url": "https://github.com/torqosorg/torqos-releases"}));
        refused(json!({"type": "github", "owner": "torqosorg", "repo": "torqos-releases", "rev": rev}));
        refused(json!({"type": "github", "owner": "NetStacks", "repo": "torqos-releases", "rev": rev}));
        refused(json!({"type": "github", "owner": "torqosorg", "repo": "torqos-releases", "host": "evil.example", "rev": rev}));
        refused(json!({"type": "tarball", "url": format!("https://github.com/torqosorg/torqos-releases/archive/{rev}.tar.gz")}));
        refused(json!({"type": "file", "url": "https://github.com/netstacks/torqos-releases/archive/refs/heads/stable.zip"}));
        refused(json!({"type": "tarball", "url": "https://github.com/torqosorg/torqos-releases.git/archive/stable.tar.gz"}));
        refused(json!({"type": "tarball", "url": "https://codeload.github.com/torqosorg/torqos-releases/tar.gz/refs/heads/stable"}));
        refused(json!({"type": "tarball", "url": "https://api.github.com/repos/netstacks/torqos-releases/tarball/stable"}));
        let other = |l: Value| assert_eq!(r.verdict(&l), Verdict::Other, "{l}");
        other(json!({"type": "github", "owner": "torqosorg", "repo": "torqos", "rev": rev}));
        other(json!({"type": "git", "url": "https://github.com/torqosorg/torqos", "rev": rev}));
        other(json!({"type": "tarball", "url": "https://github.com/torqosorg/torqos-releases-evil/archive/x.tar.gz"}));
        other(json!({"type": "tarball", "url": "https://codeload.github.com/torqosorg/torqos-releases-evil/tar.gz/x"}));
        other(json!({"type": "tarball", "url": "https://mirror.example/torqosorg/torqos-releases/archive/x.tar.gz"}));
        other(json!({"type": "path", "path": "/nix/store/x-source"}));
    }

    #[test]
    fn an_original_names_the_releases_in_any_form() {
        let r = releases();
        assert!(r.names(&json!({"type": "git", "url": "https://github.com/torqosorg/torqos-releases", "ref": "stable"})));
        assert!(r.names(&json!({"type": "git", "url": "https://github.com/netstacks/torqos-releases"})));
        assert!(r.names(&json!({"type": "github", "owner": "torqosorg", "repo": "torqos-releases"})));
        assert!(!r.names(&json!({"type": "git", "url": "https://example.com/elsewhere"})));
        assert!(!r.names(&Value::Null), "no original: names nothing");
    }

    #[test]
    fn a_source_at_an_earlier_address_moves_with_its_ref() {
        let r = releases();
        assert_eq!(r.moved("git+https://github.com/netstacks/torqos-releases?ref=stable").as_deref(), Some("git+https://github.com/torqosorg/torqos-releases?ref=stable"));
        assert_eq!(r.moved("git+https://github.com/netstacks/torqos-releases.git?ref=refs/tags/v0.4.0").as_deref(), Some("git+https://github.com/torqosorg/torqos-releases?ref=refs/tags/v0.4.0"));
        for stays in ["git+https://github.com/torqosorg/torqos-releases?ref=stable", "github:netstacks/torqos-releases", "git+https://github.com/someone/torqos-releases?ref=stable", "path:/home/me/torq"] {
            assert_eq!(r.moved(stays), None, "{stays}");
        }
    }

    #[test]
    fn a_setting_is_read_strictly_where_it_matters_and_passes_by_what_it_does_not_know() {
        for bad in ["{}", r#"{"url":"https://github.com/x/y"}"#, r#"{"url":"git+https://"}"#, r#"{"url":"git+/srv/r"}"#, r#"{"url":"git+https://a/b","former":"x"}"#, "not json"] {
            assert!(Releases::parse(bad).is_err(), "{bad}");
        }
        // A field a newer TorqOS adds: passed by.
        assert!(Releases::parse(r#"{"url":"git+https://a/b","former":[],"later":1}"#).is_ok());
    }
}
