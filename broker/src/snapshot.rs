//! What may be put back from a snapshot of the data (ADR-038), and how. The paths come from a person,
//! and the folders they name hold what any account put there.
use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Component, Path};

/// Run as the owner: set aside what is there (unless root has: `$2` empty), then copy from standard
/// input, which root opened in the snapshot. Links in the way lead nowhere the owner could not go.
pub const COPY: &str = r#"set -eu
d=$1
if [ -n "$2" ] && { [ -e "$d" ] || [ -L "$d" ]; }; then
  [ ! -e "$2" ] && [ ! -L "$2" ] || { echo "$2 is already there: move it first" >&2; exit 1; }
  mv -T -- "$d" "$2"
fi
mkdir -p -- "$(dirname -- "$d")"
cp -a -H -T --reflink=auto -- /proc/self/fd/0 "$d"
"#;

/// Below `/var/lib`, what belongs to the machine and not to a service's data. `torq`, a subvolume of
/// its own, is not in the snapshots at all; the rest would put the machine's accounts back in time.
const NOT_RESTORED: &[&str] = &["torq", "nixos", "private", "systemd"];

/// Which tree of a snapshot `path` lies in (`home`, `var-lib`), and its parts below it. Only a clean
/// absolute path below a folder of `/home` or `/var/lib`, or that folder.
pub fn data_path(path: &str) -> Result<(&'static str, Vec<String>)> {
    if path.chars().any(char::is_control) || path.len() > 1024 {
        bail!("not a path to restore");
    }
    let mut parts = vec![];
    for c in Path::new(path).components().skip(1) {
        match c {
            Component::Normal(p) => parts.push(p.to_str().context("not a path to restore")?.to_string()),
            _ => bail!("{path:?}: a plain absolute path, with no `..`"),
        }
    }
    if !path.starts_with('/') {
        bail!("{path:?}: an absolute path, under /home or /var/lib");
    }
    let (tree, below) = match parts.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["home", _, ..] => ("home", parts[1..].to_vec()),
        ["var", "lib", _, ..] => ("var-lib", parts[2..].to_vec()),
        _ => bail!("{path:?}: a snapshot holds what is below /home and /var/lib, one folder down at least"),
    };
    if tree == "var-lib" && NOT_RESTORED.contains(&below[0].as_str()) {
        bail!("{path:?}: the machine's own, not a service's data: not restored from a snapshot");
    }
    Ok((tree, below))
}

/// `parts`, below `root`, with no link on the way, and the first part's metadata: whose it all is. A
/// snapshot is read only, so what is checked here stays so.
pub fn no_links(root: &Path, parts: &[String]) -> Result<fs::Metadata> {
    let mut at = root.to_path_buf();
    let mut top = None;
    for p in parts {
        at.push(p);
        let m = fs::symlink_metadata(&at).with_context(|| format!("{} is not in the snapshot", at.display()))?;
        if m.is_symlink() {
            bail!("{} is a link in the snapshot: name what it leads to", at.display());
        }
        top.get_or_insert(m);
    }
    top.context("nothing named")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_may_be_restored() {
        assert_eq!(data_path("/home/alice").unwrap(), ("home", vec!["alice".to_string()]));
        assert_eq!(data_path("/home/alice/a b/c.txt").unwrap().1, ["alice", "a b", "c.txt"]);
        assert_eq!(data_path("/var/lib/postgresql/16").unwrap(), ("var-lib", vec!["postgresql".to_string(), "16".to_string()]));
        for bad in ["", "/", "/home", "/var", "/var/lib", "/etc/shadow", "/nix/store", "home/alice", "/home/alice/../bob", "/home/../etc", "/home/a\nb", "/var/lib/torq", "/var/lib/torq/state/ledger.jsonl", "/var/lib/nixos/uid-map", "/var/lib/private/x", "/var/lib/systemd/x", "/var/libx/y"] {
            assert!(data_path(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_link_in_a_snapshot_is_not_followed() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("alice/docs")).unwrap();
        fs::write(d.path().join("alice/docs/a"), "x").unwrap();
        std::os::unix::fs::symlink("/etc", d.path().join("alice/etc")).unwrap();
        let parts = |s: &str| s.split('/').map(String::from).collect::<Vec<_>>();
        assert!(no_links(d.path(), &parts("alice/docs/a")).is_ok());
        assert!(no_links(d.path(), &parts("alice/etc/shadow")).is_err(), "through a link");
        assert!(no_links(d.path(), &parts("alice/etc")).is_err(), "a link itself");
        assert!(no_links(d.path(), &parts("alice/none")).is_err());
    }
}
