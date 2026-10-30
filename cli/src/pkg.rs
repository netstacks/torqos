//! `kestrel pkg`: installed software is JSON in the config repo (ADR-011). The pure parts live
//! here; `main.rs` does the I/O (reading the repo, evaluating, talking to the broker).

use anyhow::{Context, Result};

/// Which package list a command changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    System,
    User(String),
}

impl Target {
    /// Path in the config repo. `users/<name>.packages.json` is the exact shape the broker treats as
    /// user tier; `packages.json` is system tier.
    pub fn path(&self) -> String {
        match self {
            Target::System => "packages.json".into(),
            Target::User(u) => format!("users/{u}.packages.json"),
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Target::System => "system-wide".into(),
            Target::User(u) => format!("for {u}"),
        }
    }
}

/// The user names the broker accepts as user tier (broker/src/policy.rs, `is_user_data`).
pub fn valid_user(name: &str) -> bool {
    let mut c = name.chars();
    matches!(c.next(), Some('a'..='z' | '_')) && c.all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_' | '-'))
}

/// What the CLI accepts as a package name: nixpkgs attribute characters, at most one dot. Which
/// package sets are allowed, and whether the package exists and is available here, is decided by
/// the system's own module when the change is evaluated — one rule, not two copies of it.
pub fn valid_package(name: &str) -> bool {
    let parts: Vec<&str> = name.split('.').collect();
    parts.len() <= 2
        && parts.iter().all(|p| {
            let mut c = p.chars();
            matches!(c.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
                && c.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '\'' | '+' | '-'))
        })
}

pub fn parse_list(text: &str) -> Result<Vec<String>> {
    serde_json::from_str(text).context("not a JSON list of package names")
}

/// One name per line, sorted and de-duplicated, so every change is a minimal, readable diff.
pub fn render(list: &[String]) -> String {
    let mut v = list.to_vec();
    v.sort();
    v.dedup();
    if v.is_empty() {
        return "[]\n".into();
    }
    format!("{}\n", serde_json::to_string_pretty(&v).expect("a list of strings serialises"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edit {
    Add,
    Remove,
}

/// The new list, and the names that actually change it (adding what is there, or removing what
/// is not, is skipped).
pub fn edit(current: &[String], names: &[String], how: Edit) -> (Vec<String>, Vec<String>) {
    let mut list = current.to_vec();
    let mut changed = Vec::new();
    for n in names {
        let present = list.contains(n);
        match how {
            Edit::Add if !present => list.push(n.clone()),
            Edit::Remove if present => list.retain(|x| x != n),
            _ => continue,
        }
        if !changed.contains(n) {
            changed.push(n.clone());
        }
    }
    (list, changed)
}

/// Pull the proposal number out of `kestrel-broker propose` output ("proposal #12 · tier: ...").
pub fn proposal_seq(broker_output: &str) -> Option<u64> {
    let rest = broker_output.split("proposal #").nth(1)?;
    rest.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn paths_match_the_broker_tiers() {
        assert_eq!(Target::System.path(), "packages.json");
        assert_eq!(Target::User("viper".into()).path(), "users/viper.packages.json");
    }

    #[test]
    fn user_names() {
        for ok in ["viper", "_svc", "a-b_9"] {
            assert!(valid_user(ok), "{ok}");
        }
        for bad in ["", "Viper", "-x", "a.b", "a/b", "..", "ca sey"] {
            assert!(!valid_user(bad), "{bad}");
        }
    }

    #[test]
    fn package_names() {
        for ok in ["htop", "python3Packages.requests", "gcc13", "g++", "vim_configurable", "font-awesome", "xorg'"] {
            assert!(valid_package(ok), "{ok}");
        }
        // Injection into the nix expression, path tricks, too deep: refused before anything runs.
        for bad in ["", "a.b.c", "\"htop\"", "htop;", "${x}", "a b", "../x", ".htop", "htop.", "1password"] {
            assert!(!valid_package(bad), "{bad}");
        }
    }

    #[test]
    fn render_is_sorted_deduplicated_one_per_line() {
        assert_eq!(render(&v(&["ripgrep", "htop", "htop"])), "[\n  \"htop\",\n  \"ripgrep\"\n]\n");
        assert_eq!(render(&[]), "[]\n");
        assert_eq!(parse_list(&render(&v(&["b", "a"]))).unwrap(), v(&["a", "b"]));
    }

    #[test]
    fn parse_rejects_non_lists() {
        assert!(parse_list("{\"htop\": true}").is_err());
        assert!(parse_list("[1, 2]").is_err());
        assert_eq!(parse_list("[]").unwrap(), Vec::<String>::new());
    }

    #[test]
    fn edit_reports_only_real_changes() {
        let (l, c) = edit(&v(&["htop"]), &v(&["htop", "ripgrep", "ripgrep"]), Edit::Add);
        assert_eq!((l, c), (v(&["htop", "ripgrep"]), v(&["ripgrep"])));
        let (l, c) = edit(&v(&["htop", "ripgrep"]), &v(&["htop", "nope"]), Edit::Remove);
        assert_eq!((l, c), (v(&["ripgrep"]), v(&["htop"])));
        let (_, c) = edit(&v(&["htop"]), &v(&["htop"]), Edit::Add);
        assert!(c.is_empty());
    }

    #[test]
    fn finds_the_proposal_number() {
        assert_eq!(proposal_seq("proposal #12 · tier: user · actor: user\nfiles: []"), Some(12));
        assert_eq!(proposal_seq("no proposal here"), None);
    }
}
