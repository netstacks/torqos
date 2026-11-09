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
#[derive(Debug, Clone)]
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

/// What the running system needs before it is all in use (ADR-049): a switch changes files at once, but a
/// kernel, its boot image, modules, firmware or graphics driver only at the next boot, and COSMIC's session
/// only at the next login.
#[derive(Debug, Clone, PartialEq)]
pub enum Needs {
    Nothing,
    /// What would be taken into use by logging out and in again.
    LogOut(Vec<&'static str>),
    /// What would be taken into use by a restart.
    Restart(Vec<&'static str>),
}

impl Needs {
    /// The sentence torqcli, the applet and TorqOS Settings all say; None when nothing is needed.
    pub fn line(&self) -> Option<String> {
        match self {
            Needs::Nothing => None,
            Needs::LogOut(what) => Some(format!("Log out and back in to use the new {}.", and(what))),
            Needs::Restart(what) => Some(format!("Restart to use the new {}.", and(what))),
        }
    }
}

fn and(what: &[&str]) -> String {
    match what {
        [] => "system".into(),
        [one] => one.to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// What `current` (the running system) needs, against `booted` (the system the machine started) and, for a
/// desktop session, `session` (the system the session started on, as the applet recorded it).
pub fn needs(booted: &std::path::Path, current: &std::path::Path, session: Option<&std::path::Path>) -> Needs {
    let link = |sys: &std::path::Path, what: &str| std::fs::read_link(sys.join(what)).ok();
    let mut restart = vec![];
    for (what, name) in [("kernel", "kernel"), ("initrd", "boot image"), ("kernel-modules", "kernel modules"), ("firmware", "firmware")] {
        if link(booted, what) != link(current, what) {
            restart.push(name);
        }
    }
    if driver(booted) != driver(current) {
        restart.push("graphics driver");
    }
    // The desktop turned on while the machine runs without it: its login screen starts at the next boot.
    if desktop(current).is_some() && desktop(booted).is_none() {
        restart.push("desktop");
    }
    if !restart.is_empty() {
        return Needs::Restart(restart);
    }
    match session {
        Some(s) if desktop(s) != desktop(current) => Needs::LogOut(vec!["COSMIC"]),
        _ => Needs::Nothing,
    }
}

/// The graphics driver a system's activation links at /run/opengl-driver.
fn driver(sys: &std::path::Path) -> Option<String> {
    let script = std::fs::read_to_string(sys.join("activate")).ok()?;
    let at = script.find("-graphics-drivers")?;
    let start = script[..at].rfind("/nix/store/")?;
    Some(script[start..at].to_string())
}

/// COSMIC's session in a system: the compositor and the session, by their store paths; None without a desktop.
fn desktop(sys: &std::path::Path) -> Option<Vec<std::path::PathBuf>> {
    ["cosmic-comp", "cosmic-session", "cosmic-panel"]
        .iter()
        .map(|b| std::fs::canonicalize(sys.join("sw/bin").join(b)).ok())
        .collect()
}

#[cfg(test)]
mod needs_tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::path::Path;

    /// A system: its boot files at `boot`'s version, its driver, and COSMIC at `cosmic` (none: no desktop).
    fn system(root: &Path, name: &str, boot: &str, drv: &str, cosmic: Option<&str>) -> std::path::PathBuf {
        let s = root.join(name);
        std::fs::create_dir_all(s.join("sw/bin")).unwrap();
        for what in ["kernel", "initrd", "kernel-modules", "firmware"] {
            symlink(format!("/nix/store/{boot}-{what}"), s.join(what)).unwrap();
        }
        std::fs::write(s.join("activate"), format!("ln -sfn /nix/store/{drv}-graphics-drivers /run/opengl-driver\n")).unwrap();
        if let Some(v) = cosmic {
            for b in ["cosmic-comp", "cosmic-session", "cosmic-panel"] {
                let target = root.join(format!("{v}-{b}"));
                std::fs::write(&target, "").unwrap();
                symlink(&target, s.join("sw/bin").join(b)).unwrap();
            }
        }
        s
    }

    #[test]
    fn what_a_switch_needs() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        let old = system(r, "old", "a", "m1", Some("c1"));
        assert_eq!(needs(&old, &old, Some(&old)), Needs::Nothing);
        // A new COSMIC: the next login; with no session to compare, nothing a person can do here.
        let cosmic = system(r, "cosmic", "a", "m1", Some("c2"));
        assert_eq!(needs(&old, &cosmic, Some(&old)), Needs::LogOut(vec!["COSMIC"]));
        assert_eq!(needs(&old, &cosmic, None), Needs::Nothing);
        assert_eq!(needs(&old, &cosmic, Some(&cosmic)), Needs::Nothing, "a session started on it already uses it");
        // A new kernel and driver: the next boot, whatever the session.
        let kernel = system(r, "kernel", "b", "m2", Some("c2"));
        assert_eq!(needs(&old, &kernel, Some(&old)), Needs::Restart(vec!["kernel", "boot image", "kernel modules", "firmware", "graphics driver"]));
        // The desktop turned on, from a machine booted without it.
        let console = system(r, "console", "a", "m1", None);
        assert_eq!(needs(&console, &old, None), Needs::Restart(vec!["desktop"]));
        assert_eq!(Needs::Restart(vec!["kernel", "graphics driver"]).line().unwrap(), "Restart to use the new kernel and graphics driver.");
        assert_eq!(Needs::LogOut(vec!["COSMIC"]).line().unwrap(), "Log out and back in to use the new COSMIC.");
    }
}

/// The machine at a glance, for COSMIC's panel (ADR-049): each part the command line's own.
#[derive(Debug, Clone)]
pub struct Glance {
    pub version: &'static str,
    /// Nothing is wrong: the machine is as its history says.
    pub well: bool,
    /// The first thing `show system alarms` says is wrong, when something is.
    pub alarm: Option<String>,
    /// A commit confirmed waiting: its change number and deadline (`commit` confirms it).
    pub confirm: Option<(u64, String)>,
    /// What the running system needs before it is all in use.
    pub needs: Needs,
    /// Proposals waiting for a person, and how many of them are tainted.
    pub waiting: (usize, usize),
    /// The latest change that took effect, in the log's words.
    pub last: Option<String>,
}

/// `session`: the system the desktop session started on, for what a log-out would take into use.
pub fn glance(session: Option<&std::path::Path>) -> Result<Glance> {
    let cli = cli();
    let (v, report) = crate::status_report(&cli, None)?;
    let well = matches!(report.fix, status::Fix::Nothing) && report.exit == 0;
    let alarm = if well { None } else { report.text.lines().map(str::trim).find(|l| !l.is_empty()).map(String::from) };
    let p = &v["pending_confirm"];
    let confirm = p["applied"].as_u64().map(|seq| (seq, history::one_line(p["deadline"].as_str().unwrap_or("?"), 40)));
    let entries = crate::read_ledger(&cli)?;
    let w = history::waiting_proposals(&entries);
    let needs = needs(std::path::Path::new("/run/booted-system"), std::path::Path::new("/run/current-system"), session);
    Ok(Glance {
        version: env!("CARGO_PKG_VERSION"),
        well,
        alarm,
        confirm,
        needs,
        waiting: (w.len(), w.iter().filter(|e| e.tainted).count()),
        last: history::last_change(&entries),
    })
}

/// The command that confirms a commit confirmed waiting, as a person would type it.
pub const CONFIRM: &str = "torqcli -c \"configure; commit\"";
