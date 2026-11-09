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

/// TorqOS's version as the machine runs it now: a panel started before an update is told the new one.
fn version() -> String {
    let release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    release.lines().find_map(|l| l.strip_prefix("VERSION_ID=")).map(|v| v.trim_matches('"').to_string()).filter(|v| !v.is_empty()).unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string())
}

/// The machine at a glance: `show system alarms`.
#[derive(Debug, Clone)]
pub struct Machine {
    /// TorqOS's version.
    pub version: String,
    /// Nothing is wrong: the machine is as its history says.
    pub well: bool,
    /// What `show system alarms` prints, each line a sentence.
    pub alarms: String,
}

pub fn machine() -> Result<Machine> {
    let (_, report) = crate::status_report(&cli(), None, crate::run::watch_log()?)?;
    Ok(Machine { version: version(), well: matches!(report.fix, status::Fix::Nothing) && report.exit == 0, alarms: report.text })
}

/// An error as the window shows it: cleaned, as the terminal cleans what it prints.
pub fn said(e: &anyhow::Error) -> String {
    history::clean(&format!("{e:#}"))
}

/// The last `limit` changes, oldest first: `show system ledger`'s lines.
pub fn history(limit: usize) -> Result<Vec<history::Row>> {
    let entries = crate::read_ledger(&cli())?;
    Ok(history::rows(&entries, limit, &history::Clock::system()))
}

