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
        let out = within(self.command(Path::new(tools::TORQCLI), &a), SESSION_TIMEOUT).context("running torqcli agent session")?;
        if !out.status.success() {
            bail!("{}", clean::clean(String::from_utf8_lossy(&out.stderr).trim()));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

/// How long a call to the broker about the session may take (it answers in well under a second; the
/// socket's own limit is 10 s a step): past it, the call has failed — and a report that failed ends the
/// session, never taken as made.
pub const SESSION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// `cmd`'s output, its input closed, in a process group of its own, each output at most 1 MiB; killed, and an
/// error, if it has not ended within `limit`.
fn within(mut cmd: Command, limit: std::time::Duration) -> Result<std::process::Output> {
    use std::io::Read;
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    let mut child = cmd.process_group(0).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let group = rustix::process::Pid::from_child(&child);
    let read = |p: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut b = Vec::new();
            if let Some(p) = p {
                let _ = p.take(tools::MAX_PROGRAM_BYTES as u64).read_to_end(&mut b);
            }
            b
        })
    };
    let out = read(child.stdout.take().map(|o| Box::new(o) as Box<dyn Read + Send>));
    let err = read(child.stderr.take().map(|e| Box::new(e) as Box<dyn Read + Send>));
    let deadline = std::time::Instant::now() + limit;
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if std::time::Instant::now() >= deadline {
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
            let _ = child.kill();
            let _ = child.wait();
            bail!("no answer within {} s", limit.as_secs());
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    // Nothing it left behind holds its output open.
    let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
    Ok(std::process::Output { status, stdout: out.join().unwrap_or_default(), stderr: err.join().unwrap_or_default() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_call_is_bounded() {
        let sh = |s: &str| {
            let mut c = Command::new("/bin/sh");
            c.args(["-c", s]);
            c
        };
        let o = within(sh("echo id; echo why >&2"), std::time::Duration::from_secs(5)).unwrap();
        assert_eq!((o.stdout.as_slice(), o.stderr.as_slice()), (b"id\n".as_slice(), b"why\n".as_slice()));
        let t = std::time::Instant::now();
        // A child holding the output open goes with it (its group).
        let e = within(sh("(sleep 60) & sleep 60"), std::time::Duration::from_millis(300)).unwrap_err();
        assert!(e.to_string().contains("no answer within"), "{e}");
        assert!(t.elapsed() < std::time::Duration::from_secs(10), "{:?}", t.elapsed());
        assert_eq!(SESSION_TIMEOUT.as_secs(), 30);
    }
}
