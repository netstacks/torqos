//! A person's desktop, saved (ADR-045 4, amended 2026-11-10): COSMIC keeps one small file per setting
//! under `~/.config/cosmic/<component>/v<n>/<key>`; they are kept in the person's own configuration,
//! `users/<name>/desktop.json` (user tier, no password), so a machine rebuilt from the repository comes
//! back looking the same, and an undo or a pull puts the desktop back as it was.
//!
//! Three versions of each setting: the configuration's (saved), this desktop's (local), and the one both
//! last agreed on (applied, kept in `~/.local/state/torq`). What changed since they agreed tells which
//! side is newer: the person's change is saved, the configuration's is put on the desktop. Settings of
//! this machine rather than the person (which display the panel is on, when to sleep) are left out.

use crate::candidate::Repo;
use crate::session::Machine;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The leaf in a person's folder (broker/user-leaves.json).
pub const FILE: &str = "desktop.json";

// What the broker accepts (broker/src/policy.rs mirrors these): what it would refuse is never sent.
pub const MAX_VALUE: usize = 64 * 1024;
pub const MAX_KEYS: usize = 2000;
pub const MAX_TOTAL: usize = 1024 * 1024;

/// When a person's desktop is saved.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Saving {
    /// By itself, as the person changes it.
    #[default]
    Automatic,
    /// Only when they ask (`request system desktop save`).
    Manual,
}

/// `users/<name>/desktop.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Saved {
    #[serde(default)]
    pub save: Saving,
    /// Each COSMIC setting, `<component>/v<n>/<key>`, as COSMIC wrote it.
    #[serde(default)]
    pub cosmic: BTreeMap<String, String>,
}

impl Saved {
    fn render(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)? + "\n")
    }
}

/// A setting's name as COSMIC lays it out: a reverse-DNS component, its version, a key.
pub fn key_ok(k: &str) -> bool {
    let word = |s: &str| !s.is_empty() && s.len() <= 128 && !s.starts_with('-') && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    let mut parts = k.split('/');
    let (Some(component), Some(version), Some(key), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else { return false };
    let dotted: Vec<&str> = component.split('.').collect();
    dotted.len() >= 2
        && dotted.iter().all(|p| word(p))
        && version.strip_prefix('v').is_some_and(|n| !n.is_empty() && n.len() <= 4 && n.bytes().all(|b| b.is_ascii_digit()))
        && word(key)
}

pub fn value_ok(v: &str) -> bool {
    v.len() <= MAX_VALUE && !v.contains('\0')
}

/// This machine's, not the person's: which display the panel and dock are on, and when it sleeps (the
/// hardware decides that: some laptops cannot wake their display).
pub fn machine_specific(k: &str) -> bool {
    let component = k.split('/').next().unwrap_or_default();
    let key = k.rsplit('/').next().unwrap_or_default();
    (component.starts_with("com.system76.CosmicPanel.") && key == "output") || (component == "com.system76.CosmicIdle" && key.starts_with("suspend_"))
}

/// Where this person's desktop and what TorqOS last knew of it are.
pub struct Paths {
    pub cosmic: PathBuf,
    pub state: PathBuf,
}

impl Paths {
    pub fn of_home() -> Result<Paths> {
        let home = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?);
        let config = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(|| home.join(".config"));
        Ok(Paths { cosmic: config.join("cosmic"), state: home.join(".local/state/torq/desktop-applied.json") })
    }
}

fn dirs(p: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(p) else { return vec![] };
    entries
        .flatten()
        // Never through a link: only what is really there.
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| Some((e.file_name().into_string().ok()?, e.path())))
        .collect()
}

/// This desktop's settings, as COSMIC wrote them.
pub fn local(cosmic: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut total = 0;
    for (component, c) in dirs(cosmic) {
        for (version, v) in dirs(&c) {
            let Ok(files) = std::fs::read_dir(&v) else { continue };
            for f in files.flatten() {
                let Some(name) = f.file_name().into_string().ok() else { continue };
                let key = format!("{component}/{version}/{name}");
                if !key_ok(&key) || machine_specific(&key) || !f.file_type().is_ok_and(|t| t.is_file()) {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(f.path()) else { continue };
                if value_ok(&text) && out.len() < MAX_KEYS && total + text.len() <= MAX_TOTAL {
                    total += text.len();
                    out.insert(key, text);
                }
            }
        }
    }
    out
}

/// What brings the desktop and the configuration together.
#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    /// The person's newer values, for the configuration (None: back to COSMIC's default).
    pub save: BTreeMap<String, Option<String>>,
    /// The configuration's newer values, for the desktop (None: COSMIC's default).
    pub apply: BTreeMap<String, Option<String>>,
}