/// What the running system needs before it is all in use (ADR-049). A restart is almost never one of them: the
/// machine goes on working on the kernel it started with, and a boot image is only read at boot.
#[derive(Debug, Clone, PartialEq)]
pub enum Needs {
    Nothing,
    /// In use at the next start, with nothing to do until then: said, never asked for.
    NextStart(Vec<&'static str>),
    /// What would be taken into use by logging out and in again.
    LogOut(Vec<&'static str>),
    /// What cannot be in use without a restart.
    Restart(Vec<&'static str>),
}

impl Needs {
    /// The sentence torqcli, the applet and TorqOS Settings all say when something is asked of the person.
    pub fn line(&self) -> Option<String> {
        match self {
            Needs::Nothing | Needs::NextStart(_) => None,
            Needs::LogOut(what) => Some(format!("Log out and back in to use the new {}.", and(what))),
            Needs::Restart(what) => Some(format!("Restart to use the new {}.", and(what))),
        }
    }

    /// What is ready for the next start: nothing to do about it.
    pub fn note(&self) -> Option<String> {
        match self {
            Needs::NextStart(what) => Some(format!("A new {} is installed, for the next time this machine starts. There is no need to restart for it.", and(what))),
            _ => None,
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
    // The desktop turned on while the machine runs without it: its login screen starts at the next boot.
    if desktop(current).is_some() && desktop(booted).is_none() {
        return Needs::Restart(vec!["desktop"]);
    }
    if let Some(s) = session {
        let mut what = vec![];
        if desktop(s) != desktop(current) {
            what.push("COSMIC");
        }
        if driver(s) != driver(current) {
            what.push("graphics driver");
        }
        if !what.is_empty() {
            return Needs::LogOut(what);
        }
    }
    // Only a new kernel waits for a start. A boot image, the firmware and the modules of a new one are not
    // news: the first is read only at boot, NixOS keeps the running kernel's modules, and firmware is read
    // when a driver loads.
    if link(booted, "kernel") != link(current, "kernel") {
        return Needs::NextStart(vec!["Linux kernel"]);
    }
    Needs::Nothing
}

/// The graphics driver a system links at /run/opengl-driver: its tmpfiles line's target.
fn driver(sys: &std::path::Path) -> Option<String> {
    let lines = std::fs::read_to_string(sys.join("etc/tmpfiles.d/graphics-driver.conf")).ok()?;
    let line = lines.lines().find(|l| l.contains("'/run/opengl-driver'"))?;
    line.split_whitespace().last().filter(|t| t.starts_with("/nix/store/")).map(String::from)
}

/// What only a new login takes up: COSMIC's compositor and its session, by their store paths; None without a desktop.
fn desktop(sys: &std::path::Path) -> Option<Vec<std::path::PathBuf>> {
    ["cosmic-comp", "cosmic-session"]
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
        // As NixOS writes it.
        std::fs::create_dir_all(s.join("etc/tmpfiles.d")).unwrap();
        std::fs::write(s.join("etc/tmpfiles.d/graphics-driver.conf"), format!("'L+' '/run/opengl-driver' '-' '-' '-' '-' /nix/store/{drv}-graphics-drivers\n'r' '/run/opengl-driver-32' '-' '-' '-' '-' \n")).unwrap();
        if let Some(v) = cosmic {
            for b in ["cosmic-comp", "cosmic-session"] {
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
        // What the laptop did: only the boot image differs (a host name, zram). Nothing is asked for.
        let image = system(r, "image", "a", "m1", Some("c1"));
        std::fs::remove_file(image.join("initrd")).unwrap();
        symlink("/nix/store/other-initrd", image.join("initrd")).unwrap();
        assert_eq!(needs(&old, &image, Some(&old)), Needs::Nothing, "a boot image is only read at boot");
        // A new COSMIC: the next login; with no session to compare, nothing a person can do here.
        let cosmic = system(r, "cosmic", "a", "m1", Some("c2"));
        assert_eq!(needs(&old, &cosmic, Some(&old)), Needs::LogOut(vec!["COSMIC"]));
        assert_eq!(needs(&old, &cosmic, None), Needs::Nothing);
        assert_eq!(needs(&old, &cosmic, Some(&cosmic)), Needs::Nothing, "a session started on it already uses it");
        // A new graphics driver: the compositor has the old one loaded until the next login.
        let driver = system(r, "driver", "a", "m2", Some("c1"));
        assert_eq!(needs(&old, &driver, Some(&old)), Needs::LogOut(vec!["graphics driver"]));
        // A new kernel: for the next start, and nothing is asked. Never a restart.
        let kernel = system(r, "kernel", "b", "m1", Some("c1"));
        assert_eq!(needs(&old, &kernel, Some(&old)), Needs::NextStart(vec!["Linux kernel"]));
        assert_eq!(Needs::NextStart(vec!["Linux kernel"]).line(), None, "said, not asked for");
        assert!(Needs::NextStart(vec!["Linux kernel"]).note().unwrap().contains("no need to restart"));
        // The one case: the desktop turned on, from a machine booted without it.
        let console = system(r, "console", "a", "m1", None);
        assert_eq!(needs(&console, &old, None), Needs::Restart(vec!["desktop"]));
        assert_eq!(Needs::LogOut(vec!["COSMIC", "graphics driver"]).line().unwrap(), "Log out and back in to use the new COSMIC and graphics driver.");
    }
}

/// The machine at a glance, for COSMIC's panel (ADR-049): each part the command line's own.
#[derive(Debug, Clone)]
pub struct Glance {
    pub version: String,
    /// Nothing is wrong: the machine is as its history says.
    pub well: bool,
    /// The first thing `show system alarms` says is wrong, when something is.
    pub alarm: Option<String>,
    /// A commit confirmed waiting (`commit` confirms it).
    pub confirm: Option<Confirm>,
    /// The person's draft differs from the configuration in effect.
    pub draft: bool,
    /// What the running system needs before it is all in use.
    pub needs: Needs,
    /// Proposals waiting for a person, and how many of them are tainted.
    pub waiting: (usize, usize),
    /// The latest change that took effect, in the log's words.
    pub last: Option<String>,
}

/// A commit confirmed waiting.
#[derive(Debug, Clone, PartialEq)]
pub struct Confirm {
    pub seq: u64,
    /// Its deadline, in local time.
    pub deadline: String,
    /// Its deadline, seconds since the epoch, for a countdown.
    pub at: Option<i64>,
}

/// Seconds since the epoch, now.
pub fn now() -> i64 {
    history::Clock::system().now
}

/// `session`: the system the desktop session started on, for what a log-out would take into use.
/// `draft`: `Console::uncommitted`, as the caller asked it.
pub fn glance(session: Option<&std::path::Path>, draft: bool) -> Result<Glance> {
    let cli = cli();
    let (v, report) = crate::status_report(&cli, None, crate::run::watch_log()?)?;
    let well = matches!(report.fix, status::Fix::Nothing) && report.exit == 0;
    let alarm = if well { None } else { report.text.lines().map(str::trim).find(|l| !l.is_empty()).map(String::from) };
    let p = &v["pending_confirm"];
    let clock = history::Clock::system();
    let confirm = p["applied"].as_u64().map(|seq| {
        let ts = p["deadline"].as_str().unwrap_or("?");
        Confirm { seq, deadline: history::one_line(&history::local_stamp(ts, &clock), 40), at: history::parse_ts(ts) }
    });
    let entries = crate::read_ledger(&cli)?;
    let w = history::waiting_proposals(&entries);
    let needs = needs(std::path::Path::new("/run/booted-system"), std::path::Path::new("/run/current-system"), session);
    Ok(Glance {
        version: version(),
        well,
        alarm,
        confirm,
        draft,
        needs,
        waiting: (w.len(), w.iter().filter(|e| e.tainted).count()),
        last: history::last_change(&entries),
    })
}



// ── What a session started on ────────────────────────────────────────────────────────────────────

/// Where this login's system is recorded: the login's runtime directory outlives a logout while an SSH
/// session is open, so the name is the login's own.
fn session_file() -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")?;
    let id = std::env::var("XDG_SESSION_ID").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "desktop".into());
    Some(PathBuf::from(dir).join(format!("torq-session-system-{id}")))
}

/// The system this login's desktop started on, if the applet has recorded one.
pub fn session_recorded() -> Option<PathBuf> {
    std::fs::read_link(session_file()?).ok()
}

/// The system this login's desktop started on, recorded the first time it is asked (the applet, which
/// starts with the session), so an applet restarted mid-session still knows it.
pub fn session_system() -> Option<PathBuf> {
    if let Some(s) = session_recorded() {
        return Some(s);
    }
    let now = std::fs::read_link("/run/current-system").ok()?;
    if let Some(f) = session_file() {
        let _ = std::os::unix::fs::symlink(&now, f);
    }
    Some(now)
}

// ── The command line, in a window ────────────────────────────────────────────────────────────────

pub use crate::complete::Choice;
pub use crate::session::Mode;

/// Words as one line of a command, quoted where a word needs it.
pub fn line(words: &[&str]) -> String {
    words.iter().map(|w| crate::syntax::quote(w)).collect::<Vec<_>>().join(" ")
}

/// Which of torqcli's sessions a line runs in. The draft is the person's own saved candidate, shared with
/// their terminal's shell.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum At {
    /// What the pages read: operational mode.
    Read,
    /// What the pages change: configuration mode, from the top.
    Draft,
    /// The Commands page's: whichever mode the person has gone to.
    Typed,
    /// What only asks whether the draft has changes: never held up by a `commit check`.
    Watch,
}

/// What a typed line did: its output or why not, and where the session is now.
#[derive(Debug, Clone)]
pub struct Typed {
    pub output: Result<String, String>,
    pub configuration: bool,
    pub edit: Vec<String>,
}

/// Where a command's questions go, each with the way back for its answer.
type Asks = std::sync::mpsc::Sender<(String, std::sync::mpsc::Sender<bool>)>;

enum Job {
    Run(At, String, std::sync::mpsc::Sender<Result<String, String>>, Option<Asks>),
    Typed(String, std::sync::mpsc::Sender<Typed>, Option<Asks>),
    Choices(At, String, std::sync::mpsc::Sender<Vec<Choice>>),
    Uncommitted(std::sync::mpsc::Sender<bool>),
}

/// torqcli's own sessions, on threads of their own (a session is not to be shared between threads, and a
/// `commit check` must not hold up what a page reads). Cloned freely; each call blocks its caller.
#[derive(Clone)]
pub struct Console {
    reads: std::sync::mpsc::Sender<Job>,
    edits: std::sync::mpsc::Sender<Job>,
    watch: std::sync::mpsc::Sender<Job>,
}

impl Default for Console {
    fn default() -> Self {
        Self::start()
    }
}

impl Console {
    pub fn start() -> Self {
        let spawn = |sessions: &'static [At]| {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || serve(sessions, rx));
            tx
        };
        Console { reads: spawn(&[At::Read]), edits: spawn(&[At::Draft, At::Typed]), watch: spawn(&[At::Watch]) }
    }

    /// Only what the panel needs: whether the draft has changes.
    pub fn watcher() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || serve(&[At::Watch], rx));
        let none = || std::sync::mpsc::channel().0;
        Console { reads: none(), edits: none(), watch: tx }
    }

