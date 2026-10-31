//! `torqcli` (ADR-016): the shell itself — the real `Machine` behind a session (Nix evaluation and
//! the broker, the same way `torq pkg` reaches them) and the entry point. The commands are in
//! session.rs, the tree in tree.rs, the candidate in candidate.rs.

use crate::candidate::{Candidate, Repo};
use crate::session::{Machine, Mode, Session};
use crate::tree::Schema;
use crate::{broker, check_repo, config_name, copy_tree, evaluate, history, me, run, scratch_dir, Cli, Cmd, BROKER, REPO};
use anyhow::{bail, Context, Result};
use std::fs;
use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;

/// The schema of the running system (modules/schema.nix).
const SCHEMA: &str = "/etc/torq/schema.json";

struct Real<'a> {
    cli: &'a Cli,
}

/// A broker outcome as a result, its message cleaned for the terminal.
fn done(o: run::Outcome, what: &str) -> Result<serde_json::Value> {
    match o {
        run::Outcome::Done(v) => Ok(v),
        run::Outcome::Busy => bail!("{what}: another change is being made right now; try again in a moment"),
        run::Outcome::Refused(m) => bail!("{what} refused: {}", history::one_line(&m, 300)),
        run::Outcome::Broken(m) => bail!("{what}: {m}"),
    }
}

impl Machine for Real<'_> {
    fn check(&self, files: &[(String, Option<String>)]) -> Result<()> {
        let scratch = scratch_dir()?;
        let copy = scratch.path().join("config");
        copy_tree(&self.cli.repo, &copy)?;
        for (path, content) in files {
            let f = copy.join(path);
            match content {
                Some(c) => {
                    fs::create_dir_all(f.parent().context("a tree file has a folder")?)?;
                    fs::write(&f, c)?;
                }
                None => {
                    let _ = fs::remove_file(&f);
                }
            }
        }
        let log = run::start_log()?;
        let _p = run::Progress::start("Checking");
        let host = config_name(&copy)?;
        evaluate(&copy, &host, &log)
    }

    fn commit(&self, diff: &str, base: &str, tier: &str, reason: &str, confirm: Option<u32>) -> Result<u64> {
        let scratch = scratch_dir()?;
        let file = scratch.path().join("change.diff");
        fs::write(&file, diff)?;
        let log = run::start_log()?;
        let file = file.to_string_lossy();
        let proposed = done(
            run::broker_json(broker(self.cli, &["propose", "--diff", &file, "--base", base, "--reason", reason, "--actor", "user"]), &log)?,
            "commit",
        )?;
        let seq = proposed["seq"].as_u64().context("the broker did not say which proposal it made")?;
        // What was shown (and, for the system, asked) must be what the broker applies.
        if proposed["tier"].as_str() != Some(tier) {
            bail!(
                "not committed: the broker treats this as a {} change, not a {tier} one (recorded as proposal {seq}, not applied)",
                history::one_line(proposed["tier"].as_str().unwrap_or("?"), 20)
            );
        }
        let (seq_s, minutes) = (seq.to_string(), confirm.map(|m| m.to_string()));
        let mut args = vec!["apply", &seq_s, "--approved-by"];
        let who = me()?;
        args.push(&who);
        if let Some(m) = &minutes {
            args.extend(["--confirm", m]);
        }
        let applied = {
            let _p = if crate::asks_for_password(self.cli) { None } else { Some(run::Progress::start("Committing")) };
            done(run::broker_json(broker(self.cli, &args), &log)?, "commit")?
        };
        applied["seq"].as_u64().context("the broker did not say which change it applied")
    }

    fn pending_confirm(&self) -> Result<Option<(u64, String)>> {
        let log = run::start_log()?;
        let v = match run::broker_status(broker(self.cli, &["status"]), &log)? {
            Ok(v) => v,
            Err(why) => bail!("cannot check for a commit confirmed waiting: {why}"),
        };
        Ok(v["pending_confirm"]["applied"].as_u64().map(|seq| (seq, v["pending_confirm"]["deadline"].as_str().unwrap_or("?").to_string())))
    }

    fn confirm(&self) -> Result<u64> {
        let log = run::start_log()?;
        let v = done(run::broker_json(broker(self.cli, &["confirm", "--approved-by", &me()?]), &log)?, "confirm")?;
        v["seq"].as_u64().context("the broker did not say what it recorded")
    }

    fn ask(&self, question: &str) -> bool {
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            return false;
        }
        print!("{question}[y/N] ");
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        std::io::stdin().lock().read_line(&mut answer).is_ok() && matches!(answer.trim(), "y" | "yes")
    }
}

