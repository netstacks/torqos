//! The candidate configuration (ADR-016, docs/CLI-DESIGN.md, "The commit model"): a person's
//! uncommitted `set` / `delete`, kept per user until `commit` or `rollback 0`, with the commit it was
//! started from (its base). The config repo itself is only ever read here — with git plumbing, never
//! its working tree, hooks or the caller's git settings — and only the broker writes it.

use crate::tree::{Config, Data, Schema};
use anyhow::{bail, Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The machine's config repo, read-only.
pub struct Repo {
    root: PathBuf,
}

/// One configuration in the repo's history, newest first: `rollback <n>` loads the n-th.
#[derive(Debug, Clone, PartialEq)]
pub struct Commit {
    pub id: String,
    /// When it was made (ISO 8601, UTC).
    pub date: String,
    /// Its first line (the change's reason).
    pub subject: String,
}

impl Repo {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Repo { root: root.into() }
    }

    /// git on the repo, with nothing of the caller's environment or configuration that could point
    /// it elsewhere or run code: no GIT_* variables, no global or system config, no hooks, no
    /// fsmonitor. safe.directory names just this repo (it belongs to root).
    fn git(&self, args: &[&str]) -> Result<Vec<u8>> {
        let mut c = Command::new("git");
        for (k, _) in std::env::vars_os() {
            if k.to_string_lossy().starts_with("GIT_") {
                c.env_remove(&k);
            }
        }
        let root = self.root.to_string_lossy();
        let out = c
            .env("TZ", "UTC") // dates in the ledger's time
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(["-C", &root, "-c", &format!("safe.directory={root}"), "-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor=false"])
            .args(args)
            .output()
            .context("running git")?;
        if !out.status.success() {
            bail!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(out.stdout)
    }

    pub fn head(&self) -> Result<String> {
        Ok(String::from_utf8_lossy(&self.git(&["rev-parse", "--verify", "HEAD^{commit}"])?).trim().to_string())
    }

    /// The last `count` configurations, newest first (rollback 0 is the current one). The broker
    /// makes one commit per change on one branch, and resets an undone one away, so the branch's
    /// first-parent history is the machine's history of configurations.
    pub fn history(&self, count: usize) -> Result<Vec<Commit>> {
        let out = self.git(&["log", "--first-parent", "-z", &format!("-n{count}"), "--date=iso-strict-local", "--format=%H%x1f%cd%x1f%s", "HEAD", "--"])?;
        String::from_utf8_lossy(&out)
            .split('\0')
            .filter(|r| !r.trim().is_empty())
            .map(|r| {
                let mut f = r.trim_start_matches('\n').splitn(3, '\x1f');
                Ok(Commit {
                    id: f.next().context("git log")?.to_string(),
                    date: f.next().context("git log")?.to_string(),
                    subject: f.next().unwrap_or_default().to_string(),
                })
            })
            .collect()
    }

    /// The tree's files at `rev` (exact bytes): only paths the schema describes, and only regular
    /// files — a symlink or anything else at a tree path is refused, never followed.
    pub fn tree_files(&self, schema: &Schema, rev: &str) -> Result<BTreeMap<String, Vec<u8>>> {
        // A commit id (from head() or history()) or HEAD — never anything git could read as an
        // option or a path (`--output=…`, `HEAD:system`).
        let is_id = rev.len() == 40 && rev.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if !(is_id || rev == "HEAD") {
            bail!("not a commit id: {rev:?}");
        }
        let commit = format!("{rev}^{{commit}}");
        let listing = self.git(&["ls-tree", "-r", "-z", "--full-tree", "--end-of-options", &commit])?;
        let mut files = BTreeMap::new();
        for entry in listing.split(|b| *b == 0).filter(|e| !e.is_empty()) {
            let entry = String::from_utf8_lossy(entry);
            let (meta, path) = entry.split_once('\t').context("git ls-tree")?;
            let mut meta = meta.split(' ');
            let (mode, kind, id) = (meta.next().unwrap_or_default(), meta.next().unwrap_or_default(), meta.next().unwrap_or_default());
            if !schema.describes(path) {
                continue;
            }
            if mode != "100644" || kind != "blob" {
                bail!("{path} at {rev} is not a plain data file (git mode {mode}: a link, an executable or a folder)");
            }
            files.insert(path.to_string(), self.git(&["cat-file", "blob", id])?);
        }
        Ok(files)
    }
}

/// A person's candidate: the configuration they are editing, and the commit it started from.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub base: String,
    pub config: Config,
}

impl Candidate {
    /// A fresh candidate: the configuration at `rev` (`rollback <n>` passes an older one), based on
    /// the current HEAD — a rollback is a new change on top of today's configuration.
    pub fn from_repo(repo: &Repo, schema: &Schema, rev: &str) -> Result<Candidate> {
        let base = repo.head()?;
        let files = repo.tree_files(schema, rev)?;
        let config = schema.load(files.iter().map(|(p, b)| (p.as_str(), b.as_slice())))?;
        Ok(Candidate { base, config })
    }