    fn queue(&self, at: At) -> &std::sync::mpsc::Sender<Job> {
        match at {
            At::Read => &self.reads,
            At::Draft | At::Typed => &self.edits,
            At::Watch => &self.watch,
        }
    }

    /// Run a line, its output cleaned as the terminal's is.
    pub fn run(&self, at: At, line: &str) -> Result<String, String> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.queue(at).send(Job::Run(at, line.to_string(), tx, None)).map_err(|_| GONE.to_string())?;
        rx.recv().unwrap_or_else(|_| Err(GONE.to_string()))
    }

    /// Run a line that may ask a person something: `ask` answers each question.
    pub fn act(&self, at: At, line: &str, ask: impl Fn(String) -> bool + Send) -> Result<String, String> {
        let (tx, rx) = std::sync::mpsc::channel();
        let (asks, questions) = std::sync::mpsc::channel();
        self.queue(at).send(Job::Run(at, line.to_string(), tx, Some(asks))).map_err(|_| GONE.to_string())?;
        std::thread::scope(|scope| {
            scope.spawn(move || answering(questions, ask));
            rx.recv().unwrap_or_else(|_| Err(GONE.to_string()))
        })
    }

    /// A line typed on the Command Line: `ask` answers what it asks.
    pub fn typed(&self, line: &str, ask: impl Fn(String) -> bool + Send) -> Typed {
        let (tx, rx) = std::sync::mpsc::channel();
        let (asks, questions) = std::sync::mpsc::channel();
        let gone = || Typed { output: Err(GONE.to_string()), configuration: false, edit: vec![] };
        if self.edits.send(Job::Typed(line.to_string(), tx, Some(asks))).is_err() {
            return gone();
        }
        std::thread::scope(|scope| {
            scope.spawn(move || answering(questions, ask));
            rx.recv().unwrap_or_else(|_| gone())
        })
    }

    /// What may come next after `before` (the line up to the cursor): `?` and Tab's answer.
    pub fn choices(&self, at: At, before: &str) -> Vec<Choice> {
        let (tx, rx) = std::sync::mpsc::channel();
        if self.queue(at).send(Job::Choices(at, before.to_string(), tx)).is_err() {
            return vec![];
        }
        rx.recv().unwrap_or_default()
    }

    /// Whether the person's draft differs from the configuration in effect.
    pub fn uncommitted(&self) -> bool {
        let (tx, rx) = std::sync::mpsc::channel();
        if self.watch.send(Job::Uncommitted(tx)).is_err() {
            return false;
        }
        rx.recv().unwrap_or(false)
    }
}

