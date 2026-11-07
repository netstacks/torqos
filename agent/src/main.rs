//! `torq-doctor "<question>"` (ADR-042): TorqOS's agent in doctor mode, run as `torq-agent` (by
//! `torq doctor`, through `torq agent run`). It reads — the journal, the hardware, the configuration,
//! the ledger and alarms, NixOS's option docs — and explains. It proposes nothing and changes
//! nothing: it has no tool that writes, and no code that reaches the broker but torqcli's `show`.

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
use std::process::Command;

const SETTINGS: &str = "/etc/torq/agent.json";

/// The machine: a program run with nothing of this process's environment but a fixed PATH, its
/// home and plain output, its input closed, its output and its time bounded (tools::bounded).
struct Machine {
    home: PathBuf,
}

impl tools::Host for Machine {
    fn run(&self, program: &Path, args: &[String]) -> Result<String> {
        let mut cmd = Command::new(program);
        cmd.args(args)
            .env_clear()
            .env("PATH", "/run/current-system/sw/bin")
            .env("HOME", &self.home)
            .env("NO_COLOR", "1")
            .env("TERM", "dumb")
            .env("LANG", "C.UTF-8");
        tools::bounded(cmd, tools::MAX_PROGRAM_BYTES, tools::PROGRAM_TIMEOUT).with_context(|| format!("running {}", program.display()))
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("torq-doctor: {}", clean::clean(&format!("{e:#}")));
            std::process::exit(1);
        }
    }
}

fn run(args: &[String]) -> Result<()> {
    let question = args.join(" ");
    if question.trim().is_empty() || args.first().is_some_and(|a| a.starts_with('-')) {
        bail!("usage: torq-doctor \"<question>\" (as a person: torq doctor \"<question>\")");
    }
    let home = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?);
    let cfg = doctor::Config::load(Path::new(SETTINGS), &home)?;
    let machine = Machine { home };
    doctor::ask(&cfg, &machine, &question, &mut std::io::stdout(), &mut std::io::stderr())?;
    Ok(())
}
