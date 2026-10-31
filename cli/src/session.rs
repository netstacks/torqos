//! A torqcli session (ADR-016, docs/CLI-DESIGN.md): operational and configuration mode, and the
//! commands of each in Juniper syntax. Everything that reaches the broker or evaluates Nix goes
//! through `Machine`, so the whole command layer is testable without either.

use crate::candidate::{self, Candidate, Repo};
use crate::syntax::{self, CommitOptions, Display, FileAction, Op, Style};
use crate::whole::Data;
use crate::tree::Schema;
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
    /// A person edits `text` (the config file `path`) in their editor; None: they left it unchanged.
    fn edit(&self, path: &str, text: &str) -> Result<Option<String>>;
    /// `show system alarms`: what is not as the ledger says (and a commit confirmed waiting); with
    /// `detail`, the broker's facts behind it (commits, store paths).
    fn alarms(&self, detail: bool) -> Result<String>;
    /// `show packages available <term>…`.
    fn search(&self, terms: &[String]) -> Result<String>;
    /// `request system repair`: return the machine to its last ledgered configuration.
    fn repair(&self) -> Result<String>;
    /// SSH as the configuration at `base` has it, and as it will with `files` changed — evaluated, so
    /// every way of turning it on counts (the catalog, `services openssh`, its `sshd` alias, …).
    fn ssh(&self, base: &str, files: &[(String, Option<String>)]) -> Result<(Ssh, Ssh)>;
    /// Turning SSH on: who it admits, and the keys that will be able to log in as them — read from
    /// the configuration at `base` with `files` changed, as it will be. One line per key (and a
    /// warning line above a key with options); none: nobody could log in.
    fn ssh_keys(&self, base: &str, files: &[(String, Option<String>)]) -> Result<(String, Vec<String>)>;
    /// Whether sshd is on outside the catalog with password logins allowed (`services.openssh.enable`
    /// in a .nix file): `show services` must not say SSH is off then.
    fn ssh_outside(&self) -> Result<bool>;
    /// `show system ledger <count>`: the ledger's last entries, as a person reads them.
    fn ledger(&self, count: usize) -> Result<String>;
    /// `request system revert <change>`: undo that one change (a new change forward), asked as a
    /// commit is (`me`: who is asking; their own folder needs no question).
    fn revert(&self, seq: u64, me: &str) -> Result<String>;
    /// The configurations so far, newest first (at most `count`): the numbers of `rollback <n>` and
    /// `show system commit`. From the ledger (ADR-016): its entries that produced a configuration.
    fn configurations(&self, repo: &Repo, count: usize) -> Result<History>;
    /// `request system update [all]`: the `flake.lock` that `nix flake update` (of `torq`, or with
    /// `all` every input) makes from the configuration at commit `base`.
    fn update_lock(&self, base: &str, all: bool) -> Result<String>;
}


/// The configurations (newest first), and where the ledger says the config repo is — by the
/// broker's own count (`ledgered_commit`), which repairs and interrupted changes are part of.
#[derive(Debug, Clone, PartialEq)]
pub struct History {
    pub configurations: Vec<Configuration>,
    pub at: Option<String>,
}

/// One configuration in the history: the commit, and the ledger entry that made it.
#[derive(Debug, Clone, PartialEq)]
pub struct Configuration {
    pub id: String,
    pub date: String,
    /// Who: the person who approved it, else the actor (`broker` for an automatic rollback).
    pub who: String,
    pub what: String,
    /// The ledger entry (None: read from git, without one).
    pub seq: Option<u64>,
}

/// SSH in an evaluated configuration.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Ssh {
    pub on: bool,
    /// By TorqOS's catalog (`torq services enabled ssh`): keys only, only the owner.
    pub catalog: bool,
    pub passwords: bool,
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
    /// Every NixOS setting of the system (nixos.rs): the tree's paths.
    pub tree: crate::nixos::Tree,
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