const GONE: &str = "torqcli's session in this window has stopped";

/// Lines cleaned of anything that could fool the eye: the ledger's reasons are not ours.
fn cleaned(out: String) -> String {
    out.lines().map(|l| format!("{}\n", history::clean(l))).collect()
}

/// A worker: its sessions, and the jobs for them, in order.
fn serve(sessions: &'static [At], jobs: std::sync::mpsc::Receiver<Job>) {
    let cli = cli();
    // What a command asks a person goes to whoever gave this job its `Asks`; with none, there is nobody to ask.
    let asks: std::rc::Rc<std::cell::RefCell<Option<Asks>>> = Default::default();
    let asking = asks.clone();
    let real = crate::shell::Real::asking(
        &cli,
        Box::new(move |question| {
            use crate::session::Answer;
            let Some(tx) = asking.borrow().clone() else { return Answer::NobodyToAsk };
            let (back, answer) = std::sync::mpsc::channel();
            if tx.send((cleaned(question.to_string()), back)).is_err() {
                return Answer::NobodyToAsk;
            }
            match answer.recv() {
                Ok(true) => Answer::Yes,
                Ok(false) => Answer::No,
                Err(_) => Answer::NobodyToAsk,
            }
        }),
    );
    let session = |at: At| open_session(&cli, &real, Some(crate::candidate::Candidate::default_path()?), at);
    let mut made: Vec<(At, Result<crate::session::Session<'_>, String>)> = sessions.iter().map(|&at| (at, session(at).map_err(|e| said(&e)))).collect();
    for job in jobs {
        match job {
            Job::Run(at, line, reply, question) => {
                *asks.borrow_mut() = question;
                let r = match of(&mut made, at) {
                    Some(Ok(s)) => {
                        s.reread();
                        s.exec(&line).map(cleaned).map_err(|e| said(&e))
                    }
                    Some(Err(e)) => Err(e.clone()),
                    None => Err(GONE.to_string()),
                };
                *asks.borrow_mut() = None;
                let _ = reply.send(r);
            }
            Job::Typed(line, reply, question) => {
                *asks.borrow_mut() = question;
                let t = match of(&mut made, At::Typed) {
                    Some(Ok(s)) => {
                        s.reread();
                        let output = match refused_here(s.mode, &line) {
                            Some(why) => Err(why.to_string()),
                            None => s.exec(&line).map(cleaned).map_err(|e| said(&e)),
                        };
                        // `exit` at the top of operational mode: the window stays, and so does the session.
                        s.done = false;
                        Typed { output, configuration: s.mode == Mode::Configuration, edit: s.edit_path().to_vec() }
                    }
                    Some(Err(e)) => Typed { output: Err(e.clone()), configuration: false, edit: vec![] },
                    None => Typed { output: Err(GONE.to_string()), configuration: false, edit: vec![] },
                };
                *asks.borrow_mut() = None;
                let _ = reply.send(t);
            }
            Job::Choices(at, before, reply) => {
                let c = match of(&mut made, at) {
                    Some(Ok(s)) => {
                        s.reread();
                        choices_of(s, &before)
                    }
                    _ => vec![],
                };
                let _ = reply.send(c);
            }
            Job::Uncommitted(reply) => {
                let u = match of(&mut made, At::Watch) {
                    Some(Ok(s)) => {
                        s.reread();
                        s.uncommitted().unwrap_or(false)
                    }
                    _ => false,
                };
                let _ = reply.send(u);
            }
        }
    }
}

/// The complete words before the cursor, and the word being typed; None inside an open quote.
fn at_cursor(before: &str) -> Option<(Vec<String>, String)> {
    crate::session::words(before).ok()?;
    let partial = if before.ends_with(char::is_whitespace) { "" } else { before.rsplit(char::is_whitespace).next().unwrap_or("") };
    let done = &before[..before.len() - partial.len()];
    Some((crate::session::words(done).ok()?, partial.to_string()))
}

fn choices_of(s: &mut crate::session::Session<'_>, before: &str) -> Vec<Choice> {
    let Some((words, partial)) = at_cursor(before) else { return vec![] };
    let at = crate::complete::At {
        mode: s.mode,
        edit: s.edit_path().to_vec(),
        data: s.help_data(),
        configurations: crate::complete::Later::new(Vec::new),
        files: s.nix_files(),
        interfaces: vec![],
    };
    crate::complete::choices_at(&crate::complete::Know { schema: &s.schema, tree: &s.tree }, &at, &words, &partial)
}

/// Whether a line must go to a terminal: it has a person type a new secret, or opens their editor. A yes or no
/// is asked in the window, and an approval in COSMIC's own dialog (ADR-045 5), never a password in a window
/// TorqOS draws.
pub fn needs_terminal(mode: Mode, line: &str) -> bool {
    fn types(op: &crate::syntax::Op) -> bool {
        use crate::syntax::{FileAction, LoadFrom, Op, Request};
        match op {
            Op::Request(Request::Password { .. } | Request::BackupKey { .. }) => true,
            Op::File { action: FileAction::Edit, .. } => true,
            Op::Load { from: LoadFrom::Terminal, .. } => true,
            Op::Run(op) => types(op),
            Op::Top { then: Some(op) } => types(op),
            _ => false,
        }
    }
    crate::session::words(line).ok().and_then(|w| crate::grammar::read_line(mode, &w).ok()).is_some_and(|l| types(&l.op))
}

/// What the window will not do, and why: a line that would hold the shared draft for this window alone,
/// and shut out its own pages and the terminal a commit goes to.
pub fn refused_here(mode: Mode, line: &str) -> Option<&'static str> {
    let op = crate::session::words(line).ok().and_then(|w| crate::grammar::read_line(mode, &w).ok())?.op;
    matches!(op, crate::syntax::Op::Configure { exclusive: true }).then_some("configure exclusive holds the draft for one session: use it in a terminal, where it is yours alone")
}

