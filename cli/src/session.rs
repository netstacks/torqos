//! A torqcli session (ADR-016, docs/CLI-DESIGN.md): operational and configuration mode, and the
//! commands of each in Juniper syntax. Everything that reaches the broker or evaluates Nix goes
//! through `Machine`, so the whole command layer is testable without either.

use crate::candidate::{self, Candidate, Repo};
use crate::syntax::{self, CommitOptions, Op, Pipes, Style};
use crate::tree::{Config, Kind, Schema};
use anyhow::{bail, Context, Result};
use std::path::PathBuf;

/// What the session needs from the machine: evaluation and the broker.
pub trait Machine {
    /// `commit check`: does the configuration at commit `base`, with these tree files changed
    /// (None: deleted), evaluate?
    fn check(&self, base: &str, files: &[(String, Option<String>)]) -> Result<()>;
    /// Propose `diff` against `base` and apply it (with `confirm` minutes: commit confirmed). The
    /// broker's tier must be `tier`. Returns the applied ledger entry's number.
    fn commit(&self, diff: &str, base: &str, tier: &str, reason: &str, confirm: Option<u32>) -> Result<u64>;
    /// A commit confirmed waiting, if there is one.
    fn pending_confirm(&self) -> Result<Option<Waiting>>;
    /// Keep the waiting commit confirmed.
    fn confirm(&self) -> Result<u64>;
    /// Ask a person (a system-tier commit); false when there is no one to ask.
    fn ask(&self, question: &str) -> bool;
    /// `show system alarms`: what is not as the ledger says (and a commit confirmed waiting).
    fn alarms(&self) -> Result<String>;
    /// `show packages available <term>…`.
    fn search(&self, terms: &[String]) -> Result<String>;
    /// `request system repair`: return the machine to its last ledgered configuration.
    fn repair(&self) -> Result<String>;
}


/// A commit confirmed waiting for its author.
#[derive(Debug, Clone, PartialEq)]
pub struct Waiting {
    /// Its change (the applied ledger entry).
    pub seq: u64,
    pub deadline: String,
    /// Who made it: the one who may confirm it.
    pub by: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    Operational,
    Configuration,
}

pub struct Session<'a> {
    pub schema: Schema,
    pub repo: Repo,
    /// Where the candidate is kept between sessions; None: a private one (`torqcli -c`), discarded
    /// at the end.
    pub saved: Option<PathBuf>,
    pub mode: Mode,
    /// The syntax spoken (`switch cli` changes it for the session).
    pub style: Style,
    /// Where configuration mode is (`edit system`; Cisco: the submode).
    edit: Vec<String>,
    candidate: Option<Candidate>,
    machine: &'a dyn Machine,
    /// Who is at the shell (their login name): their own folder needs no question.
    me: String,
    /// Set by `exit` in operational mode (or `quit`).
    pub done: bool,
}