pub fn plan(saved: &BTreeMap<String, String>, applied: &BTreeMap<String, String>, local: &BTreeMap<String, String>) -> Plan {
    let mut p = Plan::default();
    let keys: BTreeSet<&String> = saved.keys().chain(applied.keys()).chain(local.keys()).collect();
    for k in keys {
        let (s, a, l) = (saved.get(k), applied.get(k), local.get(k));
        if l == a {
            // Not changed here since they agreed: the configuration's is newer, if it differs.
            if s != a {
                p.apply.insert(k.clone(), s.cloned());
            }
        } else if a.is_none() && s.is_some() {
            // Never agreed here and the configuration has it: a machine yet to take it (rebuilt, adopted).
            if l != s {
                p.apply.insert(k.clone(), s.cloned());
            }
        } else if s != l {
            p.save.insert(k.clone(), l.cloned());
        }
    }
    p
}

/// What the two agree on afterwards; where they still differ (a change not saved), what they last agreed.
fn agreed(saved: &BTreeMap<String, String>, local: &BTreeMap<String, String>, before: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let keys: BTreeSet<&String> = saved.keys().chain(local.keys()).chain(before.keys()).collect();
    keys.into_iter()
        .filter_map(|k| match (saved.get(k), local.get(k)) {
            (s, l) if s == l => s.map(|v| (k.clone(), v.clone())),
            _ => before.get(k).map(|v| (k.clone(), v.clone())),
        })
        .collect()
}

fn read_applied(state: &Path) -> BTreeMap<String, String> {
    std::fs::read(state).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn write_atomically(path: &Path, text: &[u8]) -> Result<()> {
    let dir = path.parent().context("a file has a folder")?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{}.torq", path.file_name().and_then(|n| n.to_str()).unwrap_or("x")));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Put the configuration's values on the desktop: COSMIC sees its files change and takes them at once.
fn apply(cosmic: &Path, changes: &BTreeMap<String, Option<String>>) -> Vec<String> {
    let mut failed = vec![];
    for (k, v) in changes {
        // Checked again here: these names come from the configuration and become paths.
        if !key_ok(k) {
            failed.push(k.clone());
            continue;
        }
        let path = cosmic.join(k);
        let done = match v {
            Some(text) => write_atomically(&path, text.as_bytes()),
            None => match std::fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
                _ => Ok(()),
            },
        };
        if done.is_err() {
            failed.push(k.clone());
        }
    }
    failed
}

fn path_of(me: &str) -> String {
    format!("users/{me}/{FILE}")
}

/// The configuration's `desktop.json` of `me` at HEAD: its bytes, and what it says.
fn saved_of(repo: &Repo, me: &str) -> Result<(Option<Vec<u8>>, Saved)> {
    let bytes = repo.file_at("HEAD", &path_of(me))?;
    let saved = match &bytes {
        Some(b) => serde_json::from_slice(b).with_context(|| format!("reading {}", path_of(me)))?,
        None => Saved::default(),
    };
    Ok((bytes, saved))
}

/// A few words for what changed: "panel, theme".
pub fn what(keys: impl Iterator<Item = String>) -> String {
    let word = |k: &str| {
        let c = k.split('/').next().unwrap_or_default();
        let c = c.strip_prefix("com.system76.Cosmic").unwrap_or(c);
        match c.split('.').next().unwrap_or(c) {
            "Theme" | "Tk" => "theme".to_string(),
            "Background" => "wallpaper".to_string(),
            "Panel" if c.ends_with("Dock") => "dock".to_string(),
            "Panel" => "panel".to_string(),
            "AppList" => "dock's apps".to_string(),
            "Settings" if c.ends_with("Shortcuts") => "shortcuts".to_string(),
            "Comp" => "windows and input".to_string(),
            other => other.trim_start_matches("Applet").to_lowercase(),
        }
    };
    let words: BTreeSet<String> = keys.map(|k| word(&k)).collect();
    let mut list: Vec<String> = words.into_iter().collect();
    if list.len() > 4 {
        let n = list.len() - 3;
        list.truncate(3);
        list.push(format!("{n} more"));
    }
    list.join(", ")
}

/// Commit `me`'s new `desktop.json`: their own folder, no question.
fn commit(machine: &dyn Machine, repo: &Repo, me: &str, old: Option<&[u8]>, new: &Saved, reason: &str) -> Result<u64> {
    check(new)?;
    let scratch = crate::scratch_dir()?;
    let text = new.render()?;
    let diff = crate::candidate::unified_diff_bytes(scratch.path(), &[(path_of(me), old, Some(text.as_bytes()))])?;
    machine.commit(&diff, &repo.head()?, "user", reason, None)
}

/// How a sync went.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Synced {
    /// The change that saved the person's settings.
    pub saved: Option<u64>,
    /// Settings the configuration put on the desktop.
    pub applied: usize,
    /// The person's changes not saved (saving is manual).
    pub unsaved: usize,
    /// Settings that could not be put on the desktop.
    pub failed: Vec<String>,
}