/// Whether a line only loads something into the draft, however long that takes: it runs with the draft's
/// session, from configuration mode (there an update only loads), so that what the pages read is not held up.
pub fn loads_draft(line: &str) -> bool {
    use crate::syntax::{Op, Request};
    crate::session::words(line).ok().and_then(|w| crate::grammar::read_line(Mode::Operational, &w).ok()).is_some_and(|l| matches!(l.op, Op::Request(Request::Update { .. } | Request::Pull | Request::SoftwareRollback { .. })))
}

/// Run `script` (torqcli's `-c`) in COSMIC's terminal, and wait until it is closed: `draft`, on the person's
/// saved candidate. It stays open at the end, for what happened to be read. Whether the script succeeded.
pub fn in_terminal(script: &str, draft: bool) -> Result<bool, String> {
    let run = if draft { "torqcli --candidate -c \"$1\"" } else { "torqcli -c \"$1\"" };
    let shell = format!("{run}; echo $? > \"$2\"; echo; read -r -p 'Press Enter to close. ' _");
    let dir = tempfile::tempdir().map_err(|e| format!("making a private folder: {e}"))?;
    let status = dir.path().join("status");
    std::process::Command::new("cosmic-term")
        // Not a daemon: it would return at once, with the commit still asking.
        .args(["--no-daemon", "-e", "sh", "-c", &shell, "sh", script])
        .arg(&status)
        .status()
        .map_err(|e| format!("opening COSMIC's terminal: {e}"))?;
    Ok(std::fs::read_to_string(&status).is_ok_and(|s| s.trim() == "0"))
}

/// A line as `-c` reads it, each word quoted: a `;` inside a typed word must not split the script.
fn requoted(line: &str) -> String {
    match crate::session::words(line) {
        Ok(w) => w.iter().map(|x| if x == "|" { x.clone() } else { crate::syntax::quote(x) }).collect::<Vec<_>>().join(" "),
        Err(_) => line.to_string(),
    }
}

/// The script `in_terminal` runs for a line of the given mode: in configuration mode the draft's, at the
/// place (`edit`) the line was typed at, whose meaning depends on it.
pub fn terminal_script(mode: Mode, edit: &[String], line: &str) -> (String, bool) {
    let line = requoted(line);
    match mode {
        Mode::Configuration if edit.is_empty() => (format!("configure; {line}"), true),
        Mode::Configuration => {
            let place: Vec<&str> = std::iter::once("edit").chain(edit.iter().map(String::as_str)).collect();
            (format!("configure; {}; {line}", self::line(&place)), true)
        }
        Mode::Operational => (line, false),
    }
}

// ── The pages that are the command line's output ─────────────────────────────────────────────────

