//! `torq-mcp` (ADR-051 7, step 5): TorqOS's tools for a coding tool — an MCP server on standard input and
//! output (agent/src/mcp.rs), started by the tool, run as the person's agent account under `torq agent run`.
//! Its session at the broker is started and reported a coding tool's before it reads a message.
//!
//! `torq-mcp --link`: TorqOS's server and skill linked into Claude Code's settings in this account's home
//! (agent/src/link.rs) — run by the launcher, as the agent, at each start.

// No unsafe code and no dead code, ever (ADR-020); in a test build `deny`, as the doctor's.
#![forbid(unsafe_code)]
#![cfg_attr(not(test), forbid(dead_code))]
#![cfg_attr(test, deny(dead_code))]

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use torq_agent::{clean::clean, link, machine::Machine, mcp};

const USAGE: &str = "usage: torq-mcp (an MCP server on standard input and output, for a coding tool run as your agent: torq agent run) | torq-mcp --link (TorqOS's server and skill into Claude Code's settings, in this account's home)";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let done = match args.as_slice() {
        [] => serve(),
        [l] if l == "--link" => link_claude_code(),
        _ => Err(anyhow::anyhow!("{USAGE}")),
    };
    if let Err(e) = done {
        eprintln!("torq-mcp: {}", clean(&format!("{e:#}")));
        std::process::exit(1);
    }
}

fn home() -> Result<PathBuf> {
    let h = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?);
    if !h.is_absolute() {
        bail!("HOME is not an absolute path");
    }
    Ok(h)
}

fn serve() -> Result<()> {
    // The journal only where the agent may read it, as the doctor offers it (/etc/torq/agent.json; none, no journal).
    let journal = std::fs::read_to_string("/etc/torq/agent.json")
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v["journal"].as_bool())
        .unwrap_or(false);
    let machine = Machine { home: home()? };
    let mut server = mcp::Server::start(&machine, journal, PathBuf::from("/etc/torq/nixos-schema.json"))?;
    eprintln!("torq-mcp: session {} at the broker, a coding tool's: tainted, so what it proposes waits for a person", server.session());
    mcp::serve(&mut server, &mut std::io::stdin().lock(), &mut std::io::stdout().lock())
}

fn link_claude_code() -> Result<()> {
    for note in link::claude_code(&home()?, link::MCP, Path::new(link::SKILL)) {
        eprintln!("torq agent run: {}", clean(&note));
    }
    Ok(())
}
