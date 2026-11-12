//! How every entry point opens the agent's window with a question and what it is about (ADR-051 4, step 4):
//! the key, the launcher, TorqOS Settings' and the applets' **Ask the agent**, a crash's notification. The
//! question and its context are written to a file of the person's own, in their runtime directory (0700),
//! and the window is handed its path — never the text on a command line, which every account on the machine
//! can read in /proc. The window reads the file (only one of the person's own, there, not a link) and removes
//! it. What is attached is bounded here, and is untrusted text: asked with it, the agent's session is
//! tainted (the service reports `attached` before the model reads it).

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// The most of a handed-over question kept: about one line of the window's box, so that what the person asks is
/// what they see before they ask (the review of step 5: a 2 000-character question hid most of itself in one
/// line). What an entry point has more to say goes in what is attached, shown in full above the box.
pub const MAX_QUESTION: usize = 200;
/// The most of what is attached kept (a crash's backtrace, an alarm, a failed commit's output).
pub const MAX_CONTEXT: usize = 16_000;
/// The largest handoff file read.
const MAX_FILE: u64 = 64 * 1024;

/// A question for the agent and what it is about. It is never sent by itself: the window puts it in its box,
/// with what is attached shown above, and the person asks (a handoff file is the person's own, but any of
/// their programs could write one).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Handoff {
    /// One line of at most `MAX_QUESTION` characters: the window's box shows it.
    pub question: String,
    #[serde(default)]
    pub context: Option<String>,
}

/// `s` cut to at most `max` characters, said so where it was.
fn bounded(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(40)).collect();
    out.push_str("\n[… cut: longer than TorqOS attaches]");
    out
}

impl Handoff {
    /// A question (made one line), with what it is about: both bounded. An empty context is none.
    pub fn new(question: &str, context: Option<&str>) -> Handoff {
        Handoff {
            question: crate::history::one_line(question, MAX_QUESTION),
            context: context.map(str::trim).filter(|c| !c.is_empty()).map(|c| bounded(c, MAX_CONTEXT)),
        }
    }

    /// Whether asking with it taints the session: anything attached is untrusted text.
    pub fn taints(&self) -> bool {
        self.context.is_some()
    }
}

/// What an **Ask the agent** button is about (TorqOS Settings, TorqOS's applet, the tunnels applet): the
/// question it opens the window with, and the thing itself attached.
#[derive(Debug, Clone, PartialEq)]
pub enum About<'a> {
    /// An alarm, as `show system alarms` says it.
    Alarm(&'a str),
    /// What a command said when it failed: the command, and its words.
    Failure { command: &'a str, said: &'a str },
    /// A setting of the configuration: its place, its help, its value now.
    Setting { place: &'a str, help: &'a str, value: &'a str },
    /// The person's draft, as `show | compare` shows it.
    Draft(&'a str),
    /// An SSH tunnel that will not come up: its name, where it goes, and its last word.
    Tunnel { name: &'a str, destination: &'a str, state: &'a str },
}

impl About<'_> {
    pub fn handoff(&self) -> Handoff {
        let (q, c) = match self {
            About::Alarm(a) => ("What does this alarm mean, and how do I fix it?".to_string(), format!("TorqOS's alarm:\n{a}")),
            About::Failure { command, said } => (format!("Why did `{command}` fail, and what can I do?"), format!("The command: {command}\nWhat it said:\n{said}")),
            About::Setting { place, help, value } => (format!("What does {place} do, and should I change it?"), format!("The setting: {place}\nIts help: {help}\nIts value now: {value}")),
            About::Draft(diff) => ("What do these changes do? Is anything risky in them?".to_string(), format!("My draft, not yet committed (show | compare):\n{diff}")),
            About::Tunnel { name, destination, state } => (format!("Why won't my SSH tunnel {name} come up?"), format!("The tunnel: {name}, to {destination}\nIts state: {state}")),
        };
        Handoff::new(&q, Some(&c))
    }
}

/// The person's own directory for handoffs: `$XDG_RUNTIME_DIR/torq-agent` (their runtime directory is
/// theirs, 0700, made by logind).
fn dir() -> Result<PathBuf> {
    let run = std::env::var_os("XDG_RUNTIME_DIR").context("no XDG_RUNTIME_DIR: not in a session")?;
    Ok(PathBuf::from(run).join("torq-agent"))
}

/// Written for the window, in `dir`: a new file of the person's (0600, never one that was there), its path.
pub fn write_in(dir: &Path, h: &Handoff) -> Result<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).with_context(|| format!("making {}", dir.display()))?;
    let text = serde_json::to_string(h)?;
    for n in 0..64u32 {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let path = dir.join(format!("ask-{}-{now}-{n}.json", std::process::id()));
        match std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32).open(&path) {
            Ok(mut f) => {
                f.write_all(text.as_bytes())?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(anyhow::Error::from(e).context(format!("writing {}", path.display()))),
        }
    }
    bail!("could not make a file in {}", dir.display())
}