/// Split a command line into words as a shell does: whitespace-separated, "double" or 'single'
/// quotes for a word with spaces, `|` always its own word.
pub fn words(line: &str) -> Result<Vec<String>> {
    let (mut out, mut cur, mut any) = (Vec::new(), String::new(), false);
    // The quote open, if one is: `"` or `'`.
    let mut open: Option<char> = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        let quoted = open == Some('"');
        match c {
            // Inside single quotes everything is itself, up to the next one.
            _ if open == Some('\'') => {
                if c == '\'' {
                    open = None;
                } else {
                    cur.push(c);
                }
            }
            '"' => {
                open = if quoted { None } else { Some('"') };
                any = true;
            }
            '\'' if !quoted => {
                open = Some('\'');
                any = true;
            }
            // As in a shell's double quotes: `\"` and `\\` are escapes, any other `\` is itself (so a
            // `grep "\d+"` pattern arrives whole).
            '\\' if quoted => match chars.next().context("a line cannot end in \\")? {
                c @ ('"' | '\\') => cur.push(c),
                c => {
                    cur.push('\\');
                    cur.push(c);
                }
            },
            c if c.is_whitespace() && !quoted => {
                if any {
                    word(&mut out, std::mem::take(&mut cur))?;
                    any = false;
                }
            }
            '|' if !quoted => {
                if any {
                    word(&mut out, std::mem::take(&mut cur))?;
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
    match open {
        Some('\'') => bail!("missing closing ' (an apostrophe in text: put the text in \"double quotes\")"),
        Some(q) => bail!("missing closing {q}"),
        None => {}
    }
    if any {
        word(&mut out, cur)?;
    }
    Ok(out)
}

/// A word from the text (not a pipe): a quoted `"|"` alone would read as one, so it is refused.
fn word(out: &mut Vec<String>, w: String) -> Result<()> {
    if w == "|" {
        bail!("\"|\" alone cannot be a word: it would read as a pipe");
    }
    out.push(w);
    Ok(())
}

impl<'a> Session<'a> {
    pub fn new(schema: Schema, tree: crate::nixos::Tree, repo: Repo, saved: Option<PathBuf>, machine: &'a dyn Machine, me: &str, style: Style) -> Self {
        Session { schema, tree, repo, saved, mode: Mode::Operational, style, edit: Vec::new(), candidate: None, machine, me: me.into(), done: false }
    }

    /// The whole tree over both kinds of files (whole.rs).
    fn whole(&self) -> crate::whole::Whole<'_> {
        crate::whole::Whole { schema: &self.schema, tree: &self.tree }
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
        self.exec_words(&words(line)?)
    }

    /// Run one command, already split into words (`torqcli <command>`: as the calling shell split
    /// them; a word that is exactly `|` is a pipe).
    pub fn exec_words(&mut self, w: &[String]) -> Result<String> {
        // A comment (`## software: …` in a saved configuration) is not a command.
        if w.first().is_some_and(|x| x.starts_with('#')) {
            return Ok(String::new());
        }
        if w.is_empty() {
            return Ok(String::new());
        }
        // A Cisco bare path is read against the candidate it will change: load it first.
        if self.mode == Mode::Configuration {
            self.candidate()?;
        }
        let (op, filters) = syntax::parse_line(self.style, self.mode, w, &|ws| self.is_place(ws))?;
        crate::filter::apply(&filters, self.run(op)?)
    }

    /// Whether `words`, from where configuration mode is, name a place (somewhere to go), not a setting.
    fn is_place(&self, words: &[String]) -> bool {
        self.whole().is_place(&self.here(words))
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
            Op::ShowActive { path, display } => {
                // The commit named is the one shown: read at that id, not at HEAD again.
                let head = self.repo.head()?;
                let active = self.configuration_of(&head)?;
                let mut out = self.view(&active, &path, display, &format!("commit {head}"))?;
                if path.is_empty() {
                    out.push_str(&self.footer(&head, None)?);
                }
                Ok(out)
            }
            Op::ShowCommits { .. } | Op::ShowRollback { .. } | Op::ShowAlarms { .. } | Op::ShowServices | Op::ShowPackages { .. } | Op::ShowLedger { .. } => {
                self.show_operational(op)
            }
            Op::Request { words } => self.request(&words),
            Op::File { action, path } => self.file(action, path),
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
                self.candidate()?;
                if !self.whole().is_place(&full) {
                    bail!("{}: not somewhere to go (a setting takes a value)", full.join(" "));
                }
                self.edit = full;
                Ok(String::new())
            }
            Op::Set { words } => {
                let full = self.here(&words);
                let mut d = self.candidate_data()?;
                self.whole().set(&mut d, &full)?;
                self.put_data(d)?;
                self.save()?;
                Ok(String::new())
            }
            Op::Delete { words, all } => {
                // Everything below a place, or a whole list of values, goes only with `all`: an Enter
                // pressed before the value must not take them all.
                let full = self.here(&words);
                let del = match self.style {
                    Style::Juniper => "delete",
                    Style::Cisco => "no",
                };
                let mut d = self.candidate_data()?;
                self.whole().delete(&mut d, &full, all, del, &words)?;
                self.put_data(d)?;
                self.save()?;
                Ok(String::new())
            }
            Op::ShowCandidate { path, display } => {
                let d = self.candidate_data()?;
                let c = self.candidate()?;
                let (base, lock) = (c.base.clone(), c.lock.clone());
                let here = self.here(&path);
                let mut out = self.view(&d, &here, display, &format!("the candidate, from commit {base}"))?;
                if here.is_empty() {
                    out.push_str(&self.footer(&base, lock.as_deref())?);
                }
                Ok(out)
            }
            Op::Compare { display } => self.compare(display),
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
        // Out of a named entry means out of its set too (`services httpd virtualHosts example.com` →
        // `services httpd`), as in Junos and NSO: the entry and its set are one step.
        let entry = self.edit.len() >= 2 && self.tree.is_entry(&self.edit);
        self.edit.pop();
        if entry {
            self.edit.pop();
        }
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

    /// What a whole configuration at `rev` does not show in its tree, as comments: the software it
    /// runs (`lock`: the candidate's, if it changes it) and the files outside the tree that also set
    /// things (Nix). So `show configuration` names everything that makes the device, and stays a
    /// file `load` reads (`#` lines are comments).
    fn footer(&self, rev: &str, lock: Option<&str>) -> Result<String> {
        let mut out = String::new();
        let lock = match lock {
            Some(l) => Some(l.to_string()),
            None => self.repo.file_at(rev, "flake.lock")?.map(|b| String::from_utf8_lossy(&b).into_owned()),
        };
        if let Some(lock) = lock {
            let v: serde_json::Value = serde_json::from_str(&lock).unwrap_or_default();
            // The machine's own inputs (what it follows from them is theirs).
            let direct: Vec<String> = v["nodes"]["root"]["inputs"]
                .as_object()
                .map(|o| o.iter().filter_map(|(_, n)| n.as_str().map(String::from)).collect())
                .unwrap_or_default();
            let shown: Vec<String> = crate::pkg::lock_moves("", &lock)
                .iter()
                .filter(|m| direct.contains(&m.name))
                .map(|m| format!("{} {}", crate::history::one_line(&m.name, 30), crate::pkg::move_dates(m).1))
                .collect();
            if !shown.is_empty() {
                out.push_str(&format!("## software: {}\n", shown.join(", ")));
            }
        }
        // A config file attached in the tree is part of the configuration (shown as `<place> config <file>`).
        let attached = self.configuration_of(rev)?.attached;
        let outside: Vec<String> = self
            .repo
            .paths(rev)?
            .into_iter()
            // (flake.nix is TorqOS's own: its software is the line above.)
            .filter(|p| p != "flake.lock" && p != "flake.nix" && p != crate::attach::FILE && !self.schema.describes(p) && crate::settings::top_of(p).is_none())
            .filter(|p| !attached.values().any(|f| f == p)).map(|p| crate::history::one_line(&p, 60)).collect();
        if !outside.is_empty() {
            out.push_str(&format!("## also set outside this configuration, in Nix: {}\n", outside.join(", ")));
        }
        Ok(out)
    }

    /// A configuration, or the part below `path`, as `display` asks. `origin`: where it comes from
    /// (`| display detail`).
    fn view(&self, d: &Data, path: &[String], display: Option<Display>, origin: &str) -> Result<String> {
        let w = self.whole();
        match display {
            None => w.show(d, path),
            Some(Display::Set) => w.display_set(d, path),
            Some(Display::Detail) => {
                let mut out = w.show(d, path)?;
                out.push_str(&format!("## {origin}\n"));
                // Each file behind what is shown, with the settings it holds.
                let mut by_file: std::collections::BTreeMap<String, Vec<String>> = Default::default();
                for (words, file) in w.files(d) {
                    if words.starts_with(path) {
                        by_file.entry(file).or_default().push(words.join(" "));
                    }
                }
                for (file, held) in by_file {
                    out.push_str(&format!("## {file}: {}\n", held.join(", ")));
                }
                Ok(out)
            }
            Some(Display::Diff) => bail!("| display diff is for a comparison"),
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

    fn show_operational(&mut self, op: Op) -> Result<String> {
        match op {
            Op::ShowCommits { display } => {
                let mut out = String::new();
                let h = self.machine.configurations(&self.repo, 50)?;
                let all = &h.configurations;
                let width = all.iter().map(|c| c.who.chars().count()).max().unwrap_or(0);
                for (n, c) in all.iter().enumerate() {
                    out.push_str(&format!("{n:<3} {}  {:<width$}  {}\n", c.date, c.who, c.what));
                    if display == Some(Display::Detail) {
                        let seq = c.seq.map(|s| format!("  (ledger #{s})")).unwrap_or_default();
                        out.push_str(&format!("    commit {}{seq}\n", crate::history::one_line(&c.id, 64)));
                    }
                }
                // The ledger says where the config repo is (by the broker's count); if it is elsewhere, 0 —
                // what is active — is not the newest line above.
                let head = self.repo.head()?;
                if h.at.as_ref().is_some_and(|at| *at != head) {
                    out.push_str(&format!("## the config repo is at {}, not where the ledger says: `show system alarms`\n", short(&head)));
                }
                Ok(out)
            }
            Op::ShowRollback { n, compare, display } => {
                let n_id = self.commit_at(n)?;
                let n_data = self.configuration_of(&n_id)?;
                match compare {
                    None => Ok(self.view(&n_data, &[], display, &format!("commit {n_id} (rollback {n})"))? + &self.footer(&n_id, None)?),
                    // What going from configuration m to configuration n changes.
                    Some(m) => {
                        let m_id = self.commit_at(m)?;
                        match display {
                            Some(Display::Diff) => {
                                let (old, new) = (self.repo.all_files(&m_id)?, self.repo.all_files(&n_id)?);
                                commits_diff(&old, &new)
                            }
                            _ => {
                                let m_data = self.configuration_of(&m_id)?;
                                Ok(self.whole().compare(&m_data, &n_data))
                            }
                        }
                    }
                }
            }
            Op::ShowAlarms { display } => self.machine.alarms(display == Some(Display::Detail)),
            Op::ShowServices => {
                let active = self.configuration_of("HEAD")?.config;
                let on: Vec<String> = match active.files.get("services.json") {
                    Some(crate::tree::Data::List(l)) => l.clone(),
                    _ => vec![],
                };
                let catalog = self.schema.nodes.get("services").and_then(|n| n.values.clone()).unwrap_or_default();
                let width = catalog.keys().map(|k| k.len()).max().unwrap_or(0);
                let mut out: String = catalog
                    .iter()
                    .map(|(name, what)| format!("{} {name:<width$}  {what}\n", if on.contains(name) { "on " } else { "off" }))
                    .collect();
                if !on.iter().any(|s| s == "ssh") && self.machine.ssh_outside()? {
                    out.push_str("! SSH is on anyway, outside the catalog, with password logins allowed (a .nix file sets \
                                  services.openssh.enable): `set services ssh` makes it keys only, once that line is gone\n");
                }
                Ok(out)
            }
            Op::ShowPackages { terms } => self.machine.search(&terms),
            Op::ShowLedger { count } => self.machine.ledger(count.unwrap_or(20)),
            other => bail!("not an operational show: {other:?}"),
        }
    }

    /// The commit n changes back (`rollback <n>`'s numbering: 0 is the current one).
    /// 0 is what is active (HEAD), whatever the ledger says; the others are the ledger's.
    fn commit_at(&self, n: usize) -> Result<String> {
        if n == 0 {
            return self.repo.head();
        }
        let all = self.machine.configurations(&self.repo, n + 1)?.configurations;
        let c = all.get(n).with_context(|| format!("there are only {} configurations", all.len()))?;
        if !self.repo.has_commit(&c.id)? {
            bail!("configuration {n} (commit {}) is no longer in the config repo", short(&c.id));
        }
        Ok(c.id.clone())
    }

    /// The configuration at commit `id`.
    fn configuration_of(&self, id: &str) -> Result<Data> {
        let files = self.repo.tree_files(&self.schema, id)?;
        data_of(&self.schema, &files)
    }

    /// The candidate's tree data.
    fn candidate_data(&mut self) -> Result<Data> {
        let c = self.candidate()?;
        Ok(Data { config: c.config.clone(), settings: c.settings.clone(), attached: c.attached.clone() })
    }

    /// Put the tree data back into the candidate (not saved yet).
    fn put_data(&mut self, d: Data) -> Result<()> {
        let c = self.candidate()?;
        (c.config, c.settings, c.attached) = (d.config, d.settings, d.attached);
        Ok(())
    }

    fn request(&mut self, w: &[String]) -> Result<String> {
        match w.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
            ["system", "repair"] => self.machine.repair(),
            ["system", "revert", n] => {
                let seq: u64 = n.parse().with_context(|| format!("request system revert <change>: '{n}' is not a change number"))?;
                self.machine.revert(seq, &self.me)
            }
            ["system", "update"] => self.update(false),
            ["system", "update", "all"] => self.update(true),
            ["system", "software", "rollback", n] => self.software_rollback(syntax::number(n)?),
            _ => bail!("request: one of: system repair, system revert <change>, system update [all], system software rollback <n>"),
        }
    }

    /// `request system update [all]`: the newest TorqOS (with `all`, every input) into the candidate.
    fn update(&mut self, all: bool) -> Result<String> {
        let base = self.candidate()?.base.clone();
        let new = self.machine.update_lock(&base, all)?;
        self.load_software(new, "This machine has the newest software: nothing to load.\n")
    }

    /// `request system software rollback <n>`: commit n's software into the candidate.
    fn software_rollback(&mut self, n: usize) -> Result<String> {
        let id = self.commit_at(n)?;
        let lock = self.repo.file_at(&id, "flake.lock")?.with_context(|| format!("configuration {n} has no flake.lock"))?;
        let lock = String::from_utf8(lock).with_context(|| format!("configuration {n}'s flake.lock is not text"))?;
        self.load_software(lock, &format!("Configuration {n} has this machine's software already: nothing to load.\n"))
    }

    /// Put `lock` in the candidate as its software and say what it changes (or `same`, if nothing).
    fn load_software(&mut self, lock: String, same: &str) -> Result<String> {
        if serde_json::from_str::<serde_json::Value>(&lock).map_or(true, |v| !v.is_object()) {
            bail!("that flake.lock is not JSON; nothing loaded");
        }
        let head = self.repo.head()?;
        let base = self.candidate()?.base.clone();
        let base_lock = self.repo.file_at(&base, "flake.lock")?;
        let shown = software_compare(base_lock.as_deref(), &Some(lock.clone()));
        let c = self.candidate()?;
        let had = c.lock.is_some();
        c.lock = (!shown.is_empty()).then_some(lock);
        self.save()?;
        let mut out = match (shown.is_empty(), had) {
            (true, false) => same.to_string(),
            (true, true) => format!("{}The software loaded into the candidate before is dropped.\n", same.replace(": nothing to load", "")),
            (false, _) => format!("{shown}Loaded into the candidate: `commit` (in configuration mode) installs it.\n"),
        };
        if base != head {
            out.push_str(&format!(
                "The configuration has changed since your candidate was started ({} → {}): `commit` will be \
                 refused; `rollback 0` starts over (without this software), then load it again.\n",
                short(&base),
                short(&head)
            ));
        }
        Ok(out)
    }

    /// What the candidate changes from its base (`| display diff`: as the raw diff of the files).
    fn compare(&mut self, display: Option<Display>) -> Result<String> {
        let a = self.against()?;
        if display == Some(Display::Diff) {
            let scratch = tempfile::tempdir()?;
            return candidate::unified_diff(scratch.path(), &a.changes(&self.schema)?);
        }
        a.compare(&self.whole())
    }

    fn rollback(&mut self, n: usize) -> Result<String> {
        let id = self.commit_at(n).with_context(|| format!("rollback {n}"))?;
        // A configuration, not software: what the candidate had loaded (`request system update`) stays —
        // except for `rollback 0`, which starts over.
        let kept = if n == 0 { None } else { self.candidate()?.lock.clone() };
        let mut c = Candidate::from_repo(&self.repo, &self.schema, &id)?;
        c.lock = kept.clone();
        self.candidate = Some(c);
        self.save()?;
        Ok(if kept.is_some() { "load complete (the software loaded into the candidate is kept)\n".into() } else { "load complete\n".into() })
    }

    /// Whether the candidate differs from its base.
    fn uncommitted(&mut self) -> Result<bool> {
        if self.candidate.is_none() && self.saved.as_ref().is_none_or(|p| !p.exists()) {
            return Ok(false);
        }
        let a = self.against()?;
        Ok(!a.changes(&self.schema)?.is_empty())
    }

    /// The candidate and what it is based on, read once.
    fn against(&mut self) -> Result<Against> {
        let data = self.candidate_data()?;
        let (base, new_lock, nix) = {
            let c = self.candidate()?;
            (c.base.clone(), c.lock.clone(), c.nix.clone())
        };
        let files = self.repo.tree_files(&self.schema, &base)?;
        let lock = self.repo.file_at(&base, "flake.lock")?;
        let base_nix = nix.keys().map(|p| Ok((p.clone(), self.repo.file_at(&base, p)?))).collect::<Result<_>>()?;
        Ok(Against { base, files, lock, data, new_lock, nix, base_nix })
    }

    fn commit(&mut self, o: CommitOptions) -> Result<String> {
        let s = self.schema.clone();
        let head = self.repo.head()?;
        let a = self.against()?;
        let base = a.base.clone();
        let changes = a.changes(&s)?;
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
        let compare = a.compare(&self.whole())?;
        let mut shown: String = compare.lines().map(|l| format!("{}\n", crate::history::clean(l))).collect();
        // The catalog's SSH admits its owner's keys only: with none, nobody could log in; with some,
        // approving it approves exactly those (as the changed configuration has them).
        // Whether this turns SSH on is read from the evaluated configurations, before and after: every
        // way counts (the catalog, `services openssh`, its `sshd` alias, a config file). Only tree and Nix files can.
        let ssh_relevant = files.iter().any(|(p, _)| p == "services.json" || p.starts_with(crate::settings::DIR) || p == crate::attach::FILE || p.ends_with(".nix"));
        let (was, now) = if ssh_relevant { self.machine.ssh(&base, &files)? } else { Default::default() };
        if now.on && !was.on {
            let (owner, keys) = self.machine.ssh_keys(&base, &files)?;
            if now.catalog {
                if keys.is_empty() {
                    bail!(
                        "not committed: SSH here lets in {owner}'s keys only, and they have none, so nobody could \
                         log in; add a public key to ~/.ssh/authorized_keys first"
                    );
                }
                shown.push_str(&format!("Keys that will be able to log in as {owner}:\n{}\n", keys.join("\n")));
            } else {
                // Turned on outside TorqOS's catalog: its rules (keys only, only the owner) are not there.
                shown.push_str("SSH is turned on outside TorqOS's catalog: its rules (keys only, only the owner) do not apply.\n");
                if now.passwords {
                    shown.push_str("! Password logins are allowed (services openssh settings PasswordAuthentication).\n");
                }
                if !keys.is_empty() {
                    shown.push_str(&format!("Keys that will be able to log in as {owner}, among others:\n{}\n", keys.join("\n")));
                }
            }
        }
        // How the machine boots: a mistake shows only at the next restart (no `commit confirmed` timer runs
        // on a system that does not come up), so say so before anyone approves it.
        let section = |l: &str| l.strip_prefix("[edit ").and_then(|s| s.strip_suffix(']')).and_then(|s| s.split(' ').next()).map(String::from);
        let boots = compare.lines().filter_map(section).any(|top| matches!(top.as_str(), "fileSystems" | "boot" | "swapDevices"))
            || files.iter().any(|(p, _)| p == "hardware-configuration.nix");
        if boots {
            shown.push_str("! This changes how the machine starts (file systems, boot): a mistake shows only at the next restart, where `commit confirmed` cannot undo it; the earlier configurations stay in the boot menu.\n");
        }
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
            Some(m) => format!(
                "commit confirmed will be automatically rolled back in {m} minute{} unless confirmed (change #{seq})\ncommit complete\n",
                if m == 1 { "" } else { "s" }
            ),
            None => format!("commit complete (change #{seq})\n"),
        })
    }

    /// `file list|show|edit|delete`: the config repo's own Nix files, the escape hatch for what is
    /// not a value (docs/CONFIG-IS-THE-DEVICE.md). In configuration mode, the candidate's (edits
    /// included); otherwise, the active configuration's. Edits wait in the candidate for `commit`.
    fn file(&mut self, action: FileAction, path: Option<String>) -> Result<String> {
        let editing = self.mode == Mode::Configuration;
        let (rev, nix, attached) = if editing {
            let d = self.candidate_data()?;
            let c = self.candidate()?;
            (c.base.clone(), c.nix.clone(), d.attached)
        } else {
            let head = self.repo.head()?;
            let attached = self.configuration_of(&head)?.attached;
            (head, Default::default(), attached)
        };
        let text_of = |s: &Self, p: &str| -> Result<Option<String>> {
            match nix.get(p) {
                Some(t) => Ok(t.clone()),
                None => s.repo.file_at(&rev, p)?.map(|b| String::from_utf8(b).with_context(|| format!("{p}: not text"))).transpose(),
            }
        };
        let Some(path) = path else {
            // `file list`: every Nix file, where each is attached, and what the candidate changed.
            let mut all: std::collections::BTreeSet<String> = self.repo.paths(&rev)?.into_iter().filter(|p| p.ends_with(".nix")).collect();
            all.extend(nix.keys().cloned());
            let mut out = String::new();
            for p in all {
                let state = match nix.get(&p) {
                    Some(None) => " (deleted in the candidate)",
                    Some(Some(_)) if self.repo.file_at(&rev, &p)?.is_none() => " (new in the candidate)",
                    Some(Some(_)) => " (edited in the candidate)",
                    None => "",
                };
                let at: Vec<&str> = attached.iter().filter(|(_, f)| **f == p).map(|(a, _)| a.as_str()).collect();
                let at: Vec<&str> = at.into_iter().map(|a| if a.is_empty() { "the top" } else { a }).collect();
                let at = if at.is_empty() { String::new() } else { format!("  [attached at: {}]", at.join("; ")) };
                let at = if p == "flake.nix" { "  (TorqOS's own: it builds the machine from the tree)".to_string() } else { at };
                out.push_str(&format!("{p}{state}{at}\n"));
            }
            return Ok(out);
        };
        crate::attach::valid(&path)?;
        match action {
            FileAction::Show => match text_of(self, &path)? {
                Some(t) => Ok(if t.ends_with('\n') || t.is_empty() { t } else { t + "\n" }),
                None => bail!("{path}: no such file in the configuration"),
            },
            FileAction::Edit => {
                let was = text_of(self, &path)?;
                let start = was.clone().unwrap_or_else(|| "{ ... }:\n{\n}\n".into());
                let Some(now) = self.machine.edit(&path, &start)? else { return Ok(format!("{path}: unchanged\n")) };
                if Some(&now) == was.as_ref() {
                    return Ok(format!("{path}: unchanged\n"));
                }
                if now.contains('\0') {
                    bail!("{path}: not text; not kept");
                }
                self.candidate()?.nix.insert(path.clone(), Some(now));
                self.save()?;
                let hint = if attached.values().any(|f| *f == path) || self.repo.file_at(&rev, &path)?.is_some() {
                    String::new()
                } else {
                    format!(" (not attached anywhere yet: `set <place> config {path}`)")
                };
                Ok(format!("{path}: changed in the candidate{hint}; `show | compare` shows it, `commit` makes it so\n"))
            }
            FileAction::Delete => {
                if text_of(self, &path)?.is_none() {
                    bail!("{path}: no such file in the configuration");
                }
                if let Some((at, _)) = attached.iter().find(|(_, f)| **f == path) {
                    bail!("{path} is attached at `{at}`: `delete {at} config` first");
                }
                let exists = self.repo.file_at(&rev, &path)?.is_some();
                let c = self.candidate()?;
                if exists { c.nix.insert(path.clone(), None) } else { c.nix.remove(&path) };
                self.save()?;
                Ok(format!("{path}: deleted in the candidate\n"))
            }
            FileAction::List => unreachable!("file list has no path"),
        }
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

/// The tree data in a commit's files: the kept files, nixos/*.json, config-files.json.
fn data_of(s: &Schema, files: &std::collections::BTreeMap<String, Vec<u8>>) -> Result<Data> {
    let config = s.load(files.iter().map(|(p, b)| (p.as_str(), b.as_slice())))?;
    let settings = crate::settings::load(files.iter().map(|(p, b)| (p.as_str(), b.as_slice())))?;
    let attached = crate::attach::load(files.get(crate::attach::FILE).map(Vec::as_slice))?;
    Ok(Data { config, settings, attached })
}

/// The raw change between two commits: every file (the tree's, the Nix files, `flake.lock`), byte
/// for byte as committed (not as TorqOS would write them now).
fn commits_diff(old: &std::collections::BTreeMap<String, Vec<u8>>, new: &std::collections::BTreeMap<String, Vec<u8>>) -> Result<String> {
    let paths: std::collections::BTreeSet<&String> = old.keys().chain(new.keys()).collect();
    let changes: Vec<candidate::RawChange> = paths
        .into_iter()
        .filter(|p| old.get(*p) != new.get(*p))
        .map(|p| (p.clone(), old.get(p).map(Vec::as_slice), new.get(p).map(Vec::as_slice)))
        .collect();
    let scratch = tempfile::tempdir()?;
    candidate::unified_diff_bytes(scratch.path(), &changes)
}

/// The candidate and its base, read once: the base's tree files and `flake.lock`, the candidate's.
struct Against {
    base: String,
    files: std::collections::BTreeMap<String, Vec<u8>>,
    lock: Option<Vec<u8>>,
    data: Data,
    new_lock: Option<String>,
    /// Config files as edited in the candidate, and as they are at the base (None: not there).
    nix: std::collections::BTreeMap<String, Option<String>>,
    base_nix: std::collections::BTreeMap<String, Option<Vec<u8>>>,
}

impl Against {
    fn base_data(&self, s: &Schema) -> Result<Data> {
        data_of(s, &self.files)
    }

    /// What the candidate changes: the tree's files (every kind), the config files edited, then the
    /// software. A NixOS settings file changes when its values do, not its formatting.
    fn changes(&self, s: &Schema) -> Result<Vec<candidate::Change<'_>>> {
        let mut c = candidate::changes(s, &self.files, &self.data.config)?;
        let base = self.base_data(s)?;
        let new = crate::settings::files(&self.data.settings);
        let tops: std::collections::BTreeSet<&String> = base.settings.keys().chain(self.data.settings.keys()).collect();
        for top in tops {
            let empty = |v: Option<&serde_json::Value>| v.is_none_or(|v| v.as_object().is_some_and(|o| o.is_empty()));
            let (was, now) = (base.settings.get(top), self.data.settings.get(top));
            if was == now || (empty(was) && empty(now)) {
                continue;
            }
            let path = format!("{}{top}.json", crate::settings::DIR);
            c.push((path.clone(), self.files.get(&path).map(Vec::as_slice), new.get(&path).cloned()));
        }
        if base.attached != self.data.attached {
            let path = crate::attach::FILE.to_string();
            c.push((path.clone(), self.files.get(&path).map(Vec::as_slice), crate::attach::render(&self.data.attached)));
        }
        for (path, text) in &self.nix {
            let was = self.base_nix.get(path).cloned().flatten();
            if was.as_deref() != text.as_deref().map(str::as_bytes) {
                c.push((path.clone(), self.base_nix.get(path).and_then(|b| b.as_deref()), text.clone()));
            }
        }
        c.extend(candidate::lock_change(self.lock.as_deref(), &self.new_lock));
        Ok(c)
    }

    /// `show | compare`: the tree's changes by NixOS path, the config files edited as their raw diffs
    /// (as written: Nix is shown, never interpreted), then the software's.
    fn compare(&self, w: &crate::whole::Whole) -> Result<String> {
        let mut out = w.compare(&self.base_data(w.schema)?, &self.data);
        let edited: Vec<candidate::Change> = self
            .nix
            .iter()
            .filter(|(p, t)| self.base_nix.get(*p).cloned().flatten().as_deref() != t.as_deref().map(str::as_bytes))
            .map(|(p, t)| (p.clone(), self.base_nix.get(p).and_then(|b| b.as_deref()), t.clone()))
            .collect();
        if !edited.is_empty() {
            let scratch = tempfile::tempdir()?;
            out.push_str(&format!("[edit config files]\n{}", candidate::unified_diff(scratch.path(), &edited)?));
        }
        out.push_str(&software_compare(self.lock.as_deref(), &self.new_lock));
        Ok(out)
    }
}

/// The software a candidate changes, as `show | compare` shows it: each input moved, and a
/// downgrade said so.
fn software_compare(old: Option<&[u8]>, new: &Option<String>) -> String {
    let Some(new) = new else { return String::new() };
    let old = old.map(|o| String::from_utf8_lossy(o).into_owned()).unwrap_or_default();
    if old == *new {
        return String::new();
    }
    let mut out = String::from("[edit system software]\n");
    let moves = crate::pkg::lock_moves(&old, new);
    if moves.is_empty() {
        out.push_str("    flake.lock changes (`| display diff` shows how)\n");
    }
    let mut older = false;
    for m in &moves {
        // Names come from upstream flakes: one line each, cleaned, so none can draw a line of its own.
        let name = crate::history::one_line(&m.name, 40);
        let (before, after) = crate::pkg::move_dates(m);
        if m.was {
            out.push_str(&format!("-   {name} {before};\n"));
        }
        if m.is {
            out.push_str(&format!("+   {name} {after};\n"));
        }
        // Older to the second; an input whose date is unknown on either side says nothing.
        older |= matches!((m.before, m.after), (Some(b), Some(a)) if a < b);
    }
    if older {
        out.push_str("## older software than now: a downgrade\n");
    }
    out
}

/// A ledger reason from the comparison: its `+`/`-` lines, short.
fn summary(compare: &str) -> String {
    // Each `+`/`-` line with its section (`[edit torq cli userStyles]` + `+   dev cisco;` →
    // `+ torq cli userStyles dev cisco;`): a ledger reason is read on its own.
    let mut section = String::new();
    let mut lines: Vec<String> = Vec::new();
    let mut was = String::new();
    for l in compare.lines() {
        if let Some(s) = l.strip_prefix("[edit").and_then(|s| s.strip_suffix(']')) {
            section = s.trim().to_string();
        } else if section == "config files" {
            // A raw diff: just which file, and how.
            if let Some(p) = l.strip_prefix("--- ") {
                was = p.strip_prefix("a/").unwrap_or(p).to_string();
            } else if let Some(p) = l.strip_prefix("+++ ") {
                lines.push(match p.strip_prefix("b/") {
                    Some(p) if was == "/dev/null" => format!("+ config file {p}"),
                    Some(p) => format!("~ config file {p}"),
                    None => format!("- config file {was}"),
                });
            }
        } else if let Some((sign, rest)) = l.split_at_checked(1).filter(|(s, _)| *s == "+" || *s == "-") {
            let at = if section.is_empty() { String::new() } else { format!("{section} ") };
            lines.push(format!("{sign} {at}{}", rest.trim()));
        } else if !l.starts_with('#') && !l.trim().is_empty() {
            lines.push(l.trim().to_string());
        }
    }
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

    /// Tree files a change writes (None: deletes), as the machine is given them.
    type Files = Vec<(String, Option<String>)>;

    #[derive(Default)]
    struct Fake {
        commits: RefCell<Vec<(String, String, String, String, Option<u32>)>>,
        checks: RefCell<Vec<Files>>,
        answer: bool,
        pending: Option<Waiting>,
        confirmed: RefCell<u32>,
        asked: RefCell<Vec<String>>,
        /// The broker refuses the commit.
        refuse: bool,
        /// The bases `request system update` was asked about.
        bases: RefCell<Vec<String>>,
        /// The ledger's configurations, if a test gives them; else the repo's history stands in.
        ledger: Option<Vec<Configuration>>,
        /// Where the ledger says the repo is, if a test says (else: its newest configuration).
        ledger_at: Option<String>,
        /// sshd on outside the catalog.
        ssh_outside: bool,
        /// SSH before and after a commit, if a test says.
        ssh_state: Option<(Ssh, Ssh)>,
        /// The keys turning SSH on would admit, and what was asked about.
        keys: Vec<String>,
        ssh_asked: RefCell<Vec<Files>>,
        /// What `file edit` leaves a file as (None: unchanged), and what it was given.
        edit_to: Option<String>,
        edited: RefCell<Vec<(String, String)>>,
        ssh_calls: RefCell<usize>,
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
        fn edit(&self, path: &str, text: &str) -> Result<Option<String>> {
            self.edited.borrow_mut().push((path.into(), text.into()));
            Ok(self.edit_to.clone())
        }
        fn alarms(&self, detail: bool) -> Result<String> {
            Ok(if detail { "all well\n  running system /nix/store/x\n".into() } else { "all well\n".into() })
        }
        fn search(&self, terms: &[String]) -> Result<String> {
            Ok(format!("searched {}\n", terms.join(" ")))
        }
        fn repair(&self) -> Result<String> {
            Ok("repaired\n".into())
        }
        fn ledger(&self, count: usize) -> Result<String> {
            Ok(format!("ledger {count}\n"))
        }
        fn ssh_outside(&self) -> Result<bool> {
            Ok(self.ssh_outside)
        }
        /// As the evaluator would: the catalog's `ssh`, or `services openssh|sshd enable true` in the
        /// changed files (a test may say otherwise).
        fn ssh(&self, _base: &str, files: &[(String, Option<String>)]) -> Result<(Ssh, Ssh)> {
            *self.ssh_calls.borrow_mut() += 1;
            if let Some(s) = self.ssh_state {
                return Ok(s);
            }
            let has = |path: &str, f: &dyn Fn(&serde_json::Value) -> bool| {
                files.iter().any(|(p, c)| p == path && c.as_deref().and_then(|c| serde_json::from_str(c).ok()).is_some_and(|v| f(&v)))
            };
            let catalog = has("services.json", &|v| v.as_array().is_some_and(|a| a.iter().any(|x| x == "ssh")));
            let nixos = has("nixos/services.json", &|v| v.pointer("/openssh/enable") == Some(&serde_json::Value::Bool(true)) || v.pointer("/sshd/enable") == Some(&serde_json::Value::Bool(true)));
            Ok((Ssh::default(), Ssh { on: catalog || nixos, catalog, passwords: nixos && !catalog }))
        }
        fn ssh_keys(&self, _base: &str, files: &[(String, Option<String>)]) -> Result<(String, Vec<String>)> {
            self.ssh_asked.borrow_mut().push(files.to_vec());
            Ok(("dev".into(), self.keys.clone()))
        }
        fn revert(&self, seq: u64, me: &str) -> Result<String> {
            Ok(format!("reverted {seq} for {me}\n"))
        }
        fn configurations(&self, repo: &Repo, count: usize) -> Result<History> {
            let configurations: Vec<Configuration> = match &self.ledger {
                Some(l) => l.iter().take(count).cloned().collect(),
                None => repo
                    .history(count)?
                    .into_iter()
                    .map(|c| Configuration { id: c.id, date: c.date, who: "dev".into(), what: c.subject, seq: None })
                    .collect(),
            };
            let at = self.ledger_at.clone().or_else(|| configurations.first().map(|c| c.id.clone()));
            Ok(History { configurations, at })
        }
        fn update_lock(&self, base: &str, all: bool) -> Result<String> {
            self.bases.borrow_mut().push(base.into());
            Ok(lock(if all { NEWER_ALL } else { NEWER }))
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
    fn w(s: &str) -> Vec<String> {
        words(s).unwrap()
    }
    fn nixos_tree() -> crate::nixos::Tree {
        crate::nixos::Tree::parse(include_str!("nixos-fixture.json")).unwrap()
    }
    fn session<'a>(d: &Path, saved: Option<PathBuf>, m: &'a Fake) -> Session<'a> {
        Session::new(Schema::parse(include_str!("schema-fixture.json")).unwrap(), nixos_tree(), Repo::new(d), saved, m, "dev", Style::Juniper)
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
    fn a_quoted_bar_alone_is_never_a_pipe() {
        let e = format!("{:#}", words(r#"show "|" compare"#).unwrap_err());
        assert!(e.contains("cannot be a word"), "{e}");
        assert!(words(r#"commit comment "a | b""#).is_ok(), "inside other text it is text");
        assert_eq!(words("grep 'a \\d '\"\"").unwrap(), ["grep", "a \\d "], "single quotes: all literal");
        assert_eq!(words(r#"grep "\d\"\\""#).unwrap(), ["grep", r#"\d"\"#], "double quotes: only \" and \\ escape");
        assert_eq!(words(r#"commit comment "it's""#).unwrap(), ["commit", "comment", "it's"]);
        assert!(words("grep 'open").is_err());
        let e = format!("{:#}", words("commit comment it's fine").unwrap_err());
        assert!(e.contains("put the text in \"double quotes\""), "an apostrophe opens a quote, as in a shell: {e}");
    }

    /// A `flake.lock` whose `torq` input was last changed at `t` (seconds).
    fn lock(t: u64) -> String {
        format!(r#"{{"nodes":{{"root":{{"inputs":{{"torq":"torq"}}}},"torq":{{"locked":{{"lastModified":{t},"narHash":"h{t}"}}}}}},"version":7}}"#)
    }
    /// The software the fake machine finds newer (`request system update`), and with `all`.
    const NEWER: u64 = 1_793_400_000;
    const NEWER_ALL: u64 = 1_793_500_000;
    /// The test repo, then two commits of software: `OLD` (rollback 1), then `NOW` (rollback 0).
    const OLD: u64 = 1_790_000_000;
    const NOW: u64 = 1_793_000_000;
    fn repo_with_software() -> tempfile::TempDir {
        let d = repo();
        sh(d.path(), &format!("printf '%s' '{}' > flake.lock && git add -A && git commit -qm old-software", lock(OLD)));
        sh(d.path(), &format!("printf '%s' '{}' > flake.lock && git commit -qam software", lock(NOW)));
        d
    }

    #[test]
    fn request_system_update_loads_the_software_into_the_candidate() {
        let (d, m) = (repo_with_software(), Fake { answer: true, ..Default::default() });
        let mut s = session(d.path(), None, &m);
        let out = s.exec("request system update").unwrap();
        let (now, newer) = (crate::pkg::ymd(NOW), crate::pkg::ymd(NEWER));
        assert_eq!(
            out,
            format!("[edit system software]\n-   torq {now};\n+   torq {newer};\nLoaded into the candidate: `commit` (in configuration mode) installs it.\n")
        );
        s.exec("configure").unwrap();
        assert!(s.exec("show | compare").unwrap().starts_with(&format!("[edit system software]\n-   torq {now};")), "nothing else changed");
        let raw = s.exec("show | compare | display diff").unwrap();
        assert!(raw.contains("+++ b/flake.lock") && raw.contains(&format!("h{NEWER}")), "{raw}");
        s.exec("commit check").unwrap();
        assert_eq!(m.checks.borrow().last().unwrap(), &vec![("flake.lock".to_string(), Some(lock(NEWER)))], "checked with the new software");
        assert_eq!(s.exec("commit").unwrap(), "commit complete (change #42)\n");
        let (diff, _, tier, reason, _) = m.commits.borrow()[0].clone();
        assert_eq!(diff, raw, "commit proposes what | display diff showed");
        assert_eq!(tier, "system", "software is the system's");
        assert!(reason.contains(&format!("+ system software torq {newer};")), "{reason}");
        assert!(m.asked.borrow()[0].contains("[edit system software]"), "a person sees it before it is committed");
        // `all`: every input.
        let out = s.exec("run request system update all").unwrap();
        assert!(out.contains(&format!("+   torq {};", crate::pkg::ymd(NEWER_ALL))) && out.ends_with("Loaded into the candidate: `commit` (in configuration mode) installs it.\n"), "{out}");
    }

    #[test]
    fn up_to_date_software_loads_nothing() {
        let (d, m) = (repo(), Fake::default());
        sh(d.path(), &format!("printf '%s' '{}' > flake.lock && git add -A && git commit -qm newest", lock(NEWER)));
        let mut s = session(d.path(), None, &m);
        assert_eq!(s.exec("request system update").unwrap(), "This machine has the newest software: nothing to load.\n");
        assert_eq!(s.exec("configure").unwrap(), "Entering configuration mode\n");
        assert_eq!(s.exec("show | compare").unwrap(), "");
    }

    #[test]
    fn software_rollback_loads_an_earlier_commits_software_as_a_downgrade() {
        let (d, m) = (repo_with_software(), Fake::default());
        let saved = d.path().join("saved.json");
        let mut s = session(d.path(), Some(saved.clone()), &m);
        let out = s.exec("request system software rollback 1").unwrap();
        let (now, old) = (crate::pkg::ymd(NOW), crate::pkg::ymd(OLD));
        assert!(out.starts_with(&format!("[edit system software]\n-   torq {now};\n+   torq {old};\n## older software than now: a downgrade\n")), "{out}");
        assert_eq!(
            s.exec("request system software rollback 0").unwrap(),
            "Configuration 0 has this machine's software already.\nThe software loaded into the candidate before is dropped.\n",
            "going back to the base's software drops the downgrade, and says so"
        );
        let e = format!("{:#}", s.exec("request system software rollback 2").unwrap_err());
        assert!(e.contains("configuration 2 has no flake.lock"), "{e}");
        // Software that is not a flake.lock is never loaded.
        let (d2, m2) = (repo_with_software(), Fake::default());
        sh(d2.path(), "printf 'not json' > flake.lock && git commit -qam broken");
        let mut s2 = session(d2.path(), None, &m2);
        let e = format!("{:#}", s2.exec("request system software rollback 0").unwrap_err());
        assert!(e.contains("not JSON; nothing loaded"), "{e}");
        // Loading the same software again leaves nothing to commit; another session sees what is kept.
        s.exec("request system software rollback 1").unwrap();
        let mut again = session(d.path(), Some(saved.clone()), &m);
        again.exec("configure").unwrap();
        assert!(again.exec("show | compare").unwrap().contains(&format!("+   torq {old};")), "the candidate keeps its software");
        assert_eq!(again.exec("exit").unwrap(), "The configuration has been changed but not committed (it is kept: `configure` returns to it).\n");
        again.exec("configure").unwrap();
        assert_eq!(again.exec("rollback 0").unwrap(), "load complete\n");
        assert_eq!(again.exec("show | compare").unwrap(), "", "rollback 0 discards the software too");
        // `rollback <n>` loads a configuration, never its software.
        again.exec("rollback 1").unwrap();
        assert!(!again.exec("show | compare").unwrap().contains("software"), "rollback n leaves the software alone");
    }

    #[test]
    fn the_software_is_loaded_against_the_candidates_base() {
        let (d, m) = (repo_with_software(), Fake::default());
        let mut s = session(d.path(), None, &m);
        s.exec("configure").unwrap();
        s.exec("set system time-zone UTC").unwrap();
        let base = s.repo.head().unwrap();
        sh(d.path(), "printf '{ \"host-name\": \"other\" }\\n' > system/host.json && git commit -qam other");
        let out = s.exec("run request system update").unwrap();
        assert_eq!(m.bases.borrow().as_slice(), [base], "the candidate's base, not HEAD");
        assert!(out.contains("has changed since your candidate was started") && out.contains("`rollback 0` starts over"), "{out}");
    }

    #[test]
    fn rollback_n_keeps_the_loaded_software_rollback_0_drops_it() {
        let (d, m) = (repo_with_software(), Fake::default());
        let mut s = session(d.path(), None, &m);
        s.exec("request system update").unwrap();
        s.exec("configure").unwrap();
        assert_eq!(s.exec("rollback 3").unwrap(), "load complete (the software loaded into the candidate is kept)\n");
        let c = s.exec("show | compare").unwrap();
        assert!(c.contains("hostName one;") && c.contains("[edit system software]"), "{c}");
        assert_eq!(s.exec("rollback 0").unwrap(), "load complete\n");
        assert_eq!(s.exec("show | compare").unwrap(), "");
    }

    #[test]
    fn the_software_comparison_shows_every_move_safely() {
        let node = |t: u64| format!(r#"{{"locked":{{"lastModified":{t},"narHash":"h{t}"}}}}"#);
        let lock = |nodes: &[(&str, String)]| {
            let n: Vec<String> = nodes.iter().map(|(k, v)| format!("{}:{v}", serde_json::to_string(k).unwrap())).collect();
            format!(r#"{{"nodes":{{"root":{{}},{}}},"version":7}}"#, n.join(","))
        };
        let (a, b) = (lock(&[("torq", node(NOW)), ("extra", node(OLD))]), lock(&[("torq", node(NOW - 10))]));
        let out = software_compare(Some(a.as_bytes()), &Some(b.clone()));
        assert!(out.contains("-   extra ") && !out.contains("+   extra"), "a removed input shows: {out}");
        assert!(out.contains("## older software than now: a downgrade"), "ten seconds older, in the same minute, is older: {out}");
        let evil = lock(&[("torq", node(NOW)), ("x;\n+   fine", node(NEWER))]);
        let out = software_compare(Some(b.as_bytes()), &Some(evil));
        assert!(!out.lines().any(|l| l.starts_with("+   fine")), "a name draws no line of its own: {out}");
    }

    #[test]
    fn loading_the_bases_software_says_it_drops_what_was_loaded() {
        let (d, m) = (repo(), Fake::default());
        sh(d.path(), &format!("printf '%s' '{}' > flake.lock && git add -A && git commit -qm old", lock(OLD)));
        sh(d.path(), &format!("printf '%s' '{}' > flake.lock && git commit -qam newest", lock(NEWER)));
        let mut s = session(d.path(), None, &m);
        s.exec("request system software rollback 1").unwrap();
        assert_eq!(s.exec("request system update").unwrap(), "This machine has the newest software.\nThe software loaded into the candidate before is dropped.\n");
        assert_eq!(s.exec("configure").unwrap(), "Entering configuration mode\n");
        assert_eq!(s.exec("show | compare").unwrap(), "");
    }

    #[test]
    fn a_linked_flake_lock_is_never_followed() {
        let (d, m) = (repo(), Fake::default());
        sh(d.path(), "printf '{}' > real.lock && ln -s real.lock flake.lock && git add -A && git commit -qm link");
        let mut s = session(d.path(), None, &m);
        let e = format!("{:#}", s.exec("request system software rollback 0").unwrap_err());
        assert!(e.contains("flake.lock at") && e.contains("is not a plain file"), "{e}");
    }

    /// After a commit confirmed rolls back, the config repo is reset to the commit before it: git's
    /// history no longer has it, the ledger does. The numbers are the ledger's.
    #[test]
    fn rollback_numbers_are_the_ledgers() {
        let d = repo();
        let ids: Vec<String> = Repo::new(d.path()).history(2).unwrap().into_iter().map(|c| c.id).collect();
        let (two, one) = (ids[0].clone(), ids[1].clone());
        sh(d.path(), "git reset -q --hard HEAD~1");
        let conf = |id: &str, who: &str, what: &str, seq| Configuration { id: id.into(), date: "d".into(), who: who.into(), what: what.into(), seq: Some(seq) };
        let m = Fake {
            ledger: Some(vec![conf(&one, "broker", "Change #2 rolled back", 3), conf(&two, "viper", "host two", 2), conf(&one, "user", "init", 1)]),
            ..Default::default()
        };
        let mut s = session(d.path(), None, &m);
        let commits = s.exec("show system commit").unwrap();
        assert_eq!(commits, "0   d  broker  Change #2 rolled back\n1   d  viper   host two\n2   d  user    init\n");
        assert!(s.exec("show system commit | display detail").unwrap().contains(&format!("    commit {two}  (ledger #2)\n")));
        assert_eq!(s.exec("show system rollback 1").unwrap(), "networking {\n    hostName two;\n}\n", "git's history alone has no 1");
        s.exec("configure").unwrap();
        s.exec("rollback 1").unwrap();
        assert!(s.exec("show | compare").unwrap().contains("+   hostName two;"));
        let e = format!("{:#}", s.exec("rollback 3").unwrap_err());
        assert!(e.contains("rollback 3: there are only 3 configurations"), "{e}");
    }

    #[test]
    fn a_repo_away_from_the_ledger_is_said() {
        let d = repo();
        let first = Repo::new(d.path()).history(2).unwrap()[1].id.clone();
        let m = Fake { ledger: Some(vec![Configuration { id: first, date: "d".into(), who: "u".into(), what: "w".into(), seq: Some(1) }]), ..Default::default() };
        let mut s = session(d.path(), None, &m);
        let out = s.exec("show system commit").unwrap();
        assert!(out.ends_with("not where the ledger says: `show system alarms`\n"), "{out}");
        s.exec("configure").unwrap();
        s.exec("set system host-name x").unwrap();
        s.exec("rollback 0").unwrap();
        assert_eq!(s.exec("show").unwrap(), "networking {\n    hostName two;\n}\n", "rollback 0: what is active (HEAD)");
        // 0 is what is active for every command, not the ledger's newest.
        assert_eq!(s.exec("run show system rollback 0").unwrap(), "networking {\n    hostName two;\n}\n");
        let e = format!("{:#}", s.exec("run request system software rollback 0").unwrap_err());
        assert!(e.contains("configuration 0 has no flake.lock"), "HEAD's, not the ledger's: {e}");
    }

    #[test]
    fn the_drift_note_counts_as_the_broker_does() {
        // An interrupted change (which numbers nothing) moved the repo: the broker says all is well.
        let d = repo();
        let ids: Vec<String> = Repo::new(d.path()).history(2).unwrap().into_iter().map(|c| c.id).collect();
        let conf = |id: &str| Configuration { id: id.into(), date: "d".into(), who: "u".into(), what: "w".into(), seq: Some(1) };
        let m = Fake { ledger: Some(vec![conf(&ids[1])]), ledger_at: Some(ids[0].clone()), ..Default::default() };
        let mut s = session(d.path(), None, &m);
        assert!(!s.exec("show system commit").unwrap().contains("##"), "no drift");
        // A configuration whose commit is gone says so, not git's error.
        let missing = "0123456789abcdef0123456789abcdef01234567";
        let m = Fake { ledger: Some(vec![conf(&ids[0]), conf(missing)]), ..Default::default() };
        let mut s = session(d.path(), None, &m);
        let e = format!("{:#}", s.exec("show system rollback 1").unwrap_err());
        assert!(e.contains("configuration 1 (commit 0123456789ab) is no longer in the config repo"), "{e}");
    }

    #[test]
    fn the_ledger_and_reverting_one_change() {
        let (d, m) = (repo(), Fake::default());
        let mut s = session(d.path(), None, &m);
        assert_eq!(s.exec("show system ledger").unwrap(), "ledger 20\n");
        assert_eq!(s.exec("show system ledger 5 | wc -l").unwrap(), "1\n");
        assert!(format!("{:#}", s.exec("show system ledger 0").unwrap_err()).contains("1 to 10000"));
        assert_eq!(s.exec("request system revert 7").unwrap(), "reverted 7 for dev\n");
        assert!(format!("{:#}", s.exec("request system revert x").unwrap_err()).contains("not a change number"));
        s.exec("configure").unwrap();
        assert_eq!(s.exec("run request system revert 3").unwrap(), "reverted 3 for dev\n");
    }

    #[test]
    fn turning_ssh_on_shows_the_keys_it_admits_and_needs_one() {
        let (d, m) = (repo(), Fake { answer: true, ..Default::default() });
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "set torq services enabled ssh"]);
        let e = format!("{:#}", s.exec("commit").unwrap_err());
        assert!(e.contains("lets in dev's keys only, and they have none"), "{e}");
        assert!(m.commits.borrow().is_empty() && m.asked.borrow().is_empty(), "nothing asked, nothing committed");
        assert_eq!(m.ssh_asked.borrow()[0], vec![("services.json".to_string(), Some("[\n  \"ssh\"\n]\n".to_string()))], "as the change will have it");

        let m = Fake { answer: true, keys: vec!["    256 SHA256:abc dev@mac (ED25519)  (from /home/dev/.ssh/authorized_keys)".into()], ..Default::default() };
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "set torq services enabled ssh"]);
        assert_eq!(s.exec("commit").unwrap(), "commit complete (change #42)\n");
        let q = m.asked.borrow()[0].clone();
        assert!(q.contains("Keys that will be able to log in as dev:\n    256 SHA256:abc dev@mac (ED25519)  (from /home/dev/.ssh/authorized_keys)\n"), "{q}");
    }

    #[test]
    fn ssh_already_on_asks_no_keys() {
        let (d, m) = (repo(), Fake { answer: true, ..Default::default() });
        sh(d.path(), "printf '[\\n  \"ssh\"\\n]\\n' > services.json && git add -A && git commit -qm ssh");
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "set system host-name three"]);
        s.exec("commit").unwrap();
        assert!(m.ssh_asked.borrow().is_empty());
    }

    #[test]
    fn show_services_says_when_ssh_runs_outside_the_catalog() {
        let (d, m) = (repo(), Fake { ssh_outside: true, ..Default::default() });
        let mut s = session(d.path(), None, &m);
        let out = s.exec("show services").unwrap();
        assert!(out.lines().any(|l| l.starts_with("off ssh")) && out.contains("! SSH is on anyway, outside the catalog"), "{out}");
        let (d, m) = (repo(), Fake::default());
        assert!(!session(d.path(), None, &m).exec("show services").unwrap().contains('!'));
    }

    /// The whole device: what it runs, and what is set outside the tree, as comments a file keeps.
    #[test]
    fn show_configuration_names_the_whole_device() {
        let (d, m) = (repo(), Fake::default());
        let lock = r#"{"nodes":{"root":{"inputs":{"torq":"torq","nixpkgs":["torq","nixpkgs"]}},"torq":{"locked":{"lastModified":1793000000}},"nixpkgs":{"locked":{"lastModified":1790000000}}},"version":7}"#;
        sh(d.path(), &format!("printf '%s' '{lock}' > flake.lock && printf '{{ }}\\n' > machine.nix && printf '{{ }}\\n' > flake.nix && git add -A && git commit -qm device"));
        let mut s = session(d.path(), None, &m);
        let out = s.exec("show configuration").unwrap();
        assert_eq!(
            out,
            "networking {\n    hostName two;\n}\n## software: torq 2026-10-26\n## also set outside this configuration, in Nix: machine.nix\n",
            "the machine's own inputs only (nixpkgs follows torq\x27s)"
        );
        assert!(!s.exec("show configuration system").unwrap().contains("##"), "a part of it is just that part");
        assert!(s.exec("show configuration | display set").unwrap().ends_with("in Nix: machine.nix\n"));
        // The candidate names the software it would install.
        s.exec("request system update").unwrap();
        s.exec("configure").unwrap();
        assert!(s.exec("show").unwrap().contains(&format!("## software: torq {}\n", crate::pkg::ymd(NEWER))));
        // And what it shows reads back: comments are not commands.
        let saved = s.exec("run show configuration | display set").unwrap();
        for line in saved.lines() {
            s.exec(line).unwrap_or_else(|e| panic!("{line}: {e:#}"));
        }
    }

    /// A NixOS setting commits as its nixos/ file, system tier, asked; a person's own packages,
    /// by their NixOS path, stay user tier.
    #[test]
    fn nixos_settings_commit_as_their_files_at_their_tier() {
        let (d, m) = (repo(), Fake { answer: true, ..Default::default() });
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "set virtualisation docker enable true", "set networking firewall allowedTCPPorts [ 80 443 ]"]);
        assert_eq!(s.exec("show | compare").unwrap(), "[edit networking firewall]\n+   allowedTCPPorts [ 80 443 ];\n[edit virtualisation docker]\n+   enable true;\n");
        s.exec("commit check").unwrap();
        let checked: Vec<String> = m.checks.borrow()[0].iter().map(|(p, _)| p.clone()).collect();
        assert_eq!(checked, ["nixos/networking.json", "nixos/virtualisation.json"], "checked as the files it writes");
        assert_eq!(s.exec("commit").unwrap(), "commit complete (change #42)\n");
        let (diff, _, tier, reason, _) = m.commits.borrow()[0].clone();
        assert!(diff.contains("+++ b/nixos/virtualisation.json") && diff.contains("\"enable\": true"), "{diff}");
        assert_eq!(tier, "system");
        assert_eq!(reason, "+ networking firewall allowedTCPPorts [ 80 443 ]; + virtualisation docker enable true;");
        assert!(m.asked.borrow()[0].contains("This changes the system (all users)."));
        // Your own packages, by their NixOS path: your folder, no question.
        let (d, m) = (repo(), Fake::default());
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "set torq packages user dev [ htop ]"]);
        assert_eq!(s.exec("commit").unwrap(), "commit complete (change #42)\n");
        assert_eq!(m.commits.borrow()[0].2, "user");
        assert!(m.commits.borrow()[0].0.contains("+++ b/users/dev/packages.json"));
        assert!(m.asked.borrow().is_empty());
    }

    /// Config files: attached in the tree, edited in the candidate, shown as their raw diff, committed
    /// as the files they are (system tier, asked), kept across sessions; SSH counted however it is set.
    #[test]
    fn config_files_are_part_of_the_configuration() {
        let (d, m) = (repo(), Fake { answer: true, edit_to: Some("{ ... }:\n{\n  services.httpd.enable = true;\n}\n".into()), ..Default::default() });
        let saved = d.path().join("candidate.json");
        let mut s = session(d.path(), Some(saved.clone()), &m);
        run(&mut s, &["configure", "file edit services/httpd.nix"]);
        assert_eq!(m.edited.borrow()[0], ("services/httpd.nix".into(), "{ ... }:\n{\n}\n".into()), "a new file starts as an empty module");
        run(&mut s, &["set services httpd config services/httpd.nix"]);
        assert_eq!(s.exec("show services httpd").unwrap(), "config services/httpd.nix;\n");
        assert_eq!(s.exec("show services | display set").unwrap(), "set services httpd config services/httpd.nix\n", "whole lines, to paste back");
        run(&mut s, &["edit services"]);
        assert_eq!(s.exec("show | display set").unwrap(), "set services httpd config services/httpd.nix\n", "from where you are, too");
        run(&mut s, &["top"]);
        // Another session (the candidate as saved) sees the same.
        let mut s2 = session(d.path(), Some(saved), &m);
        s2.exec("configure").unwrap();
        assert_eq!(s2.exec("file show services/httpd.nix").unwrap(), "{ ... }:\n{\n  services.httpd.enable = true;\n}\n");
        assert_eq!(s2.exec("file list").unwrap(), "services/httpd.nix (new in the candidate)  [attached at: services httpd]\n");
        let compare = s.exec("show | compare").unwrap();
        assert!(compare.starts_with("[edit services httpd]\n+   config services/httpd.nix;\n[edit config files]\n"), "{compare}");
        assert!(compare.contains("+++ b/services/httpd.nix") && compare.contains("+  services.httpd.enable = true;"), "{compare}");
        // Deleting an attached file is refused until it is not attached.
        assert!(s.exec("file delete services/httpd.nix").unwrap_err().to_string().contains("`delete services httpd config` first"));
        assert!(s.exec("file edit ../x.nix").is_err() && s.exec("set services httpd config /etc/x.nix").is_err());
        assert_eq!(s.exec("commit").unwrap(), "commit complete (change #42)\n");
        let (diff, _, tier, _, _) = m.commits.borrow()[0].clone();
        assert!(diff.contains("+++ b/config-files.json") && diff.contains("\"services httpd\": \"services/httpd.nix\""), "{diff}");
        assert!(diff.contains("+++ b/services/httpd.nix"), "{diff}");
        assert_eq!(tier, "system");
        assert!(m.asked.borrow()[0].contains("services.httpd.enable = true"), "the person approves the Nix as written");
        assert_eq!(*m.ssh_calls.borrow(), 1, "a config file may turn SSH on: SSH is evaluated");
        // Only a Nix file changed: SSH is still evaluated.
        let (d, m) = (repo(), Fake { answer: true, edit_to: Some("{ ... }: { }\n".into()), ..Default::default() });
        sh(d.path(), "printf '{ ... }:\\n{\\n}\\n' > system.nix && git add -A && git commit -qm nix");
        let saved = d.path().join("c2.json");
        let mut s = session(d.path(), Some(saved.clone()), &m);
        run(&mut s, &["configure", "file edit system.nix"]);
        // Kept as it is edited: another session sees it, with nothing else done.
        let mut s2 = session(d.path(), Some(saved), &m);
        assert_eq!(run(&mut s2, &["configure", "file list"]).lines().last(), Some("system.nix (edited in the candidate)"));
        run(&mut s, &["commit"]);
        assert_eq!(*m.ssh_calls.borrow(), 1);
        let (diff, _, _, reason, _) = m.commits.borrow()[0].clone();
        assert!(diff.contains("+++ b/system.nix"));
        assert_eq!(reason, "~ config file system.nix", "the ledger says which file");
        // A deletion is kept too, and said as one.
        let saved = d.path().join("c3.json");
        run(&mut session(d.path(), Some(saved.clone()), &m), &["configure", "file delete system.nix"]);
        assert_eq!(run(&mut session(d.path(), Some(saved), &m), &["configure", "file list"]).lines().last(), Some("system.nix (deleted in the candidate)"));
        // Unattaching: `delete <place> config`; `delete <place> all` takes it too.
        let mut d2 = crate::whole::Data::default();
        d2.attached.insert("services httpd".into(), "a.nix".into());
        let x = s.whole();
        x.delete(&mut d2.clone(), &w("services httpd config"), false, "delete", &[]).unwrap();
        let mut d3 = d2.clone();
        x.delete(&mut d3, &w("services httpd"), true, "delete", &[]).unwrap();
        assert!(d3.attached.is_empty());
        assert!(x.delete(&mut crate::whole::Data::default(), &w("services httpd config"), false, "delete", &[]).is_err());
        // Below named entries, `config` is no name (a reserved word), and no place to attach.
        let mut d4 = crate::whole::Data::default();
        assert!(x.set(&mut d4, &w("services httpd virtualHosts config documentRoot /srv")).unwrap_err().to_string().contains("keeps the word"));
        assert!(x.set(&mut d4, &w("services httpd virtualHosts config x.nix")).is_err() && d4 == Default::default(), "not attached there either");
    }

    /// In operational mode, the active configuration's files; editing is configuration mode's.
    #[test]
    fn file_outside_configuration_mode_only_reads() {
        let (d, m) = (repo(), Fake::default());
        sh(d.path(), "printf '{ ... }: { }\\n' > system.nix && git add -A && git commit -qm nix");
        let mut s = session(d.path(), None, &m);
        assert_eq!(s.exec("file show system.nix").unwrap(), "{ ... }: { }\n");
        assert_eq!(s.exec("file list").unwrap(), "system.nix\n");
        assert!(s.exec("file edit system.nix").unwrap_err().to_string().contains("configuration mode"));
        assert!(s.exec("file show flake.nix").is_err(), "the repo's own structure is not a config file");
        assert!(s.exec("show configuration").unwrap().contains("in Nix: system.nix"));
        // Once attached, it is part of the configuration: shown there, not as outside it.
        sh(d.path(), "printf '{ \"services httpd\": \"system.nix\" }\\n' > config-files.json && git add -A && git commit -qm attach");
        let shown = s.exec("show configuration").unwrap();
        assert!(shown.contains("httpd {\n        config system.nix;") && !shown.contains("in Nix"), "{shown}");
        sh(d.path(), "touch flake.nix && git add -A && git commit -qm flake");
        assert!(!s.exec("show configuration").unwrap().contains("in Nix"), "flake.nix is TorqOS's own, not set outside");
        assert_eq!(s.exec("file list").unwrap(), "flake.nix  (TorqOS's own: it builds the machine from the tree)\nsystem.nix  [attached at: services httpd]\n");
        // At the top of the tree, and TorqOS's own flake.nix said as such.
        sh(d.path(), "printf '{ \"\": \"system.nix\" }\\n' > config-files.json && touch flake.nix && git add -A && git commit -qm top");
        assert!(s.exec("show configuration").unwrap().starts_with("config system.nix;\n"));
        assert_eq!(s.exec("file list").unwrap(), "flake.nix  (TorqOS's own: it builds the machine from the tree)\nsystem.nix  [attached at: the top]\n");
        let mut d2 = crate::whole::Data::default();
        s.whole().set(&mut d2, &w("config a.nix")).unwrap();
        assert_eq!(crate::attach::load(crate::attach::render(&d2.attached).as_deref().map(str::as_bytes)).unwrap(), d2.attached, "a top-level attachment reads back");
    }

    /// A change to how the machine starts is said so before anyone approves it; others are not.
    #[test]
    fn boot_changes_are_warned_about() {
        for (line, warned) in [
            ("set fileSystems /boot fsType vfat", true),
            ("set boot loader timeout 3", true),
            ("set virtualisation docker enable true", false),
            ("set environment variables BOOT x", false),
        ] {
            let (d, m) = (repo(), Fake { answer: true, ..Default::default() });
            let mut s = session(d.path(), None, &m);
            run(&mut s, &["configure", line, "commit"]);
            let asked = m.asked.borrow()[0].clone();
            assert_eq!(asked.contains("how the machine starts"), warned, "{line}: {asked}");
        }
        // Editing the attached hardware file is warned about too.
        let (d, m) = (repo(), Fake { answer: true, edit_to: Some("{ ... }: { }\n".into()), ..Default::default() });
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "file edit hardware-configuration.nix", "commit"]);
        assert!(m.asked.borrow()[0].contains("how the machine starts"));
    }

    #[test]
    fn ssh_by_any_path_is_said_as_it_is() {
        // `services openssh`, or NixOS's `sshd` alias for it: on, outside the catalog — the question
        // says the catalog's rules do not apply, and that passwords are allowed.
        for line in ["set services openssh enable true", "set services sshd enable true"] {
            let (d, m) = (repo(), Fake { answer: true, ..Default::default() });
            let mut s = session(d.path(), None, &m);
            run(&mut s, &["configure", line]);
            s.exec("commit").unwrap_or_else(|e| panic!("{line}: {e:#}"));
            let q = m.asked.borrow()[0].clone();
            assert!(q.contains("outside TorqOS's catalog: its rules (keys only, only the owner) do not apply") && q.contains("! Password logins are allowed"), "{line}: {q}");
        }
        // What the evaluation says counts, not what the files look like: already on, nothing to say.
        let on = Ssh { on: true, catalog: false, passwords: true };
        let (d, m) = (repo(), Fake { answer: true, ssh_state: Some((on, on)), ..Default::default() });
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "set services openssh ports [ 2222 ]", "commit"]);
        assert!(!m.asked.borrow()[0].contains("SSH is turned on"), "{}", m.asked.borrow()[0]);
        // Not a tree file that could turn it on: not even asked about.
        let (d, m) = (repo(), Fake { answer: true, ssh_state: Some((Ssh::default(), on)), ..Default::default() });
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "set networking hostName x", "commit"]);
        assert!(!m.asked.borrow()[0].contains("SSH"), "only services.json and nixos/ files can");
    }

    #[test]
    fn fenced_and_reserved_in_the_session() {
        let (d, m) = (repo(), Fake::default());
        let mut s = session(d.path(), None, &m);
        s.exec("configure").unwrap();
        for (line, why) in [
            ("set torq broker rebuildCommand x", "not settable in the configuration tree"),
            ("set environment etc torq/broker.json text x", "not settable in the configuration tree"),
            ("set services httpd virtualHosts _x forceSSL true", "the tree holds data only"),
        ] {
            assert!(format!("{:#}", s.exec(line).unwrap_err()).contains(why), "{line}");
        }
        assert_eq!(s.exec("show | compare").unwrap(), "", "nothing changed");
    }

    #[test]
    fn a_raw_diff_between_commits_is_every_file_as_committed() {
        let (d, m) = (repo(), Fake::default());
        sh(d.path(), "printf '{\"nodes\":{}}' > flake.lock && printf '{ }\\n' > flake.nix && git add -A && git commit -qm software");
        let mut s = session(d.path(), None, &m);
        let raw = s.exec("show system rollback 0 compare 1 | display diff").unwrap();
        assert!(raw.contains("+++ b/flake.lock\n") && raw.contains("+{\"nodes\":{}}") && raw.contains("+++ b/flake.nix\n"), "{raw}");
        assert!(!raw.contains("host.json"), "only what changed: {raw}");
        assert_eq!(s.exec("show system rollback 0 compare 1").unwrap(), "", "the tree did not change");
    }

    #[test]
    fn display_detail_names_each_users_files() {
        let (d, m) = (repo(), Fake::default());
        let mut s = session(d.path(), None, &m);
        // By its NixOS path, and by its old name: the same file.
        run(&mut s, &["configure", "set torq packages user dev htop", "set users dev cli style cisco"]);
        let out = s.exec("show torq | display detail").unwrap();
        assert!(out.contains("## users/dev/cli.json: torq cli userStyles dev\n") && out.contains("## users/dev/packages.json: torq packages user dev\n"), "{out}");
        assert!(!out.contains("system/"), "only what is shown: {out}");
        let net = s.exec("show networking | display detail").unwrap();
        assert!(net.contains("## system/host.json: networking hostName\n") && !net.contains("users/"), "{net}");
    }

    #[test]
    fn display_detail_and_diff_show_what_is_behind() {
        let (d, m) = (repo(), Fake { answer: true, ..Default::default() });
        let mut s = session(d.path(), None, &m);
        let head = s.repo.head().unwrap();
        let first = s.repo.history(2).unwrap()[1].id.clone();
        assert_eq!(
            s.exec("show configuration | display detail").unwrap(),
            format!("networking {{\n    hostName two;\n}}\n## commit {head}\n## system/host.json: networking hostName\n"),
            "only the settings set (no time-zone here)"
        );
        assert!(s.exec("show configuration users | display detail").unwrap().ends_with(&format!("## commit {head}\n")), "no files below users");
        let commits = s.exec("show system commit | display detail").unwrap();
        assert!(commits.contains(&format!("\n    commit {head}\n")) && commits.ends_with(&format!("    commit {first}\n")), "{commits}");
        assert!(s.exec("show system rollback 1 | display detail").unwrap().contains(&format!("hostName one;\n}}\n## commit {first} (rollback 1)\n")));
        let raw = s.exec("show system rollback 0 compare 1 | display diff").unwrap();
        assert_eq!(raw, "--- a/system/host.json\n+++ b/system/host.json\n@@ -1 +1 @@\n-{ \"host-name\": \"one\" }\n+{ \"host-name\": \"two\" }\n", "the files as committed");
        assert_eq!(s.exec("show system alarms | display detail").unwrap(), "all well\n  running system /nix/store/x\n");
        assert_eq!(s.exec("show system alarms").unwrap(), "all well\n");
        run(&mut s, &["configure", "set system host-name three"]);
        let raw = s.exec("show | compare | display diff").unwrap();
        assert!(raw.contains("+++ b/system/host.json") && raw.contains("+  \"host-name\": \"three\""), "{raw}");
        assert_eq!(s.exec("show | compare | display diff").unwrap(), {
            // What `show | compare | display diff` shows is exactly what commit proposes.
            s.exec("commit").unwrap();
            m.commits.borrow()[0].0.clone()
        });
        run(&mut s, &["set system host-name four"]);
        let detail = s.exec("show networking | display detail").unwrap();
        assert!(detail.contains("hostName four;") && detail.contains(&format!("## the candidate, from commit {head}\n")), "{detail}");
    }

    #[cfg(target_os = "linux")] // the filters' sandbox
    #[test]
    fn filters_work_on_what_a_show_prints() {
        let (d, m) = (repo(), Fake::default());
        let mut s = session(d.path(), None, &m);
        assert_eq!(s.exec("show configuration | grep host").unwrap(), "    hostName two;\n");
        assert_eq!(s.exec("show configuration | grep -v host | wc -l").unwrap(), "2\n");
        assert_eq!(s.exec("show configuration | grep nothing").unwrap(), "", "nothing matched: no error");
        assert_eq!(s.exec("show system commit | wc -l").unwrap(), "2\n");
        assert_eq!(s.exec("show configuration | display set | grep -o 'hostName [a-z]*'").unwrap(), "hostName two\n");
        run(&mut s, &["configure", "set system host-name three", "set system time-zone UTC"]);
        assert_eq!(s.exec("show | compare | grep '^+'").unwrap(), "+   hostName three;\n+   timeZone UTC;\n");
        assert_eq!(s.exec("show time | grep -o 'timeZone [A-Z]*' | tr A-Z a-z").unwrap(), "timezone utc\n");
        assert_eq!(s.exec("run show configuration | grep host").unwrap(), "    hostName two;\n", "run: the active one, filtered");
        s.exec("switch cli").unwrap();
        assert_eq!(s.exec("do show running-config | grep -c host").unwrap(), "1\n");
        assert!(format!("{:#}", s.exec("show configuration diff | grep -Q x").unwrap_err()).starts_with("| grep: "));
        assert!(s.exec("system host-name four | wc -l").is_err());
        assert_eq!(s.exec("show configuration | grep host").unwrap(), "    hostName three;\n", "the refused line changed nothing");
    }

    #[test]
    fn a_user_change_commits_without_asking() {
        let (d, m) = (repo(), Fake::default());
        let mut s = session(d.path(), None, &m);
        let out = run(&mut s, &["configure", "set users dev cli style cisco", "show | compare"]);
        assert!(out.contains("[edit torq cli userStyles]\n+   dev cisco;\n"), "{out}");
        let out = s.exec("commit").unwrap();
        assert_eq!(out, "commit complete (change #42)\n");
        let c = m.commits.borrow();
        let (diff, base, tier, reason, confirm) = &c[0];
        assert!(diff.contains("+++ b/users/dev/cli.json") && diff.contains("+  \"style\": \"cisco\""), "{diff}");
        assert_eq!((base.as_str(), tier.as_str(), reason.as_str(), *confirm), (s.repo.head().unwrap().as_str(), "user", "+ torq cli userStyles dev cisco;", None));
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
        assert!(m.asked.borrow()[0].contains("-   hostName two;\n+   hostName three;"), "the question shows the change");
        // The candidate is kept for another try.
        assert!(s.exec("show | compare").unwrap().contains("hostName three"));
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
        assert_eq!(s.exec("show | compare").unwrap(), "[edit networking]\n-   hostName two;\n+   hostName one;\n");
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
        assert!(run(&mut s, &["configure", "show | compare"]).contains("+   dev [ htop ];"));
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
        assert_eq!(m.commits.borrow()[0].3, "- networking hostName two; + networking hostName three;");
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
        assert!(s.exec("show | compare").unwrap().contains("dev cisco"));
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
        run(&mut s, &["configure", "edit services httpd virtualHosts example.com", "set forceSSL true"]);
        assert_eq!(s.edit_path(), w("services httpd virtualHosts example.com"));
        assert_eq!(s.exec("show").unwrap(), "forceSSL true;\n");
        run(&mut s, &["up"]);
        assert_eq!(s.edit_path(), w("services httpd"), "out of an entry and its set: one step");
        assert!(format!("{:#}", s.exec("edit enable").unwrap_err()).contains("not somewhere to go"));
        run(&mut s, &["top", "edit system", "set time-zone UTC"]);
        assert!(s.exec("show | compare").unwrap().contains("[edit time]\n+   timeZone UTC;"), "an old name, from where you are");
        run(&mut s, &["top"]);
        assert!(format!("{:#}", s.exec("up").unwrap_err()).contains("already at the top"));
        // `exit` goes up a level, and out of configuration mode at the top.
        run(&mut s, &["edit virtualisation", "exit"]);
        assert_eq!((s.mode, s.edit_path().is_empty()), (Mode::Configuration, true));
        run(&mut s, &["edit virtualisation", "exit configuration-mode"]);
        assert_eq!(s.mode, Mode::Operational);
    }

    /// Deleting a whole container or list entry takes `all`, so a slip cannot take everything below.
    #[test]
    fn delete_of_a_container_needs_all() {
        let (d, m) = (repo(), Fake::default());
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "set torq cli userStyles dev cisco", "set torq packages system [ htop vim ]"]);
        run(&mut s, &["set virtualisation docker enable true", "set virtualisation docker rootless enable true"]);
        for (bad, why) in [
            ("delete virtualisation docker", "say `delete virtualisation docker all`"),
            ("delete virtualisation", "say `delete virtualisation all`"),
            ("delete virtualisation docker enable all", "`all` is for a whole place or list"),
            ("delete networking hostName all", "`all` is for a whole place or list"),
            // A whole list of values too: Enter pressed before the value (review of f705cc8).
            ("delete torq packages system", "say `delete torq packages system all`"),
        ] {
            assert!(format!("{:#}", s.exec(bad).unwrap_err()).contains(why), "{bad}");
        }
        // The hint is relative to where you are.
        run(&mut s, &["edit virtualisation docker"]);
        assert!(format!("{:#}", s.exec("delete rootless").unwrap_err()).contains("say `delete rootless all`"));
        run(&mut s, &["delete rootless all", "top", "delete torq packages system htop", "delete torq packages system all", "delete torq cli userStyles dev"]);
        let c = s.exec("show | compare").unwrap();
        assert_eq!(c, "[edit virtualisation docker]\n+   enable true;\n");
    }

    /// Out of a list entry is out of the list; a bare list is nowhere to stand; a command word cannot
    /// name an entry (review of f705cc8).
    #[test]
    fn up_leaves_a_list_entry_whole() {
        let (d, m) = (repo(), Fake::default());
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "edit services httpd virtualHosts example.com", "up"]);
        assert_eq!(s.edit_path(), w("services httpd"));
        run(&mut s, &["up", "up"]);
        assert!(s.edit_path().is_empty());
        // A command word as an entry's name would be ambiguous (`delete … all`).
        assert!(format!("{:#}", s.exec("set services httpd virtualHosts all forceSSL true").unwrap_err()).contains("keeps the word for a command"));
        s.schema.nodes.get_mut("users").unwrap().values.as_mut().unwrap().insert("all".into(), String::new());
        assert!(format!("{:#}", s.exec("set users all packages vim").unwrap_err()).contains("keeps the word for a command"), "the old names too");
    }

    /// The same session in Cisco style: bare paths set, a container path is a submode, `no`
    /// deletes, `show configuration diff` compares, `end` leaves.
    #[test]
    fn a_session_in_cisco_style() {
        let (d, m) = (repo(), Fake { answer: true, ..Fake::default() });
        let mut s = Session::new(Schema::parse(include_str!("schema-fixture.json")).unwrap(), nixos_tree(), Repo::new(d.path()), None, &m, "dev", Style::Cisco);
        run(&mut s, &["config", "networking hostName three", "services httpd virtualHosts example.com", "forceSSL true"]);
        assert_eq!(s.edit_path(), w("services httpd virtualHosts example.com"), "a bare path to a place is a submode");
        run(&mut s, &["exit"]);
        assert_eq!(s.edit_path(), w("services httpd"), "out of a named entry is out of its set (review of f705cc8)");
        run(&mut s, &["top"]);
        assert_eq!(s.mode, Mode::Configuration);
        assert_eq!(
            s.exec("show configuration diff").unwrap(),
            "[edit networking]\n-   hostName two;\n+   hostName three;\n[edit services httpd virtualHosts example.com]\n+   forceSSL true;\n"
        );
        run(&mut s, &["no services httpd virtualHosts example.com forceSSL", "do show system alarms"]);
        assert!(format!("{:#}", s.exec("set system host-name x").unwrap_err()).contains("in Cisco style"));
        assert_eq!(s.exec("commit").unwrap(), "commit complete (change #42)\n");
        assert_eq!(s.exec("end").unwrap(), "");
        assert_eq!(s.mode, Mode::Operational);
        assert_eq!(s.exec("show running-config").unwrap(), "networking {\n    hostName two;\n}\n", "the fake commits nothing");
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
        assert!(s.exec("show | compare").unwrap().contains("+   timeZone UTC;"));
    }

    #[test]
    fn earlier_configurations_and_their_differences() {
        let (d, m) = (repo(), Fake::default());
        let mut s = session(d.path(), None, &m);
        assert_eq!(s.exec("show system rollback 1").unwrap(), "networking {\n    hostName one;\n}\n");
        assert_eq!(s.exec("show system rollback 0 compare 1").unwrap(), "[edit networking]\n-   hostName one;\n+   hostName two;\n");
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
        let e = format!("{:#}", s.exec("request system software rollback 0").unwrap_err());
        assert!(e.contains("configuration 0 has no flake.lock"), "{e}");
        let out = s.exec("request system update").unwrap();
        assert!(out.contains("+   torq ") && !out.contains("-   torq") && !out.contains("downgrade"), "a repo without a flake.lock: torq comes, nothing goes, no downgrade claimed: {out}");
        assert_eq!(s.exec("config").unwrap().lines().next(), Some("Entering configuration mode"));
    }

    #[test]
    fn operational_mode_shows_the_active_configuration() {
        let (d, m) = (repo(), Fake::default());
        let mut s = session(d.path(), None, &m);
        assert_eq!(s.exec("show configuration").unwrap(), "networking {\n    hostName two;\n}\n");
        assert_eq!(s.exec("show configuration | display set").unwrap(), "set networking hostName two\n");
        assert!(s.exec("show system commit").unwrap().starts_with("0   "));
        assert!(format!("{:#}", s.exec("set system host-name x").unwrap_err()).contains("unknown command: set"));
        run(&mut s, &["configure", "set system host-name three"]);
        assert_eq!(s.exec("run show configuration").unwrap(), "networking {\n    hostName two;\n}\n", "run: the active one");
        assert_eq!(s.mode, Mode::Configuration);
        s.exec("exit").unwrap();
        s.exec("exit").unwrap();
        assert!(s.done);
    }
}
