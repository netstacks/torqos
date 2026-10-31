//! A torqcli session (ADR-016, docs/CLI-DESIGN.md): operational and configuration mode, and the
//! commands of each in Juniper syntax. Everything that reaches the broker or evaluates Nix goes
//! through `Machine`, so the whole command layer is testable without either.

use crate::candidate::{self, Candidate, Repo};
use crate::tree::{Config, Schema};
use anyhow::{bail, Context, Result};
use std::path::PathBuf;

/// What the session needs from the machine: evaluation and the broker.
pub trait Machine {
    /// `commit check`: does the configuration with these tree files evaluate?
    fn check(&self, files: &[(String, Option<String>)]) -> Result<()>;
    /// Propose `diff` against `base` and apply it (with `confirm` minutes: commit confirmed). The
    /// broker's tier must be `tier`. Returns the applied ledger entry's number.
    fn commit(&self, diff: &str, base: &str, tier: &str, reason: &str, confirm: Option<u32>) -> Result<u64>;
    /// A commit confirmed waiting: (its change number, its deadline).
    fn pending_confirm(&self) -> Result<Option<(u64, String)>>;
    /// Keep the waiting commit confirmed.
    fn confirm(&self) -> Result<u64>;
    /// Ask a person (a system-tier commit); false when there is no one to ask.
    fn ask(&self, question: &str) -> bool;
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
    pub fn new(schema: Schema, repo: Repo, saved: Option<PathBuf>, machine: &'a dyn Machine) -> Self {
        Session { schema, repo, saved, mode: Mode::Operational, candidate: None, machine, done: false }
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
                let changed = self.uncommitted()?;
                self.mode = Mode::Operational;
                Ok(if changed {
                    "The configuration has been changed but not committed (it is kept: `configure` returns to it).\n".into()
                } else {
                    String::new()
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
        let ws: Vec<&str> = w.iter().map(String::as_str).collect();
        let (check, confirm, comment) = match ws.as_slice() {
            ["check"] => (true, None, None),
            [] => (false, None, None),
            ["comment", c] => (false, None, Some(c.to_string())),
            ["confirmed"] => (false, Some(10), None),
            ["confirmed", m] => (false, Some(m.parse().with_context(|| format!("commit confirmed: '{m}' is not a number of minutes"))?), None),
            _ => bail!("commit: one of: commit, commit check, commit comment \"…\", commit confirmed [<minutes>]"),
        };
        let s = self.schema.clone();
        let head = self.repo.head()?;
        let (base, config) = {
            let c = self.candidate()?;
            (c.base.clone(), c.config.clone())
        };
        let base_files = self.repo.tree_files(&s, &base)?;
        let changes = candidate::changes(&s, &base_files, &config)?;
        if check {
            if base != head {
                bail!("the configuration has changed since your candidate was started: `rollback 0` starts over");
            }
            let files: Vec<(String, Option<String>)> = changes.iter().map(|(p, _, n)| (p.clone(), n.clone())).collect();
            self.machine.check(&files)?;
            return Ok("configuration check succeeds\n".into());
        }
        if changes.is_empty() {
            // Junos: committing again within the window confirms a commit confirmed.
            if let Some((seq, _)) = self.machine.pending_confirm()? {
                if confirm.is_none() {
                    let n = self.machine.confirm()?;
                    return Ok(format!("commit complete: change #{seq} confirmed (#{n})\n"));
                }
            }
            return Ok("commit complete (no changes)\n".into());
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
        let files: Vec<(String, Option<String>)> = changes.iter().map(|(p, _, n)| (p.clone(), n.clone())).collect();
        self.machine.check(&files)?;
        let base_config = s.load(base_files.iter().map(|(p, b)| (p.as_str(), b.as_slice())))?;
        let compare = s.compare(&base_config, &config);
        if tier == "system" && !self.machine.ask(&format!("{compare}This changes the system (all users). Commit? ")) {
            bail!("not committed: a system change needs a person to approve it at a terminal");
        }
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
        pending: Option<(u64, String)>,
        confirmed: RefCell<u32>,
        asked: RefCell<Vec<String>>,
    }
    impl Machine for Fake {
        fn check(&self, files: &[(String, Option<String>)]) -> Result<()> {
            self.checks.borrow_mut().push(files.to_vec());
            Ok(())
        }
        fn commit(&self, diff: &str, base: &str, tier: &str, reason: &str, confirm: Option<u32>) -> Result<u64> {
            self.commits.borrow_mut().push((diff.into(), base.into(), tier.into(), reason.into(), confirm));
            Ok(42)
        }
        fn pending_confirm(&self) -> Result<Option<(u64, String)>> {
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
        Session::new(Schema::parse(include_str!("schema-fixture.json")).unwrap(), Repo::new(d), saved, m)
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
        let m = Fake { pending: Some((42, "soon".into())), ..Fake::default() };
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