/// Read a handoff the window was given, and remove it: only a regular file of `uid`'s, directly in `dir`,
/// not reached through a link, and not too large — anything else is refused.
pub fn take_from(dir: &Path, path: &Path, uid: u32) -> Result<Handoff> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    if path.parent() != Some(dir) {
        bail!("{}: not a handoff of TorqOS's ({} only)", path.display(), dir.display());
    }
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        // Never through a link; never waiting (a FIFO left there would hold the window: opened without
        // blocking, its type is then checked on the open descriptor, before anything is read).
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    let m = f.metadata()?;
    if !m.is_file() || m.uid() != uid || m.len() > MAX_FILE {
        bail!("{}: not a handoff of yours", path.display());
    }
    let mut text = String::new();
    (&mut f).take(MAX_FILE).read_to_string(&mut text)?;
    let _ = std::fs::remove_file(path);
    let h: Handoff = serde_json::from_str(&text).context("not a handoff TorqOS wrote")?;
    // Bounded again: whoever wrote it.
    Ok(Handoff::new(&h.question, h.context.as_deref()))
}

/// Read the handoff at `path` in the person's own directory.
pub fn take(path: &Path) -> Result<Handoff> {
    take_from(&dir()?, path, rustix::process::getuid().as_raw())
}

/// The window's command line for a handoff: `torq-agent-ui --ask <file>`, started as a unit of the person's
/// own user manager (`systemd-run --user`, both by absolute path) — so it lives in a unit of its own, with the
/// user manager's environment (its session's as the session's start gave it), not in whatever started it (the
/// crash watcher's service, the launcher's plugin). `--service-type=exec`: systemd-run returns once the window's
/// program has been started — and fails, said, if it could not be (step 5's second review); what happens to the
/// window after that is the window's (its journal).
pub fn window_command(file: &Path) -> std::process::Command {
    let mut c = std::process::Command::new(crate::app::system_program("systemd-run"));
    c.args(["--user", "--collect", "--quiet", "--service-type=exec", "--"]).arg(crate::app::system_program("torq-agent-ui")).arg("--ask").arg(file);
    c
}