/// How a button acts on its command.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum How {
    /// Run it (in a terminal, if it asks: `needs_terminal`).
    Run,
    /// Open the Commands page with it typed so far, to finish: operational mode.
    FillOp,
    /// The same, in configuration mode.
    FillCf,
}

/// A button: its label, the torqcli command, and how it acts.
#[derive(Clone, Copy, Debug)]
pub struct Action {
    pub label: &'static str,
    pub command: &'static str,
    pub how: How,
}

/// A page that shows commands' output and offers actions: nothing on it is the window's own.
#[derive(Clone, Copy, Debug)]
pub struct View {
    /// Its name on the command line of the application (`torq-settings data`).
    pub id: &'static str,
    pub name: &'static str,
    pub icon: &'static str,
    /// A title, and the command whose output is that section.
    pub sections: &'static [(&'static str, &'static str)],
    pub actions: &'static [Action],
}

const fn run(label: &'static str, command: &'static str) -> Action {
    Action { label, command, how: How::Run }
}
const fn op(label: &'static str, command: &'static str) -> Action {
    Action { label, command, how: How::FillOp }
}
const fn cf(label: &'static str, command: &'static str) -> Action {
    Action { label, command, how: How::FillCf }
}

/// What the agent may do without asking, and every button on the Proposals page besides each proposal's own.
pub const AGENT: &[Action] = &[op("Trust the agent for a while…", "request system agent trust "), run("End that trust now", "request system agent trust revoke")];

pub static VIEWS: &[View] = &[
    View {
        id: "software",
        name: "Software",
        icon: "software-update-available-symbolic",
        sections: &[("What this machine runs", "show version")],
        actions: &[
            run("Load the newest TorqOS into the draft", "request system update"),
            run("Load everything newer, nixpkgs too", "request system update all"),
            op("Load an earlier configuration's software…", "request system software rollback "),
            op("Search the software you can install…", "show packages "),
            cf("Add a package for everyone…", "set environment systemPackages "),
            cf("Add a package for one person…", "set users users "),
            cf("Remove a package…", "delete environment systemPackages "),
        ],
    },
    View {
        id: "services",
        name: "Services",
        icon: "network-server-symbolic",
        sections: &[("Turned on by the configuration", "show services"), ("The network", "show interfaces")],
        actions: &[cf("Turn a service on or change it…", "set services "), cf("Turn a service off…", "delete services ")],
    },
    View {
        id: "data",
        name: "Data",
        icon: "drive-harddisk-system-symbolic",
        sections: &[("Snapshots, each from before a change", "show system snapshots"), ("Backups", "show system backups"), ("Disks", "show system storage")],
        actions: &[
            run("Take a snapshot now", "request system snapshot take"),
            op("Put a path back from a snapshot…", "request system snapshot restore "),
            op("Delete a snapshot…", "request system snapshot delete "),
            cf("Declare a backup…", "set services restic backups "),
            op("Give a backup its key…", "request system backup key "),
            op("Put a path back from a backup…", "request system restore "),
        ],
    },
    View {
        id: "machine",
        name: "This Machine",
        icon: "computer-symbolic",
        sections: &[("How long it has run", "show system uptime"), ("People", "show users"), ("Its configuration's remote", "show system remote"), ("Nix files of the configuration", "file list")],
        actions: &[
            run("Change your password", "request system password"),
            op("Change someone's password…", "request system password "),
            run("Return the machine to its history", "request system repair"),
            run("Load what waits on the remote into the draft", "request system pull"),
            run("Show this machine's key for the remote", "request system remote key"),
            run("Clear the remote's alarm", "request system remote clear"),
            run("Say no more of a ledger disagreement", "request system ledger acknowledge"),
            run("Cancel the commit scheduled", "clear system commit"),
        ],
    },
];

#[cfg(test)]
mod page_tests {
    use super::*;
    use crate::grammar::{read_line, walk};
    use crate::session::words;

    fn whole(mode: Mode, c: &str) {
        read_line(mode, &words(c).unwrap()).unwrap_or_else(|e| panic!("`{c}` is not a command of torqcli: {e:#}"));
    }

    /// Every action the window offers is a torqcli command (ADR-045 2): the grammar's own reading of it.
    #[test]
    fn everything_the_pages_offer_is_a_command() {
        let check = |a: &Action| match a.how {
            How::Run => whole(Mode::Operational, a.command),
            How::FillOp => assert!(walk(Mode::Operational, &words(a.command).unwrap()).is_ok(), "`{}`", a.command),
            How::FillCf => assert!(walk(Mode::Configuration, &words(a.command).unwrap()).is_ok(), "`{}`", a.command),
        };
        for v in VIEWS {
            for (_, c) in v.sections {
                whole(Mode::Operational, c);
            }
            v.actions.iter().for_each(check);
        }
        AGENT.iter().for_each(check);
        for c in ["show system alarms", "show system ledger", "show system proposals", "show system commit", "help nginx"] {
            whole(Mode::Operational, c);
        }
        for c in ["show", "show | compare", "commit check", "commit comment \"x\" confirmed 5", "commit at 12:00", "rollback 0", "rollback 3", "set a b", "delete a b"] {
            whole(Mode::Configuration, c);
        }
        whole(Mode::Operational, "request system revert 5");
        whole(Mode::Operational, "request system apply 5");
        whole(Mode::Configuration, "commit");
    }