/// Bring the desktop and the configuration together: the configuration's newer values onto the desktop,
/// and the person's saved when saving is automatic or `save_now`.
pub fn sync(machine: &dyn Machine, repo: &Repo, me: &str, paths: &Paths, save_now: bool) -> Result<Synced> {
    let (bytes, saved) = saved_of(repo, me)?;
    let before = read_applied(&paths.state);
    let mut here = local(&paths.cosmic);
    let p = plan(&saved.cosmic, &before, &here);
    let mut out = Synced { failed: apply(&paths.cosmic, &p.apply), ..Synced::default() };
    out.applied = p.apply.len() - out.failed.len();
    for (k, v) in &p.apply {
        if out.failed.contains(k) {
            continue;
        }
        match v {
            Some(v) => here.insert(k.clone(), v.clone()),
            None => here.remove(k),
        };
    }
    let mut now = saved.clone();
    if !p.save.is_empty() && (save_now || saved.save == Saving::Automatic) {
        for (k, v) in &p.save {
            match v {
                Some(v) => now.cosmic.insert(k.clone(), v.clone()),
                None => now.cosmic.remove(k),
            };
        }
        let reason = format!("Desktop: {}", what(p.save.keys().cloned()));
        out.saved = Some(commit(machine, repo, me, bytes.as_deref(), &now, &reason)?);
    } else {
        out.unsaved = p.save.len();
    }
    let state = agreed(&now.cosmic, &here, &before);
    write_atomically(&paths.state, &serde_json::to_vec(&state)?)?;
    Ok(out)
}

/// Save automatically, or only when asked.
pub fn set_saving(machine: &dyn Machine, repo: &Repo, me: &str, how: Saving) -> Result<String> {
    let (bytes, mut saved) = saved_of(repo, me)?;
    if saved.save == how {
        return Ok(format!("Your desktop is already saved {}.\n", words(how)));
    }
    saved.save = how;
    let seq = commit(machine, repo, me, bytes.as_deref(), &saved, &format!("Desktop: saved {}", words(how)))?;
    Ok(format!("Your desktop is saved {} from now on (change #{seq}).\n", words(how)))
}

fn words(how: Saving) -> &'static str {
    match how {
        Saving::Automatic => "automatically",
        Saving::Manual => "only when you ask",
    }
}

/// Where the desktop and the configuration stand, for `show system desktop` and TorqOS Settings.
#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub saving: Saving,
    pub saved: usize,
    /// The person's changes not yet saved, by a few words.
    pub unsaved: Vec<String>,
    /// The configuration's values still to be put on the desktop.
    pub to_apply: usize,
}

pub fn status(repo: &Repo, me: &str, paths: &Paths) -> Result<Status> {
    let (_, saved) = saved_of(repo, me)?;
    let p = plan(&saved.cosmic, &read_applied(&paths.state), &local(&paths.cosmic));
    Ok(Status { saving: saved.save, saved: saved.cosmic.len(), unsaved: p.save.keys().cloned().collect(), to_apply: p.apply.len() })
}

pub fn shown(s: &Status) -> String {
    let mut out = format!("Your desktop (COSMIC's settings): {} saved in your configuration, saved {}.\n", s.saved, words(s.saving));
    if s.unsaved.is_empty() {
        out.push_str("  Everything you changed is saved.\n");
    } else {
        out.push_str(&format!("  Not saved yet: {} ({} settings). request system desktop save saves them.\n", what(s.unsaved.iter().cloned()), s.unsaved.len()));
    }
    if s.to_apply > 0 {
        out.push_str(&format!("  {} settings from your configuration are still to be put on this desktop (at the next login, or request system desktop save).\n", s.to_apply));
    }
    out
}

