//! Running the broker quietly (docs/CLI-DESIGN.md, ADR-014): its output goes to a log file, the
//! terminal shows one progress line, and its result is read as JSON — never parsed from its text.

use anyhow::{Context, Result};
use serde_json::Value;
use std::fs;
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// `$XDG_STATE_HOME/kestrel/last.log` (default `~/.local/state/kestrel/last.log`): the full output
/// of the last change, shown only when something fails.
/// Never a shared directory: without a home there is no log (it would be readable, and replaceable,
/// by others in /tmp).
pub fn log_path() -> Result<PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .context("neither XDG_STATE_HOME nor HOME is set, so there is nowhere private to keep the log")?;
    Ok(state.join("kestrel/last.log"))
}

/// Start a fresh log for this command: private to the user (0600), and never written through a
/// symlink.
pub fn start_log() -> Result<PathBuf> {
    use std::os::unix::fs::OpenOptionsExt;
    let p = log_path()?;
    fs::create_dir_all(p.parent().unwrap()).with_context(|| format!("creating {}", p.display()))?;
    if fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_symlink()) {
        fs::remove_file(&p).with_context(|| format!("removing the symlink {}", p.display()))?;
    }
    let f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&p)
        .with_context(|| format!("writing {}", p.display()))?;
    use std::os::unix::fs::PermissionsExt;
    f.set_permissions(fs::Permissions::from_mode(0o600))?; // also for a log created before this
    Ok(p)
}

/// One line on the terminal while something runs: a spinner, the phase and the time so far.
/// Nothing at all when stderr is not a terminal. Cleared when dropped.
pub struct Progress {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Progress {
    pub fn start(phase: &str) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        // A dumb terminal cannot redraw a line in place: show nothing rather than a stream of frames.
        if !std::io::stderr().is_terminal() || std::env::var("TERM").is_ok_and(|t| t == "dumb") {
            return Progress { stop, handle: None };
        }
        let (flag, phase) = (stop.clone(), phase.to_string());
        let handle = std::thread::spawn(move || {
            let frames = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
            let started = Instant::now();
            let mut i = 0;
            while !flag.load(Ordering::Relaxed) {
                let line = format!("  {} {phase}…  {}", frames[i % frames.len()], crate::ui::elapsed(started.elapsed()));
                eprint!("\r\x1b[2K{line}");
                let _ = std::io::stderr().flush();
                std::thread::sleep(Duration::from_millis(100));
                i += 1;
            }
            eprint!("\r\x1b[2K");
            let _ = std::io::stderr().flush();
        });
        Progress { stop, handle: Some(handle) }
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// How a broker command ended, read from its exit code and JSON (CLI-DESIGN.md, "The broker's
/// JSON interface"). Messages are the broker's text and must be cleaned before printing.
#[derive(Debug)]
pub enum Outcome {
    Done(Value),
    /// Another operation is running (exit 75).
    Busy,
    /// Refused or failed (exit 1), with the broker's message.
    Refused(String),
    /// The broker itself did not answer as it should (crash, bad arguments, not JSON).
    Broken(String),
}

/// Run `broker` (already built with its arguments) with `--json`: stdout is the result, stderr
/// (and the rebuild's output) goes to `log`.
pub fn broker_json(mut broker: Command, log: &std::path::Path) -> Result<Outcome> {
    let log_file = fs::OpenOptions::new().append(true).create(true).open(log).context("opening the log")?;
    let out = broker
        .arg("--json")
        .stdin(Stdio::inherit()) // sudo may need to ask for a password
        .stdout(Stdio::piped())
        .stderr(Stdio::from(log_file))
        .output()
        .context("running the broker")?;
    let v: Option<Value> = serde_json::from_slice(&out.stdout).ok();
    let error = || v.as_ref().and_then(|v| v["error"].as_str()).unwrap_or_default().to_string();
    let outcome = match (out.status.code(), &v) {
        (Some(0), Some(v)) => Outcome::Done(v.clone()),
        (Some(75), _) => Outcome::Busy,
        (Some(1), Some(_)) => Outcome::Refused(error()),
        // 2: it did not understand the request — an installed broker older than this command.
        (Some(2), _) => Outcome::Broken("the installed broker is older than this kestrel command (update the machine first)".into()),
        (Some(code), _) => Outcome::Broken(format!("the broker stopped unexpectedly (exit code {code})")),
        (None, _) => Outcome::Broken("the broker was stopped before it finished".into()),
    };
    // With --json the broker's own explanation is on stdout: keep it in the log too, which is
    // where a person looks when something failed (stdout itself is only ever parsed).
    if !matches!(outcome, Outcome::Done(_)) {
        let mut f = fs::OpenOptions::new().append(true).open(log).context("opening the log")?;
        writeln!(f, "\nkestrel-broker {:?}: {}", out.status.code(), String::from_utf8_lossy(&out.stdout).trim())?;
    }
    Ok(outcome)
}