/// The shell: `torqcli` (interactive) or `torqcli -c "<command>; <command>…"` (a private candidate,
/// stops at the first error). Returns the exit code.
pub fn main(args: &[String]) -> i32 {
    match run_shell(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {}", history::one_line(&format!("{e:#}"), 300));
            1
        }
    }
}

fn run_shell(args: &[String]) -> Result<i32> {
    let env = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_string());
    let cli = Cli {
        repo: PathBuf::from(env("TORQ_CONFIG_REPO", REPO)),
        broker: env("TORQ_BROKER", BROKER),
        diff: false,
        cmd: Cmd::Status { repair: false },
    };
    check_repo(&cli)?;
    let schema = Schema::parse(&fs::read_to_string(SCHEMA).with_context(|| format!("reading {SCHEMA} (is this TorqOS?)"))?)?;
    let machine = Real { cli: &cli };
    let script = match args {
        [] => None,
        [flag, s] if flag == "-c" => Some(s.clone()),
        _ => bail!("usage: torqcli [-c \"<command>; <command>…\"]"),
    };
    let saved = if script.is_some() { None } else { Some(Candidate::default_path()?) };
    let mut s = Session::new(schema, Repo::new(&cli.repo), saved, &machine);
    match script {
        Some(script) => {
            for line in script.split([';', '\n']).map(str::trim).filter(|l| !l.is_empty()) {
                match s.exec(line) {
                    Ok(out) => print_clean(&out),
                    Err(e) => {
                        eprintln!("error: {}: {}", history::one_line(line, 80), history::one_line(&format!("{e:#}"), 300));
                        return Ok(1);
                    }
                }
            }
            Ok(0)
        }
        None => interactive(&mut s),
    }
}

/// Output line by line, each cleaned of anything that could fool a terminal (the configuration is
/// checked on load, but the ledger's reasons and messages are not ours).
fn print_clean(out: &str) {
    for line in out.lines() {
        println!("{}", history::clean(line));
    }
}

/// The prompt: `user@host> ` in operational mode, `[edit]` then `user@host# ` in configuration mode.
fn prompt(mode: Mode, user: &str, host: &str) -> (Option<&'static str>, String) {
    match mode {
        Mode::Operational => (None, format!("{user}@{host}> ")),
        Mode::Configuration => (Some("\n[edit]"), format!("{user}@{host}# ")),
    }
}

/// The interactive shell: with a line editor (history, Tab, `?`) on a terminal, one plain line at a
/// time otherwise (piped input).
fn interactive(s: &mut Session) -> Result<i32> {
    let host = fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_default().trim().to_string();
    let user = me()?;
    if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        return editor::run(s, &user, &host);
    }
    let stdin = std::io::stdin();
    loop {
        let (above, p) = prompt(s.mode, &user, &host);
        if let Some(a) = above {
            println!("{a}");
        }
        print!("{p}");
        std::io::stdout().flush()?;
        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            println!();
            return Ok(0);
        }
        run_line(s, line.trim());
        if s.done {
            return Ok(0);
        }
    }
}

fn run_line(s: &mut Session, line: &str) {
    match s.exec(line) {
        Ok(out) => print_clean(&out),
        Err(e) => println!("error: {}", history::one_line(&format!("{e:#}"), 300)),
    }
}

/// The line editor: rustyline, with Tab and `?` answered by complete.rs.
mod editor {
    use super::{prompt, run_line};
    use crate::complete;
    use crate::session::{words, Mode, Session};
    use crate::tree::Schema;
    use anyhow::Result;
    use rustyline::completion::{Completer, Pair};
    use rustyline::error::ReadlineError;
    use rustyline::highlight::Highlighter;
    use rustyline::hint::Hinter;
    use rustyline::validate::Validator;
    use rustyline::{Cmd, ConditionalEventHandler, Context, Event, EventContext, EventHandler, KeyEvent, RepeatCount};
    use std::sync::{Arc, Mutex};

