//! Permission tiers and path policy.
//!
//! The tier of a change is the highest tier of any file it touches. Paths are relative to the
//! config repo root. This mapping is deliberately simple and conservative for M1: anything not
//! recognised is `System`.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Observe,
    User,
    System,
    Forbidden,
}

impl fmt::Display for Tier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Tier::Observe => "observe",
            Tier::User => "user",
            Tier::System => "system",
            Tier::Forbidden => "forbidden",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Actor {
    User,
    Agent,
}

impl fmt::Display for Actor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Actor::User => "user",
            Actor::Agent => "agent",
        })
    }
}

/// Paths the agent may never change, no matter what. Changing them requires actor=user, and in
/// the real product only through a UI the user operates directly.
const FORBIDDEN_PREFIXES: &[&str] = &[
    "policy/",      // AI permissions and tier rules
    "secureboot/",  // key enrollment settings
    "ledger/",      // anything about audit configuration
];

/// The per-user data files that are user tier: `users/<name>/<leaf>`. An explicit allow-list, not
/// a directory: a leaf a later release starts reading (say `ssh-keys.json`) must not become user
/// tier by accident, and a file planted today must not become active then. Any `.nix` file is a
/// full NixOS module (sudo rules, services, ...) wherever it lives, so it is system tier. Keep in
/// step with `userLeaves` in modules/packages.nix. A leaf is added only together with the typed
/// module that reads it (`cli.json` comes with the CLI-style setting), and with its shape in
/// `user_leaf_ok`.
pub const USER_LEAVES: &[&str] = &["packages.json"];

/// The most packages one user's list may hold: a lightly-approved change must not be able to start
/// hours of downloads and builds while holding the broker lock.
pub const MAX_USER_PACKAGES: usize = 256;

/// Whether `value` has exactly the shape the user leaf `leaf` takes. Checked by the broker before
/// accepting a proposal, so data the leaf's module would reject is refused up front instead of
/// failing the rebuild; the module's own types remain the authority on meaning.
pub fn user_leaf_ok(leaf: &str, value: &serde_json::Value) -> bool {
    match leaf {
        // A list of nixpkgs attribute names.
        "packages.json" => value
            .as_array()
            .is_some_and(|names| names.len() <= MAX_USER_PACKAGES && names.iter().all(|n| n.is_string())),
        _ => false,
    }
}

/// User tier: exactly `users/<name>/<leaf>` with an allow-listed leaf. The broker also checks the
/// content of such a file before accepting it (`Broker::check_user_data`).
pub fn is_user_data(p: &str) -> bool {
    let Some((name, leaf)) = p.strip_prefix("users/").and_then(|rest| rest.split_once('/')) else {
        return false;
    };
    // A conservative user name: what `useradd` accepts by default, minus uppercase.
    let mut chars = name.chars();
    let valid_name = matches!(chars.next(), Some('a'..='z' | '_'))
        && chars.all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_' | '-'));
    valid_name && USER_LEAVES.contains(&leaf)
}

pub fn tier_for_path(path: &str) -> Tier {
    let p = path.trim_start_matches("./");
    if p.contains("..") {
        return Tier::Forbidden;
    }
    if FORBIDDEN_PREFIXES.iter().any(|f| p.starts_with(f)) {
        return Tier::Forbidden;
    }
    if is_user_data(p) {
        return Tier::User;
    }
    Tier::System
}

pub fn tier_for_paths<S: AsRef<str>>(paths: &[S]) -> Tier {
    paths
        .iter()
        .map(|p| tier_for_path(p.as_ref()))
        .max()
        .unwrap_or(Tier::Observe)
}

pub fn check_allowed(actor: Actor, tier: Tier) -> Result<()> {
    if actor == Actor::Agent && tier == Tier::Forbidden {
        bail!("refused: the agent may not change forbidden paths (policy, secure boot, ledger)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies() {
        assert_eq!(tier_for_path("users/casey/packages.json"), Tier::User);
        assert_eq!(tier_for_path("./users/casey/packages.json"), Tier::User);
        assert_eq!(tier_for_path("users/casey/cli.json"), Tier::System, "no leaf without a typed reader");
        assert_eq!(tier_for_path("users/casey.nix"), Tier::System, "a NixOS module is never user tier");
        assert_eq!(tier_for_path("desktop.nix"), Tier::System);
        assert_eq!(tier_for_path("policy/tiers.toml"), Tier::Forbidden);
        assert_eq!(tier_for_path("users/../policy/x"), Tier::Forbidden);
        assert_eq!(tier_for_paths(&["users/a.nix", "system.nix"]), Tier::System);
        assert_eq!(tier_for_paths::<&str>(&[]), Tier::Observe);
    }

    /// Only allow-listed per-user data files are user tier. Anything that could be a module, any
    /// other leaf (a later `ssh-keys.json` must not become user tier by accident), and anything that
    /// escapes the exact `users/<name>/<leaf>` shape is system tier.
    #[test]
    fn user_tier_is_an_allow_list_of_data_files() {
        for p in [
            "users/casey.nix",
            "users/default.nix",
            "users/casey/default.nix",
            "users/casey/packages.nix",
            "users/casey/ssh-keys.json",
            "users/casey/environment.json",
            "users/casey/packages.json.bak",
            "users/casey/sub/packages.json",
            "users/casey/packages.json/x",
            "users/casey.packages.json",
            "users/.hidden/packages.json",
            "users/-x/packages.json",
            "users/Casey/packages.json",
            "users/ca sey/packages.json",
            "users//packages.json",
            "users/packages.json",
            "themes/dark.json",
            "users",
        ] {
            assert_eq!(tier_for_path(p), Tier::System, "{p}");
        }
        for p in ["users/casey/packages.json", "users/_svc/packages.json", "users/a-b_9/packages.json"] {
            assert_eq!(tier_for_path(p), Tier::User, "{p}");
        }
        assert_eq!(USER_LEAVES, ["packages.json"]);
    }

    /// Each user leaf has exactly one shape; anything else is refused at propose rather than failing
    /// at the rebuild.
    #[test]
    fn user_leaf_shapes() {
        let ok = |leaf: &str, json: &str| user_leaf_ok(leaf, &serde_json::from_str(json).unwrap());
        assert!(ok("packages.json", r#"["htop", "python3Packages.requests"]"#));
        assert!(ok("packages.json", "[]"));
        for bad in ["{}", "[1]", "[{}]", "[null]", "\"htop\"", "42"] {
            assert!(!ok("packages.json", bad), "{bad}");
        }
        let at_most = |n: usize| format!("[{}]", vec!["\"p\""; n].join(","));
        assert!(ok("packages.json", &at_most(256)));
        assert!(!ok("packages.json", &at_most(257)));
        assert!(!ok("unknown.json", "[]"), "a leaf without a shape is never ok");
    }

    #[test]
    fn agent_cannot_touch_forbidden() {
        assert!(check_allowed(Actor::Agent, Tier::Forbidden).is_err());
        assert!(check_allowed(Actor::User, Tier::Forbidden).is_ok());
        assert!(check_allowed(Actor::Agent, Tier::System).is_ok());
    }
}
