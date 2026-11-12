//! TorqOS's releases (ADR-028): which repositories are them, one rule for torqcli and the broker, so the
//! two cannot drift. A source there must be a commit the release key signed, at lock (torqcli) and at
//! apply (the broker). The machine's setting (/etc/torq/releases.json, modules/releases.nix) names the
//! releases' address now, and the addresses they had before (`former`): a machine installed from an older
//! release still names one in its flake.nix, and it is the same repository, under its old name, so it is
//! checked the same (ADR-028, amended 2026-11-12).

use serde::Deserialize;
use serde_json::Value;

/// Where the machine's setting is.
pub const FILE: &str = "/etc/torq/releases.json";

/// The machine's releases.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Releases {
    /// The releases now, as a flake reference: `git+https://…/torqos-releases?ref=stable`.
    pub url: String,
    /// Their earlier addresses (`https://…`), still the releases.
    #[serde(default)]
    pub former: Vec<String>,
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

    /// Whether `repo` (`https://…`, as git or a lock names it, any case, with `.git` or not) is the releases,
    /// under their name now or an earlier one.
    pub fn is_repo(&self, repo: &str) -> bool {
        let repo = normal(repo);
        std::iter::once(self.fetch_url()).chain(self.former.iter().cloned()).any(|r| normal(&r) == repo)
    }

    /// The commit a locked input takes from the releases, if it is theirs: as git, or as GitHub's
    /// `github:<owner>/<repo>`. None: not the releases (or no commit named).
    pub fn commit_of(&self, locked: &Value) -> Option<String> {
        let repo = match locked["type"].as_str()? {
            "git" => locked["url"].as_str()?.to_string(),
            "github" => format!("https://github.com/{}/{}", locked["owner"].as_str()?, locked["repo"].as_str()?),
            _ => return None,
        };
        self.is_repo(&repo).then(|| locked["rev"].as_str().map(String::from)).flatten()
    }

    /// Whether a locked input is the releases' (so must be signed by the release key).
    pub fn is_release(&self, locked: &Value) -> bool {
        let repo = match locked["type"].as_str() {
            Some("git") => locked["url"].as_str().map(String::from),
            Some("github") => locked["owner"].as_str().zip(locked["repo"].as_str()).map(|(o, r)| format!("https://github.com/{o}/{r}")),
            _ => None,
        };
        repo.is_some_and(|r| self.is_repo(&r))
    }

    /// A source (`git+https://…?ref=…`) at an earlier address of the releases, as the same source at their
    /// address now (its `?ref=…` kept). None: not at an earlier address.
    pub fn moved(&self, source: &str) -> Option<String> {
        let at = base(source).strip_prefix("git+")?;
        if normal(at) == normal(&self.fetch_url()) || !self.former.iter().any(|f| normal(f) == normal(at)) {
            return None;
        }
        Some(format!("{}{}", base(&self.url), &source[base(source).len()..]))
    }
}

/// A flake reference without its query, and without a `/` at its end.
fn base(url: &str) -> &str {
    url.split('?').next().unwrap_or(url).trim_end_matches('/')
}

/// A repository's address as GitHub and git take it alike: no `.git`, no `/` after it, any case.
pub fn normal(repo: &str) -> String {
    base(repo.strip_prefix("git+").unwrap_or(repo)).trim_end_matches(".git").trim_end_matches('/').to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn releases() -> Releases {
        Releases::parse(r#"{"url":"git+https://github.com/torqosorg/torqos-releases?ref=stable","former":["https://github.com/netstacks/torqos-releases"]}"#).unwrap()
    }

    #[test]
    fn the_releases_are_their_address_now_and_their_earlier_ones() {
        let r = releases();
        assert_eq!(r.fetch_url(), "https://github.com/torqosorg/torqos-releases");
        for at in ["https://github.com/torqosorg/torqos-releases", "https://github.com/netstacks/torqos-releases", "https://GitHub.com/NetStacks/torqos-releases.git/", "git+https://github.com/netstacks/torqos-releases?ref=stable"] {
            assert!(r.is_repo(at), "{at}");
        }
        for not in ["https://github.com/torqosorg/torqos", "https://github.com/netstacks/torqos-releases-evil", "https://example.com/torqos-releases", "https://github.com/someone/torqos-releases"] {
            assert!(!r.is_repo(not), "{not}");
        }
        let rev = "a".repeat(40);
        for (locked, theirs) in [
            (json!({"type": "git", "url": "https://github.com/torqosorg/torqos-releases", "rev": rev}), true),
            (json!({"type": "git", "url": "https://github.com/netstacks/torqos-releases.git", "rev": rev}), true),
            (json!({"type": "github", "owner": "NetStacks", "repo": "torqos-releases", "rev": rev}), true),
            (json!({"type": "github", "owner": "torqosorg", "repo": "torqos", "rev": rev}), false),
            (json!({"type": "path", "path": "/nix/store/x-source"}), false),
        ] {
            assert_eq!(r.is_release(&locked), theirs, "{locked}");
            assert_eq!(r.commit_of(&locked).is_some(), theirs, "{locked}");
        }
        // Without earlier addresses (a setting written before them): only the address now.
        let only = Releases::parse(r#"{"url":"git+https://github.com/torqosorg/torqos-releases?ref=stable"}"#).unwrap();
        assert!(only.is_repo("https://github.com/torqosorg/torqos-releases") && !only.is_repo("https://github.com/netstacks/torqos-releases"));
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
    fn a_setting_is_read_strictly() {
        for bad in ["{}", r#"{"url":"https://github.com/x/y"}"#, r#"{"url":"git+https://"}"#, r#"{"url":"git+/srv/r"}"#, r#"{"url":"git+https://a/b","other":1}"#, "not json"] {
            assert!(Releases::parse(bad).is_err(), "{bad}");
        }
    }
}