    /// What Tab and `?` need: the schema, and the mode the session is in now.
    #[derive(Clone)]
    struct Help {
        schema: Arc<Schema>,
        mode: Arc<Mutex<Mode>>,
    }

    /// The complete words before the cursor, and the one being typed. None inside a quoted string.
    fn at_cursor(before: &str) -> Option<(Vec<String>, String)> {
        if before.matches('"').count() % 2 == 1 {
            return None;
        }
        let partial = if before.ends_with(char::is_whitespace) { "" } else { before.rsplit(char::is_whitespace).next().unwrap_or("") };
        let done = &before[..before.len() - partial.len()];
        Some((words(done).ok()?, partial.to_string()))
    }

    impl Help {
        fn choices(&self, before: &str) -> Option<(Vec<complete::Choice>, String)> {
            let (w, partial) = at_cursor(before)?;
            let mode = *self.mode.lock().unwrap();
            Some((complete::choices(&self.schema, mode, &w, &partial), partial))
        }
    }

    impl Completer for Help {
        type Candidate = Pair;
        fn complete(&self, line: &str, pos: usize, _: &Context<'_>) -> rustyline::Result<(usize, Vec<Pair>)> {
            let Some((choices, partial)) = self.choices(&line[..pos]) else { return Ok((pos, vec![])) };
            let pairs = complete::tab(&choices).into_iter().map(|(display, replacement)| Pair { display, replacement }).collect();
            Ok((pos - partial.len(), pairs))
        }
    }
    impl Hinter for Help {
        type Hint = String;
    }
    impl Highlighter for Help {}
    impl Validator for Help {}
    impl rustyline::Helper for Help {}

    /// `?`: list what may come next, Junos style, and redraw the line as it was.
    struct Question(Help);
    impl ConditionalEventHandler for Question {
        fn handle(&self, _: &Event, _: RepeatCount, _: bool, ctx: &EventContext) -> Option<Cmd> {
            let (choices, _) = self.0.choices(&ctx.line()[..ctx.pos()])?; // in quotes: a plain '?'
            let text = complete::render(&choices);
            print!("?\r\n{}", text.replace('\n', "\r\n"));
            Some(Cmd::Repaint)
        }
    }

    #[cfg(test)]
    #[test]
    fn the_word_at_the_cursor() {
        let v = |s: &str| s.split_whitespace().map(String::from).collect::<Vec<_>>();
        assert_eq!(at_cursor("set sys"), Some((v("set"), "sys".into())));
        assert_eq!(at_cursor("set system "), Some((v("set system"), "".into())));
        assert_eq!(at_cursor(""), Some((vec![], "".into())));
        assert_eq!(at_cursor("commit comment \"why"), None, "inside quotes: `?` is a character");
        assert_eq!(at_cursor("commit comment \"why\" "), Some((vec!["commit".into(), "comment".into(), "why".into()], "".into())));
    }

    pub fn run(s: &mut Session, user: &str, host: &str) -> Result<i32> {
        let help = Help { schema: Arc::new(s.schema.clone()), mode: Arc::new(Mutex::new(s.mode)) };
        let config = rustyline::Config::builder().completion_type(rustyline::CompletionType::List).auto_add_history(true).build();
        let mut ed: rustyline::Editor<Help, rustyline::history::DefaultHistory> = rustyline::Editor::with_config(config)?;
        ed.set_helper(Some(help.clone()));
        ed.bind_sequence(KeyEvent::from('?'), EventHandler::Conditional(Box::new(Question(help.clone()))));
        loop {
            *help.mode.lock().unwrap() = s.mode;
            let (above, p) = prompt(s.mode, user, host);
            if let Some(a) = above {
                println!("{a}");
            }
            match ed.readline(&p) {
                Ok(line) => run_line(s, line.trim()),
                // Ctrl-C: drop the line, as a shell does.
                Err(ReadlineError::Interrupted) => continue,
                Err(ReadlineError::Eof) => {
                    println!();
                    return Ok(0);
                }
                Err(e) => return Err(e.into()),
            }
            if s.done {
                return Ok(0);
            }
        }
    }
}