    /// Where a user keeps their candidate: their own state directory, never shared.
    pub fn default_path() -> Result<PathBuf> {
        let state = match std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()) {
            Some(s) => PathBuf::from(s),
            None => PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(".local/state"),
        };
        Ok(state.join("torq/candidate.json"))
    }

    /// The saved candidate, if there is one (None: start from the active configuration). Only from
    /// a private folder, and never through a link.
    pub fn load(path: &Path, schema: &Schema) -> Result<Option<Candidate>> {
        if let Some(dir) = path.parent().filter(|d| d.exists()) {
            private_dir(dir)?;
        }
        if fs::symlink_metadata(path).is_ok_and(|m| !m.file_type().is_file()) {
            bail!("{} is not a plain file; remove it", path.display());
        }
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let v: serde_json::Value = serde_json::from_slice(&bytes).with_context(|| format!("reading {}", path.display()))?;
        if v["version"] != 1 {
            bail!("{}: not a candidate this torqcli reads; `rollback 0` starts a new one", path.display());
        }
        let base = v["base"].as_str().context("candidate without a base")?.to_string();
        let files = v["files"].as_object().context("candidate without files")?;
        let rendered: Vec<(String, String)> =
            files.iter().map(|(p, c)| Ok((p.clone(), c.as_str().context("candidate file")?.to_string()))).collect::<Result<_>>()?;
        let config = schema.load(rendered.iter().map(|(p, c)| (p.as_str(), c.as_bytes())))?;
        Ok(Some(Candidate { base, config }))
    }

    /// Save it atomically, readable by its owner only, in a folder only its owner can write: anyone
    /// else who could replace the file could change what the user commits under their name.
    pub fn save(&self, path: &Path) -> Result<()> {
        let dir = path.parent().context("candidate path")?;
        private_dir(dir)?;
        let files: serde_json::Map<String, serde_json::Value> =
            self.config.files.iter().map(|(p, d)| (p.clone(), d.render().into())).collect();
        let body = serde_json::to_vec_pretty(&serde_json::json!({ "version": 1, "base": self.base, "files": files }))?;
        let mut tmp = tempfile::NamedTempFile::new_in(dir)?; // created 0600
        tmp.write_all(&body)?;
        tmp.as_file().sync_all()?;
        tmp.persist(path).map_err(|e| e.error).with_context(|| format!("saving {}", path.display()))?;
        fs::File::open(dir)?.sync_all()?;
        Ok(())
    }

    pub fn discard(path: &Path) -> Result<()> {
        match fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }
}

/// Make sure `dir` exists, belongs to this user and only they can write it (created 0700).
fn private_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    if !dir.exists() {
        if let Some(parent) = dir.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::DirBuilder::new().mode(0o700).create(dir).or_else(|e| if dir.is_dir() { Ok(()) } else { Err(e) })?;
    }
    let me = fs::metadata("/proc/self").context("reading /proc/self")?.uid();
    let m = fs::symlink_metadata(dir)?;
    if !m.is_dir() || m.uid() != me || m.permissions().mode() & 0o022 != 0 {
        bail!(
            "{} must be a folder of your own that only you can write (it holds your uncommitted configuration)",
            dir.display()
        );
    }
    Ok(())
}

/// The changes from the base's files (exact text) to the candidate's: per changed file, its path,
/// old text (None: new) and new text (None: deleted). A file whose data is unchanged is no change,
/// however it is formatted.
pub fn changes<'a>(schema: &Schema, base: &'a BTreeMap<String, Vec<u8>>, candidate: &Config) -> Result<Vec<(String, Option<&'a [u8]>, Option<String>)>> {
    let before = schema.load(base.iter().map(|(p, b)| (p.as_str(), b.as_slice())))?;
    let paths: BTreeSet<&String> = base.keys().chain(candidate.files.keys()).collect();
    Ok(paths
        .into_iter()
        .filter(|p| before.files.get(*p) != candidate.files.get(*p))
        .map(|p| (p.clone(), base.get(p).map(Vec::as_slice), candidate.files.get(p).map(Data::render)))
        .collect())
}