/// Split a command line into words: whitespace-separated, "double quotes" for a word with spaces,
/// `|` always its own word.
pub fn words(line: &str) -> Result<Vec<String>> {
    let (mut out, mut cur, mut quoted, mut any) = (Vec::new(), String::new(), false, false);
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            '\\' if quoted => cur.push(chars.next().context("a line cannot end in \\")?),
            c if c.is_whitespace() && !quoted => {
                if any {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            '|' if !quoted => {
                if any {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
                out.push("|".into());
            }
            c => {
                cur.push(c);
                any = true;
            }
        }
    }
    if quoted {
        bail!("missing closing \"");
    }
    if any {
        out.push(cur);
    }
    Ok(out)
}

impl<'a> Session<'a> {
    pub fn new(schema: Schema, repo: Repo, saved: Option<PathBuf>, machine: &'a dyn Machine, me: &str, style: Style) -> Self {
        Session { schema, repo, saved, mode: Mode::Operational, style, edit: Vec::new(), candidate: None, machine, me: me.into(), done: false }
    }

    /// The configuration that is committed now.
    fn active(&self) -> Result<Config> {
        let files = self.repo.tree_files(&self.schema, "HEAD")?;
        self.schema.load(files.iter().map(|(p, b)| (p.as_str(), b.as_slice())))
    }

    fn candidate(&mut self) -> Result<&mut Candidate> {
        if self.candidate.is_none() {
            let c = match &self.saved {
                Some(p) => Candidate::load(p, &self.schema)?,
                None => None,
            };
            self.candidate = Some(match c {
                Some(c) => c,
                None => Candidate::from_repo(&self.repo, &self.schema, "HEAD")?,
            });
        }
        Ok(self.candidate.as_mut().unwrap())
    }

    fn save(&self) -> Result<()> {
        if let (Some(p), Some(c)) = (&self.saved, &self.candidate) {
            c.save(p)?;
        }
        Ok(())
    }

    /// Throw the candidate away: the next edit starts from the active configuration.
    fn discard(&mut self) -> Result<()> {
        self.candidate = None;
        if let Some(p) = &self.saved {
            Candidate::discard(p)?;
        }
        Ok(())
    }

    /// Run one command line; returns what to print.
    pub fn exec(&mut self, line: &str) -> Result<String> {
        let w = words(line)?;
        if w.is_empty() {
            return Ok(String::new());
        }
        let op = syntax::parse(self.style, self.mode, &w, &|ws| self.is_place(ws))?;
        self.run(op)
    }

    /// Whether `words`, from where configuration mode is, name a container or list entry.
    fn is_place(&self, words: &[String]) -> bool {
        let full: Vec<String> = self.edit.iter().chain(words).cloned().collect();
        let cfg = self.candidate.as_ref().map(|c| &c.config);
        self.schema.resolve_in(&full, cfg).is_ok_and(|t| {
            t.values.is_empty() && !t.node.is_empty() && matches!(self.schema.nodes[&t.node].kind, Kind::Container | Kind::List)
        })
    }

    /// Where configuration mode is (`edit system`): its words, for the prompt.
    pub fn edit_path(&self) -> &[String] {
        &self.edit
    }

    /// `words` from where configuration mode is.
    fn here(&self, words: &[String]) -> Vec<String> {
        self.edit.iter().chain(words).cloned().collect()
    }

    fn run(&mut self, op: Op) -> Result<String> {
        match op {
            Op::Configure => self.configure(),
            Op::ShowActive { path, pipes } => {
                let active = self.active()?;
                Ok(if pipes.display_set { self.display_set(&active, &path)? } else { self.schema.show(&active, &path)? })
            }
            Op::ShowCommits | Op::ShowRollback { .. } | Op::ShowAlarms | Op::ShowServices | Op::ShowPackages { .. } => {
                self.show_operational(op)
            }
            Op::Request { words } => self.request(&words),
            Op::SwitchCli => {
                self.style = match self.style {
                    Style::Juniper => Style::Cisco,
                    Style::Cisco => Style::Juniper,
                };
                Ok(format!("Switched to the {} style (for this session)\n", self.style.name()))
            }
            Op::Exit if self.mode == Mode::Operational => {
                self.done = true;
                Ok(String::new())
            }
            Op::Exit if !self.edit.is_empty() => {
                self.up();
                Ok(String::new())
            }
            Op::Exit | Op::End => self.leave(),
            Op::Up => {
                if self.edit.is_empty() {
                    bail!("already at the top of the configuration");
                }
                self.up();
                Ok(String::new())
            }
            Op::Top => {
                self.edit.clear();
                Ok(String::new())
            }
            Op::Edit { words } => {
                let full = self.here(&words);
                let cfg = self.candidate()?.config.clone();
                let t = self.schema.resolve_in(&full, Some(&cfg))?;
                let kind = (!t.node.is_empty()).then(|| self.schema.nodes[&t.node].kind);
                if !t.values.is_empty() || !matches!(kind, Some(Kind::Container | Kind::List)) {
                    bail!("{}: not somewhere to go (a setting takes a value)", full.join(" "));
                }
                self.edit = t.words;
                Ok(String::new())
            }
            Op::Set { words } => {
                let s = self.schema.clone();
                let full = self.here(&words);
                s.set(&mut self.candidate()?.config, &full)?;
                self.save()?;
                Ok(String::new())
            }
            Op::Delete { words, all } => {
                let s = self.schema.clone();
                let full = self.here(&words);
                let cfg = self.candidate()?.config.clone();
                let t = s.resolve_in(&full, Some(&cfg))?;
                let whole = t.node.is_empty() || matches!(s.nodes[&t.node].kind, Kind::Container | Kind::List);
                let (del, no) = match self.style {
                    Style::Juniper => ("delete", "delete"),
                    Style::Cisco => ("no", "no"),
                };
                if whole && !all {
                    bail!("{del} {}: deletes everything below it — say `{no} {} all`", full.join(" "), full.join(" "));
                }
                if all && !whole {
                    bail!("{del} {}: `all` is for a whole container", full.join(" "));
                }
                s.delete(&mut self.candidate()?.config, &full)?;
                self.save()?;
                Ok(String::new())
            }
            Op::ShowCandidate { path, pipes } => self.show_candidate(&self.here(&path), &pipes),
            Op::Compare => self.compare(),
            Op::Rollback { n } => self.rollback(n),
            Op::Commit(o) => self.commit(o),
            Op::Run(op) => {
                let was = self.mode;
                self.mode = Mode::Operational;
                let r = self.run(*op);
                self.mode = was;
                self.done = false;
                r
            }
        }
    }

    /// Up one level: out of a list entry (its name and the list), else one word.
    fn up(&mut self) {
        self.edit.pop();
    }

    /// Out of configuration mode, saying if the candidate holds changes.
    fn leave(&mut self) -> Result<String> {
        // Leave first: a candidate that cannot be read must not keep anyone in here.
        self.mode = Mode::Operational;
        self.edit.clear();
        let changed = self.uncommitted().unwrap_or(true);
        Ok(match (changed, self.saved.is_some()) {
            (false, _) => String::new(),
            (true, true) => "The configuration has been changed but not committed (it is kept: `configure` returns to it).\n".into(),
            (true, false) => "The configuration has been changed but not committed.\n".into(),
        })
    }

    /// `| display set`, of the whole configuration or the part below `path`.
    fn display_set(&self, cfg: &Config, path: &[String]) -> Result<String> {
        let all = self.schema.display_set(cfg);
        if path.is_empty() {
            return Ok(all);
        }
        let t = self.schema.resolve_in(path, Some(cfg))?;
        let prefix = format!("set {}", t.words.iter().map(|w| syntax::quote(w)).collect::<Vec<_>>().join(" "));
        Ok(all.lines().filter(|l| *l == prefix || l.starts_with(&format!("{prefix} "))).map(|l| format!("{l}\n")).collect())
    }

    fn configure(&mut self) -> Result<String> {
        self.mode = Mode::Configuration;
        let head = self.repo.head()?;
        let c = self.candidate()?;
        let mut out = String::from("Entering configuration mode\n");
        if c.base != head {
            out.push_str(&format!(
                "The configuration has been changed since your candidate was started ({} → {}): \
                 `show | compare` is against the old one, and `commit` will be refused; \
                 `rollback 0` starts over from the current configuration.\n",
                short(&c.base),
                short(&head)
            ));
        }
        Ok(out)
    }

    fn show_operational(&mut self, op: Op) -> Result<String> {
        match op {
            Op::ShowCommits => {
                let mut out = String::new();
                for (n, c) in self.repo.history(50)?.iter().enumerate() {
                    out.push_str(&format!("{n:<3} {}  {}\n", c.date, c.subject));
                }
                Ok(out)
            }
            Op::ShowRollback { n, compare } => {
                let n_cfg = self.configuration_at(n)?;
                Ok(match compare {
                    None => self.schema.show(&n_cfg, &[])?,
                    // What going from configuration m to configuration n changes.
                    Some(m) => self.schema.compare(&self.configuration_at(m)?, &n_cfg),
                })
            }
            Op::ShowAlarms => self.machine.alarms(),
            Op::ShowServices => {
                let active = self.active()?;
                let on: Vec<String> = match active.files.get("services.json") {
                    Some(crate::tree::Data::List(l)) => l.clone(),
                    _ => vec![],
                };
                let catalog = self.schema.nodes.get("services").and_then(|n| n.values.clone()).unwrap_or_default();
                let width = catalog.keys().map(|k| k.len()).max().unwrap_or(0);
                Ok(catalog
                    .iter()
                    .map(|(name, what)| format!("{} {name:<width$}  {what}\n", if on.contains(name) { "on " } else { "off" }))
                    .collect())
            }
            Op::ShowPackages { terms } => self.machine.search(&terms),
            other => bail!("not an operational show: {other:?}"),
        }
    }

    /// The configuration n changes back (`rollback <n>`'s numbering: 0 is the current one).
    fn configuration_at(&self, n: usize) -> Result<Config> {
        let history = self.repo.history(n + 1)?;
        let c = history.get(n).with_context(|| format!("there are only {} configurations", history.len()))?;
        let files = self.repo.tree_files(&self.schema, &c.id)?;
        self.schema.load(files.iter().map(|(p, b)| (p.as_str(), b.as_slice())))
    }

    fn request(&mut self, w: &[String]) -> Result<String> {
        match w.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
            ["system", "repair"] => self.machine.repair(),
            ["system", "update", ..] | ["system", "software", "rollback", ..] => bail!(
                "not in torqcli yet: the software (flake.lock) is not part of the candidate — use `torq update` for now"
            ),
            _ => bail!("request: one of: system repair"),
        }
    }

    fn show_candidate(&mut self, path: &[String], pipes: &Pipes) -> Result<String> {
        let cfg = self.candidate()?.config.clone();
        Ok(if pipes.display_set { self.display_set(&cfg, path)? } else { self.schema.show(&cfg, path)? })
    }

    /// What the candidate changes from its base.
    fn compare(&mut self) -> Result<String> {
        let s = self.schema.clone();
        let base = self.candidate()?.base.clone();
        let base_files = self.repo.tree_files(&s, &base)?;
        let before = s.load(base_files.iter().map(|(p, b)| (p.as_str(), b.as_slice())))?;
        Ok(s.compare(&before, &self.candidate()?.config))
    }

    fn rollback(&mut self, n: usize) -> Result<String> {
        let history = self.repo.history(n + 1)?;
        let commit = history.get(n).with_context(|| format!("rollback {n}: there are only {} configurations", history.len()))?;
        self.candidate = Some(Candidate::from_repo(&self.repo, &self.schema, &commit.id)?);
        self.save()?;
        Ok("load complete\n".into())
    }

    /// Whether the candidate differs from its base.
    fn uncommitted(&mut self) -> Result<bool> {
        if self.candidate.is_none() && self.saved.as_ref().is_none_or(|p| !p.exists()) {
            return Ok(false);
        }
        let s = self.schema.clone();
        let base = self.candidate()?.base.clone();
        let base_files = self.repo.tree_files(&s, &base)?;
        Ok(!candidate::changes(&s, &base_files, &self.candidate()?.config)?.is_empty())
    }

    fn commit(&mut self, o: CommitOptions) -> Result<String> {
        let s = self.schema.clone();
        let head = self.repo.head()?;
        let (base, config) = {
            let c = self.candidate()?;
            (c.base.clone(), c.config.clone())
        };
        let base_files = self.repo.tree_files(&s, &base)?;
        let changes = candidate::changes(&s, &base_files, &config)?;
        let files: Vec<(String, Option<String>)> = changes.iter().map(|(p, _, n)| (p.clone(), n.clone())).collect();
        if o.check {
            if base != head {
                bail!("the configuration has changed since your candidate was started: `rollback 0` starts over");
            }
            self.machine.check(&base, &files)?;
            return Ok("configuration check succeeds\n".into());
        }
        // A commit confirmed waiting freezes every other change (the broker refuses them): say so
        // before evaluating, asking or proposing anything.
        let waiting = self.machine.pending_confirm()?;
        if changes.is_empty() {
            let Some(w) = waiting else { return Ok("commit complete (no changes)\n".into()) };
            if o.confirm.is_some() {
                bail!("change #{} is already waiting for confirmation until {}: `commit` confirms it", w.seq, w.deadline);
            }
            // Junos: committing again within the window confirms it — but only its author may.
            if w.by.as_deref().is_some_and(|by| by != self.me) {
                bail!("change #{} is waiting for {} to confirm it; it rolls back at {} unless they do", w.seq, w.by.as_deref().unwrap_or("?"), w.deadline);
            }
            let n = self.machine.confirm()?;
            return Ok(format!("commit complete: change #{} confirmed (#{n})\n", w.seq));
        }
        if let Some(w) = waiting {
            bail!(
                "change #{} (commit confirmed) is waiting until {}: nothing else can change until it is confirmed \
                 (`commit` with no changes) or rolled back",
                w.seq, w.deadline
            );
        }
        if base != head {
            bail!(
                "the configuration has changed since your candidate was started ({} → {}): \
                 `rollback 0` starts over from the current configuration",
                short(&base),
                short(&head)
            );
        }
        let tier = self.tier_of(&changes);
        // A person approves at a terminal: a system change, and a change to someone else's folder.
        let others = self.others_folders(&changes);
        self.machine.check(&base, &files)?;
        let base_config = s.load(base_files.iter().map(|(p, b)| (p.as_str(), b.as_slice())))?;
        let compare = s.compare(&base_config, &config);
        let shown: String = compare.lines().map(|l| format!("{}\n", crate::history::clean(l))).collect();
        let why = match (tier, others.is_empty()) {
            ("system", _) => Some("This changes the system (all users).".to_string()),
            (_, false) => Some(format!("This changes the settings of: {}.", others.join(", "))),
            _ => None,
        };
        if let Some(why) = why {
            if !self.machine.ask(&format!("{shown}{why} Commit? ")) {
                bail!("not committed: this change needs a person to approve it at a terminal");
            }
        }
        let (confirm, comment) = (o.confirm, o.comment);
        let scratch = tempfile::tempdir()?;
        let diff = candidate::unified_diff(scratch.path(), &changes)?;
        let reason = comment.unwrap_or_else(|| summary(&compare));
        let seq = self.machine.commit(&diff, &base, tier, &reason, confirm)?;
        self.discard()?;
        Ok(match confirm {
            Some(m) => format!("commit confirmed will be automatically rolled back in {m} minutes unless confirmed (change #{seq})\ncommit complete\n"),
            None => format!("commit complete (change #{seq})\n"),
        })
    }

    /// The tier a set of changed files is: user if every one is a user-tier node's file.
    fn tier_of(&self, changes: &[(String, Option<&[u8]>, Option<String>)]) -> &'static str {
        let user = changes.iter().all(|(p, _, _)| self.schema.tier_of(p) == Some("user"));
        if user { "user" } else { "system" }
    }

    /// The users other than you whose folders (users/<name>/) the changes touch.
    fn others_folders(&self, changes: &[(String, Option<&[u8]>, Option<String>)]) -> Vec<String> {
        let mut names: Vec<String> = changes
            .iter()
            .filter_map(|(p, _, _)| p.strip_prefix("users/")?.split('/').next().map(String::from))
            .filter(|n| *n != self.me)
            .collect();
        names.sort();
        names.dedup();
        names
    }
}

