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

/// One line at a time. (Line editing, `?` and Tab come next.)
fn interactive(s: &mut Session) -> Result<i32> {
    let host = fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_default().trim().to_string();
    let user = me()?;
    let stdin = std::io::stdin();
    loop {
        match s.mode {
            Mode::Operational => print!("{user}@{host}> "),
            Mode::Configuration => print!("\n[edit]\n{user}@{host}# "),
        }
        std::io::stdout().flush()?;
        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            println!();
            return Ok(0);
        }
        match s.exec(line.trim()) {
            Ok(out) => print_clean(&out),
            Err(e) => println!("error: {}", history::one_line(&format!("{e:#}"), 300)),
        }
        if s.done {
            return Ok(0);
        }
    }
}
