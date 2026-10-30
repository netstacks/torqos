//! Git operations on the local config repo. Shells out to `git`; the broker identity is fixed.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
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
        let user_name = format!("user.name={NAME}");
        let user_email = format!("user.email={EMAIL}");
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .arg("-c")
            .arg(&user_name)
            .arg("-c")
            .arg(&user_email)
            .arg("-c")
            .arg("commit.gpgsign=false") // TODO(M3): sign with the broker key
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

    /// Files a unified diff would touch, without applying it.
    pub fn diff_files(&self, diff_path: &Path) -> Result<Vec<String>> {
        let p = diff_path.to_string_lossy().into_owned();
        self.git(&["apply", "--check", p.as_str()])
            .context("diff does not apply cleanly to the current config")?;
        let numstat = self.git(&["apply", "--numstat", p.as_str()])?;
        Ok(numstat
            .lines()
            .filter_map(|l| l.split('\t').nth(2).map(String::from))
            .collect())
    }

    pub fn apply_and_commit(&self, diff_path: &Path, message: &str) -> Result<String> {
        self.ensure_clean()?;
        let p = diff_path.to_string_lossy().into_owned();
        self.git(&["apply", "--index", p.as_str()])?;
        self.git(&["commit", "-q", "-m", message])?;
        self.head()
    }

    pub fn revert_and_commit(&self, commit: &str, message: &str) -> Result<String> {
        self.ensure_clean()?;
        self.git(&["revert", "--no-commit", commit])?;
        self.git(&["commit", "-q", "-m", message])?;
        self.head()
    }

    pub fn hard_reset(&self, commit: &str) -> Result<()> {
        self.git(&["reset", "-q", "--hard", commit])?;
        Ok(())
    }

    pub fn show(&self, commit: &str) -> Result<String> {
        self.git(&["show", "--stat", "--patch", commit])
    }
}
