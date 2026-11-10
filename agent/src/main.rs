//! `torq-doctor [--propose] "<question>"` (ADR-042): TorqOS's agent, run as the person's agent account, `torq-agent-<person>` (by `torq
//! doctor` and `torq agent propose`, through `torq agent run`). It reads — the journal, the hardware,
//! the configuration, the ledger and alarms, NixOS's option docs — and explains; it starts its session
//! at the broker and reports there each read of untrusted text. In propose mode it may also propose a
//! change, through torqcli as the agent, which stops at the proposal: it never applies one. With
//! `--propose --task`, one task of the person's agent service (ADR-051 3), its events on standard output.

// No unsafe code and no dead code, ever (ADR-020); in a test build `deny`, as torqcli's (cli/src/main.rs).
#![forbid(unsafe_code)]
#![cfg_attr(not(test), forbid(dead_code))]
#![cfg_attr(test, deny(dead_code))]

mod clean;
mod doctor;
mod model;
mod session;
mod tools;

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::io::{BufRead, Read, Write};
use std::process::Command;
use torq_wire::agent::{Event, Task, MAX_LINE};

const SETTINGS: &str = "/etc/torq/agent.json";

/// The machine: a program run with nothing of this process's environment but a fixed PATH, its
/// home and plain output, its input closed, its output and its time bounded (tools::bounded).
struct Machine {
    home: PathBuf,
}

impl Machine {
    fn command(&self, program: &Path, args: &[String]) -> Command {
        let mut cmd = Command::new(program);
        cmd.args(args)
            .env_clear()
            .env("PATH", "/run/current-system/sw/bin")
            .env("HOME", &self.home)
            .env("NO_COLOR", "1")
            .env("TERM", "dumb")
            .env("LANG", "C.UTF-8");
        cmd
    }
}

impl tools::Host for Machine {
    fn run(&self, program: &Path, args: &[String]) -> Result<String> {
        tools::bounded(self.command(program, args), tools::MAX_PROGRAM_BYTES, tools::PROGRAM_TIMEOUT).with_context(|| format!("running {}", program.display()))
    }

    fn propose(&self, script: &str, session: &str) -> Result<String> {
        let mut cmd = self.command(Path::new(tools::TORQCLI), &["-c".into(), script.into()]);
        cmd.env("TORQ_AGENT_SESSION", session);
        tools::bounded(cmd, tools::MAX_PROGRAM_BYTES, tools::PROPOSE_TIMEOUT).context("running torqcli to propose")
    }

    fn session(&self, args: &[&str]) -> Result<String> {
        let mut a: Vec<String> = vec!["agent".into(), "session".into()];
        a.extend(args.iter().map(|s| s.to_string()));
        let out = self.command(Path::new(tools::TORQCLI), &a).stdin(std::process::Stdio::null()).output().context("running torqcli agent session")?;
        if !out.status.success() {
            bail!("{}", clean::clean(String::from_utf8_lossy(&out.stderr).trim()));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let task = matches!(args.as_slice(), [p, t] if p == "--propose" && t == "--task");
    match if task { run_task() } else { run(&args) } {
        Ok(()) => {}
        Err(e) => {
            let why = clean::clean(&format!("{e:#}"));
            eprintln!("torq-doctor: {why}");
            if task {
                let _ = emit(&mut std::io::stdout(), Event::Error { text: why });
            }
            std::process::exit(1);
        }
    }
}

/// `[--propose] [--] <question>`: whether in propose mode, and the question.
fn parse(args: &[String]) -> Result<(bool, String)> {
    let usage = "usage: torq-doctor [--propose] [--] \"<question>\" | torq-doctor --propose --task (as a person: torq doctor \"<question>\", torq agent propose \"<ask>\", torq ask \"<question>\")";
    let (propose, rest) = match args {
        [p, rest @ ..] if p == "--propose" => (true, rest),
        rest => (false, rest),
    };
    let (dashes, rest) = match rest {
        [d, rest @ ..] if d == "--" => (true, rest),
        rest => (false, rest),
    };
    let question = rest.join(" ");
    if question.trim().is_empty() || (!dashes && rest.first().is_some_and(|a| a.starts_with('-'))) {
        bail!("{usage}");
    }
    Ok((propose, question))
}

fn run(args: &[String]) -> Result<()> {
    let (propose, question) = parse(args)?;
    let home = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?);
    let cfg = doctor::Config::load(Path::new(SETTINGS), &home)?;
    let machine = Machine { home };
    doctor::ask(&cfg, &machine, &question, propose, &mut std::io::stdout(), &mut std::io::stderr())?;
    Ok(())
}

/// One event, one JSON line, flushed at once: the agent service relays it as it comes.
fn emit(out: &mut dyn Write, e: Event) -> Result<()> {
    writeln!(out, "{}", serde_json::to_string(&e)?)?;
    out.flush()?;
    Ok(())
}

/// `--propose --task` (ADR-051 3, the agent service's way): one task, its `Task` (the question, the
/// conversation's recent turns, what the person attached) a JSON line on standard input; each step an
/// event on standard output (`tainted`, `called`, `proposed`), the answer last (`say`, headed as a
/// tainted session's answer is), or `error`. What happens on the way goes to standard error, as ever.
fn run_task() -> Result<()> {
    let mut line = String::new();
    std::io::stdin().lock().take(MAX_LINE as u64 + 1).read_line(&mut line).context("reading the task")?;
    if line.len() > MAX_LINE {
        bail!("the task is longer than {MAX_LINE} bytes");
    }
    let task: Task = serde_json::from_str(&line).context("reading the task")?;
    if task.question.trim().is_empty() {
        bail!("the task has no question");
    }
    let home = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?);
    let cfg = doctor::Config::load(Path::new(SETTINGS), &home)?;
    let machine = Machine { home };
    let mut answer = Vec::new();
    let stdout = std::io::stdout();
    doctor::ask_task(&cfg, &machine, &task, true, &mut answer, &mut std::io::stderr(), &mut |e| emit(&mut stdout.lock(), e))?;
    emit(&mut stdout.lock(), Event::Say { text: String::from_utf8_lossy(&answer).into_owned() })
}

#[cfg(test)]
mod tests {
    #[test]
    fn what_it_is_asked() {
        let a = |s: &[&str]| super::parse(&s.iter().map(|x| x.to_string()).collect::<Vec<_>>());
        assert_eq!(a(&["why", "slow?"]).unwrap(), (false, "why slow?".into()));
        assert_eq!(a(&["--propose", "--", "install", "htop"]).unwrap(), (true, "install htop".into()));
        assert_eq!(a(&["--", "-x"]).unwrap(), (false, "-x".into()));
        assert!(a(&["-x"]).is_err() && a(&[]).is_err() && a(&["--propose"]).is_err());
    }
}