/// A ledger reason from the comparison: its `+`/`-` lines, short.
fn summary(compare: &str) -> String {
    let lines: Vec<&str> = compare.lines().filter(|l| !l.starts_with('[')).map(|l| l.trim()).collect();
    let s = lines.join(" ");
    if s.chars().count() > 120 { format!("{}…", s.chars().take(119).collect::<String>()) } else { s }
}

fn short(id: &str) -> &str {
    &id[..id.len().min(12)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::path::Path;
    use std::process::Command;

    #[derive(Default)]
    struct Fake {
        commits: RefCell<Vec<(String, String, String, String, Option<u32>)>>,
        checks: RefCell<Vec<Vec<(String, Option<String>)>>>,
        answer: bool,
        pending: Option<Waiting>,
        confirmed: RefCell<u32>,
        asked: RefCell<Vec<String>>,
        /// The broker refuses the commit.
        refuse: bool,
    }
    impl Machine for Fake {
        fn check(&self, _base: &str, files: &[(String, Option<String>)]) -> Result<()> {
            self.checks.borrow_mut().push(files.to_vec());
            Ok(())
        }
        fn commit(&self, diff: &str, base: &str, tier: &str, reason: &str, confirm: Option<u32>) -> Result<u64> {
            if self.refuse {
                bail!("commit refused: busy");
            }
            self.commits.borrow_mut().push((diff.into(), base.into(), tier.into(), reason.into(), confirm));
            Ok(42)
        }
        fn pending_confirm(&self) -> Result<Option<Waiting>> {
            Ok(self.pending.clone())
        }
        fn confirm(&self) -> Result<u64> {
            *self.confirmed.borrow_mut() += 1;
            Ok(43)
        }
        fn ask(&self, q: &str) -> bool {
            self.asked.borrow_mut().push(q.into());
            self.answer
        }
        fn alarms(&self) -> Result<String> {
            Ok("all well\n".into())
        }
        fn search(&self, terms: &[String]) -> Result<String> {
            Ok(format!("searched {}\n", terms.join(" ")))
        }
        fn repair(&self) -> Result<String> {
            Ok("repaired\n".into())
        }
    }

    fn sh(dir: &Path, script: &str) {
        let st = Command::new("sh").arg("-c").arg(script).current_dir(dir).env("GIT_CONFIG_GLOBAL", "/dev/null").status().unwrap();
        assert!(st.success(), "{script}");
    }
    fn repo() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        sh(d.path(), "git init -q -b main && git config user.name t && git config user.email t@t \
            && mkdir -p system && printf '{ \"host-name\": \"one\" }\\n' > system/host.json && git add -A && git commit -q -m first \
            && printf '{ \"host-name\": \"two\" }\\n' > system/host.json && git commit -qam second");
        d
    }
    fn session<'a>(d: &Path, saved: Option<PathBuf>, m: &'a Fake) -> Session<'a> {
        Session::new(Schema::parse(include_str!("schema-fixture.json")).unwrap(), Repo::new(d), saved, m, "dev", Style::Juniper)
    }
    fn waiting(by: &str) -> Option<Waiting> {
        Some(Waiting { seq: 42, deadline: "soon".into(), by: Some(by.into()) })
    }
    fn run(s: &mut Session, lines: &[&str]) -> String {
        lines.iter().map(|l| s.exec(l).unwrap_or_else(|e| panic!("{l}: {e:#}"))).collect()
    }

    #[test]
    fn words_split_like_a_shell_with_pipes() {
        assert_eq!(words("show | compare").unwrap(), ["show", "|", "compare"]);
        assert_eq!(words("show|display set").unwrap(), ["show", "|", "display", "set"]);
        assert_eq!(words(r#"commit comment "two words" "#).unwrap(), ["commit", "comment", "two words"]);
        assert_eq!(words(r#"set a "say \"hi\"""#).unwrap(), ["set", "a", "say \"hi\""]);
        assert_eq!(words(r#"set a """#).unwrap(), ["set", "a", ""]);
        assert!(words(r#"set "open"#).is_err());
    }

    #[test]
    fn a_user_change_commits_without_asking() {
        let (d, m) = (repo(), Fake::default());
        let mut s = session(d.path(), None, &m);
        let out = run(&mut s, &["configure", "set users dev cli style cisco", "show | compare"]);
        assert!(out.contains("[edit users dev cli]\n+   style cisco;\n"), "{out}");
        let out = s.exec("commit").unwrap();
        assert_eq!(out, "commit complete (change #42)\n");
        let c = m.commits.borrow();
        let (diff, base, tier, reason, confirm) = &c[0];
        assert!(diff.contains("+++ b/users/dev/cli.json") && diff.contains("+  \"style\": \"cisco\""), "{diff}");
        assert_eq!((base.as_str(), tier.as_str(), reason.as_str(), *confirm), (s.repo.head().unwrap().as_str(), "user", "+   style cisco;", None));
        assert!(m.asked.borrow().is_empty(), "user tier: no question");
        assert_eq!(m.checks.borrow().len(), 1, "checked before committing");
        assert_eq!(s.exec("show | compare").unwrap(), "", "the candidate starts over after a commit");
    }

    #[test]
    fn a_system_change_needs_a_person() {
        let (d, m) = (repo(), Fake::default());
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "set system host-name three"]);
        let e = format!("{:#}", s.exec("commit").unwrap_err());
        assert!(e.contains("needs a person to approve it"), "{e}");
        assert!(m.commits.borrow().is_empty());
        assert!(m.asked.borrow()[0].contains("-   host-name two;\n+   host-name three;"), "the question shows the change");
        // The candidate is kept for another try.
        assert!(s.exec("show | compare").unwrap().contains("host-name three"));
        let m = Fake { answer: true, ..Fake::default() };
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "set system host-name three", "commit comment \"rename\""]);
        assert_eq!((m.commits.borrow()[0].2.as_str(), m.commits.borrow()[0].3.as_str()), ("system", "rename"));
    }

    #[test]
    fn a_stale_candidate_is_refused_until_rollback_0() {
        let (d, m) = (repo(), Fake { answer: true, ..Fake::default() });
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "set system host-name three"]);
        sh(d.path(), "printf '{ \"host-name\": \"elsewhere\" }\\n' > system/host.json && git commit -qam third");
        let e = format!("{:#}", s.exec("commit").unwrap_err());
        assert!(e.contains("has changed since your candidate") && e.contains("rollback 0"), "{e}");
        assert!(format!("{:#}", s.exec("commit check").unwrap_err()).contains("rollback 0"));
        assert!(m.commits.borrow().is_empty());
        run(&mut s, &["rollback 0", "set system host-name three", "commit"]);
        assert_eq!(m.commits.borrow().len(), 1);
    }

    #[test]
    fn rollback_loads_an_earlier_configuration() {
        let (d, m) = (repo(), Fake::default());
        let mut s = session(d.path(), None, &m);
        assert_eq!(run(&mut s, &["configure", "rollback 1"]), "Entering configuration mode\nload complete\n");
        assert_eq!(s.exec("show | compare").unwrap(), "[edit system]\n-   host-name two;\n+   host-name one;\n");
        assert!(format!("{:#}", s.exec("rollback 9").unwrap_err()).contains("only 2 configurations"));
        assert!(format!("{:#}", s.exec("rollback x").unwrap_err()).contains("not a number"));
        for huge in ["rollback 18446744073709551615", "run show system rollback 18446744073709551615"] {
            assert!(format!("{:#}", s.exec(huge).unwrap_err()).contains("go up to 10000"), "{huge}");
        }
    }

    #[test]
    fn commit_check_and_commit_confirmed() {
        let (d, m) = (repo(), Fake { answer: true, ..Fake::default() });
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "set system time-zone UTC"]);
        assert_eq!(s.exec("commit check").unwrap(), "configuration check succeeds\n");
        assert_eq!(m.checks.borrow()[0], [("system/host.json".to_string(), Some("{\n  \"host-name\": \"two\",\n  \"time-zone\": \"UTC\"\n}\n".to_string()))]);
        assert!(m.commits.borrow().is_empty(), "check commits nothing");
        let out = s.exec("commit confirmed 5").unwrap();
        assert!(out.contains("rolled back in 5 minutes"), "{out}");
        assert_eq!(m.commits.borrow()[0].4, Some(5));
        // Committing again, with nothing changed, confirms it (as in Junos).
        let m = Fake { pending: waiting("dev"), ..Fake::default() };
        let mut s = session(d.path(), None, &m);
        assert_eq!(run(&mut s, &["configure", "commit"]), "Entering configuration mode\ncommit complete: change #42 confirmed (#43)\n");
        assert_eq!(*m.confirmed.borrow(), 1);
        let m = Fake::default();
        let mut s = session(d.path(), None, &m);
        assert_eq!(run(&mut s, &["configure", "commit"]), "Entering configuration mode\ncommit complete (no changes)\n");
    }

    #[test]
    fn a_saved_candidate_outlives_the_session_a_private_one_does_not() {
        let (d, m) = (repo(), Fake::default());
        let state = tempfile::tempdir().unwrap();
        let saved = state.path().join("candidate.json");
        let mut s = session(d.path(), Some(saved.clone()), &m);
        let out = run(&mut s, &["configure", "set users dev packages htop", "exit"]);
        assert!(out.contains("changed but not committed"), "{out}");
        assert_eq!(s.mode, Mode::Operational);
        let mut s = session(d.path(), Some(saved.clone()), &m);
        assert!(run(&mut s, &["configure", "show | compare"]).contains("+   packages [ htop ];"));
        // `torqcli -c`: a private candidate; nothing is saved.
        let mut p = session(d.path(), None, &m);
        run(&mut p, &["configure", "set users dev cli style cisco"]);
        assert!(!fs_has(state.path(), "cli.json"));
    }

    /// While a commit confirmed waits, a commit with changes is refused before anything is evaluated,
    /// asked or proposed; only its author confirms it (review of 4a12b45).
    #[test]
    fn a_waiting_commit_confirmed_comes_first() {
        let (d, m) = (repo(), Fake { answer: true, pending: waiting("dev"), ..Fake::default() });
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "set system host-name three"]);
        let e = format!("{:#}", s.exec("commit").unwrap_err());
        assert!(e.contains("change #42 (commit confirmed) is waiting"), "{e}");
        assert!(m.checks.borrow().is_empty() && m.asked.borrow().is_empty() && m.commits.borrow().is_empty());
        let m = Fake { pending: waiting("ana"), ..Fake::default() };
        let mut s = session(d.path(), None, &m);
        let e = format!("{:#}", run_err(&mut s, &["configure"], "commit"));
        assert!(e.contains("waiting for ana to confirm it"), "{e}");
        assert_eq!(*m.confirmed.borrow(), 0);
        let m = Fake { pending: waiting("dev"), ..Fake::default() };
        let mut s = session(d.path(), None, &m);
        let e = format!("{:#}", run_err(&mut s, &["configure"], "commit confirmed 5"));
        assert!(e.contains("already waiting for confirmation") && e.contains("`commit` confirms it"), "{e}");
    }

    fn run_err(s: &mut Session, before: &[&str], line: &str) -> anyhow::Error {
        run(s, before);
        s.exec(line).unwrap_err()
    }

    #[test]
    fn commit_options_parse_and_are_bounded() {
        let (d, m) = (repo(), Fake::default());
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "set users dev cli style cisco"]);
        for (bad, why) in [("commit confirmed 0", "1 to 1440"), ("commit confirmed 1441", "1 to 1440"), ("commit confirmed 99999999999", "too many"), ("commit now", "one of")] {
            assert!(format!("{:#}", s.exec(bad).unwrap_err()).contains(why), "{bad}");
        }
        assert!(m.checks.borrow().is_empty(), "refused before anything is checked");
        s.exec("commit confirmed 5 comment \"ssh, keys only\"").unwrap();
        let c = m.commits.borrow();
        assert_eq!((c[0].3.as_str(), c[0].4), ("ssh, keys only", Some(5)));
    }

    /// The ledger reason is built from the comparison and starts with `-` for any changed value: it is
    /// passed through as it is (shell.rs gives it to the broker as `--reason=…`).
    #[test]
    fn a_reason_may_start_with_a_dash() {
        let (d, m) = (repo(), Fake { answer: true, ..Fake::default() });
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "set system host-name three", "commit"]);
        assert_eq!(m.commits.borrow()[0].3, "-   host-name two; +   host-name three;");
    }

    /// Your own folder is yours; anyone else's takes a person's yes, like a system change.
    #[test]
    fn another_users_folder_needs_a_person() {
        let (d, m) = (repo(), Fake::default());
        let mut s = session(d.path(), None, &m);
        s.schema.nodes.get_mut("users").unwrap().values.as_mut().unwrap().insert("ana".into(), String::new());
        run(&mut s, &["configure", "set users ana cli style cisco"]);
        let e = format!("{:#}", s.exec("commit").unwrap_err());
        assert!(e.contains("needs a person"), "{e}");
        assert!(m.asked.borrow()[0].contains("This changes the settings of: ana."), "{:?}", m.asked.borrow());
        assert!(m.commits.borrow().is_empty());
    }

    /// A refused commit keeps the candidate; `-c`'s exit does not promise to keep it.
    #[test]
    fn a_failed_commit_keeps_the_candidate() {
        let (d, m) = (repo(), Fake { refuse: true, ..Fake::default() });
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "set users dev cli style cisco"]);
        assert!(format!("{:#}", s.exec("commit").unwrap_err()).contains("busy"));
        assert!(s.exec("show | compare").unwrap().contains("style cisco"));
        assert_eq!(s.exec("exit").unwrap(), "The configuration has been changed but not committed.\n");
    }

    fn fs_has(dir: &Path, needle: &str) -> bool {
        std::fs::read_dir(dir).unwrap().flatten().any(|e| std::fs::read_to_string(e.path()).unwrap_or_default().contains(needle))
    }

    /// `edit` / `up` / `top` move through the tree; commands are relative to where you are.
    #[test]
    fn moving_through_the_tree() {
        let (d, m) = (repo(), Fake::default());
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "edit users dev", "set cli style cisco", "edit cli"]);
        assert_eq!(s.edit_path(), ["users", "dev", "cli"]);
        assert_eq!(s.exec("show").unwrap(), "style cisco;\n");
        run(&mut s, &["up"]);
        assert_eq!(s.edit_path(), ["users", "dev"]);
        assert!(format!("{:#}", s.exec("edit cli style").unwrap_err()).contains("not somewhere to go"));
        run(&mut s, &["top", "set system time-zone UTC"]);
        assert!(s.edit_path().is_empty());
        assert!(format!("{:#}", s.exec("up").unwrap_err()).contains("already at the top"));
        // `exit` goes up a level, and out of configuration mode at the top.
        run(&mut s, &["edit system", "exit"]);
        assert_eq!((s.mode, s.edit_path().is_empty()), (Mode::Configuration, true));
        run(&mut s, &["edit system", "exit configuration-mode"]);
        assert_eq!(s.mode, Mode::Operational);
    }

    /// Deleting a whole container or list entry takes `all`, so a slip cannot take everything below.
    #[test]
    fn delete_of_a_container_needs_all() {
        let (d, m) = (repo(), Fake::default());
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "set users dev cli style cisco"]);
        for (bad, why) in [("delete users dev", "say `delete users dev all`"), ("delete system", "say `delete system all`"), ("delete system host-name all", "`all` is for a whole container")] {
            assert!(format!("{:#}", s.exec(bad).unwrap_err()).contains(why), "{bad}");
        }
        run(&mut s, &["delete users dev all"]);
        assert!(!s.exec("show | compare").unwrap().contains("style"));
    }

    /// The same session in Cisco style: bare paths set, a container path is a submode, `no`
    /// deletes, `show configuration diff` compares, `end` leaves.
    #[test]
    fn a_session_in_cisco_style() {
        let (d, m) = (repo(), Fake { answer: true, ..Fake::default() });
        let mut s = Session::new(Schema::parse(include_str!("schema-fixture.json")).unwrap(), Repo::new(d.path()), None, &m, "dev", Style::Cisco);
        run(&mut s, &["config", "system host-name three", "users dev", "cli style juniper", "exit", "exit"]);
        assert!(s.edit_path().is_empty());
        assert_eq!(
            s.exec("show configuration diff").unwrap(),
            "[edit system]\n-   host-name two;\n+   host-name three;\n[edit users dev cli]\n+   style juniper;\n"
        );
        run(&mut s, &["no users dev cli style", "do show system alarms"]);
        assert!(format!("{:#}", s.exec("set system host-name x").unwrap_err()).contains("in Cisco style"));
        assert_eq!(s.exec("commit").unwrap(), "commit complete (change #42)\n");
        assert_eq!(s.exec("end").unwrap(), "");
        assert_eq!(s.mode, Mode::Operational);
        assert_eq!(s.exec("show running-config").unwrap(), "system {\n    host-name two;\n}\n", "the fake commits nothing");
    }

    /// `switch cli` changes the syntax for the session, wherever you are.
    #[test]
    fn switch_cli_changes_the_syntax() {
        let (d, m) = (repo(), Fake::default());
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "edit system"]);
        assert_eq!(s.exec("switch cli").unwrap(), "Switched to the cisco style (for this session)\n");
        run(&mut s, &["time-zone UTC", "no time-zone"]);
        run(&mut s, &["switch cli", "set time-zone UTC"]);
        assert!(s.exec("show | compare").unwrap().contains("+   time-zone UTC;"));
    }

    #[test]
    fn earlier_configurations_and_their_differences() {
        let (d, m) = (repo(), Fake::default());
        let mut s = session(d.path(), None, &m);
        assert_eq!(s.exec("show system rollback 1").unwrap(), "system {\n    host-name one;\n}\n");
        assert_eq!(s.exec("show system rollback 0 compare 1").unwrap(), "[edit system]\n-   host-name one;\n+   host-name two;\n");
        assert!(format!("{:#}", s.exec("show system rollback 5").unwrap_err()).contains("only 2 configurations"));
        assert!(format!("{:#}", s.exec("show system rollback x").unwrap_err()).contains("not a number"));
    }

    #[test]
    fn services_alarms_packages_and_requests() {
        let (d, m) = (repo(), Fake::default());
        sh(d.path(), "echo '[\"ssh\"]' > services.json && git add -A && git commit -qm ssh");
        let mut s = session(d.path(), None, &m);
        let out = s.exec("show services").unwrap();
        assert!(out.lines().any(|l| l.starts_with("on  ssh ")) && out.lines().any(|l| l.starts_with("off podman")), "{out}");
        assert_eq!(s.exec("show system alarms").unwrap(), "all well\n");
        assert_eq!(s.exec("show packages available ripgrep").unwrap(), "searched ripgrep\n");
        assert_eq!(s.exec("request system repair").unwrap(), "repaired\n");
        assert!(format!("{:#}", s.exec("request system update").unwrap_err()).contains("use `torq update`"));
        assert_eq!(s.exec("config").unwrap().lines().next(), Some("Entering configuration mode"));
    }

    #[test]
    fn operational_mode_shows_the_active_configuration() {
        let (d, m) = (repo(), Fake::default());
        let mut s = session(d.path(), None, &m);
        assert_eq!(s.exec("show configuration").unwrap(), "system {\n    host-name two;\n}\n");
        assert_eq!(s.exec("show configuration | display set").unwrap(), "set system host-name two\n");
        assert!(s.exec("show system commit").unwrap().starts_with("0   "));
        assert!(format!("{:#}", s.exec("set system host-name x").unwrap_err()).contains("unknown command: set"));
        run(&mut s, &["configure", "set system host-name three"]);
        assert_eq!(s.exec("run show configuration").unwrap(), "system {\n    host-name two;\n}\n", "run: the active one");
        assert_eq!(s.mode, Mode::Configuration);
        s.exec("exit").unwrap();
        s.exec("exit").unwrap();
        assert!(s.done);
    }
}
