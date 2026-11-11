//! The machine, as TorqOS's agent programs run things on it (`torq-doctor`, `torq-mcp`): a program run with
//! nothing of the caller's environment but a fixed PATH, its home and plain output, its input closed, its
//! output and its time bounded (tools::bounded); torqcli, by its absolute path, for proposals and for the
//! session at the broker.

use crate::{clean, tools};
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Machine {
    pub home: PathBuf,
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