/// Open the agent's window with `h` (raising it, with the question in, if it is open).
pub fn open_window(h: &Handoff) -> Result<()> {
    let file = write_in(&dir()?, h)?;
    // Waited for (bounded): nothing of it is left behind.
    let c = window_command(&file);
    match super::setup::output_within(c, std::time::Duration::from_secs(10)) {
        Some(_) => Ok(()),
        None => {
            let _ = std::fs::remove_file(&file);
            bail!("could not start the agent's window (torq-agent-ui, through systemd-run --user): see `journalctl --user -n 20`")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn me() -> u32 {
        rustix::process::getuid().as_raw()
    }

    #[test]
    fn bounded_and_tainting() {
        let h = Handoff::new("  why did it fail?  ", Some(&"x".repeat(MAX_CONTEXT * 3)));
        assert_eq!(h.question, "why did it fail?");
        let c = h.context.as_deref().unwrap();
        assert!(c.chars().count() <= MAX_CONTEXT && c.ends_with("longer than TorqOS attaches]"), "{}", c.len());
        assert!(h.taints(), "attached: tainting");
        assert_eq!(Handoff::new("q", Some("   ")).context, None, "nothing attached is none");
        assert!(!Handoff::new("hello", None).taints());
        assert_eq!(Handoff::new("why\nis it\r\n  slow?", None).question, "why is it slow?", "one line: the box shows it all");
        assert!(Handoff::new(&"q".repeat(MAX_QUESTION + 10), None).question.chars().count() <= MAX_QUESTION);
    }

    /// A handed-over question is about one line of the window's box (200 characters), cut visibly, however long
    /// the file that brought it: what the person sends is what they saw (the second review of step 5).
    #[test]
    fn a_handed_over_question_fits_the_box() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("torq-agent");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
        let file = dir.join("ask-long.json");
        std::fs::write(&file, serde_json::to_string(&Handoff { question: "why ".repeat(500), context: None }).unwrap()).unwrap();
        let q = take_from(&dir, &file, me()).unwrap().question;
        assert!(q.chars().count() <= 200 && q.ends_with('…'), "{} characters", q.chars().count());
    }

    /// Written 0600 in the person's directory and read back once; a file elsewhere, a link, someone else's or
    /// too large is refused.
    #[test]
    fn written_and_taken_once() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("torq-agent");
        let h = Handoff::new("about this crash", Some("SIGSEGV in sh"));
        let p = write_in(&dir, &h).unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(take_from(&dir, &p, me()).unwrap(), h);
        assert!(!p.exists(), "removed once read");
        assert!(take_from(&dir, &p, me()).is_err(), "only once");
        let p = write_in(&dir, &h).unwrap();
        assert!(take_from(&dir, &p, me() + 1).is_err(), "not someone else's");
        let elsewhere = d.path().join("ask.json");
        std::fs::write(&elsewhere, serde_json::to_string(&h).unwrap()).unwrap();
        assert!(take_from(&dir, &elsewhere, me()).is_err(), "only in the handoff directory");
        let link = dir.join("ask-link.json");
        std::os::unix::fs::symlink(&elsewhere, &link).unwrap();
        assert!(take_from(&dir, &link, me()).is_err(), "never through a link");
        let big = dir.join("ask-big.json");
        std::fs::write(&big, "x".repeat(MAX_FILE as usize + 1)).unwrap();
        assert!(take_from(&dir, &big, me()).is_err(), "too large");
        // A FIFO left there: refused at once, never waited on (the window would hang on it).
        let fifo = dir.join("ask-fifo.json");
        rustix::fs::mknodat(rustix::fs::CWD, &fifo, rustix::fs::FileType::Fifo, rustix::fs::Mode::from_raw_mode(0o600), 0).unwrap();
        let t = std::time::Instant::now();
        assert!(take_from(&dir, &fifo, me()).is_err(), "not a file");
        assert!(t.elapsed() < std::time::Duration::from_secs(2), "never waited on");
        // Bounded again when read, whoever wrote it.
        let long = dir.join("ask-long.json");
        std::fs::write(&long, serde_json::to_string(&Handoff { question: "q".into(), context: Some("y".repeat(MAX_CONTEXT + 500)) }).unwrap()).unwrap();
        assert!(take_from(&dir, &long, me()).unwrap().context.unwrap().chars().count() <= MAX_CONTEXT);
    }

    /// The window by absolute path, in a unit of its own, given the file, never the text.
    #[test]
    fn the_window_is_given_a_file() {
        let c = window_command(Path::new("/run/user/1000/torq-agent/ask-1.json"));
        assert_eq!(c.get_program(), "/run/current-system/sw/bin/systemd-run");
        assert_eq!(c.get_args().collect::<Vec<_>>(), ["--user", "--collect", "--quiet", "--service-type=exec", "--", "/run/current-system/sw/bin/torq-agent-ui", "--ask", "/run/user/1000/torq-agent/ask-1.json"]);
    }

    /// Each button's question, and the thing attached: tainting, the person asks, bounded.
    #[test]
    fn what_each_button_hands_over() {
        let cases = [
            (About::Alarm("⚠ The remote did not take the last push"), "alarm", "The remote did not take"),
            (About::Failure { command: "commit", said: "error: option does not exist" }, "`commit` fail", "option does not exist"),
            (About::Setting { place: "services openssh enable", help: "Whether to run sshd", value: "true" }, "services openssh enable", "Its value now: true"),
            (About::Draft("[edit services]\n+  openssh enable true"), "these changes", "+  openssh enable true"),
            (About::Tunnel { name: "work", destination: "me@host", state: "Reconnecting: the host's key is not known" }, "tunnel work", "the host's key is not known"),
        ];
        for (about, in_question, in_context) in cases {
            let h = about.handoff();
            assert!(h.question.contains(in_question), "{h:?}");
            assert!(h.context.as_deref().unwrap().contains(in_context), "{h:?}");
            assert!(h.taints(), "{h:?}");
        }
        let long = "x".repeat(MAX_CONTEXT * 2);
        assert!(About::Draft(&long).handoff().context.unwrap().chars().count() <= MAX_CONTEXT);
    }
}