    /// Only what has a person type a secret, or edit a file, goes to a terminal.
    #[test]
    fn what_goes_to_a_terminal() {
        let t = |m, c| needs_terminal(m, c);
        for c in ["file edit a.nix", "load merge terminal", "run request system password", "top load set terminal"] {
            assert!(t(Mode::Configuration, c), "{c}");
        }
        for c in ["request system password", "request system password sam", "request system backup key b"] {
            assert!(t(Mode::Operational, c), "{c}");
        }
        for c in ["commit", "commit comment \"x\"", "commit confirmed 5", "commit at 12:00", "commit check", "set a b", "delete", "top delete", "rollback 1", "show | compare", "run request system repair"] {
            assert!(!t(Mode::Configuration, c), "{c}: asked in the window, approved in the dialog");
        }
        for c in ["request system revert 5", "request system apply 5", "request system repair", "request system snapshot take", "clear system commit", "show system alarms", "request system update"] {
            assert!(!t(Mode::Operational, c), "{c}");
        }
    }

    #[test]
    fn a_line_is_quoted_where_it_must_be() {
        assert_eq!(line(&["set", "a", "b c"]), "set a \"b c\"");
    }

    #[test]
    fn the_terminal_runs_the_draft_from_configuration_mode() {
        assert_eq!(terminal_script(Mode::Configuration, &[], "commit"), ("configure; commit".to_string(), true));
        // A bare `delete` at a place deletes that place: never everything.
        assert_eq!(terminal_script(Mode::Configuration, &["services".into(), "openssh".into()], "delete"), ("configure; edit services openssh; delete".to_string(), true));
        // A `;` in a word stays in the word.
        assert_eq!(terminal_script(Mode::Configuration, &[], "commit comment \"a;b\""), ("configure; commit comment \"a;b\"".to_string(), true));
        assert_eq!(terminal_script(Mode::Operational, &[], "request system repair"), ("request system repair".to_string(), false));
    }
}

/// One of a worker's sessions.
fn of<'b, 's>(made: &'b mut [(At, Result<crate::session::Session<'s>, String>)], at: At) -> Option<&'b mut Result<crate::session::Session<'s>, String>> {
    made.iter_mut().find(|(a, _)| *a == at).map(|(_, s)| s)
}

/// A proposal waiting for a person.
#[derive(Debug, Clone)]
pub struct Proposal {
    pub seq: u64,
    pub reason: String,
    pub by: String,
    pub tainted: bool,
}

/// The proposals waiting: `show system proposals`' rows.
pub fn proposals() -> Result<Vec<Proposal>> {
    let entries = crate::read_ledger(&cli())?;
    Ok(history::waiting_proposals(&entries)
        .into_iter()
        .map(|e| Proposal { seq: e.seq, reason: history::one_line(&e.reason, 300), by: history::one_line(&e.actor, 40), tainted: e.tainted })
        .collect())
}

/// `show system commit`'s configurations: each number (for `rollback <n>`) and its line.
pub fn configurations(shown: &str) -> Vec<(u32, String)> {
    shown
        .lines()
        .filter_map(|l| {
            let (n, rest) = l.trim().split_once(char::is_whitespace)?;
            Some((n.parse().ok()?, rest.trim().to_string()))
        })
        .collect()
}

/// COSMIC's own dialog for restarting: it asks first, and counts down.
pub fn ask_restart() {
    spawn("cosmic-osd", &["restart"]);
}

/// COSMIC's own dialog for logging out: it asks first, and counts down.
pub fn ask_log_out() {
    spawn("cosmic-osd", &["log-out"]);
}

/// TorqOS Settings, at a page (`VIEWS`' ids, or overview, configuration, changes, history, proposals, commands).
pub fn open_settings(page: &str) {
    spawn("torq-settings", &[page]);
}

fn spawn(program: &str, args: &[&str]) {
    let _ = std::process::Command::new(program).args(args).spawn();
}

#[cfg(test)]
#[test]
fn the_configurations_shown_with_their_numbers() {
    let shown = "Rollback  Change  When  By  What\n0         #16     then  me  One\n1         #14     then  me  Two\n";
    assert_eq!(configurations(shown), vec![(0, "#16     then  me  One".to_string()), (1, "#14     then  me  Two".to_string())]);
}

