//! A torqcli session (ADR-016, docs/CLI-DESIGN.md): operational and configuration mode, and the
//! commands of each in Juniper syntax. Everything that reaches the broker or evaluates Nix goes
//! through `Machine`, so the whole command layer is testable without either.

use crate::candidate::{self, Candidate, Repo};
use crate::tree::{Config, Schema};
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

/// The pipes after a `show`: `| compare`, `| display set`.
#[derive(Debug, Default, PartialEq)]
struct Pipes {
    compare: bool,
    display_set: bool,
}

fn split_pipes(w: &[String]) -> Result<(Vec<String>, Pipes)> {
    let mut parts = w.split(|x| x == "|");
    let head = parts.next().unwrap_or_default().to_vec();
    let mut p = Pipes::default();
    for part in parts {
        match part.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
            ["compare"] => p.compare = true,
            ["display", "set"] => p.display_set = true,
            other => bail!("unknown pipe: | {}; one of: compare, display set", other.join(" ")),
        }
    }
    Ok((head, p))
}

impl<'a> Session<'a> {
    pub fn new(schema: Schema, repo: Repo, saved: Option<PathBuf>, machine: &'a dyn Machine, me: &str) -> Self {
        Session { schema, repo, saved, mode: Mode::Operational, candidate: None, machine, me: me.into(), done: false }
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
        let Some(first) = w.first().map(String::as_str) else { return Ok(String::new()) };
        let rest = &w[1..];
        match (self.mode, first) {
            (Mode::Operational, "configure") => self.configure(),
            (Mode::Operational, "show") => self.show_operational(rest),
            (Mode::Operational, "exit" | "quit") => {
                self.done = true;
                Ok(String::new())
            }
            (Mode::Configuration, "set") => {
                let s = self.schema.clone();
                s.set(&mut self.candidate()?.config, rest)?;
                self.save()?;
                Ok(String::new())
            }
            (Mode::Configuration, "delete") => {
                let s = self.schema.clone();
                s.delete(&mut self.candidate()?.config, rest)?;
                self.save()?;
                Ok(String::new())
            }
            (Mode::Configuration, "show") => self.show_candidate(rest),
            (Mode::Configuration, "rollback") => self.rollback(rest),
            (Mode::Configuration, "commit") => self.commit(rest),
            (Mode::Configuration, "run") => {
                let was = self.mode;
                self.mode = Mode::Operational;
                let r = self.exec(&rest.iter().map(|x| quote_word(x)).collect::<Vec<_>>().join(" "));
                self.mode = was;
                self.done = false;
                r
            }
            (Mode::Configuration, "exit" | "quit" | "top") => {
                // Leave first: a candidate that cannot be read must not keep anyone in here.
                self.mode = Mode::Operational;
                let changed = self.uncommitted().unwrap_or(true);
                Ok(match (changed, self.saved.is_some()) {
                    (false, _) => String::new(),
                    (true, true) => "The configuration has been changed but not committed (it is kept: `configure` returns to it).\n".into(),
                    (true, false) => "The configuration has been changed but not committed.\n".into(),
                })
            }
            (Mode::Operational, other) => bail!("unknown command: {other}; one of: configure, show, exit"),
            (Mode::Configuration, other) => {
                bail!("unknown command: {other}; one of: set, delete, show, commit, rollback, run, exit")
            }
        }
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

    fn show_operational(&mut self, w: &[String]) -> Result<String> {
        match w.first().map(String::as_str) {
            Some("configuration") => {
                let (path, pipes) = split_pipes(&w[1..])?;
                if pipes.compare {
                    bail!("| compare is for the candidate, in configuration mode");
                }
                let active = self.active()?;
                Ok(if pipes.display_set { self.schema.display_set(&active) } else { self.schema.show(&active, &path)? })
            }
            Some("system") if w.get(1).map(String::as_str) == Some("commit") => {
                let mut out = String::new();
                for (n, c) in self.repo.history(50)?.iter().enumerate() {
                    out.push_str(&format!("{n:<3} {}  {}\n", c.date, c.subject));
                }
                Ok(out)
            }
            _ => bail!("show: one of: configuration, system commit"),
        }
    }

    fn show_candidate(&mut self, w: &[String]) -> Result<String> {
        let (path, pipes) = split_pipes(w)?;
        let s = self.schema.clone();
        let base_files = {
            let base = self.candidate()?.base.clone();
            self.repo.tree_files(&s, &base)?
        };
        let c = self.candidate()?;
        if pipes.compare {
            let base = s.load(base_files.iter().map(|(p, b)| (p.as_str(), b.as_slice())))?;
            return Ok(s.compare(&base, &c.config));
        }
        Ok(if pipes.display_set { s.display_set(&c.config) } else { s.show(&c.config, &path)? })
    }

    fn rollback(&mut self, w: &[String]) -> Result<String> {
        let n: usize = match w {
            [] => 0,
            [n] => n.parse().with_context(|| format!("rollback: '{n}' is not a number"))?,
            _ => bail!("rollback: takes one number"),
        };
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

    fn commit(&mut self, w: &[String]) -> Result<String> {
        let o = parse_commit(w)?;
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

/// `commit` options: `check` alone, or any of `confirmed [<minutes>]` and `comment "<text>"`.
struct CommitOptions {
    check: bool,
    confirm: Option<u32>,
    comment: Option<String>,
}

fn parse_commit(w: &[String]) -> Result<CommitOptions> {
    let usage = || anyhow::anyhow!("commit: one of: commit, commit check, commit [confirmed [<minutes>]] [comment \"…\"]");
    if w.len() == 1 && w[0] == "check" {
        return Ok(CommitOptions { check: true, confirm: None, comment: None });
    }
    let (mut confirm, mut comment, mut i) = (None, None, 0);
    while i < w.len() {
        match w[i].as_str() {
            "confirmed" if confirm.is_none() => {
                let mut m = 10;
                if let Some(n) = w.get(i + 1).filter(|n| n.bytes().all(|b| b.is_ascii_digit())) {
                    m = n.parse().map_err(|_| anyhow::anyhow!("commit confirmed: '{n}' minutes is too many"))?;
                    i += 1;
                }
                if !(1..=1440).contains(&m) {
                    bail!("commit confirmed takes 1 to 1440 minutes, not {m}");
                }
                confirm = Some(m);
            }
            "comment" if comment.is_none() => {
                comment = Some(w.get(i + 1).ok_or_else(usage)?.clone());
                i += 1;
            }
            _ => return Err(usage()),
        }
        i += 1;
    }
    Ok(CommitOptions { check: false, confirm, comment })
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

fn quote_word(w: &str) -> String {
    if w.contains([' ', '"', '|']) { format!("\"{}\"", w.replace('\\', "\\\\").replace('"', "\\\"")) } else { w.to_string() }
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
        Session::new(Schema::parse(include_str!("schema-fixture.json")).unwrap(), Repo::new(d), saved, m, "dev")
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
