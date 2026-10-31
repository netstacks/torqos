//! Config files attached in the tree (docs/CONFIG-IS-THE-DEVICE.md): `set services httpd config
//! services/httpd.nix` attaches an ordinary NixOS module file of the config repo at that point, for
//! what is not a value. Kept in `config-files.json` (where in the tree → which file), which
//! modules/nixos-settings.nix turns into NixOS `imports`; the files themselves are plain Nix, system
//! tier, shown as their raw diffs before anyone commits them.
//!
//! Pure: it reads and writes the file contents it is given.

use crate::tree::Val;
use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;

/// The config repo file that says which config files are attached where.
pub const FILE: &str = "config-files.json";

/// Where in the tree (its words, joined by spaces) → the config repo file attached there.
pub type Attached = BTreeMap<String, String>;

/// Whether `rel` may be a config file: a `.nix` file inside the config repo — relative, no `..`, no
/// empty or odd parts, nothing but plain path characters — as modules/nixos-settings.nix requires.
pub fn valid(rel: &str) -> Result<()> {
    let parts: Vec<&str> = rel.split('/').collect();
    let plain = rel.chars().all(|c| c.is_ascii_alphanumeric() || "._-/".contains(c));
    if !rel.ends_with(".nix") || rel.starts_with('/') || !plain || parts.iter().any(|p| p.is_empty() || *p == "." || *p == "..") {
        bail!("'{rel}' must be a .nix file inside the config repo (like services/httpd.nix: relative, no ..)");
    }
    // Not one of the files TorqOS keeps the configuration's own structure in.
    if matches!(rel, "flake.nix" | "hardware-configuration.nix") {
        bail!("'{rel}' is the config repo's own: attach a file of your own");
    }
    Ok(())
}

/// Read `config-files.json` (if there is one), each entry checked.
pub fn load(bytes: Option<&[u8]>) -> Result<Attached> {
    let Some(b) = bytes else { return Ok(Attached::new()) };
    let a: Attached = serde_json::from_slice(b).with_context(|| format!("{FILE}: not a JSON object of strings; fix the file (as raw JSON) first"))?;
    for (at, rel) in &a {
        if at.is_empty() || at.chars().any(char::is_control) {
            bail!("{FILE}: an attachment without a place");
        }
        valid(rel).with_context(|| format!("{FILE}: {at}"))?;
    }
    Ok(a)
}

/// `config-files.json` as TorqOS writes it; None: no attachments, no file.
pub fn render(a: &Attached) -> Option<String> {
    (!a.is_empty()).then(|| format!("{}\n", serde_json::to_string_pretty(a).unwrap_or_default()))
}

/// Each attachment as an entry: `<place> config <file>`.
pub fn entries(a: &Attached) -> Vec<(Vec<String>, Val)> {
    a.iter()
        .map(|(at, rel)| {
            let mut w: Vec<String> = at.split(' ').map(String::from).collect();
            w.push("config".into());
            (w, Val::One(rel.clone()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_nix_files_inside_the_repo() {
        for ok in ["services/httpd.nix", "custom_apps/my-app.nix", "a.nix"] {
            valid(ok).unwrap_or_else(|e| panic!("{ok}: {e:#}"));
        }
        for bad in ["../x.nix", "/etc/x.nix", "a/../b.nix", "notes.txt", "./a.nix", "a//b.nix", "a b.nix", "flake.nix", "hardware-configuration.nix", ""] {
            assert!(valid(bad).is_err(), "{bad}");
        }
        assert!(load(Some(br#"{"x": "../y.nix"}"#)).is_err());
        assert!(load(Some(br#"["a.nix"]"#)).is_err());
        let a = load(Some(br#"{"services httpd": "services/httpd.nix"}"#)).unwrap();
        assert_eq!(entries(&a), [(vec!["services".into(), "httpd".into(), "config".into()], Val::One("services/httpd.nix".into()))]);
        assert_eq!(load(render(&a).as_deref().map(str::as_bytes)).unwrap(), a, "reads back");
        assert_eq!(render(&Attached::new()), None);
    }
}
