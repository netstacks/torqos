//! `torq-tunnel`: what `torq-tunnel@<name>.service` runs (ADR-050 2). It reads the person's tunnel of that
//! name, checks the whole file, and becomes ssh with the one command line the library makes: nothing of
//! the file reaches ssh but a checked destination and checked forwards. `torq-tunnel check` says whether the
//! file is right, for a person at a terminal.

// No unsafe code and no dead code, ever: `forbid` cannot be undone by any `#[allow]` below it,
// so the compiler is the guard (docs/DECISIONS.md ADR-020).
#![forbid(unsafe_code)]
#![forbid(dead_code)]

use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode};
use torq::tunnels;

/// A wrong file (sysexits' EX_CONFIG): the unit does not start it again until the person does
/// (`RestartPreventExitStatus=78`), since it would be refused every time.
const CONFIG: u8 = 78;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let all = match tunnels::load() {
        Ok(all) => all,
        Err(e) => {
            eprintln!("{e:#}");
            return ExitCode::from(CONFIG);
        }
    };
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["check"] => {
            for t in &all {
                println!("{}  {}", t.name, t.describe());
            }
            ExitCode::SUCCESS
        }
        ["run", name] => {
            let Some(t) = all.iter().find(|t| t.name == *name) else {
                eprintln!("There is no tunnel `{name}` in {}.", tunnels::file().map(|p| p.display().to_string()).unwrap_or_default());
                return ExitCode::from(CONFIG);
            };
            eprintln!("Connecting to {}", t.describe());
            // Becomes ssh: systemd watches ssh itself, and its words are the journal's.
            let e = Command::new("ssh").args(t.ssh_args()).exec();
            eprintln!("cannot run ssh: {e}");
            ExitCode::FAILURE
        }
        _ => {
            eprintln!("usage: torq-tunnel run <name> | torq-tunnel check");
            ExitCode::from(2)
        }
    }
}
