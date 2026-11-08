//! What an evaluation of a committed configuration found, kept so it is not evaluated again: SSH as
//! the configuration at a commit has it (the state before a commit's change). A commit is its files
//! and its lock, so the same commit always evaluates the same. Kept in the person's own cache folder
//! (`$XDG_CACHE_HOME/torq`, else `~/.cache/torq`), 0700, never read or written through a link; an
//! entry that cannot be read is evaluated again, never trusted half-read.

use crate::session::SshFacts;
use anyhow::{bail, Context, Result};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// The form of an entry: a torqcli that reads SSH differently names its entries differently.
const FORM: &str = "ssh-v1";
/// More than this is not an entry torqcli wrote.
const MAX: u64 = 1 << 20;

/// Where entries are kept; None: nowhere (no home, or a test that keeps none).
#[derive(Debug, Clone, Default)]
pub struct Cache(Option<PathBuf>);

impl Cache {
    /// None kept: everything is evaluated.
    pub fn none() -> Self {
        Cache(None)
    }

    /// In `dir` (a test's).
    #[cfg(test)]
    pub fn at(dir: &Path) -> Self {
        Cache(Some(dir.to_path_buf()))
    }

    /// The person's own: `$XDG_CACHE_HOME/torq` (an absolute one), else `~/.cache/torq`.
    pub fn personal() -> Self {
        let base = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")));
        Cache(base.map(|b| b.join("torq")))
    }

    fn file(&self, commit: &str) -> Option<PathBuf> {
        // A commit id and nothing else: it names the file.
        let id = commit.len() >= 40 && commit.len() <= 64 && commit.chars().all(|c| c.is_ascii_hexdigit());
        self.0.as_ref().filter(|_| id).map(|d| d.join(format!("{FORM}-{commit}.json")))
    }

    /// SSH as the configuration at `commit` has it, if it is kept and reads as torqcli wrote it.
    pub fn get(&self, commit: &str) -> Option<SshFacts> {
        let f = self.file(commit)?;
        crate::candidate::private_dir(f.parent()?).ok()?;
        let mut text = Vec::new();
        no_link(&f).ok()?.take(MAX + 1).read_to_end(&mut text).ok()?;
        if text.len() as u64 > MAX {
            return None;
        }
        serde_json::from_slice(&text).ok()
    }

    /// Keep `facts` as the configuration at `commit`'s. Not kept is no failure: it is evaluated
    /// again next time.
    pub fn put(&self, commit: &str, facts: &SshFacts) {
        let _ = self.try_put(commit, facts);
    }

    fn try_put(&self, commit: &str, facts: &SshFacts) -> Result<()> {
        let f = self.file(commit).context("nowhere to keep it")?;
        let dir = f.parent().context("a folder")?;
        crate::candidate::private_dir(dir)?;
        // Written whole, then renamed into place: a rename replaces a link, never follows it.
        let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
        tmp.write_all(&serde_json::to_vec(facts)?)?;
        tmp.persist(&f).map_err(|e| e.error)?;
        Ok(())
    }
}

/// `path` opened for reading, refused if it is a link (or not a plain file).
fn no_link(path: &Path) -> Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let f = fs::OpenOptions::new().read(true).custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32).open(path)?;
    if !f.metadata()?.is_file() {
        bail!("{} is not a plain file", path.display());
    }
    Ok(f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Ssh;

    const C: &str = "0123456789abcdef0123456789abcdef01234567";

    fn facts(on: bool) -> SshFacts {
        SshFacts { state: Ssh { on, passwords: false }, admits: serde_json::json!({"owner": "dev"}) }
    }

    #[test]
    fn kept_by_commit_privately_and_never_through_a_link() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let c = Cache::at(&d.path().join("torq"));
        assert_eq!(c.get(C), None, "nothing kept yet");
        c.put(C, &facts(true));
        assert_eq!(c.get(C), Some(facts(true)));
        let dir = d.path().join("torq");
        assert_eq!(fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700, "made 0700");
        // Not a commit id: neither kept nor read.
        c.put("../x", &facts(true));
        assert_eq!(c.get("../x"), None);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        // What cannot be read is evaluated again: a broken entry, a link.
        let f = dir.join(format!("{FORM}-{C}.json"));
        fs::write(&f, "{not json").unwrap();
        assert_eq!(c.get(C), None);
        let elsewhere = d.path().join("planted.json");
        fs::write(&elsewhere, serde_json::to_vec(&facts(false)).unwrap()).unwrap();
        fs::remove_file(&f).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &f).unwrap();
        assert_eq!(c.get(C), None, "a link is never followed");
        c.put(C, &facts(true));
        assert_eq!(fs::read(&elsewhere).unwrap(), serde_json::to_vec(&facts(false)).unwrap(), "nor written through");
        assert_eq!(c.get(C), Some(facts(true)));
        // A folder others can write is not the person's own.
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o777)).unwrap();
        assert_eq!(c.get(C), None);
        assert_eq!(Cache::none().get(C), None);
    }
}