/// Refuse a saved file the broker would refuse: said here first, in these words.
pub fn check(saved: &Saved) -> Result<()> {
    let total: usize = saved.cosmic.values().map(String::len).sum();
    if saved.cosmic.len() > MAX_KEYS || total > MAX_TOTAL {
        bail!("too much to save: {} settings, {} bytes", saved.cosmic.len(), total);
    }
    if let Some((k, _)) = saved.cosmic.iter().find(|(k, v)| !key_ok(k) || !value_ok(v)) {
        bail!("{k} is not a setting TorqOS can save");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }
    const K: &str = "com.system76.CosmicPanel.Panel/v1/size";

    #[test]
    fn a_settings_name_is_cosmics_layout_and_never_a_path() {
        assert!(key_ok(K));
        assert!(key_ok("com.system76.CosmicBackground/v1/same-on-all"));
        for bad in ["", "a/v1/b", "com.x/1/b", "com.x/v1", "com.x/v1/b/c", "../x/v1/b", "com.x/v1/..", "com..x/v1/b", "com.x/v1/-b", "com.x/v12345/b", "com.x/v1/a b"] {
            assert!(!key_ok(bad), "{bad}");
        }
        assert!(machine_specific("com.system76.CosmicPanel.Dock/v1/output"));
        assert!(machine_specific("com.system76.CosmicIdle/v1/suspend_on_ac_time"));
        assert!(!machine_specific("com.system76.CosmicIdle/v1/screen_off_time"));
        assert!(!machine_specific(K));
    }

    #[test]
    fn the_newer_side_wins() {
        let none = map(&[]);
        // The person changed it: saved.
        let p = plan(&map(&[(K, "S")]), &map(&[(K, "S")]), &map(&[(K, "L")]));
        assert_eq!(p.save, [(K.to_string(), Some("L".to_string()))].into());
        assert!(p.apply.is_empty());
        // The configuration changed (an undo, a pull): onto the desktop.
        let p = plan(&map(&[(K, "S2")]), &map(&[(K, "S")]), &map(&[(K, "S")]));
        assert_eq!(p.apply, [(K.to_string(), Some("S2".to_string()))].into());
        // An undo of a first save: back to COSMIC's default.
        let p = plan(&none, &map(&[(K, "L")]), &map(&[(K, "L")]));
        assert_eq!(p.apply, [(K.to_string(), None)].into());
        // Reset to default here: no longer kept.
        let p = plan(&map(&[(K, "S")]), &map(&[(K, "S")]), &none);
        assert_eq!(p.save, [(K.to_string(), None)].into());
        // A rebuilt machine: COSMIC's defaults there give way to the configuration.
        let p = plan(&map(&[(K, "S")]), &none, &map(&[(K, "default")]));
        assert_eq!(p.apply, [(K.to_string(), Some("S".to_string()))].into());
        // The first save on a machine: everything here.
        let p = plan(&none, &none, &map(&[(K, "L")]));
        assert_eq!(p.save, [(K.to_string(), Some("L".to_string()))].into());
        // In step: nothing.
        assert_eq!(plan(&map(&[(K, "S")]), &map(&[(K, "S")]), &map(&[(K, "S")])), Plan::default());
    }

    #[test]
    fn a_change_not_saved_stays_one() {
        // Manual: the person's L is not saved; what they agreed stays S, so L is still to save next time.
        let state = agreed(&map(&[(K, "S")]), &map(&[(K, "L")]), &map(&[(K, "S")]));
        assert_eq!(state, map(&[(K, "S")]));
        assert_eq!(plan(&map(&[(K, "S")]), &state, &map(&[(K, "L")])).save.len(), 1);
        // Saved: they agree on L.
        assert_eq!(agreed(&map(&[(K, "L")]), &map(&[(K, "L")]), &map(&[(K, "S")])), map(&[(K, "L")]));
    }

    #[test]
    fn the_desktop_is_read_as_cosmic_lays_it_out_and_written_back() {
        let d = tempfile::tempdir().unwrap();
        let c = d.path().join("cosmic");
        let w = |k: &str, v: &str| write_atomically(&c.join(k), v.as_bytes()).unwrap();
        w(K, "M");
        w("com.system76.CosmicPanel.Panel/v1/output", "\"eDP-1\"");
        w("com.system76.CosmicPanel./v1/x", "odd");
        std::os::unix::fs::symlink("/etc/passwd", c.join("com.system76.CosmicPanel.Panel/v1/link")).unwrap();
        assert_eq!(local(&c), map(&[(K, "M")]), "machine-specific, odd names and links left out");
        let failed = apply(&c, &[(K.to_string(), Some("L".to_string())), ("../../evil/v1/x".to_string(), Some("x".to_string()))].into());
        assert_eq!(failed, ["../../evil/v1/x"]);
        assert_eq!(std::fs::read_to_string(c.join(K)).unwrap(), "L");
        assert!(!d.path().join("evil").exists());
        apply(&c, &[(K.to_string(), None)].into());
        assert!(!c.join(K).exists());
    }

    #[test]
    fn the_saved_file_reads_back_and_says_what_changed() {
        let s = Saved { save: Saving::Manual, cosmic: map(&[(K, "M")]) };
        assert_eq!(serde_json::from_str::<Saved>(&s.render().unwrap()).unwrap(), s);
        assert_eq!(serde_json::from_str::<Saved>("{}").unwrap(), Saved::default());
        assert!(serde_json::from_str::<Saved>(r#"{"other": 1}"#).is_err());
        assert_eq!(what([K.to_string(), "com.system76.CosmicTheme.Dark/v1/accent".to_string()].into_iter()), "panel, theme");
        assert!(check(&Saved { cosmic: map(&[("x/v1/y", "z")]), ..Saved::default() }).is_err());
    }
}
