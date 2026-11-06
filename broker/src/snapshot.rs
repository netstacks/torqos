//! What may be put back from a snapshot of the data (ADR-038), and how. The paths come from a person,
//! and the folders they name hold what any account put there.
use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;

/// Run as the owner, reading what root reads from the snapshot on standard input: a tar of a folder's
/// contents, or a file's bytes. It sets aside what is there (unless root has: `$2` empty), writes, and
/// on failure puts things back. Links in the way lead nowhere the owner could not go.
const WRITE: &str = r#"set -eu
d=$1 aside=$2 kind=$3 mode=$4
moved=
if [ -n "$aside" ] && { [ -e "$d" ] || [ -L "$d" ]; }; then
  [ ! -e "$aside" ] && [ ! -L "$aside" ] || { echo "$aside is already there: move it first" >&2; exit 1; }
  mv -T -- "$d" "$aside"
  moved=1
fi
put() {
  mkdir -p -- "$(dirname -- "$d")" || return 1
  if [ "$kind" = folder ]; then
    mkdir -p -- "$d" && tar -x -p --no-same-owner -f - -C "$d"
  else
    cat > "$d" && chmod -- "$mode" "$d"
  fi
}
if ! put; then
  [ -n "$aside" ] || exit 1
  rm -rf -- "$d"
  [ -z "$moved" ] || mv -T -- "$aside" "$d"
  exit 1
fi
"#;

/// One restore: what is read as root, and who writes it where.
pub struct Copy<'a> {
    pub source: PathBuf,
    pub to: &'a str,
    pub set_aside: &'a str,
    pub folder: bool,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    /// `to` sits in a folder of root's: root moves what is there and makes the new one.
    pub whole: bool,
    pub had: bool,
}

impl Copy<'_> {
    pub fn run(&self) -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        // Never set-id: what a person once could run as a group they have left, or as someone else.
        let mode = self.mode & 0o777;
        if self.whole {
            if self.had {
                fs::rename(self.to, self.set_aside).with_context(|| format!("setting {} aside", self.to))?;
            }
            let made = (|| -> Result<()> {
                if self.folder {
                    fs::create_dir(self.to)?;
                } else {
                    fs::OpenOptions::new().write(true).create_new(true).open(self.to)?;
                }
                std::os::unix::fs::chown(self.to, Some(self.uid), Some(self.gid))?;
                fs::set_permissions(self.to, fs::Permissions::from_mode(mode | 0o700))?;
                Ok(())
            })();
            if let Err(e) = made.and_then(|()| self.stream(mode)) {
                self.undo();
                return Err(e);
            }
            return Ok(());
        }
        self.stream(mode)
    }

    /// Root's reader and the owner's writer, joined by a pipe and nothing else.
    fn stream(&self, mode: u32) -> Result<()> {
        let mut read = if self.folder {
            let mut c = crate::config::command("tar");
            c.args(["-c", "-f", "-", "--one-file-system", "--mode=u-s,g-s", "-C"]).arg(&self.source).arg(".");
            c
        } else {
            let mut c = crate::config::command("cat");
            c.arg("--").arg(&self.source);
            c
        };
        let mut reader = read.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().context("reading the snapshot")?;
        let pipe = reader.stdout.take().context("the snapshot's pipe")?;
        let aside = if self.whole { "" } else { self.set_aside };
        let wrote = crate::config::command("setpriv")
            .args(["--reuid", &self.uid.to_string(), "--regid", &self.gid.to_string(), "--clear-groups", "--no-new-privs", "--", "sh", "-c", WRITE, "sh"])
            .args([self.to, aside, if self.folder { "folder" } else { "file" }, &format!("{mode:o}")])
            .stdin(pipe)
            .output()
            .context("running the copy")?;
        let read_ok = reader.wait().context("reading the snapshot")?.success();
        if !wrote.status.success() {
            bail!("{}", String::from_utf8_lossy(&wrote.stderr).trim());
        }
        if !read_ok {
            bail!("the snapshot could not be read whole");
        }
        Ok(())
    }

    /// Back as it was, for a folder of root's: its parent is no one else's, and `remove_dir_all` follows no link.
    fn undo(&self) {
        let _ = if self.folder { fs::remove_dir_all(self.to) } else { fs::remove_file(self.to) };
        if self.had {
            let _ = fs::rename(self.set_aside, self.to);
        }
    }
}

/// A snapshot's label: digits, as `torq-snapshot` makes them. A ledger can come from elsewhere.
pub fn label_ok(label: &str) -> bool {
    !label.is_empty() && label.len() <= 20 && label.bytes().all(|b| b.is_ascii_digit())
}

/// The path as checked, whatever spelling it came in.
pub fn written_out(tree: &str, parts: &[String]) -> String {
    format!("{}/{}", if tree == "home" { "/home" } else { "/var/lib" }, parts.join("/"))
}

/// The primary group of the account with `uid` now, from /etc/passwd's text: a group kept from a
/// snapshot may be one the person has left.
pub fn primary_group(passwd: &str, uid: u32) -> Option<u32> {
    passwd.lines().find_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        (f.len() >= 4 && f[2].parse() == Ok(uid)).then(|| f[3].parse().ok()).flatten()
    })
}

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

/// `parts`, below `root`, with no link on the way: the first part's metadata (whose it all is) and
/// the last's. A snapshot is read only, so what is checked here stays so.
pub fn no_links(root: &Path, parts: &[String]) -> Result<(fs::Metadata, fs::Metadata)> {
    let mut at = root.to_path_buf();
    let mut top = None;
    let mut last = None;
    for p in parts {
        at.push(p);
        let m = fs::symlink_metadata(&at).with_context(|| format!("{} is not in the snapshot", at.display()))?;
        if m.is_symlink() {
            bail!("{} is a link in the snapshot: name what it leads to", at.display());
        }
        top.get_or_insert(m.clone());
        last = Some(m);
    }
    Ok((top.context("nothing named")?, last.context("nothing named")?))
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
    fn labels_and_accounts() {
        assert!(label_ok("1793974624357"));
        for bad in ["", "/", "..", "1/2", "12a", "../../etc", "123456789012345678901"] {
            assert!(!label_ok(bad), "{bad:?}");
        }
        let passwd = "root:x:0:0::/root:/bin/sh\nbob:x:1001:100::/home/bob:/bin/sh\nodd:x:7\n";
        assert_eq!((primary_group(passwd, 1001), primary_group(passwd, 7), primary_group(passwd, 99)), (Some(100), None, None));
        assert_eq!(written_out("var-lib", &["pg".to_string(), "16".to_string()]), "/var/lib/pg/16");
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
