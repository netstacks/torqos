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

/// User tier: data files that only affect one user's own environment. Deliberately an exact shape,
/// not a directory: any `.nix` file is a full NixOS module (sudo rules, services, ...) no matter
/// where it lives, so it is system tier. Today that is `users/<name>.packages.json`, read by
/// `modules/packages.nix` into that user's profile.
fn is_user_data(p: &str) -> bool {
    let Some(name) = p.strip_prefix("users/").and_then(|f| f.strip_suffix(".packages.json")) else {
        return false;
    };
    // A conservative user name: what `useradd` accepts by default, minus uppercase.
    let mut chars = name.chars();
    matches!(chars.next(), Some('a'..='z' | '_'))
        && chars.all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_' | '-'))
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
        assert_eq!(tier_for_path("users/casey.packages.json"), Tier::User);
        assert_eq!(tier_for_path("./users/casey.packages.json"), Tier::User);
        assert_eq!(tier_for_path("users/casey.nix"), Tier::System, "a NixOS module is never user tier");
        assert_eq!(tier_for_path("desktop.nix"), Tier::System);
        assert_eq!(tier_for_path("policy/tiers.toml"), Tier::Forbidden);
        assert_eq!(tier_for_path("users/../policy/x"), Tier::Forbidden);
        assert_eq!(tier_for_paths(&["users/a.nix", "system.nix"]), Tier::System);
        assert_eq!(tier_for_paths::<&str>(&[]), Tier::Observe);
    }

    /// Only exact per-user data files are user tier; anything that could be a module, or that
    /// escapes the one-file-per-user shape, is system tier.
    #[test]
    fn user_tier_is_data_files_only() {
        for p in [
            "users/casey.nix",
            "users/default.nix",
            "users/casey/packages.json",
            "users/casey.packages.json.bak",
            "users/.packages.json",
            "users/-x.packages.json",
            "users/Casey.packages.json",
            "users/ca sey.packages.json",
            "users/casey.packages.nix",
            "themes/dark.nix",
            "themes/dark.json",
            "users",
        ] {
            assert_eq!(tier_for_path(p), Tier::System, "{p}");
        }
        for p in ["users/casey.packages.json", "users/_svc.packages.json", "users/a-b_9.packages.json"] {
            assert_eq!(tier_for_path(p), Tier::User, "{p}");
        }
    }

    #[test]
    fn agent_cannot_touch_forbidden() {
        assert!(check_allowed(Actor::Agent, Tier::Forbidden).is_err());
        assert!(check_allowed(Actor::User, Tier::Forbidden).is_ok());
        assert!(check_allowed(Actor::Agent, Tier::System).is_ok());
    }
}