/// The changes as one unified diff for `torq-broker propose` (git apply reads it): new files from
/// /dev/null, deleted ones to /dev/null.
pub fn unified_diff(scratch: &Path, changes: &[(String, Option<&[u8]>, Option<String>)]) -> Result<String> {
    let mut out = String::new();
    for (i, (path, old, new)) in changes.iter().enumerate() {
        let side = |name: &str, text: Option<&[u8]>| -> Result<(PathBuf, String)> {
            Ok(match text {
                Some(t) => {
                    let f = scratch.join(format!("{name}-{i}"));
                    fs::write(&f, t)?;
                    (f, format!("{name}/{path}"))
                }
                None => (PathBuf::from("/dev/null"), "/dev/null".to_string()),
            })
        };
        let (a, a_label) = side("a", *old)?;
        let (b, b_label) = side("b", new.as_deref().map(str::as_bytes))?;
        let o = Command::new("diff").arg("-u").args(["--label", &a_label, "--label", &b_label]).arg(&a).arg(&b).output().context("running diff")?;
        if o.status.code() != Some(1) {
            bail!("diff failed on {path}: {}", String::from_utf8_lossy(&o.stderr));
        }
        out.push_str(&String::from_utf8_lossy(&o.stdout));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> Schema {
        Schema::parse(include_str!("schema-fixture.json")).unwrap()
    }
    fn w(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }
    fn sh(dir: &Path, script: &str) {
        let st = Command::new("sh").arg("-c").arg(script).current_dir(dir).env("GIT_CONFIG_GLOBAL", "/dev/null").status().unwrap();
        assert!(st.success(), "{script}");
    }
    /// A config repo with three configurations, as the broker would have made them.
    fn repo() -> (tempfile::TempDir, Repo) {
        let d = tempfile::tempdir().unwrap();
        sh(d.path(), "git init -q -b main && git config user.name t && git config user.email t@t \
            && mkdir -p system users/dev && echo '{ x = 1; }' > flake.nix \
            && printf '{ \"host-name\": \"one\" }\\n' > system/host.json && git add -A && git commit -q -m 'first' \
            && printf '[\\n  \"htop\"\\n]\\n' > system/packages.json && git add -A && git commit -q -m 'second' \
            && printf '{ \"host-name\": \"three\" }\\n' > system/host.json && git add -A && git commit -q -m 'third'");
        let r = Repo::new(d.path());
        (d, r)
    }

    #[test]
    fn reads_history_newest_first() {
        let (_d, r) = repo();
        let h = r.history(10).unwrap();
        assert_eq!(h.iter().map(|c| c.subject.as_str()).collect::<Vec<_>>(), ["third", "second", "first"]);
        assert_eq!(h[0].id, r.head().unwrap());
    }

    #[test]
    fn reads_only_the_tree_files_at_a_commit() {
        let (_d, r) = repo();
        let s = schema();
        let now = r.tree_files(&s, "HEAD").unwrap();
        assert_eq!(now.keys().collect::<Vec<_>>(), ["system/host.json", "system/packages.json"], "flake.nix is not a tree file");
        let first = r.tree_files(&s, &r.history(3).unwrap()[2].id).unwrap();
        assert_eq!(String::from_utf8_lossy(&first["system/host.json"]), "{ \"host-name\": \"one\" }\n");
    }

    #[test]
    fn a_link_at_a_tree_path_is_refused() {
        let (d, r) = repo();
        sh(d.path(), "ln -s ../flake.nix users/dev/cli.json && git add -A && git commit -q -m link");
        let e = format!("{:#}", r.tree_files(&schema(), "HEAD").unwrap_err());
        assert!(e.contains("users/dev/cli.json") && e.contains("not a plain data file"), "{e}");
    }

    /// `rollback 2` is the configuration two changes back, as a new change on today's HEAD.
    #[test]
    fn a_rollback_loads_an_old_configuration_on_the_current_base() {
        let (_d, r) = repo();
        let s = schema();
        let c = Candidate::from_repo(&r, &s, &r.history(3).unwrap()[2].id).unwrap();
        assert_eq!(c.base, r.head().unwrap());
        assert_eq!(s.show(&c.config, &[]).unwrap(), "system {\n    host-name one;\n}\n");
    }

    #[test]
    fn a_candidate_is_saved_privately_and_read_back() {
        let (_d, r) = repo();
        let s = schema();
        let mut c = Candidate::from_repo(&r, &s, "HEAD").unwrap();
        s.set(&mut c.config, &w("users dev cli style cisco")).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("torq/candidate.json");
        c.save(&path).unwrap();
        let mode = std::os::unix::fs::PermissionsExt::mode(&fs::metadata(&path).unwrap().permissions()) & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(Candidate::load(&path, &s).unwrap(), Some(c));
        Candidate::discard(&path).unwrap();
        assert_eq!(Candidate::load(&path, &s).unwrap(), None);
        Candidate::discard(&path).unwrap(); // twice is fine
        fs::write(&path, r#"{"version":2,"base":"x","files":{}}"#).unwrap();
        assert!(format!("{:#}", Candidate::load(&path, &s).unwrap_err()).contains("rollback 0"));
    }

    /// The diff for the broker: only files whose data changed, from their exact text, and it
    /// applies to the repo as `git apply` (the broker) reads it.
    #[test]
    fn the_diff_holds_exactly_the_changes_and_applies() {
        let (d, r) = repo();
        let s = schema();
        // Hand-formatted, but the same data: no change.
        sh(d.path(), "printf '{\"host-name\":\"three\"}' > system/host.json && git commit -qam reformat");
        let base = r.tree_files(&s, "HEAD").unwrap();
        let mut c = Candidate::from_repo(&r, &s, "HEAD").unwrap();
        assert!(changes(&s, &base, &c.config).unwrap().is_empty());
        s.set(&mut c.config, &w("system time-zone UTC")).unwrap(); // changes an existing file
        s.delete(&mut c.config, &w("system packages")).unwrap(); // deletes one
        s.set(&mut c.config, &w("users dev cli style cisco")).unwrap(); // adds one
        let ch = changes(&s, &base, &c.config).unwrap();
        assert_eq!(ch.iter().map(|(p, o, n)| (p.as_str(), o.is_some(), n.is_some())).collect::<Vec<_>>(),
            [("system/host.json", true, true), ("system/packages.json", true, false), ("users/dev/cli.json", false, true)]);
        let scratch = tempfile::tempdir().unwrap();
        let diff = unified_diff(scratch.path(), &ch).unwrap();
        fs::write(scratch.path().join("change.diff"), &diff).unwrap();
        sh(d.path(), &format!("git apply --index {}", scratch.path().join("change.diff").display()));
        let applied = fs::read_to_string(d.path().join("system/host.json")).unwrap();
        assert_eq!(applied, "{\n  \"host-name\": \"three\",\n  \"time-zone\": \"UTC\"\n}\n");
        assert!(!d.path().join("system/packages.json").exists());
        assert_eq!(fs::read_to_string(d.path().join("users/dev/cli.json")).unwrap(), "{\n  \"style\": \"cisco\"\n}\n");
    }
}

#[cfg(test)]
mod review_tests {
    use super::*;

    fn schema() -> Schema {
        Schema::parse(include_str!("schema-fixture.json")).unwrap()
    }
    fn sh(dir: &Path, script: &str) {
        let st = Command::new("sh").arg("-c").arg(script).current_dir(dir).env("GIT_CONFIG_GLOBAL", "/dev/null").status().unwrap();
        assert!(st.success(), "{script}");
    }

    /// A hand-ordered list with a repeat is the same configuration after a save and a load: no
    /// phantom change (review of d373172).
    #[test]
    fn list_order_is_no_change() {
        let d = tempfile::tempdir().unwrap();
        sh(d.path(), "git init -q -b main && git config user.name t && git config user.email t@t && mkdir system \
            && echo '[\"zsh\",\"htop\",\"zsh\"]' > system/packages.json && git add -A && git commit -qm x");
        let (r, s) = (Repo::new(d.path()), schema());
        let c = Candidate::from_repo(&r, &s, "HEAD").unwrap();
        let state = tempfile::tempdir().unwrap();
        let p = state.path().join("torq/candidate.json");
        c.save(&p).unwrap();
        let again = Candidate::load(&p, &s).unwrap().unwrap();
        assert!(changes(&s, &r.tree_files(&s, "HEAD").unwrap(), &again.config).unwrap().is_empty());
    }

    #[test]
    fn the_candidate_folder_must_be_private() {
        use std::os::unix::fs::PermissionsExt;
        let state = tempfile::tempdir().unwrap();
        let dir = state.path().join("torq");
        let c = Candidate { base: "x".into(), config: Config::default() };
        c.save(&dir.join("candidate.json")).unwrap();
        assert_eq!(fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700, "created private");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o777)).unwrap();
        for e in [c.save(&dir.join("candidate.json")).unwrap_err(), Candidate::load(&dir.join("candidate.json"), &schema()).unwrap_err()] {
            assert!(format!("{e:#}").contains("only you can write"), "{e:#}");
        }
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        fs::remove_file(dir.join("candidate.json")).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", dir.join("candidate.json")).unwrap();
        assert!(format!("{:#}", Candidate::load(&dir.join("candidate.json"), &schema()).unwrap_err()).contains("not a plain file"));
    }

    #[test]
    fn only_commit_ids_reach_git() {
        let d = tempfile::tempdir().unwrap();
        sh(d.path(), "git init -q -b main && git config user.name t && git config user.email t@t && touch a && git add -A && git commit -qm x");
        let r = Repo::new(d.path());
        for bad in ["--output=/tmp/x", "HEAD:system", "HEAD~1", "main", "ABCDEF0123456789ABCDEF0123456789ABCDEF01", ""] {
            assert!(format!("{:#}", r.tree_files(&schema(), bad).unwrap_err()).contains("not a commit id"), "{bad}");
        }
        assert!(r.tree_files(&schema(), &r.head().unwrap()).is_ok());
    }
}