#[cfg(test)]
#[test]
fn the_window_keeps_what_would_lock_it_out() {
    assert!(refused_here(Mode::Operational, "configure exclusive").is_some());
    assert!(refused_here(Mode::Operational, "configure").is_none());
    assert!(loads_draft("request system update") && loads_draft("request system pull"));
    assert!(!loads_draft("show version") && !loads_draft("request system repair"));
}

// ── What the window's tree and diff viewer read ──────────────────────────────────────────────────

/// The settings of a configuration, from its `| display set` lines: each one's path and its value.
/// A list has one line per value.
pub fn settings_of(shown: &str) -> Vec<(Vec<String>, String)> {
    shown
        .lines()
        .filter_map(|l| {
            let mut w = crate::session::words(l.strip_prefix("set ")?).ok()?;
            let value = w.pop()?;
            (!w.is_empty()).then_some((w, value))
        })
        .collect()
}

/// A change's configuration number, from `show system commit`'s lines (0 is the one in effect); None: it
/// made no configuration.
pub fn rollback_of(shown: &str, seq: u64) -> Option<u32> {
    let wanted = format!("#{seq}");
    configurations(shown).into_iter().find(|(_, rest)| rest.split_whitespace().next() == Some(wanted.as_str())).map(|(n, _)| n)
}

/// What a proposal does, as the broker had git write it when it was proposed: never the proposer's own
/// text, which can say one thing while git does another (ADR-041).
pub fn proposal_effect(seq: u64) -> Result<String> {
    let cli = cli();
    let log = crate::run::watch_log()?;
    let v = match crate::run::broker_json(crate::broker(&cli, &["show", &seq.to_string()])?, &log)? {
        crate::run::Outcome::Done(v) => v,
        crate::run::Outcome::Busy => anyhow::bail!("another change is being made right now; try again in a moment"),
        crate::run::Outcome::Refused(m) | crate::run::Outcome::Broken(m) => anyhow::bail!("{}", history::one_line(&m, 300)),
    };
    Ok(cleaned(v["data"]["effect"].as_str().unwrap_or_default().to_string()))
}

#[cfg(test)]
mod tree_tests {
    use super::*;

    #[test]
    fn a_configuration_read_as_settings() {
        let shown = "set networking hostName torqtop\nset environment systemPackages htop\nset environment systemPackages jq\nset users users casey description \"Casey D\"\n## software: torq 2026\n";
        let s = settings_of(shown);
        assert_eq!(s[0], (vec!["networking".to_string(), "hostName".to_string()], "torqtop".to_string()));
        assert_eq!((s[1].1.as_str(), s[2].1.as_str()), ("htop", "jq"), "a list, a line for each");
        assert_eq!(s[3].1, "Casey D", "a value with a space is one value");
        assert_eq!(s.len(), 4, "a comment is not a setting");
    }

    #[test]
    fn a_change_finds_its_configuration() {
        let shown = "Rollback  Change  When  By  What\n0         #16     then  me  One\n1         #14     then  me  Two\n";
        assert_eq!(rollback_of(shown, 16), Some(0));
        assert_eq!(rollback_of(shown, 14), Some(1));
        assert_eq!(rollback_of(shown, 1), None, "not #1 of #14 or #16");
    }
}

/// A session of torqcli's on this machine: `saved`, the candidate it works on (None: its own).
fn open_session<'a>(cli: &Cli, machine: &'a dyn crate::session::Machine, saved: Option<PathBuf>, at: At) -> Result<crate::session::Session<'a>> {
    use anyhow::Context as _;
    let schema = crate::tree::Schema::parse(&std::fs::read_to_string(crate::shell::SCHEMA).with_context(|| format!("reading {} (is this TorqOS?)", crate::shell::SCHEMA))?)?;
    let tree = crate::nixos::Tree::parse(&std::fs::read_to_string(crate::shell::NIXOS_SCHEMA).with_context(|| format!("reading {}", crate::shell::NIXOS_SCHEMA))?)?;
    let who = crate::me()?;
    let mut s = crate::session::Session::new(schema, tree, crate::candidate::Repo::new(&cli.repo), saved, machine, &who);
    s.cache = crate::evalcache::Cache::personal();
    if at == At::Draft {
        s.exec("configure")?;
    }
    Ok(s)
}

/// Keep a commit confirmed waiting: `commit` of an empty candidate, a private one, always, so that the
/// person's draft is never swept in with it.
pub fn confirm() -> Result<String> {
    let cli = cli();
    let real = crate::shell::Real::new(&cli);
    let mut s = open_session(&cli, &real, None, At::Draft)?;
    Ok(cleaned(s.exec("commit")?))
}

/// A job's questions, each answered in turn, until the job has no more.
fn answering(questions: std::sync::mpsc::Receiver<(String, std::sync::mpsc::Sender<bool>)>, ask: impl Fn(String) -> bool) {
    for (question, back) in questions {
        let _ = back.send(ask(question));
    }
}
