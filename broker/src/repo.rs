//! Git operations on the local config repo. Shells out to `git`; the broker identity is fixed.

use anyhow::{bail, Context, Result};
use std::ffi::OsStr;
use std::path::{self, Path, PathBuf};
use std::process::Command;

pub struct Repo {
    root: PathBuf,
}

const NAME: &str = "kestrel-broker";
const EMAIL: &str = "broker@kestrel.local";

impl Repo {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn git(&self, args: &[&str]) -> Result<String> {
        self.git_env(&[], args)
    }

    fn git_env(&self, env: &[(&str, &OsStr)], args: &[&str]) -> Result<String> {
        let user_name = format!("user.name={NAME}");
        let user_email = format!("user.email={EMAIL}");
        let out = Command::new("git")
            .envs(env.iter().copied())
            .arg("-C")
            .arg(&self.root)
            .arg("-c")
            .arg(&user_name)
            .arg("-c")
            .arg(&user_email)
            .arg("-c")
            .arg("commit.gpgsign=false") // TODO(M3): sign with the broker key
            // The ledger is fsynced on every append; git must not fall behind it on power loss.
            .arg("-c")
            .arg("core.fsync=all")
            .args(args)
            .output()
            .context("running git")?;
        if !out.status.success() {
            bail!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    }

    /// Flush the repo's whole filesystem (`syncfs`): working tree, objects, refs and directory
    /// renames. `core.fsync` covers only git's own files, but the rebuild builds the working tree
    /// and the ledger is fsynced on every append, so after a power loss the repo must not be behind
    /// either. Called after every write, before the rebuild and the ledger append.
    fn sync(&self) -> Result<()> {
        let st = Command::new("sync").arg("--file-system").arg(&self.root).status().context("running sync")?;
        if !st.success() {
            bail!("sync --file-system {} failed ({st})", self.root.display());
        }
        Ok(())
    }

    pub fn is_initialised(&self) -> bool {
        self.root.join(".git").exists()
    }

    /// Initialise the repo (if needed) and commit whatever is present as the baseline.
    pub fn init(&self) -> Result<()> {
        std::fs::create_dir_all(&self.root)?;
        if !self.is_initialised() {
            self.git(&["init", "-q", "-b", "main"])?;
        }
        self.git(&["add", "-A"])?;
        if self.git(&["status", "--porcelain"])?.trim().is_empty() && self.head().is_ok() {
            return Ok(());
        }
        self.git(&["commit", "-q", "--allow-empty", "-m", "Baseline configuration\n\nLedger-Entry: init"])?;
        self.sync()?;
        Ok(())
    }

    pub fn head(&self) -> Result<String> {
        Ok(self.git(&["rev-parse", "HEAD"])?.trim().to_string())
    }

    pub fn ensure_clean(&self) -> Result<()> {
        let s = self.git(&["status", "--porcelain"])?;
        if !s.trim().is_empty() {
            bail!("config repo has uncommitted changes; only the broker may write to it:\n{s}");
        }
        Ok(())
    }

    /// Every path a unified diff would add, modify or delete, without applying it.
    ///
    /// The diff is applied to a scratch index built from HEAD and compared with HEAD, so this is
    /// what the diff *does*, not what its headers say: a rename reports both the old and the new
    /// path (numstat reports only the new one), and names are raw (`-z`), not C-quoted.
    pub fn diff_files(&self, diff_path: &Path) -> Result<Vec<String>> {
        let p = path::absolute(diff_path)?.to_string_lossy().into_owned();
        let index = path::absolute(self.root.join(".git/kestrel-proposal.index"))?;
        let env = [("GIT_INDEX_FILE", index.as_os_str())];
        let files = (|| {
            self.git_env(&env, &["read-tree", "HEAD"])?;
            self.git_env(&env, &["apply", "--cached", p.as_str()])
                .context("diff does not apply cleanly to the current config")?;
            let out = self.git_env(&env, &["diff", "--cached", "--name-only", "--no-renames", "-z", "HEAD"])?;
            Ok(out.split('\0').filter(|s| !s.is_empty()).map(String::from).collect())
        })();
        let _ = std::fs::remove_file(&index);
        files
    }

    pub fn apply_and_commit(&self, diff_path: &Path, message: &str) -> Result<String> {
        self.ensure_clean()?;
        let p = path::absolute(diff_path)?.to_string_lossy().into_owned();
        self.git(&["apply", "--index", p.as_str()])?;
        self.git(&["commit", "-q", "-m", message])?;
        self.sync()?;
        self.head()
    }

    pub fn revert_and_commit(&self, commit: &str, message: &str) -> Result<String> {
        self.ensure_clean()?;
        if let Err(e) = self.git(&["revert", "--no-commit", commit]) {
            // A conflicting revert leaves REVERT_HEAD and conflict markers behind, which would
            // block every later broker operation. Put the repo back exactly as it was.
            let _ = self.git(&["revert", "--abort"]);
            self.git(&["reset", "-q", "--hard", "HEAD"])?;
            self.sync()?;
            return Err(e.context(format!("cannot undo {commit} cleanly: later changes conflict with it")));
        }
        self.git(&["commit", "-q", "-m", message])?;
        self.sync()?;
        self.head()
    }

    pub fn hard_reset(&self, commit: &str) -> Result<()> {
        self.git(&["reset", "-q", "--hard", commit])?;
        self.sync()?;
        Ok(())
    }

    pub fn show(&self, commit: &str) -> Result<String> {
        self.git(&["show", "--stat", "--patch", commit])
    }
}
