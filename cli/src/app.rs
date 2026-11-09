//! What TorqOS Settings (ADR-045 2) asks of the command line: the same calls to the broker and the same
//! words, so the window says what `torqcli` would print. Nothing here is the window's own: a word the GUI
//! has and the command line has not is a bug in one of them.

use crate::{history, status, Cli, BROKER, REPO};
use anyhow::Result;
use std::path::PathBuf;

/// The machine, as torqcli sees it: its broker's socket and its configuration repository.
fn cli() -> Cli {
    Cli { repo: PathBuf::from(REPO), broker: BROKER.into() }
}

/// The machine at a glance: `show system alarms`.
pub struct Machine {
    /// TorqOS's version (this program's: the system's own torqcli).
    pub version: &'static str,
    /// Nothing is wrong: the machine is as its history says.
    pub well: bool,
    /// What `show system alarms` prints, each line a sentence.
    pub alarms: String,
}

pub fn machine() -> Result<Machine> {
    let (_, report) = crate::status_report(&cli(), None)?;
    Ok(Machine { version: env!("CARGO_PKG_VERSION"), well: matches!(report.fix, status::Fix::Nothing) && report.exit == 0, alarms: report.text })
}

/// The last `limit` changes, oldest first: `show system ledger`'s lines.
pub fn history(limit: usize) -> Result<Vec<history::Row>> {
    let entries = crate::read_ledger(&cli())?;
    Ok(history::rows(&entries, limit, &history::Clock::system()))
}
