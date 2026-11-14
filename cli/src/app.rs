//! What TorqOS Settings (ADR-045 2) asks of the command line: the same calls to the broker and the same
//! words, so the window says what `torqcli` would print. Nothing here is the window's own: a word the GUI
//! has and the command line has not is a bug in one of them.

use crate::{history, status, Cli, BROKER, REPO};
use anyhow::Result;
use std::collections::BTreeSet;
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
            Needs::NextStart(what) => Some(format!("A new {} is installed: it takes effect the next time this machine starts.", and(what))),
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
    // Only a new kernel, or a module added to the running one, waits for a start. A boot image and firmware are
    // not news: the first is read only at boot, and firmware when a driver loads.
    if link(booted, "kernel") != link(current, "kernel") {
        return Needs::NextStart(vec!["Linux kernel"]);
    }
    if link(booted, "kernel-modules") != link(current, "kernel-modules") {
        return Needs::NextStart(vec!["kernel module"]);
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
        assert!(Needs::NextStart(vec!["Linux kernel"]).note().unwrap().contains("next time this machine starts"));
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
    let waiting = waiting_on_me(&entries, &crate::me().unwrap_or_default());
    let needs = needs(std::path::Path::new("/run/booted-system"), std::path::Path::new("/run/current-system"), session);
    Ok(Glance {
        version: version(),
        well,
        alarm,
        confirm,
        draft,
        needs,
        waiting: (waiting.len(), waiting.iter().filter(|e| e.tainted).count()),
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
                        s.follow_schemas(std::path::Path::new(crate::shell::SCHEMA), std::path::Path::new(crate::shell::NIXOS_SCHEMA));
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
                        s.follow_schemas(std::path::Path::new(crate::shell::SCHEMA), std::path::Path::new(crate::shell::NIXOS_SCHEMA));
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
                        s.follow_schemas(std::path::Path::new(crate::shell::SCHEMA), std::path::Path::new(crate::shell::NIXOS_SCHEMA));
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
                        s.follow_schemas(std::path::Path::new(crate::shell::SCHEMA), std::path::Path::new(crate::shell::NIXOS_SCHEMA));
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

/// Whether a line must go to a terminal: it has a person type a new secret or prints one, or opens their editor. A yes or no
/// is asked in the window, and an approval in COSMIC's own dialog (ADR-045 5), never a password in a window
/// TorqOS draws.
pub fn needs_terminal(mode: Mode, line: &str) -> bool {
    fn types(op: &crate::syntax::Op) -> bool {
        use crate::syntax::{FileAction, LoadFrom, Op, Request};
        match op {
            Op::Request(Request::Password { .. } | Request::BackupKey { .. }) => true,
            // A secret's value typed, or printed: never in the window. Its Secrets page takes one in masked fields, by
            // another way (`secret_set`), and shows none (ADR-052 step 5).
            Op::Request(Request::Secret { act: crate::secrets::Act::Set | crate::secrets::Act::Show, .. }) => true,
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

// ── The Secrets page: the list, and a value typed in the window (ADR-052 step 5) ──────────────────

pub use torq_wire::secrets::{Approve, Kind};

/// A secret as the Secrets page lists it: `show system secrets`' own reading (`secrets::listed`). Never its
/// value: nothing that makes one of these has read it.
#[derive(Debug, Clone, PartialEq)]
pub struct Secret {
    pub name: String,
    /// Whose it is (None: the system's).
    pub user: Option<String>,
    /// What it is and what it may be used for, in the words `show system secrets` prints.
    pub said: String,
    pub has_value: bool,
    /// What it is (None: it has no rule yet, so nothing has said).
    pub kind: Option<Kind>,
    /// The hosts it may be sent to in an HTTPS header.
    pub https: Vec<String>,
    /// The hosts it may sign for over SSH.
    pub ssh: Vec<String>,
    /// Whose agents may use it, by person: a person's own secret, that person's alone (`torq-agent` in their
    /// rules); a system secret, each person whose agent's account it names (`torq-agent-<person>`).
    pub agents: Vec<String>,
    /// When a person is asked before the agent uses it.
    pub approve: Approve,
}

fn secret_of(l: crate::secrets::Listed) -> Secret {
    let said = l.said();
    let r = l.rule;
    // A person's own rules say `torq-agent` for their own agent; the system's name each by its account.
    let agents = match &l.owner {
        Some(u) => r.as_ref().filter(|r| r.actors.iter().any(|a| a == torq_wire::secrets::AGENT)).map(|_| u.clone()).into_iter().collect(),
        None => r.iter().flat_map(|r| &r.actors).filter_map(|a| a.strip_prefix(&format!("{}-", torq_wire::secrets::AGENT))).map(String::from).collect(),
    };
    Secret {
        name: l.name,
        user: l.owner,
        said,
        has_value: l.has_value,
        kind: r.as_ref().map(|r| r.kind),
        https: r.as_ref().and_then(|r| r.https.as_ref()).map(|h| h.hosts.clone()).unwrap_or_default(),
        ssh: r.as_ref().and_then(|r| r.ssh.as_ref()).map(|s| s.hosts.clone()).unwrap_or_default(),
        agents,
        approve: r.map(|r| r.approve).unwrap_or_default(),
    }
}

/// What `show system secrets` lists, each secret apart, and why each rules file that could not be read was not.
pub fn secrets_status() -> Result<(Vec<Secret>, Vec<String>)> {
    let cli = cli();
    let kept = crate::shell::Real::new(&cli).kept_secrets(&crate::run::watch_log()?)?;
    let (mut secrets, mut unread) = (Vec::new(), Vec::new());
    for l in crate::secrets::listed(&crate::candidate::Repo::new(&cli.repo), &kept)? {
        match l {
            Ok(l) => secrets.push(secret_of(l)),
            Err(why) => unread.push(why),
        }
    }
    Ok((secrets, unread))
}

/// Before the window holds a secret's value: no other program of the same account can read it out of the
/// window's memory, and a crash leaves no core with it, as torqcli is while it holds one (ADR-040; shell.rs).
/// It stays so until the window is closed.
pub fn secret_guard() -> Result<()> {
    use anyhow::Context as _;
    rustix::process::set_dumpable_behavior(rustix::process::DumpableBehavior::NotDumpable).context("protecting the value")
}

/// `request system secret set <name> [user <person>]`, its value typed twice in the window's masked fields
/// rather than at a terminal. It goes to the broker as torqcli's own goes (`Real::keep_secret`): the request's own
/// field, and no part of any command, argument, log or message. Who may set it is the broker's to say, as for
/// torqcli: a person's own is theirs, the system's an administrator's, approved in the dialog. A refusal here
/// repeats nothing that was typed, the name included (a value typed in the wrong field).
pub fn secret_set(name: &str, user: Option<&str>, value: &str, again: &str) -> Result<String> {
    crate::secrets::typed_ok(value, again)?;
    if !torq_wire::secrets::name_ok(name) {
        anyhow::bail!("not changed: that is not a secret's name (letters, digits, - and _)");
    }
    secret_guard()?;
    let (cli, me, log) = (cli(), crate::me()?, crate::run::start_log()?);
    let real = crate::shell::Real::new(&cli);
    real.keep_secret(name, user, value, &me, &log)
}

#[cfg(test)]
mod secret_tests {
    use super::*;

    const VALUE: &str = "ghp_window/Value+7=";

    /// A stand-in for the broker: what it was run with, its environment and what the value's file held go to
    /// `dir`, the value's file must be the caller's alone, and it answers as the broker would.
    fn broker(dir: &std::path::Path, answer: &str) -> Cli {
        let script = dir.join("broker.sh");
        let d = dir.display();
        std::fs::write(
            &script,
            format!("printf '%s\\n' \"$@\" > {d}/argv\nenv > {d}/env\nwhile [ $# -gt 0 ]; do [ \"$1\" = --value-file ] && f=$2; shift; done\n[ \"$(stat -c %a \"$f\")\" = 600 ] || exit 3\ncat \"$f\" > {d}/got\nprintf %s \"$f\" > {d}/file\necho 'the broker says on stderr' >&2\n{answer}\n"),
        )
        .unwrap();
        Cli { repo: PathBuf::from(REPO), broker: format!("sh {}", script.display()) }
    }

    /// Rule 1 of the Secrets page: the value reaches the broker in its private file, and is nowhere else:
    /// not an argument, not the environment, not the log, not what is said back, kept or refused.
    #[test]
    fn a_value_from_the_window_is_in_its_private_file_and_nowhere_else() {
        for (answer, kept) in [("echo '{\"seq\":7}'", true), ("echo '{\"error\":\"not kept: the broker would not\"}'; exit 1", false)] {
            let d = tempfile::tempdir().unwrap();
            let cli = broker(d.path(), answer);
            let log = d.path().join("last.log");
            let said = crate::shell::Real::new(&cli).keep_secret("gh", Some("dev"), VALUE, "dev", &log);
            let read = |f: &str| std::fs::read_to_string(d.path().join(f)).unwrap();
            assert_eq!(read("got"), VALUE, "the broker is given the value, whole");
            assert!(!std::path::Path::new(&read("file")).exists(), "its file is gone once the broker has answered");
            let said = match (said, kept) {
                (Ok(s), true) => {
                    assert!(s.starts_with("The value of dev's secret gh is kept (ledger #7)"), "{s}");
                    s
                }
                (Err(e), false) => {
                    let e = format!("{e:#}");
                    assert_eq!(e, "request system secret set gh refused: not kept: the broker would not");
                    e
                }
                (other, _) => panic!("{other:?}"),
            };
            assert_eq!(read("argv").lines().take(5).collect::<Vec<_>>(), ["secret-set", "--name", "gh", "--user", "dev"]);
            for (what, text) in [("what is said", said), ("the arguments", read("argv")), ("the environment", read("env")), ("the log", read("last.log"))] {
                assert!(!text.contains(VALUE), "{what} holds the value: {text}");
            }
            assert!(read("last.log").contains("the broker says on stderr"), "the log is the one written to");
        }
    }

    /// Refused before anything is sent or logged, in words that repeat nothing typed: no value, the two not the
    /// same, a value larger than the broker keeps, and a name that is not one.
    #[test]
    fn what_the_window_refuses_says_nothing_that_was_typed() {
        let long = "k".repeat(crate::secrets::VALUE_MAX + 1);
        for (name, value, again, why) in [
            ("gh", "", "", "not changed: no value"),
            ("gh", VALUE, "ghp_window/Value+8=", "not changed: the two differ"),
            ("gh", long.as_str(), long.as_str(), "not changed: a value is at most 16 KiB"),
            (VALUE, "v", "v", "not changed: that is not a secret's name (letters, digits, - and _)"),
            ("../gh", "v", "v", "not changed: that is not a secret's name (letters, digits, - and _)"),
        ] {
            assert_eq!(format!("{:#}", secret_set(name, Some("dev"), value, again).unwrap_err()), why);
        }
    }

    /// The page's row is the list's own reading: its words, and the rule apart. Nothing in it is a value.
    #[test]
    fn a_listed_secret_as_the_page_has_it() {
        use torq_wire::secrets::{Https, Rule, Ssh, AGENT};
        let mut rule = Rule::new(Kind::Token);
        rule.https = Some(Https { hosts: vec!["api.github.com".into(), "uploads.github.com".into()], header: "Authorization".into(), prefix: "Bearer ".into() });
        rule.actors = vec![AGENT.into()];
        rule.approve = Approve::Never;
        let s = secret_of(crate::secrets::Listed { owner: Some("dev".into()), name: "gh".into(), has_value: true, rule: Some(rule) });
        assert_eq!(s.said, "token, has a value; https to api.github.com, uploads.github.com; the agent too, approved never");
        assert_eq!((s.name.as_str(), s.user.as_deref(), s.has_value, s.kind), ("gh", Some("dev"), true, Some(Kind::Token)));
        assert_eq!((s.https, s.ssh, s.agents, s.approve), (vec!["api.github.com".to_string(), "uploads.github.com".to_string()], vec![], vec!["dev".to_string()], Approve::Never));
        let mut rule = Rule::new(Kind::SshKey);
        rule.ssh = Some(Ssh { hosts: vec!["github.com".into()], known: vec![] });
        let s = secret_of(crate::secrets::Listed { owner: None, name: "deploy".into(), has_value: false, rule: Some(rule.clone()) });
        assert_eq!((s.kind, s.https, s.ssh, s.agents, s.approve), (Some(Kind::SshKey), vec![], vec!["github.com".to_string()], Vec::<String>::new(), Approve::FirstUse));
        // A system secret names each agent by its account: the page has the people they serve.
        rule.actors = vec![format!("{AGENT}-alice"), format!("{AGENT}-bob")];
        let s = secret_of(crate::secrets::Listed { owner: None, name: "deploy".into(), has_value: false, rule: Some(rule) });
        assert_eq!(s.agents, vec!["alice".to_string(), "bob".to_string()]);
        // A value with no rule yet: nothing says what it is.
        let s = secret_of(crate::secrets::Listed { owner: None, name: "new".into(), has_value: true, rule: None });
        assert_eq!((s.said.as_str(), s.kind, s.agents), ("has a value; used for nothing", None, Vec::<String>::new()));
    }
}

// ── A use of a secret that waits for its person, and what they let their agent use without asking ─

use crate::secrets::{Approval, Place};

/// A use of a secret by the person's agent that waits for their answer, as the broker keeps it
/// (`secret-waiting`). All of it is the broker's own knowledge but `agent_says`.
#[derive(Debug, Clone, PartialEq)]
pub struct WaitingUse {
    /// Its number: what an answer names.
    pub n: u64,
    pub secret: String,
    /// Whose secret it is (None: the system's).
    pub user: Option<String>,
    pub host: String,
    /// A signature for SSH to the host, not an HTTPS request to it.
    pub ssh: bool,
    /// A program run as the agent asked (a coding tool), not the agent itself.
    pub tool: bool,
    /// Its session read untrusted text, or it names no session.
    pub tainted: bool,
    /// It names a session: a no can be for the rest of it.
    pub session: bool,
    /// When its time is over, in seconds since the epoch (None: the broker's time could not be read).
    pub until: Option<i64>,
    /// The agent's own words for it: one line, cleaned again here. Never a fact.
    pub agent_says: String,
}

/// The uses in the broker's answer. One that does not say what it is (no number, secret, host or owner) is left
/// out: nothing is shown of it, and nothing can be answered. Where the answer does not say a use is the agent's own
/// from a session that read nothing untrusted, it is taken as the more doubtful: a coding tool's, tainted.
fn uses_of(v: &serde_json::Value) -> Vec<WaitingUse> {
    let one = |a: &serde_json::Value| -> Option<WaitingUse> {
        let f = &a["facts"];
        let user = match &f["owner"] {
            serde_json::Value::String(s) if s == "system" => None,
            o => Some(history::one_line(o["person"].as_str()?, 40)),
        };
        Some(WaitingUse {
            n: a["n"].as_u64().filter(|n| *n > 0)?,
            secret: history::one_line(f["secret"].as_str()?, 80),
            user,
            host: history::one_line(f["host"].as_str()?, 255),
            ssh: f["ssh"] == true,
            tool: match (f["caller"].as_str(), f["agent"].as_str()) {
                (Some(caller), Some(agent)) => caller == agent,
                _ => true,
            },
            tainted: f["tainted"] != false,
            session: f["session"].is_string(),
            until: a["until"].as_str().and_then(history::parse_ts),
            agent_says: history::one_line(a["agent_says"].as_str().unwrap_or_default(), AGENT_SAYS_MAX),
        })
    };
    let mut uses: Vec<WaitingUse> = v["waiting"].as_array().into_iter().flatten().filter_map(one).collect();
    uses.sort_by_key(|u| u.n);
    uses
}

/// The most of the agent's own words the dialog shows (the broker keeps no more).
const AGENT_SAYS_MAX: usize = 200;
/// How wide a line of the agent's words is drawn, in columns of a fixed-width font, after its mark.
const AGENT_SAYS_COLUMNS: usize = 30;
/// What every line of the agent's words begins with, as drawn: a left-to-right mark (U+200E), then the visible
/// mark. Without the first, a line whose first letters are Hebrew or Arabic is laid out from the right and its
/// visible mark is drawn at the far end; a left-to-right mark is a strong left-to-right character to every
/// layout that follows the Unicode bidirectional algorithm, so the line is laid out from the left.
pub const AGENT_SAYS_MARK: &str = "\u{200e}| ";
/// The line the agent's words are drawn under.
pub const AGENT_SAYS: &str = "What your agent says about it. These are its own words, and nothing has checked them:";
/// Said with the offer of an allowance that holds in a session that read untrusted text.
pub const EVEN_TAINTED_WARNING: &str = "Text your agent has read can then make it use this secret without you being asked. The secret still goes only to the hosts its rule names.";
/// Said where "Allow for…" offers only such allowances.
pub const ONLY_EVEN_TAINTED: &str = "This use comes from a session that read untrusted text, or that TorqOS cannot see into. Only an allowance that says so lets its later uses through.";
/// How long after a use is first drawn its buttons take no press: a click meant for what was there before lands
/// on nothing.
pub const USE_ARMS_AFTER: std::time::Duration = std::time::Duration::from_secs(1);
/// How long what became of one use stands alone in front of the next use that waits, unless it is read sooner.
pub const NOTE_ALONE: std::time::Duration = std::time::Duration::from_secs(4);

/// The agent's words as lines the dialog draws as they are, each beginning with `AGENT_SAYS_MARK`. The lines are
/// broken here, at `columns` of a fixed-width font (a character outside ASCII counted as two), and not by the
/// widget: so every line of them drawn has the mark, and none can pass for a row of the broker's facts.
fn marked_lines(words: &str, columns: usize) -> Vec<String> {
    let wide = |c: char| if c.is_ascii() { 1 } else { 2 };
    let cols = |s: &str| s.chars().map(wide).sum::<usize>();
    let (mut lines, mut line) = (Vec::new(), String::new());
    for word in words.split_whitespace() {
        let mut rest = word;
        while !rest.is_empty() {
            let gap = usize::from(!line.is_empty());
            if cols(&line) + gap + cols(rest) <= columns {
                if gap == 1 {
                    line.push(' ');
                }
                line.push_str(rest);
                rest = "";
            } else if line.is_empty() {
                // Longer than a line by itself: cut where the line is full, and never before its first character.
                let mut used = 0;
                let full = rest.char_indices().find(|(_, c)| {
                    used += wide(*c);
                    used > columns
                });
                let first = rest.chars().next().map_or(rest.len(), char::len_utf8);
                let cut = full.map_or(rest.len(), |(i, _)| i).max(first);
                lines.push(rest[..cut].to_string());
                rest = &rest[cut..];
            } else {
                lines.push(std::mem::take(&mut line));
            }
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines.into_iter().map(|l| format!("{AGENT_SAYS_MARK}{l}")).collect()
}

/// What "Allow for…" can give: leave for the agent to use this one secret without asking, for a while.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Allowance {
    /// In the agent's sessions running now.
    Session,
    Minutes(u32),
    /// For minutes, and even from a session that read untrusted text.
    MinutesEvenTainted(u32),
}

/// One thing "Allow for…" offers.
#[derive(Debug, Clone, PartialEq)]
pub struct Offer {
    pub allowance: Allowance,
    pub label: String,
    /// Drawn in the destructive style, with `EVEN_TAINTED_WARNING`.
    pub red: bool,
}

/// A person's answer, in the panel's dialog, to a use that waits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
    /// Allow once.
    Once,
    /// Allow for…: the allowance first (the broker asks the person's password), and a yes to this use only if it
    /// was given.
    For(Allowance),
    Deny,
    /// Deny, and don't ask again this session.
    DenySession,
}

/// What the dialog shows of one waiting use. The panel lays this out and decides nothing of it.
#[derive(Debug, Clone, PartialEq)]
pub struct UseDialog {
    pub n: u64,
    /// The broker's facts, first: a label and its value each, in the order drawn, and whether the row is drawn in
    /// the destructive style (the session's, where it read untrusted text or none is named).
    pub facts: Vec<(&'static str, String, bool)>,
    /// How long is left to answer: one short line whatever the time, so the countdown moves nothing of the dialog.
    pub left: String,
    /// The agent's own words, under `AGENT_SAYS`: lines drawn as they are (none: it said nothing).
    pub agent_says: Vec<String>,
    /// How many others wait: "2 more waiting", or that none does. Always a line, so that another use coming or
    /// going moves nothing of the dialog.
    pub more: String,
    /// What "Allow for…" offers.
    pub offers: Vec<Offer>,
    /// Said with the offers, where they are only the kind that holds under untrusted text.
    pub offers_note: Option<&'static str>,
    /// Said with the offers where the person already let the agent use this secret: what choosing one does to that.
    pub replaces: Option<String>,
    /// Whether "Deny, and don't ask again this session" is offered: only a use that names a session.
    pub deny_session: bool,
}

impl WaitingUse {
    /// "your secret gh", "the system's secret ci".
    fn whose(&self, me: &str) -> String {
        match &self.user {
            None => format!("the system's secret {}", self.secret),
            Some(u) if u == me => format!("your secret {}", self.secret),
            Some(u) => format!("{u}'s secret {}", self.secret),
        }
    }

    /// What "Allow for…" offers for this use. A use from a session that read untrusted text is covered only by an
    /// allowance that says so: the others would be given, and its next use would ask all the same, so they are
    /// not offered for it.
    fn offers(&self) -> Vec<Offer> {
        let minutes = |m: u32| if m == 480 { "480 minutes (8 hours)".to_string() } else { format!("{m} minutes") };
        if self.tainted {
            return [60, 480].into_iter().map(|m| Offer { allowance: Allowance::MinutesEvenTainted(m), label: format!("For {}, even from a session that read untrusted text", minutes(m)), red: true }).collect();
        }
        let mut offers = Vec::new();
        if self.session {
            offers.push(Offer { allowance: Allowance::Session, label: "In your agent's sessions that are running now".into(), red: false });
        }
        offers.extend([60, 480].into_iter().map(|m| Offer { allowance: Allowance::Minutes(m), label: format!("For {}", minutes(m)), red: false }));
        offers
    }

    /// Whether the dialog offers `reply` for this use.
    fn offered(&self, reply: Reply) -> bool {
        match reply {
            Reply::Once | Reply::Deny => true,
            Reply::DenySession => self.session,
            Reply::For(a) => self.offers().iter().any(|o| o.allowance == a),
        }
    }

    /// What the broker is asked for `reply`, in order: each only if the one before it was done. `replace`: the
    /// person's grant of this secret that holds already is ended first, since the broker gives no second one
    /// while it does.
    fn requests(&self, reply: Reply, replace: bool) -> Vec<Approval> {
        let answer = |approved, rest| Approval::Answer { n: self.n, approved, rest };
        let grant = |minutes, tainted_too| {
            let end = replace.then(|| Approval::Revoke { name: Some(self.secret.clone()), user: self.user.clone() });
            end.into_iter().chain([Approval::Grant { name: Some(self.secret.clone()), user: self.user.clone(), minutes, tainted_too }, answer(true, false)]).collect::<Vec<_>>()
        };
        match reply {
            Reply::Once => vec![answer(true, false)],
            Reply::Deny => vec![answer(false, false)],
            Reply::DenySession => vec![answer(false, true)],
            Reply::For(Allowance::Session) => grant(None, false),
            Reply::For(Allowance::Minutes(m)) => grant(Some(m), false),
            Reply::For(Allowance::MinutesEvenTainted(m)) => grant(Some(m), true),
        }
    }

    /// The dialog for this use, with `more` others waiting, at `now` (seconds since the epoch). `held`: the person's
    /// own grant of this very secret that holds already (`Asking::held`), which "Allow for…" would replace.
    pub fn dialog(&self, me: &str, more: usize, now: i64, held: Option<&Allowed>) -> UseDialog {
        let secret = match &self.user {
            None => format!("{} (the system's)", self.secret),
            Some(u) if u == me => format!("{} (yours)", self.secret),
            Some(u) => format!("{} ({u}'s)", self.secret),
        };
        let session = match (self.tainted, self.session, self.tool) {
            (false, ..) => "It read nothing untrusted.",
            (true, false, _) => "The use names no session, so it counts as one that read untrusted text.",
            (true, true, true) => "A coding tool's. TorqOS cannot see what it read, so it counts as one that read untrusted text.",
            (true, true, false) => "It read untrusted text.",
        };
        UseDialog {
            n: self.n,
            facts: vec![
                ("Secret", secret, false),
                ("Used for", if self.ssh { format!("signing in over SSH to {}", self.host) } else { format!("an HTTPS request to {}", self.host) }, false),
                ("Asked by", if self.tool { "a coding tool run as your agent".into() } else { "your agent".into() }, false),
                ("Its session", session.into(), self.tainted),
            ],
            left: match self.until.map(|u| u - now) {
                Some(s) if s > 0 => format!("{}:{:02} left to answer, then it is refused.", s / 60, s % 60),
                _ => "Its time is about over.".into(),
            },
            agent_says: marked_lines(&self.agent_says, AGENT_SAYS_COLUMNS),
            more: if more > 0 { format!("{more} more waiting") } else { "No other use waits".into() },
            offers: self.offers(),
            offers_note: self.tainted.then_some(ONLY_EVEN_TAINTED),
            replaces: held.map(|g| {
                let scope = if g.session { " at most, in its sessions that were running then" } else { "" };
                let short = if self.tainted && !g.even_tainted { ", but not from a session that read untrusted text" } else { "" };
                format!("You already let your agent use this secret without asking until {}{scope}{short}. Choosing one of these ends that first, then asks your password. If the new one is refused, or you cancel the password, the old one stays ended.", g.until)
            }),
            deny_session: self.session,
        }
    }

    /// The notification that a use waits: fixed words and the broker's facts (its body escaped, as a notification's
    /// may be read as markup). Never the agent's words.
    pub fn notice(&self, me: &str) -> (String, String) {
        let what = if self.ssh { "to sign in over SSH to" } else { "in an HTTPS request to" };
        ("Your agent asks to use a secret".into(), crate::agent::crash::markup(&format!("It asks for {}, {what} {}. Open this to answer. With no answer it is refused.", self.whose(me), self.host)))
    }
}

/// Whether the person has an agent here (the account `torq-agent-<them>`). Without one no use waits for them, and
/// the panel asks the broker nothing about it.
pub fn has_agent() -> bool {
    crate::me().is_ok_and(|me| crate::agent::has_agent(&crate::agent::agent_of(&me)).is_ok())
}

/// The uses of the person's agent that wait for their answer now, lowest number first; and, only where one waits,
/// every allowance in force: the dialog says when the person already allows the secret asked about. (Nothing
/// waiting, which is nearly always: one request to the broker, not two.)
pub fn uses_waiting() -> Result<(Vec<WaitingUse>, Vec<Allowed>)> {
    waiting_with(&cli(), &crate::me()?, &crate::run::watch_log()?, &history::Clock::system())
}

fn waiting_with(cli: &Cli, me: &str, log: &std::path::Path, clock: &history::Clock) -> Result<(Vec<WaitingUse>, Vec<Allowed>)> {
    let real = crate::shell::Real::new(cli);
    let uses = uses_of(&real.waiting_uses(me, log)?);
    let allowed = if uses.is_empty() { Vec::new() } else { allowed_of(&real.grants(log)?, clock) };
    Ok((uses, allowed))
}

/// Give the person's `reply` to the use `u`, from the panel's dialog. What was done, said as the command line
/// says it. `held`: their grant of that secret which holds already (`Asking::held`), as the dialog said it:
/// "Allow for…" ends it first.
pub fn answer_use(u: &WaitingUse, reply: Reply, held: Option<&Allowed>) -> Result<String> {
    answer_with(&cli(), &crate::me()?, &crate::run::start_log()?, u, reply, held.is_some())
}

fn answer_with(cli: &Cli, me: &str, log: &std::path::Path, u: &WaitingUse, reply: Reply, replace: bool) -> Result<String> {
    if !u.offered(reply) {
        anyhow::bail!("not done: that answer is not offered for use {}", u.n);
    }
    let real = crate::shell::Real::new(cli);
    let (mut said, mut ended) = (String::new(), false);
    for request in u.requests(reply, replace) {
        match (&request, real.approval(&request, Place::Dialog, me, log)) {
            (Approval::Revoke { .. } | Approval::RevokeAllOwn, Ok(v)) => {
                // Only what was ended is said: that there was nothing left to end is no news here.
                ended = v["revoked"].as_array().is_some_and(|r| !r.is_empty());
                if ended {
                    said.push_str(&request.said(&v));
                }
            }
            (_, Ok(v)) => said.push_str(&request.said(&v)),
            // The older allowance could not be ended: nothing was changed, and the use is not answered.
            (Approval::Revoke { .. } | Approval::RevokeAllOwn, Err(e)) => anyhow::bail!("{e:#}. Nothing was changed, and use {} still waits for your answer.", u.n),
            // The allowance was not given (refused, or its password not), after the older one was ended for it.
            (Approval::Grant { .. }, Err(e)) if ended => anyhow::bail!("{e:#}. What you allowed for this secret before is ended, nothing new was allowed, and use {} still waits for your answer.", u.n),
            // The allowance was not given: nothing was, and the use is not answered.
            (Approval::Grant { .. }, Err(e)) => anyhow::bail!("{e:#}. Nothing was allowed, and use {} still waits for your answer.", u.n),
            // Nothing was done before this: the answer itself was refused.
            (Approval::Answer { .. }, Err(e)) if said.is_empty() => return Err(e),
            // The allowance stands, though the yes did not reach this use (over or answered meanwhile, or the
            // broker busy): said as a failure, so the use stays shown for as long as it still waits.
            // (One line: an error is shown cleaned, its line breaks as escapes.)
            (Approval::Answer { .. }, Err(e)) => anyhow::bail!("{} But use {} was not approved by this ({e:#}). The allowance holds for the agent's later uses.", said.trim().replace('\n', " "), u.n),
        }
    }
    Ok(said)
}

/// What became of a use, said to the person: the answer they gave in the dialog, or how a use ended without one.
#[derive(Debug, Clone, PartialEq)]
pub struct Note {
    pub text: String,
    /// Something was not done, or not all of it: drawn in the destructive style, and kept until it is read.
    pub failed: bool,
    /// The use it is about.
    about: u64,
    since: std::time::Instant,
}

/// What the popup shows of the agent's uses.
#[derive(Debug, PartialEq)]
pub enum Showing<'a> {
    /// Nothing: the popup is the panel's own.
    Nothing,
    /// What became of a use, alone, with nothing else to press but its OK.
    Note(&'a Note),
    /// The dialog of the use shown, how many others wait, and why an answer to this very use was not taken.
    Use(&'a WaitingUse, usize, Option<&'a Note>),
}

/// Which waiting use the panel's dialog shows: one at a time, lowest number first, and the next only when the one
/// shown is answered or its time is over. And when a press is taken: never in the first moment after what is drawn
/// changed otherwise than by the person's own press, so a click meant for what was there lands on nothing.
#[derive(Debug, Default)]
pub struct Asking {
    shown: Option<WaitingUse>,
    more: usize,
    /// When what is drawn last changed under the person: a press is taken `USE_ARMS_AFTER` later.
    since: Option<std::time::Instant>,
    /// The uses answered here, lately: a list asked before the answer landed may still have one, and it is not shown again.
    answered: Vec<u64>,
    note: Option<Note>,
    /// The person's own allowance of the very secret the use shown asks for, where one holds already.
    held: Option<Allowed>,
}

impl Asking {
    /// The broker's list of what waits (`waiting`), at `now` (seconds since the epoch) and `at`. A use that is
    /// shown now and was not before: the person is told of it, once.
    pub fn heard(&mut self, mut waiting: Vec<WaitingUse>, now: i64, at: std::time::Instant) -> Option<WaitingUse> {
        waiting.retain(|u| !self.answered.contains(&u.n));
        waiting.sort_by_key(|u| u.n);
        let before = self.shown.as_ref().map(|u| u.n);
        if let Some(was) = self.shown.take() {
            match waiting.iter().position(|u| u.n == was.n) {
                // Still waiting: it stays the one shown, whatever else has come. How many others wait is a line of
                // the dialog: where it changes, the buttons wait again.
                Some(i) => {
                    if self.more != waiting.len() - 1 {
                        self.since = Some(at);
                    }
                    self.more = waiting.len() - 1;
                    self.shown = Some(waiting.swap_remove(i));
                    return None;
                }
                None if was.until.is_none_or(|u| now >= u) => self.say(format!("The time to answer use {} ({} for {}) is over.", was.n, was.secret, was.host), false, was.n, at),
                None => self.say(format!("Use {} ({} for {}) was answered somewhere else.", was.n, was.secret, was.host), false, was.n, at),
            }
        }
        self.more = waiting.len().saturating_sub(1);
        self.shown = waiting.into_iter().next();
        if self.shown.as_ref().map(|u| u.n) != before {
            self.since = Some(at);
        }
        self.shown.clone()
    }

    /// A note takes the place of the one before it. Except a failure not yet read: what was not done is not
    /// written over by what happened next, which is said after it.
    fn say(&mut self, text: String, failed: bool, about: u64, at: std::time::Instant) {
        self.note = Some(match self.note.take() {
            Some(unread) if unread.failed => Note { text: format!("{} {text}", unread.text), failed: true, about, since: at },
            _ => Note { text, failed, about, since: at },
        });
        self.since = Some(at);
    }

    /// The answer given here to use `n` came back. Taken (`Ok`), the use is no longer shown, and is not said to have
    /// been answered elsewhere. Not taken, or not all of it, it stays shown for as long as it waits, with why.
    pub fn replied(&mut self, n: u64, outcome: Result<String, String>, at: std::time::Instant) {
        match outcome {
            Ok(said) => {
                if self.shown.as_ref().is_some_and(|u| u.n == n) {
                    self.shown = None;
                }
                self.answered.push(n);
                // Numbers only grow, and a list asked before an answer is seconds old at most: the last few are enough.
                if self.answered.len() > 16 {
                    self.answered.remove(0);
                }
                self.say(said.trim().to_string(), false, n, at);
            }
            Err(why) => self.say(why, true, n, at),
        }
    }

    /// What the popup shows. A note about a use that is no longer the one shown stands alone first, whatever
    /// waits: it is never drawn inside another use's dialog, above that use's buttons.
    pub fn showing(&self) -> Showing<'_> {
        match (&self.shown, &self.note) {
            (Some(u), Some(note)) if note.about == u.n => Showing::Use(u, self.more, Some(note)),
            (_, Some(note)) => Showing::Note(note),
            (Some(u), None) => Showing::Use(u, self.more, None),
            (None, None) => Showing::Nothing,
        }
    }

    /// Every allowance in force (`allowed`), as asked with the list of what waits: which of them is `me`'s own of
    /// the very secret the use shown asks for (the same name, and the same owner: theirs, or the system's). The
    /// dialog says so, which changes what it draws: where that changes, its buttons wait again.
    pub fn allows(&mut self, allowed: &[Allowed], me: &str, at: std::time::Instant) {
        let held = self.shown.as_ref().and_then(|u| allowed.iter().find(|g| g.person == me && g.secret == Some((u.secret.clone(), u.user.is_none()))).cloned());
        if held != self.held {
            self.since = Some(at);
        }
        self.held = held;
    }

    /// The person's own allowance of the secret the use shown asks for, where one holds.
    pub fn held(&self) -> Option<&Allowed> {
        self.held.as_ref()
    }

    /// The use that waits in front, with how many others wait: shown, or behind a note for the moment.
    pub fn shown(&self) -> Option<(&WaitingUse, usize)> {
        self.shown.as_ref().map(|u| (u, self.more))
    }

    /// Time passing, at `at`. A note that holds a waiting use back gives way after `NOTE_ALONE`, and that use's
    /// dialog is drawn, its buttons waiting. Not a failure: that stays until it is read.
    pub fn tick(&mut self, at: std::time::Instant) {
        let blocks = matches!((&self.shown, &self.note), (Some(u), Some(n)) if n.about != u.n && !n.failed && at.saturating_duration_since(n.since) >= NOTE_ALONE);
        // Settled: nothing is waited for any more (and nothing need be drawn again for it).
        if self.settled(at) {
            self.since = None;
        }
        if blocks {
            self.note = None;
            self.since = Some(at);
        }
    }

    /// The note was read (its OK pressed) at `at`. What is drawn in its place takes no press at once.
    pub fn read(&mut self, at: std::time::Instant) {
        self.note = None;
        self.since = Some(at);
    }

    /// The popup closed. A note that was no failure has been shown, and is not kept for the next time it opens.
    pub fn closed(&mut self) {
        if self.note.as_ref().is_some_and(|n| !n.failed) {
            self.note = None;
        }
    }

    /// What is drawn changed at `at` otherwise than by an answer: the popup opened, or the dialog's choices of
    /// "Allow for…" were opened or closed. Its buttons wait again.
    pub fn drawn(&mut self, at: std::time::Instant) {
        self.since = Some(at);
    }

    /// Whether a press is taken at `at`: not in the first moment after what is drawn last changed. For the
    /// popup's own buttons too, which a note or a dialog may just have stood in front of.
    pub fn settled(&self, at: std::time::Instant) -> bool {
        self.since.is_none_or(|s| at.saturating_duration_since(s) >= USE_ARMS_AFTER)
    }

    /// Whether a change is still settling as far as `tick` has been told: the popup is drawn again until it is
    /// not, so that buttons come alive without a press.
    pub fn settling(&self) -> bool {
        self.since.is_some()
    }

    /// Whether a press of a button drawn for use `n` is taken at `at`: only for the use whose dialog is drawn, and
    /// only once it has settled.
    pub fn takes(&self, n: u64, at: std::time::Instant) -> bool {
        matches!(self.showing(), Showing::Use(u, ..) if u.n == n) && self.settled(at)
    }
}

/// A person's leave for their own agent to use secrets without asking, in force now (`secret-grants`).
#[derive(Debug, Clone, PartialEq)]
pub struct Allowed {
    /// Its entry in the ledger.
    pub seq: u64,
    /// Who gave it, to their own agent.
    pub person: String,
    /// The one secret it covers: its name, and whether it is the system's (None: all of the person's own).
    pub secret: Option<(String, bool)>,
    /// Its end, in local time.
    pub until: String,
    /// Only for the agent's sessions that were running when it was given.
    pub session: bool,
    /// It holds in a session that read untrusted text too: only ever of one named secret.
    pub even_tainted: bool,
}

/// Said, in the destructive style, of an allowance or a rule that holds whatever the agent's session read.
pub const EVEN_TAINTED: &str = "Even from a session that read untrusted text.";

impl Allowed {
    /// Whether it covers the secret `name` of `user` (None: the system's).
    pub fn covers(&self, user: Option<&str>, name: &str) -> bool {
        match &self.secret {
            Some((n, true)) => user.is_none() && n == name,
            Some((n, false)) => user == Some(self.person.as_str()) && n == name,
            None => user == Some(self.person.as_str()),
        }
    }

    /// Whose agent, what it covers and until when, as the Secrets page says it to `me`.
    pub fn said(&self, me: Option<&str>) -> String {
        let (agent, your) = if me == Some(self.person.as_str()) { ("Your agent".to_string(), "your") } else { (format!("{}'s agent", self.person), "their") };
        let what = match &self.secret {
            None => format!("all {your} own secrets"),
            Some((n, true)) => format!("the system's secret {n}"),
            Some((n, false)) => format!("{your} secret {n}"),
        };
        // The broker ends such a grant for each session when that session is eight hours old: sooner, it may be,
        // than the time its entry gives.
        let scope = if self.session { " at most, in its sessions that were running when this was allowed: each for 8 hours from its start" } else { "" };
        format!("{agent} uses {what} without asking until {}{scope}.", self.until)
    }

    /// How the Secrets page lets `me` end it: what is said under it, and the button's label with the line it runs
    /// (None: not theirs to end). One secret's by its own line, whose secret said as the grant said it; all their
    /// own by its own line, which leaves each named one. A secret whose name the line would not read as a
    /// secret's is ended only with everything they allowed.
    pub fn ending(&self, me: Option<&str>) -> (String, Option<(&'static str, String)>) {
        if me != Some(self.person.as_str()) {
            return (format!("{}'s to end.", self.person), None);
        }
        match self.secret.as_ref().map(|(n, system)| revoke_line(n, (!system).then_some(self.person.as_str()))) {
            Some(Some(line)) => (format!("Ending it asks no password: {line}"), Some(("End it", line))),
            Some(None) => (format!("A secret of this name is ended only with everything else you allowed, and no password is asked: {REVOKE_ALL}"), Some(("End all", REVOKE_ALL.into()))),
            None => (format!("Not from a session that read untrusted text. Ending it leaves what you allowed of one secret, and asks no password: {REVOKE_ALL_OWN}"), Some(("End it", REVOKE_ALL_OWN.into()))),
        }
    }
}

/// The line that ends every allowance a person gave.
pub const REVOKE_ALL: &str = "request system secret allow revoke all";
/// The line that ends their allowance of all their own secrets, and no other.
pub const REVOKE_ALL_OWN: &str = "request system secret allow all revoke";

/// The grants in the broker's answer. One that does not say what it is, is left out.
fn allowed_of(v: &serde_json::Value, clock: &history::Clock) -> Vec<Allowed> {
    let one = |g: &serde_json::Value| -> Option<Allowed> {
        let secret = match &g["covers"] {
            serde_json::Value::String(s) if s == "all-secrets" => None,
            c => Some((history::one_line(c["secret"]["name"].as_str()?, 80), c["secret"]["system"].as_bool()?)),
        };
        Some(Allowed {
            seq: g["seq"].as_u64()?,
            person: history::one_line(g["person"].as_str()?, 40),
            until: history::local_stamp(g["until"].as_str()?, clock),
            session: g["session"] == true,
            // The broker honours it only of one named secret, whatever an entry says.
            even_tainted: g["tainted_too"] == true && secret.is_some(),
            secret,
        })
    };
    v["grants"].as_array().into_iter().flatten().filter_map(one).collect()
}

/// Every allowance in force: each person's, to their own agent (the record is open to all, as the ledger is).
pub fn allowed() -> Result<Vec<Allowed>> {
    let cli = cli();
    let grants = crate::shell::Real::new(&cli).grants(&crate::run::watch_log()?)?;
    Ok(allowed_of(&grants, &history::Clock::system()))
}

/// The line that ends the person's allowance of the secret `name`: their own (`user`), or the system's. None: the
/// name is a word of the command itself (`all`, `agent`, …), which that line would not read as a secret's name.
fn revoke_line(name: &str, user: Option<&str>) -> Option<String> {
    let mut words = vec!["request", "system", "secret", "allow", "revoke", name];
    if let Some(u) = user {
        words.extend(["user", u]);
    }
    let line = self::line(&words);
    let read = crate::session::words(&line).ok().and_then(|w| crate::grammar::read_line(Mode::Operational, &w).ok())?;
    (read.op == crate::syntax::Op::Request(crate::syntax::Request::SecretApproval(Approval::Revoke { name: Some(name.to_string()), user: user.map(String::from) }))).then_some(line)
}

#[cfg(test)]
mod approval_tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// The broker's answer to `secret-waiting`, with these uses.
    fn waiting(uses: &[serde_json::Value]) -> serde_json::Value {
        serde_json::json!({ "waiting": uses })
    }

    /// A coding tool's use of dev's own secret gh, tainted, in a session, with the agent's words.
    fn tool_use(n: u64, says: &str) -> serde_json::Value {
        serde_json::json!({ "n": n, "asked": "2026-11-13T12:00:00.000Z", "until": "2026-11-13T12:02:00.000Z", "answer": null, "agent_says": says,
            "facts": { "person": "dev", "agent": "torq-agent-dev", "caller": "torq-agent-dev", "owner": { "person": "dev" }, "secret": "gh", "host": "api.github.com", "ssh": false, "session": "s1", "tainted": true, "why": "w" } })
    }

    /// The resident agent's use of the system's secret ci over SSH, from a session that read nothing untrusted.
    fn resident_use(n: u64) -> serde_json::Value {
        serde_json::json!({ "n": n, "asked": "2026-11-13T12:00:00.000Z", "until": "2026-11-13T12:02:00.000Z", "agent_says": "",
            "facts": { "person": "dev", "agent": "torq-agent-dev", "caller": "torq-resident-dev", "owner": "system", "secret": "ci", "host": "github.com", "ssh": true, "session": "s2", "tainted": false, "why": "w" } })
    }

    fn uses(v: &[serde_json::Value]) -> Vec<WaitingUse> {
        uses_of(&waiting(v))
    }

    const NOON: i64 = 1_794_571_200; // 2026-11-13T12:00:00Z

    /// What waits is read as the broker's facts, lowest number first; where the broker's answer does not say a use
    /// is clean, it is not taken as clean; one that does not say what it is, is not shown at all.
    #[test]
    fn what_waits_is_read_as_the_brokers_facts() {
        assert_eq!(history::parse_ts("2026-11-13T12:00:00.000Z"), Some(NOON));
        let read = uses(&[resident_use(9), tool_use(7, "listing  the\trepositories")]);
        assert_eq!(read.iter().map(|u| u.n).collect::<Vec<_>>(), [7, 9]);
        assert_eq!(read[0], WaitingUse { n: 7, secret: "gh".into(), user: Some("dev".into()), host: "api.github.com".into(), ssh: false, tool: true, tainted: true, session: true, until: Some(NOON + 120), agent_says: "listing the repositories".into() });
        assert_eq!((read[1].user.as_deref(), read[1].ssh, read[1].tool, read[1].tainted, read[1].session, read[1].agent_says.as_str()), (None, true, false, false, true, ""));
        // Nothing said of taint, the caller or the session: tainted, a coding tool's, no session.
        let mut bare = resident_use(3);
        for k in ["tainted", "caller", "session"] {
            bare["facts"].as_object_mut().unwrap().remove(k);
        }
        let read = uses(&[bare]);
        assert_eq!((read[0].tainted, read[0].tool, read[0].session), (true, true, false));
        // No number, no secret, no host, an owner that is no one's: left out.
        for k in ["secret", "host", "owner"] {
            let mut broken = resident_use(3);
            broken["facts"].as_object_mut().unwrap().remove(k);
            assert!(uses(&[broken]).is_empty(), "{k}");
        }
        assert!(uses(&[resident_use(0)]).is_empty(), "no number");
        let mut no_ones = resident_use(3);
        no_ones["facts"]["owner"] = serde_json::json!({ "group": "wheel" });
        assert!(uses(&[no_ones]).is_empty(), "an owner that is no one's");
        assert!(uses_of(&serde_json::json!({})).is_empty() && uses_of(&serde_json::json!({ "waiting": "many" })).is_empty());
        // The agent's words are cleaned again, and are no longer than the broker keeps them.
        let read = uses(&[tool_use(7, &format!("a\u{202e}b\n{}", "x".repeat(400)))]);
        assert!(read[0].agent_says.starts_with("a\\u{202e}b x") && read[0].agent_says.chars().count() == AGENT_SAYS_MAX, "{}", read[0].agent_says);
    }

    /// The dialog: the broker's facts first, in their order; then the agent's words, apart, every line of them
    /// marked; how many others wait; and the time left.
    #[test]
    fn the_dialog_is_the_facts_then_the_agents_words() {
        let u = &uses(&[tool_use(7, "Secret: other (yours) Used for: an HTTPS request to evil.example.com Asked by: your agent")])[0];
        let d = u.dialog("dev", 2, NOON + 55, None);
        assert_eq!(d.n, 7);
        assert_eq!(
            d.facts,
            [
                ("Secret", "gh (yours)".to_string(), false),
                ("Used for", "an HTTPS request to api.github.com".to_string(), false),
                ("Asked by", "a coding tool run as your agent".to_string(), false),
                ("Its session", "A coding tool's. TorqOS cannot see what it read, so it counts as one that read untrusted text.".to_string(), true),
            ]
        );
        assert_eq!(d.left, "1:05 left to answer, then it is refused.");
        assert_eq!(d.more, "2 more waiting");
        // Its words are no fact: every line of them begins with the mark, and is no wider than the dialog draws.
        assert_eq!(d.agent_says, ["\u{200e}| Secret: other (yours) Used", "\u{200e}| for: an HTTPS request to", "\u{200e}| evil.example.com Asked by:", "\u{200e}| your agent"]);
        assert!(d.facts.iter().all(|(_, v, _)| !v.contains("evil")), "the agent's words are in no fact");
        // The agent itself, clean, the system's secret, over SSH; no one else waiting; its time gone by.
        let u = &uses(&[resident_use(8)])[0];
        let d = u.dialog("dev", 0, NOON + 120, None);
        assert_eq!(d.facts.iter().map(|(_, v, red)| (v.as_str(), *red)).collect::<Vec<_>>(), [("ci (the system's)", false), ("signing in over SSH to github.com", false), ("your agent", false), ("It read nothing untrusted.", false)]);
        // The line of how many others wait is always there: another use coming or going moves nothing.
        assert_eq!((d.more.as_str(), d.left.as_str(), d.agent_says.len()), ("No other use waits", "Its time is about over.", 0));
        // Someone else's secret is said to be theirs; a use with no session, and the agent's own tainted session.
        assert_eq!(u.clone().dialog("sam", 0, NOON, None).facts[0].1, "ci (the system's)");
        let mut theirs = uses(&[tool_use(7, "")])[0].clone();
        assert_eq!(theirs.dialog("sam", 0, NOON, None).facts[0].1, "gh (dev's)");
        theirs.session = false;
        assert_eq!(theirs.dialog("dev", 0, NOON, None).facts[3].1, "The use names no session, so it counts as one that read untrusted text.");
        (theirs.session, theirs.tool) = (true, false);
        assert_eq!(theirs.dialog("dev", 0, NOON, None).facts[2..], [("Asked by", "your agent".to_string(), false), ("Its session", "It read untrusted text.".to_string(), true)]);
        theirs.until = None;
        assert_eq!(theirs.dialog("dev", 0, NOON, None).left, "Its time is about over.");
    }

    /// The agent's words are broken into lines here: none wider than asked, a long word cut, wide characters
    /// counted as two, and each line marked.
    #[test]
    fn the_agents_words_are_lines_with_the_mark() {
        assert_eq!(marked_lines("", 10), Vec::<String>::new());
        assert_eq!(marked_lines("   ", 10), Vec::<String>::new());
        assert_eq!(marked_lines("one two three", 7), ["\u{200e}| one two", "\u{200e}| three"]);
        assert_eq!(marked_lines("abcdefghijkl mn", 5), ["\u{200e}| abcde", "\u{200e}| fghij", "\u{200e}| kl mn"]);
        assert_eq!(marked_lines("ééééé é", 6), ["\u{200e}| ééé", "\u{200e}| éé", "\u{200e}| é"]);
        assert_eq!(marked_lines("日本", 1), ["\u{200e}| 日", "\u{200e}| 本"], "never less than a character a line");
        let long = "Secret: x ".repeat(20);
        for l in marked_lines(&long, AGENT_SAYS_COLUMNS) {
            assert!(l.starts_with(AGENT_SAYS_MARK) && l.chars().count() <= AGENT_SAYS_COLUMNS + AGENT_SAYS_MARK.chars().count(), "{l}");
        }
        // A line that begins with right-to-left letters would be laid out from the right, its mark at the far end.
        // Each line begins with a left-to-right mark, ahead of the visible one: the line is laid out from the
        // left whatever follows. One of the agent's own direction marks does not reach here as itself.
        assert_eq!(AGENT_SAYS_MARK.chars().next(), Some('\u{200e}'));
        assert_eq!(marked_lines("שלום עולם", 30), ["\u{200e}| שלום עולם"]);
        let rtl = &uses(&[tool_use(7, "\u{200f}\u{202e}שלום abc \u{200e}")])[0];
        for l in rtl.dialog("dev", 0, NOON, None).agent_says {
            assert!(l.starts_with("\u{200e}| ") && l.chars().filter(|c| history::hidden(*c)).count() == 1, "only the line's own mark: {l:?}");
        }
    }

    /// Which buttons a use is offered: a no for the rest of a session only where one is named; for a use from a
    /// session that read untrusted text, only the allowances that say so, in red; for a clean one, never those.
    #[test]
    fn what_a_use_is_offered() {
        let tool = uses(&[tool_use(7, "")])[0].clone();
        let d = tool.dialog("dev", 0, NOON, None);
        assert_eq!(
            d.offers,
            [
                Offer { allowance: Allowance::MinutesEvenTainted(60), label: "For 60 minutes, even from a session that read untrusted text".into(), red: true },
                Offer { allowance: Allowance::MinutesEvenTainted(480), label: "For 480 minutes (8 hours), even from a session that read untrusted text".into(), red: true },
            ]
        );
        assert_eq!((d.offers_note, d.deny_session), (Some(ONLY_EVEN_TAINTED), true));
        let clean = uses(&[resident_use(8)])[0].clone();
        let d = clean.dialog("dev", 0, NOON, None);
        assert_eq!(d.offers.iter().map(|o| (o.allowance, o.label.as_str(), o.red)).collect::<Vec<_>>(), [(Allowance::Session, "In your agent's sessions that are running now", false), (Allowance::Minutes(60), "For 60 minutes", false), (Allowance::Minutes(480), "For 480 minutes (8 hours)", false)]);
        assert_eq!((d.offers_note, d.deny_session), (None, true));
        let mut unnamed = tool.clone();
        unnamed.session = false;
        assert!(!unnamed.dialog("dev", 0, NOON, None).deny_session);
        // What is not on the dialog is not done, whatever message says so.
        for (u, no) in [(&tool, Reply::For(Allowance::Minutes(60))), (&tool, Reply::For(Allowance::Session)), (&tool, Reply::For(Allowance::MinutesEvenTainted(5))), (&clean, Reply::For(Allowance::MinutesEvenTainted(60))), (&clean, Reply::For(Allowance::Minutes(481))), (&unnamed, Reply::DenySession)] {
            assert!(!u.offered(no), "{no:?}");
        }
        for (u, yes) in [(&tool, Reply::Once), (&tool, Reply::Deny), (&tool, Reply::DenySession), (&tool, Reply::For(Allowance::MinutesEvenTainted(480))), (&clean, Reply::For(Allowance::Session)), (&unnamed, Reply::For(Allowance::MinutesEvenTainted(60)))] {
            assert!(u.offered(yes), "{yes:?}");
        }
    }

    /// A stand-in for the broker: each call's arguments on a line of `calls`; a grant answered by `grant`, an
    /// answer to a use by `answer` (shell).
    fn broker(dir: &std::path::Path, grant: &str, answer: &str) -> Cli {
        let script = dir.join("broker.sh");
        let d = dir.display();
        std::fs::write(&script, format!("echo \"$*\" >> {d}/calls\ncase \"$1\" in\nsecret-grant) {grant};;\n*) {answer};;\nesac\n")).unwrap();
        Cli { repo: PathBuf::from(REPO), broker: format!("sh {}", script.display()) }
    }

    const GRANTED: &str = "echo '{\"seq\":11,\"reason\":\"dev lets the agent use their secret gh without asking, for 60 minutes\",\"data\":{\"until\":\"2026-11-13T13:00:00Z\"}}'";
    const APPROVED: &str = "echo '{\"seq\":12,\"reason\":\"dev approved the agent use\"}'";
    const REFUSED: &str = "echo '{\"error\":\"the broker would not\"}'; exit 1";

    /// What each button sends, in order, and that an answer says it was given in the dialog. "Allow for…" sends
    /// the allowance first, and the yes to this use only once the allowance was given: refused (or its password
    /// not given), nothing more is sent, and the person is told the use still waits.
    #[test]
    fn what_each_button_sends() {
        let tool = uses(&[tool_use(7, "")])[0].clone();
        let clean = uses(&[resident_use(8)])[0].clone();
        let run = |u: &WaitingUse, reply: Reply, grant: &str, answer: &str| {
            let d = tempfile::tempdir().unwrap();
            let said = answer_with(&broker(d.path(), grant, answer), "dev", &d.path().join("log"), u, reply, false).map_err(|e| format!("{e:#}"));
            let calls = std::fs::read_to_string(d.path().join("calls")).unwrap_or_default();
            (said, calls.lines().map(String::from).collect::<Vec<_>>())
        };
        let (said, calls) = run(&tool, Reply::Once, GRANTED, APPROVED);
        assert_eq!((said.unwrap().as_str(), calls), ("dev approved the agent use (ledger #12)\n", vec!["secret-approve 7 --where dialog --approved-by dev --json".to_string()]));
        assert_eq!(run(&tool, Reply::Deny, GRANTED, APPROVED).1, ["secret-deny 7 --where dialog --approved-by dev --json"]);
        assert_eq!(run(&tool, Reply::DenySession, GRANTED, APPROVED).1, ["secret-deny 7 --where dialog --rest-of-session --approved-by dev --json"]);
        // The allowance, then the yes: of this one secret, whose it is, and under taint only where it says so.
        let (said, calls) = run(&tool, Reply::For(Allowance::MinutesEvenTainted(60)), GRANTED, APPROVED);
        assert_eq!(calls, ["secret-grant --name gh --user dev --minutes 60 --tainted-too --approved-by dev --json", "secret-approve 7 --where dialog --approved-by dev --json"]);
        assert_eq!(said.unwrap(), "dev lets the agent use their secret gh without asking, for 60 minutes (ledger #11)\nUntil 2026-11-13T13:00:00Z. `request system secret allow revoke` ends it.\ndev approved the agent use (ledger #12)\n");
        assert_eq!(run(&clean, Reply::For(Allowance::Session), GRANTED, APPROVED).1, ["secret-grant --name ci --session --approved-by dev --json", "secret-approve 8 --where dialog --approved-by dev --json"]);
        assert_eq!(run(&clean, Reply::For(Allowance::Minutes(480)), GRANTED, APPROVED).1[0], "secret-grant --name ci --minutes 480 --approved-by dev --json");
        // The allowance refused: no yes is sent, and the person is told the use still waits.
        let (said, calls) = run(&tool, Reply::For(Allowance::MinutesEvenTainted(480)), REFUSED, APPROVED);
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_eq!(said.unwrap_err(), "request system secret allow gh for 480 tainted-too user dev refused: the broker would not. Nothing was allowed, and use 7 still waits for your answer.");
        // The allowance given and the yes refused (the use over meanwhile): both are said.
        let (said, calls) = run(&tool, Reply::For(Allowance::MinutesEvenTainted(60)), GRANTED, REFUSED);
        assert_eq!(calls.len(), 2);
        let said = said.unwrap_err();
        assert!(said.starts_with("dev lets the agent use their secret gh") && said.ends_with("ends it. But use 7 was not approved by this (request system secret approve 7 refused: the broker would not). The allowance holds for the agent's later uses.") && !said.contains('\n'), "{said}");
        // A plain answer refused is the broker's refusal, as it said it.
        assert_eq!(run(&tool, Reply::Once, GRANTED, REFUSED).0.unwrap_err(), "request system secret approve 7 refused: the broker would not");
        // What the dialog does not offer for the use is asked of no one.
        let (said, calls) = run(&tool, Reply::For(Allowance::Minutes(60)), GRANTED, APPROVED);
        assert_eq!((said.unwrap_err().as_str(), calls.len()), ("not done: that answer is not offered for use 7", 0));
    }

    /// A stand-in for the broker that answers each request by its own arm of a shell `case` (`arms`), each call's
    /// arguments on a line of `calls`.
    fn broker_of(dir: &std::path::Path, arms: &str) -> Cli {
        let script = dir.join("broker.sh");
        std::fs::write(&script, format!("echo \"$*\" >> {}/calls\ncase \"$1\" in\n{arms}\nesac\n", dir.display())).unwrap();
        Cli { repo: PathBuf::from(REPO), broker: format!("sh {}", script.display()) }
    }

    /// The broker gives no second grant of a secret while one holds, and a use waits two minutes: where the person
    /// already allows the secret asked about, the dialog says so, and "Allow for…" ends that grant first (of that
    /// one secret, theirs or the system's as the use says), then asks for the new one. If the new one is then
    /// refused, the old one is gone, and that is what the person is told.
    #[test]
    fn allow_for_replaces_a_grant_that_holds() {
        let t0 = Instant::now();
        let grant = |seq: u64, person: &str, secret: Option<(&str, bool)>| Allowed { seq, person: person.into(), secret: secret.map(|(n, s)| (n.to_string(), s)), until: "2026-11-13 08:00:00".into(), session: false, even_tainted: false };
        let allowed = [grant(1, "sam", Some(("gh", false))), grant(2, "dev", Some(("gh", true))), grant(3, "dev", None), grant(4, "dev", Some(("other", false))), grant(5, "dev", Some(("gh", false)))];
        let mut a = Asking::default();
        a.allows(&allowed, "dev", t0);
        assert_eq!(a.held(), None, "no use is shown");
        a.heard(uses(&[tool_use(7, "")]), NOON, t0);
        a.allows(&allowed[..4], "dev", t0);
        assert_eq!(a.held(), None, "not another person's, the system's of that name, all of them, or another secret's");
        // Found at a later look: the dialog now says so, and its buttons wait again.
        let t1 = t0 + Duration::from_secs(5);
        assert!(a.takes(7, t1));
        a.allows(&allowed, "dev", t1);
        assert_eq!(a.held().map(|g| g.seq), Some(5));
        assert!(!a.takes(7, t1) && a.takes(7, t1 + USE_ARMS_AFTER));
        a.allows(&allowed, "dev", t1 + USE_ARMS_AFTER);
        assert!(a.takes(7, t1 + USE_ARMS_AFTER), "nothing changed");
        // The system's secret: the grant of the system's of that name, and not one's own.
        a.replied(7, Ok("done".into()), t1);
        a.read(t1);
        a.heard(uses(&[resident_use(8)]), NOON, t1);
        a.allows(&[grant(6, "dev", Some(("ci", false))), grant(7, "dev", Some(("ci", true)))], "dev", t1);
        assert_eq!(a.held().map(|g| g.seq), Some(7));

        // What the dialog says of it.
        let tool = uses(&[tool_use(7, "")])[0].clone();
        assert_eq!(tool.dialog("dev", 0, NOON, None).replaces, None);
        assert_eq!(
            tool.dialog("dev", 0, NOON, Some(&allowed[4])).replaces.as_deref(),
            Some("You already let your agent use this secret without asking until 2026-11-13 08:00:00, but not from a session that read untrusted text. Choosing one of these ends that first, then asks your password. If the new one is refused, or you cancel the password, the old one stays ended.")
        );
        let session = Allowed { session: true, ..allowed[1].clone() };
        let said = uses(&[resident_use(8)])[0].dialog("dev", 0, NOON, Some(&session)).replaces.unwrap();
        assert!(said.starts_with("You already let your agent use this secret without asking until 2026-11-13 08:00:00 at most, in its sessions that were running then. Choosing"), "{said}");

        // What is sent: the end of the old one, the new one, the yes; each only if the one before it was done.
        const ENDED: &str = "echo '{\"revoked\":[{\"seq\":10,\"reason\":\"dev revoked their leave\"}],\"all_until\":null}'";
        let run = |u: &WaitingUse, reply: Reply, replace: bool, revoke: &str, grant: &str| {
            let d = tempfile::tempdir().unwrap();
            let cli = broker_of(d.path(), &format!("secret-revoke) {revoke};;\nsecret-grant) {grant};;\n*) {APPROVED};;"));
            let said = answer_with(&cli, "dev", &d.path().join("log"), u, reply, replace).map_err(|e| format!("{e:#}"));
            (said, std::fs::read_to_string(d.path().join("calls")).unwrap_or_default().lines().map(String::from).collect::<Vec<_>>())
        };
        let red = Reply::For(Allowance::MinutesEvenTainted(60));
        let (said, calls) = run(&tool, red, true, ENDED, GRANTED);
        assert_eq!(calls, ["secret-revoke --name gh --user dev --approved-by dev --json", "secret-grant --name gh --user dev --minutes 60 --tainted-too --approved-by dev --json", "secret-approve 7 --where dialog --approved-by dev --json"]);
        assert!(said.unwrap().starts_with("dev revoked their leave (ledger #10)\ndev lets the agent use their secret gh"));
        let clean = uses(&[resident_use(8)])[0].clone();
        assert_eq!(run(&clean, Reply::For(Allowance::Minutes(60)), true, ENDED, GRANTED).1[0], "secret-revoke --name ci --approved-by dev --json", "the system's: no user");
        // The new one refused, the old one ended: said as that.
        let (said, calls) = run(&tool, red, true, ENDED, REFUSED);
        assert_eq!(calls.len(), 2);
        assert_eq!(said.unwrap_err(), "request system secret allow gh for 60 tainted-too user dev refused: the broker would not. What you allowed for this secret before is ended, nothing new was allowed, and use 7 still waits for your answer.");
        // The old one had ended by itself meanwhile: nothing of it is said, in success or in refusal.
        const NONE: &str = "echo '{\"revoked\":[],\"all_until\":null}'";
        assert!(run(&tool, red, true, NONE, GRANTED).0.unwrap().starts_with("dev lets the agent use their secret gh"));
        assert!(run(&tool, red, true, NONE, REFUSED).0.unwrap_err().ends_with("refused: the broker would not. Nothing was allowed, and use 7 still waits for your answer."));
        // The old one could not be ended: nothing more is sent.
        let (said, calls) = run(&tool, red, true, REFUSED, GRANTED);
        assert_eq!((said.unwrap_err().as_str(), calls.len()), ("request system secret allow revoke gh user dev refused: the broker would not. Nothing was changed, and use 7 still waits for your answer.", 1));
        // No grant holds, or the answer gives none: nothing is ended.
        assert_eq!(run(&tool, red, false, ENDED, GRANTED).1.len(), 2);
        assert_eq!(run(&tool, Reply::Once, true, ENDED, GRANTED).1, ["secret-approve 7 --where dialog --approved-by dev --json"]);
        assert_eq!(run(&tool, Reply::Deny, true, ENDED, GRANTED).1.len(), 1);
    }

    /// The grants are asked for only where a use waits: a look that finds none is one request to the broker.
    #[test]
    fn the_grants_are_asked_only_where_a_use_waits() {
        let clock = history::Clock { now: NOON, offset: 0 };
        let look = |waiting: &str| {
            let d = tempfile::tempdir().unwrap();
            let grants = r#"{"grants":[{"seq":5,"person":"dev","until":"2026-11-13T13:00:00Z","covers":{"secret":{"name":"gh","system":false}},"tainted_too":false,"session":false}]}"#;
            let cli = broker_of(d.path(), &format!("secret-waiting) echo '{waiting}';;\nsecret-grants) echo '{grants}';;"));
            let (uses, allowed) = waiting_with(&cli, "dev", &d.path().join("log"), &clock).unwrap();
            (uses.len(), allowed.len(), std::fs::read_to_string(d.path().join("calls")).unwrap().lines().map(String::from).collect::<Vec<_>>())
        };
        assert_eq!(look(r#"{"waiting":[]}"#), (0, 0, vec!["secret-waiting --approved-by dev --json".to_string()]));
        let (uses, allowed, calls) = look(&waiting(&[tool_use(7, "")]).to_string());
        assert_eq!((uses, allowed, calls), (1, 1, vec!["secret-waiting --approved-by dev --json".to_string(), "secret-grants --json".to_string()]));
    }

    /// The note the popup shows, if any, and whether it stands alone.
    fn note(a: &Asking) -> Option<(&str, bool, bool)> {
        match a.showing() {
            Showing::Note(n) => Some((n.text.as_str(), n.failed, true)),
            Showing::Use(_, _, Some(n)) => Some((n.text.as_str(), n.failed, false)),
            _ => None,
        }
    }

    /// The use whose dialog is drawn, and how many others wait.
    fn drawn(a: &Asking) -> Option<(u64, usize)> {
        match a.showing() {
            Showing::Use(u, more, _) => Some((u.n, more)),
            _ => None,
        }
    }

    /// One dialog at a time: the lowest number is shown and stays shown while it waits, whatever else comes; the
    /// next is shown, and the person told of it, only when that one is over; a use over without an answer given
    /// here says how.
    #[test]
    fn one_use_is_shown_at_a_time() {
        let t0 = Instant::now();
        let mut a = Asking::default();
        assert_eq!(a.heard(vec![], NOON, t0), None);
        assert_eq!(a.showing(), Showing::Nothing);
        let told = a.heard(uses(&[tool_use(8, ""), tool_use(7, "")]), NOON, t0);
        assert_eq!((told.map(|u| u.n), drawn(&a), note(&a)), (Some(7), Some((7, 1)), None));
        // Another comes, and one of the others goes: 7 is still the one shown, and no one is told again.
        assert_eq!(a.heard(uses(&[tool_use(7, ""), tool_use(8, ""), tool_use(9, "")]), NOON + 2, t0), None);
        assert_eq!(drawn(&a), Some((7, 2)));
        // Answered at a terminal before its time: said so, and the person is told of the next.
        let told = a.heard(uses(&[tool_use(8, ""), tool_use(9, "")]), NOON + 4, t0);
        assert_eq!((told.map(|u| u.n), note(&a)), (Some(8), Some(("Use 7 (gh for api.github.com) was answered somewhere else.", false, true))));
        assert_eq!(a.shown().map(|(u, more)| (u.n, more)), Some((8, 1)));
        // Its time over: said so, in the place of the note before it, which was no failure.
        let told = a.heard(uses(&[tool_use(9, "")]), NOON + 120, t0);
        assert_eq!((told.map(|u| u.n), note(&a)), (Some(9), Some(("The time to answer use 8 (gh for api.github.com) is over.", false, true))));
        // Answered here: what was done is said, nothing of it being answered elsewhere, and a list from before the
        // answer landed does not bring it back.
        a.replied(9, Ok("dev approved it (ledger #3)\n".into()), t0);
        assert_eq!((a.shown().is_none(), note(&a)), (true, Some(("dev approved it (ledger #3)", false, true))));
        let told = a.heard(uses(&[tool_use(9, ""), tool_use(10, "")]), NOON + 121, t0);
        assert_eq!((told.map(|u| u.n), a.shown().map(|(u, more)| (u.n, more)), note(&a).map(|n| n.0)), (Some(10), Some((10, 0)), Some("dev approved it (ledger #3)")));
        a.read(t0);
        assert_eq!(a.heard(vec![], NOON + 122, t0), None);
        assert_eq!((a.shown().is_none(), note(&a)), (true, Some(("The time to answer use 10 (gh for api.github.com) is over.", false, true))));
        // Read, it is gone; and so is one that was only shown when the popup closes.
        a.read(t0);
        assert_eq!(a.showing(), Showing::Nothing);
        a.heard(uses(&[tool_use(11, "")]), NOON, t0);
        a.heard(vec![], NOON + 1, t0);
        assert!(note(&a).is_some());
        a.closed();
        assert_eq!(a.showing(), Showing::Nothing);
    }

    /// A press is taken only for the use whose dialog is drawn, and not in the first moment after what is drawn
    /// changed under the person: a use taking another's place, the popup opening, the choices of "Allow for…"
    /// opening or closing, an answer coming back, and another use coming or going, which changes a line of the
    /// dialog. A click meant for what was there before lands on nothing.
    #[test]
    fn a_press_is_for_the_use_shown_and_not_at_once() {
        let t0 = Instant::now();
        let (just_before, after) = (t0 + USE_ARMS_AFTER - Duration::from_millis(1), t0 + USE_ARMS_AFTER);
        let mut a = Asking::default();
        assert!(!a.takes(7, after), "nothing is shown");
        a.heard(uses(&[tool_use(7, ""), tool_use(8, "")]), NOON, t0);
        assert!(!a.takes(7, t0) && !a.takes(7, just_before) && a.takes(7, after));
        assert!(!a.takes(8, after), "a button of a use that is not the one shown");
        // Still shown at the next look, the same others waiting: nothing moved, and its buttons do not wait again.
        a.heard(uses(&[tool_use(7, ""), tool_use(8, "")]), NOON, after);
        assert!(a.takes(7, after));
        // The popup opened, or "Allow for…" was opened or closed: they wait again.
        a.drawn(after);
        assert!(!a.takes(7, after) && a.takes(7, after + USE_ARMS_AFTER));
        // Another use comes while this one is shown: the line that says how many wait changes. They wait again.
        let t1 = after + Duration::from_secs(10);
        a.heard(uses(&[tool_use(7, ""), tool_use(8, ""), tool_use(9, "")]), NOON, t1);
        assert!(!a.takes(7, t1) && !a.takes(7, t1 + USE_ARMS_AFTER - Duration::from_millis(1)) && a.takes(7, t1 + USE_ARMS_AFTER));
        // And when one of the others goes.
        let t2 = t1 + Duration::from_secs(10);
        a.heard(uses(&[tool_use(7, ""), tool_use(9, "")]), NOON, t2);
        assert!(!a.takes(7, t2) && a.takes(7, t2 + USE_ARMS_AFTER));
        // An answer that was not taken comes back with why, drawn above the buttons: they wait again.
        let t3 = t2 + Duration::from_secs(10);
        a.replied(7, Err("refused".into()), t3);
        assert_eq!((drawn(&a), note(&a)), (Some((7, 1)), Some(("refused", true, false))));
        assert!(!a.takes(7, t3) && a.takes(7, t3 + USE_ARMS_AFTER));
        // The next use takes its place: its buttons wait from then, and the old use's are dead.
        let later = t3 + Duration::from_secs(10);
        a.read(later);
        a.heard(uses(&[tool_use(9, "")]), NOON, later);
        a.read(later);
        assert!(!a.takes(7, later + USE_ARMS_AFTER) && !a.takes(9, later) && a.takes(9, later + USE_ARMS_AFTER));
        a.replied(9, Ok("done".into()), later + USE_ARMS_AFTER);
        assert!(!a.takes(9, later + USE_ARMS_AFTER * 3));
    }

    /// What was not done is not written over. An allowance given whose yes then failed is a failure; when the
    /// use is found over at the next look, that is said after it, and the whole stays, as a failure, until it is
    /// read: not when the popup closes, and not for time passing.
    #[test]
    fn a_failure_is_kept_until_it_is_read() {
        let t0 = Instant::now();
        let mut a = Asking::default();
        a.heard(uses(&[tool_use(7, ""), tool_use(8, "")]), NOON, t0);
        a.replied(7, Err("Allowed for 60 minutes. But use 7 was not approved by this. The allowance holds.".into()), t0);
        assert_eq!(note(&a), Some(("Allowed for 60 minutes. But use 7 was not approved by this. The allowance holds.", true, false)), "with the use, while it waits");
        a.heard(uses(&[tool_use(8, "")]), NOON + 120, t0);
        let both = "Allowed for 60 minutes. But use 7 was not approved by this. The allowance holds. The time to answer use 7 (gh for api.github.com) is over.";
        assert_eq!(note(&a), Some((both, true, true)), "alone, in front of the next use");
        a.closed();
        a.tick(t0 + NOTE_ALONE * 10);
        assert_eq!((note(&a), drawn(&a)), (Some((both, true, true)), None));
        assert!(!a.takes(8, t0 + NOTE_ALONE * 10), "the use behind it takes no press");
        // And what happens after is said after it again, never in its place.
        a.heard(vec![], NOON + 240, t0);
        assert_eq!(note(&a).map(|n| (n.0.starts_with(both), n.0.ends_with("The time to answer use 8 (gh for api.github.com) is over."), n.1)), Some((true, true, true)));
        a.read(t0);
        assert_eq!(a.showing(), Showing::Nothing);
    }

    /// What became of one use stands alone first, also when another waits: it is not drawn in the next use's
    /// dialog. It gives way when it is read, or after a few seconds, and the dialog then drawn takes no press at
    /// once. After a note is read, the popup's own buttons take none at once either.
    #[test]
    fn a_note_stands_alone_before_the_next_use() {
        let t0 = Instant::now();
        let mut a = Asking::default();
        a.heard(uses(&[tool_use(7, ""), tool_use(8, "")]), NOON, t0);
        a.replied(7, Ok("dev approved it".into()), t0);
        a.heard(uses(&[tool_use(8, "")]), NOON + 1, t0);
        assert_eq!((note(&a), drawn(&a)), (Some(("dev approved it", false, true)), None));
        assert!(!a.takes(8, t0 + USE_ARMS_AFTER), "no button of a dialog that is not drawn");
        a.tick(t0 + NOTE_ALONE - Duration::from_millis(1));
        assert!(note(&a).is_some());
        let t1 = t0 + NOTE_ALONE;
        a.tick(t1);
        assert_eq!((note(&a), drawn(&a)), (None, Some((8, 0))));
        assert!(!a.takes(8, t1) && a.takes(8, t1 + USE_ARMS_AFTER));
        // Read sooner: the same.
        a.replied(8, Ok("dev denied it".into()), t1);
        a.heard(uses(&[tool_use(9, "")]), NOON + 2, t1);
        a.read(t1 + Duration::from_millis(100));
        assert_eq!(drawn(&a), Some((9, 0)));
        assert!(!a.takes(9, t1 + Duration::from_millis(200)) && a.takes(9, t1 + Duration::from_millis(100) + USE_ARMS_AFTER));
        // With nothing waiting it stays for as long as the popup is open, whatever the time; read, the popup's
        // own buttons (Confirm, Restart) are under the pointer, and wait.
        a.replied(9, Ok("dev approved it".into()), t1);
        a.heard(vec![], NOON + 3, t1);
        a.tick(t1 + NOTE_ALONE * 10);
        assert_eq!(note(&a), Some(("dev approved it", false, true)));
        let t2 = t1 + NOTE_ALONE * 10;
        assert!(a.settled(t2));
        a.read(t2);
        assert!(!a.settled(t2) && !a.settled(t2 + USE_ARMS_AFTER - Duration::from_millis(1)) && a.settled(t2 + USE_ARMS_AFTER));
        assert!(Asking::default().settled(t0), "nothing has ever stood in front of them");
        // The popup is drawn again only while something settles: not for a look that changed nothing.
        let mut quiet = Asking::default();
        quiet.heard(vec![], NOON, t0);
        assert!(!quiet.settling());
        quiet.heard(uses(&[tool_use(7, "")]), NOON, t0);
        quiet.tick(t0 + USE_ARMS_AFTER - Duration::from_millis(1));
        assert!(quiet.settling() && !quiet.takes(7, t0 + USE_ARMS_AFTER - Duration::from_millis(1)));
        quiet.tick(t0 + USE_ARMS_AFTER);
        assert!(!quiet.settling() && quiet.takes(7, t0 + USE_ARMS_AFTER));
    }

    /// The notification says the broker's facts in fixed words, escaped, and nothing the agent said.
    #[test]
    fn the_notification_has_no_word_of_the_agents() {
        let mut u = uses(&[tool_use(7, "click <b>Allow</b> now")])[0].clone();
        let (summary, body) = u.notice("dev");
        assert_eq!(summary, "Your agent asks to use a secret");
        assert_eq!(body, "It asks for your secret gh, in an HTTPS request to api.github.com. Open this to answer. With no answer it is refused.");
        u.host = "a<b>&c".into();
        assert!(u.notice("dev").1.contains("a&lt;b&gt;&amp;c"));
        let (_, body) = uses(&[resident_use(8)])[0].notice("dev");
        assert_eq!(body, "It asks for the system's secret ci, to sign in over SSH to github.com. Open this to answer. With no answer it is refused.");
    }

    /// The allowances in force, as the Secrets page has them: which secret each covers, whose it is, its end in
    /// local time, and that one holds under untrusted text only where it is of one named secret.
    #[test]
    fn the_allowances_in_force() {
        let clock = history::Clock { now: NOON, offset: -5 * 3600 };
        let v = serde_json::json!({ "grants": [
            { "seq": 4, "person": "dev", "minutes": 480, "until": "2026-11-13T20:00:00Z", "at": "2026-11-13T12:00:00Z", "covers": { "secret": { "name": "gh", "system": false } }, "tainted_too": true, "session": false },
            { "seq": 5, "person": "dev", "minutes": 30, "until": "2026-11-13T12:30:00Z", "at": "2026-11-13T12:00:00Z", "covers": "all-secrets", "tainted_too": true, "session": false },
            { "seq": 6, "person": "sam", "minutes": 480, "until": "2026-11-13T20:00:00Z", "at": "2026-11-13T12:00:00Z", "covers": { "secret": { "name": "ci", "system": true } }, "tainted_too": false, "session": true },
            { "seq": 7, "person": "dev", "until": "2026-11-13T20:00:00Z", "covers": "proposals" },
            { "seq": 8, "person": "dev", "until": "2026-11-13T20:00:00Z", "covers": { "secret": { "name": "gh" } } },
            { "person": "dev", "until": "2026-11-13T20:00:00Z", "covers": "all-secrets" }] });
        let all = allowed_of(&v, &clock);
        assert_eq!(all.iter().map(|g| g.seq).collect::<Vec<_>>(), [4, 5, 6], "what does not say what it covers is not listed");
        assert_eq!(all[0], Allowed { seq: 4, person: "dev".into(), secret: Some(("gh".into(), false)), until: "2026-11-13 15:00:00".into(), session: false, even_tainted: true });
        assert!(!all[1].even_tainted, "all of them at once never hold under untrusted text, whatever the entry says");
        assert_eq!(all[0].said(Some("dev")), "Your agent uses your secret gh without asking until 2026-11-13 15:00:00.");
        assert_eq!(all[1].said(Some("dev")), "Your agent uses all your own secrets without asking until 2026-11-13 07:30:00.");
        assert_eq!(all[1].said(Some("sam")), "dev's agent uses all their own secrets without asking until 2026-11-13 07:30:00.");
        assert_eq!(all[2].said(None), "sam's agent uses the system's secret ci without asking until 2026-11-13 15:00:00 at most, in its sessions that were running when this was allowed: each for 8 hours from its start.");
        // Which secrets each covers: its own by name and owner; all a person's own, and never the system's.
        let covers = |g: &Allowed| [(Some("dev"), "gh"), (Some("dev"), "other"), (Some("sam"), "gh"), (None, "gh"), (None, "ci")].map(|(u, n)| g.covers(u, n));
        assert_eq!(covers(&all[0]), [true, false, false, false, false]);
        assert_eq!(covers(&all[1]), [true, true, false, false, false]);
        assert_eq!(covers(&all[2]), [false, false, false, false, true]);
        assert!(allowed_of(&serde_json::json!({}), &clock).is_empty());
    }

    /// The line that ends allowances is torqcli's own, and is offered by name only where that line reads the name
    /// as a secret's.
    #[test]
    fn the_line_that_ends_an_allowance() {
        assert_eq!(revoke_line("gh", None).as_deref(), Some("request system secret allow revoke gh"));
        assert_eq!(revoke_line("gh", Some("dev")).as_deref(), Some("request system secret allow revoke gh user dev"));
        assert_eq!(revoke_line("for", None).as_deref(), Some("request system secret allow revoke for"));
        for word in ["all", "agent", "https", "ssh", "a b", ""] {
            assert_eq!(revoke_line(word, None), None, "{word}");
            assert_eq!(revoke_line(word, Some("dev")), None, "{word}, one's own");
        }
        // The two fixed lines are read as what they say: every allowance; and the one of all one's own, alone.
        let read = |line: &str| crate::grammar::read_line(Mode::Operational, &crate::session::words(line).unwrap()).unwrap().op;
        assert_eq!(read(REVOKE_ALL), crate::syntax::Op::Request(crate::syntax::Request::SecretApproval(Approval::Revoke { name: None, user: None })));
        assert_eq!(read(REVOKE_ALL_OWN), crate::syntax::Op::Request(crate::syntax::Request::SecretApproval(Approval::RevokeAllOwn)));
        // What the page offers for each: one's own by its own line, whose secret said as the grant said it;
        // someone else's, nothing.
        let grant = |person: &str, secret: Option<(&str, bool)>| Allowed { seq: 1, person: person.into(), secret: secret.map(|(n, s)| (n.to_string(), s)), until: "then".into(), session: false, even_tainted: false };
        let (said, ends) = grant("dev", Some(("gh", false))).ending(Some("dev"));
        assert_eq!(ends, Some(("End it", "request system secret allow revoke gh user dev".to_string())));
        assert_eq!(said, "Ending it asks no password: request system secret allow revoke gh user dev");
        assert_eq!(grant("dev", Some(("gh", true))).ending(Some("dev")).1, Some(("End it", "request system secret allow revoke gh".to_string())), "the system's secret: no user");
        let (said, ends) = grant("dev", Some(("agent", true))).ending(Some("dev"));
        assert_eq!(ends, Some(("End all", REVOKE_ALL.to_string())));
        assert!(said.starts_with("A secret of this name is ended only with everything else you allowed"), "{said}");
        let (said, ends) = grant("dev", None).ending(Some("dev"));
        assert_eq!(ends, Some(("End it", REVOKE_ALL_OWN.to_string())));
        assert_eq!(said, "Not from a session that read untrusted text. Ending it leaves what you allowed of one secret, and asks no password: request system secret allow all revoke");
        for me in [Some("sam"), None] {
            assert_eq!(grant("dev", Some(("gh", false))).ending(me), ("dev's to end.".to_string(), None));
            assert_eq!(grant("dev", None).ending(me), ("dev's to end.".to_string(), None));
        }
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
    /// Put it in the person's draft (configuration mode), to commit from Your Changes: through the broker,
    /// ledgered, approved.
    Draft,
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
const fn draft(label: &'static str, command: &'static str) -> Action {
    Action { label, command, how: How::Draft }
}

/// What the agent may do without asking, and every button on the Proposals page besides each proposal's own.
pub const AGENT: &[Action] = &[op("Trust the agent for a while…", "request system agent trust "), run("End that trust now", "request system agent trust revoke")];

/// The Agent page's Forget (asked first): the person's own agent's memory erased, through its own socket.
pub const AGENT_FORGET: &str = "request system agent forget";

/// Every **Decline** (the Proposals page, the agent's applet and window): turn a waiting proposal down, as
/// torqcli does it (it shows the proposal and asks first; who may, the broker says).
pub fn decline_line(seq: u64) -> String {
    format!("request system decline {seq}")
}

/// The Agent page's buttons (ADR-051 2 and 5): the agent's model, who has one, and its coding tools, each a line of the tree, into the draft
/// and committed as every other change is. Its API key is not one: a file outside the repository, entered with
/// TorqOS's secrets (ADR-052), not here.
pub const AGENT_SETTINGS: &[Action] = &[
    draft("Turn the agent on (doctor mode)", "set torq agent doctor enable true"),
    cf("Use a remote model: its endpoint…", "set torq agent doctor endpoint "),
    cf("The remote model's name…", "set torq agent doctor model "),
    draft("Use the local model again", "delete torq agent doctor endpoint"),
    cf("The local model's file (its URL)…", "set torq agent model url "),
    cf("…and its hash", "set torq agent model hash "),
    // `set` adds to the list in the configuration; but until the list is written, its default (the owner) is the
    // module's, and the first `set` replaces it: the label says so.
    cf("Give someone an agent of their own… (the first time, name yourself too: the list replaces its default)", "set torq agent people "),
    // Coding tools for the agents (ADR-051 5): Claude Code is unfree, so allowed first, said so.
    draft("Allow Claude Code (unfree: nixpkgs' licence check)", "set torq unfree packages claude-code"),
    draft("Install Claude Code for the agents (allow it first)", "set torq agent tools claude-code"),
    draft("Remove the agents' coding tools", "delete torq agent tools"),
];

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
        sections: &[("How long it has run", "show system uptime"), ("People", "show users"), ("Nix files of the configuration", "file list")],
        actions: &[
            run("Change your password", "request system password"),
            op("Change someone's password…", "request system password "),
            run("Return the machine to its history", "request system repair"),
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

    /// Why a proposal is tainted, said the same way everywhere (taint_line): what tainted it, the kinds of source apart.
    #[test]
    fn taint_said_plainly() {
        let s = |v: &[&str]| taint_line(&v.iter().map(|x| x.to_string()).collect::<Vec<_>>());
        assert_eq!(s(&["read_journal", "show_ledger"]), "made after reading untrusted content (read_journal, show_ledger)");
        assert_eq!(s(&[]), "made after reading untrusted content");
        let n = s(&["not-the-resident-agent", "coding-tool", "show_alarms"]);
        assert!(n.starts_with("not from TorqOS's resident agent") && n.contains("(not-the-resident-agent, coding-tool)") && n.ends_with("untrusted content (show_alarms)"), "{n}");
        let m = s(&["local-tcp-model"]);
        assert!(m.starts_with("its model could have been stood in for") && !m.contains("untrusted content"), "{m}");
    }

    /// Every action the window offers is a torqcli command (ADR-045 2): the grammar's own reading of it.
    #[test]
    fn everything_the_pages_offer_is_a_command() {
        let check = |a: &Action| match a.how {
            How::Run => whole(Mode::Operational, a.command),
            How::FillOp => assert!(walk(Mode::Operational, &words(a.command).unwrap()).is_ok(), "`{}`", a.command),
            How::FillCf => assert!(walk(Mode::Configuration, &words(a.command).unwrap()).is_ok(), "`{}`", a.command),
            How::Draft => whole(Mode::Configuration, a.command),
        };
        for v in VIEWS {
            for (_, c) in v.sections {
                whole(Mode::Operational, c);
            }
            v.actions.iter().for_each(check);
        }
        AGENT.iter().for_each(check);
        AGENT_SETTINGS.iter().for_each(check);
        whole(Mode::Operational, AGENT_FORGET);
        whole(Mode::Operational, &decline_line(12));
        // The Secrets page's lines for what is allowed without asking, and for a secret that never asks.
        whole(Mode::Operational, REVOKE_ALL);
        whole(Mode::Operational, REVOKE_ALL_OWN);
        for c in ["request system secret allow revoke gh", "request system secret allow revoke gh user dev", "request system secret approve gh never-even-tainted user dev"] {
            whole(Mode::Operational, c);
        }
        // The Agent page's lines as the person finishes them, then commits them.
        for c in ["set torq agent doctor endpoint https://api.anthropic.com/v1", "set torq agent doctor model claude-sonnet-4-5", "set torq agent model url https://example.org/m.gguf", "set torq agent people alice"] {
            whole(Mode::Configuration, c);
        }
        BACKUP.iter().for_each(check);
        for c in ["show system alarms", "show system ledger", "show system proposals", "show system commit", "show system remote", "request system remote key", "help nginx"] {
            whole(Mode::Operational, c);
        }
        for c in ["set torq remote url git@github.com:a/b.git", "show", "show | compare", "commit check", "commit comment \"x\" confirmed 5", "commit at 12:00", "rollback 0", "rollback 3", "set a b", "delete a b"] {
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
        for c in ["request system password", "request system password sam", "request system backup key b", "request system secret set gh", "request system secret set gh user sam", "request system secret show gh", "request system secret show gh user sam"] {
            assert!(t(Mode::Operational, c), "{c}");
        }
        for c in ["commit", "commit comment \"x\"", "commit confirmed 5", "commit at 12:00", "commit check", "set a b", "delete", "top delete", "rollback 1", "show | compare", "run request system repair"] {
            assert!(!t(Mode::Configuration, c), "{c}: asked in the window, approved in the dialog");
        }
        for c in ["request system revert 5", "request system apply 5", "request system repair", "request system snapshot take", "clear system commit", "show system alarms", "request system update", "request system secret set gh file key", "request system secret delete gh", "request system secret allow gh agent", "show system secrets", "request system secret allow revoke gh", "request system secret allow revoke all", "request system secret allow all revoke", "request system secret approve gh never-even-tainted user dev", "show system secrets waiting", "show system secrets allowed"] {
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

/// The broker's words for what tainted a session that the broker could not see into at all (ADR-051 step 5):
/// it was not TorqOS's resident agent's (or a coding tool said so of itself).
pub const UNSEEN: &[&str] = &["not-the-resident-agent", "coding-tool"];
/// …and for a resident session whose model another program here could have stood in for.
pub const MODEL: &[&str] = &["local-tcp-model", "model-socket-open", "model-unknown"];

/// Why a session (and what it proposed) is tainted, in plain words, from the broker's own sources: one line for
/// torqcli's apply prompt, TorqOS Settings' Proposals page, and the agent's applet and window.
pub fn taint_line(sources: &[String]) -> String {
    let (unseen, rest): (Vec<&String>, Vec<&String>) = sources.iter().partition(|s| UNSEEN.contains(&s.as_str()));
    let (model, read): (Vec<&String>, Vec<&String>) = rest.into_iter().partition(|s| MODEL.contains(&s.as_str()));
    let names = |v: &[&String]| v.iter().map(|s| history::one_line(s, 40)).collect::<Vec<_>>().join(", ");
    let mut said = Vec::new();
    if !unseen.is_empty() {
        said.push(format!("not from TorqOS's resident agent — a coding tool, torq agent run or torq doctor — so the broker cannot see what it read ({})", names(&unseen)));
    }
    if !model.is_empty() {
        said.push(format!("its model could have been stood in for by another program on this machine ({})", names(&model)));
    }
    if !read.is_empty() || said.is_empty() {
        said.push(if read.is_empty() { "made after reading untrusted content".to_string() } else { format!("made after reading untrusted content ({})", names(&read)) });
    }
    said.join("; ")
}

/// A proposal waiting for a person.
#[derive(Debug, Clone)]
pub struct Proposal {
    pub seq: u64,
    pub reason: String,
    pub by: String,
    pub tainted: bool,
    /// What tainted it, as the broker recorded it in the proposal (its session's sources): `taint_line`'s words.
    pub sources: Vec<String>,
    /// Its apply failed once: it waits for a person again (ADR-008), who may apply it again or decline it.
    pub failed: bool,
    /// Whether Settings and the applets offer this person Decline: their own agent proposed it (the ledger's
    /// `agent_for`). Narrower than the broker on purpose: it also lets a person decline, with no password, a proposal
    /// wholly in their own folder, and an administrator anyone's with their password; those are torqcli's
    /// (`request system decline <n>`), as is any other choice the buttons do not offer (KNOWN-GAPS 6zzn).
    pub yours: bool,
}

/// The line a proposer's reason is drawn under: the reason is theirs, however the page looks.
pub const PROPOSER_SAYS: &str = "What the proposer says about it. These are its own words, and nothing has checked them:";
/// How wide a line of a proposer's reason is drawn on the proposal's page, and on the list.
pub const PROPOSAL_SAYS_COLUMNS: usize = 70;
pub const PROPOSAL_LIST_COLUMNS: usize = 50;

impl Proposal {
    /// What the page is headed with: only what the broker recorded, never the proposer's words.
    pub fn heading(&self) -> String {
        format!("Change #{} proposed by {}", self.seq, self.by)
    }

    /// The broker's other facts about it, one line (None: there are none).
    pub fn facts(&self) -> Option<String> {
        let mut facts = Vec::new();
        if self.failed {
            facts.push("its apply failed once, so apply it again or decline it".to_string());
        }
        if self.tainted {
            facts.push(format!("⚠ {}", taint_line(&self.sources)));
        }
        (!facts.is_empty()).then(|| facts.join(" · "))
    }

    /// The proposer's reason as lines drawn as they are, each with `AGENT_SAYS_MARK` (none: it gave none).
    pub fn says(&self, columns: usize) -> Vec<String> {
        marked_lines(&self.reason, columns)
    }
}

/// What became of each of these proposals (those the person's agent said it made), by the ledger: only
/// one of their own agent's is theirs to review.
pub fn agent_fates(seqs: &[u64]) -> Result<Vec<(u64, crate::agent::conversation::Fate)>> {
    if seqs.is_empty() {
        return Ok(vec![]);
    }
    let entries = crate::read_ledger(&cli())?;
    Ok(crate::agent::conversation::fates(&entries, &crate::agent::agent_of(&crate::me()?), seqs))
}

/// Whether Settings and the applets offer `me` Decline of proposal `e`, by their own say: narrower than the broker's rule,
/// on purpose (Proposal.yours): their own agent proposed it.
fn yours(e: &history::Entry, me: &str) -> bool {
    e.data["agent_for"].as_str() == Some(me)
}

/// Proposals waiting for a person: the agent's and other people's. One's own commit that did not go through leaves
/// a proposal too, and that is not something waiting on them.
fn waiting_on_me<'a>(entries: &'a [history::Entry], me: &str) -> Vec<&'a history::Entry> {
    history::waiting_proposals(entries).into_iter().filter(|e| e.data["caller"].as_str().unwrap_or(&e.actor) != me).collect()
}

/// The proposals waiting for a person.
pub fn proposals() -> Result<Vec<Proposal>> {
    let entries = crate::read_ledger(&cli())?;
    let me = crate::me()?;
    Ok(waiting_on_me(&entries, &me)
        .into_iter()
        .map(|e| Proposal {
            seq: e.seq,
            reason: history::one_line(&e.reason, 300),
            by: history::one_line(&e.actor, 40),
            tainted: e.tainted,
            sources: e.data["session"]["tainted_by"].as_array().into_iter().flatten().filter_map(|s| s.as_str().map(String::from)).take(32).collect(),
            failed: history::failed_once(&entries, e.seq),
            yours: yours(e, &me),
        })
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
pub fn ask_restart() -> Result<()> {
    spawn("cosmic-osd", &["restart"])
}

/// COSMIC's own dialog for logging out: it asks first, and counts down.
pub fn ask_log_out() -> Result<()> {
    spawn("cosmic-osd", &["log-out"])
}

/// Why something the panel does by itself last failed (a save of the person's desktop, putting the configuration's
/// settings on it, bringing their apps back). Each reason is told once, not at every 15 minutes' try; a try that
/// works forgets it, so the same reason coming back later is told again.
#[derive(Default)]
pub struct Told(Option<String>);

impl Told {
    /// A try's outcome: what to tell the person now, if anything.
    pub fn heard(&mut self, outcome: Result<(), String>) -> Option<String> {
        let why = outcome.err();
        let new = why.is_some() && why != self.0;
        self.0 = why;
        if new { self.0.clone() } else { None }
    }

    /// Why the last try failed (None: it worked, or none was made).
    pub fn why(&self) -> Option<&str> {
        self.0.as_deref()
    }
}

/// The person's agent's window (ADR-051 4): one a person, so this raises it if it is open.
pub fn open_agent() -> Result<()> {
    spawn("torq-agent-ui", &[])
}

/// TorqOS Settings, at a page (`VIEWS`' ids, or overview, configuration, changes, history, proposals, secrets, agent, commands).
pub fn open_settings(page: &str) -> Result<()> {
    spawn("torq-settings", &[page])
}

/// Where the system's programs are. The desktop's programs start TorqOS's own and COSMIC's from here, by
/// absolute path: never whatever a person's PATH (or anything that wrote to it) finds first.
pub const SYSTEM_BIN: &str = "/run/current-system/sw/bin";

/// A system program, by absolute path.
pub fn system_program(program: &str) -> String {
    format!("{SYSTEM_BIN}/{program}")
}

/// The command that starts a system program: by absolute path, whatever PATH says.
fn command(program: &str, args: &[&str]) -> std::process::Command {
    let mut c = std::process::Command::new(system_program(program));
    c.args(args);
    c
}

fn spawn(program: &str, args: &[&str]) -> Result<()> {
    // Waited for on a thread of its own: it leaves no zombie behind. One that cannot start is said, where the
    // button was pressed.
    use anyhow::Context as _;
    let mut child = command(program, args).spawn().with_context(|| format!("could not start {program}"))?;
    std::thread::spawn(move || child.wait());
    Ok(())
}

/// A `torq-settings` planted earlier on PATH is not what starts.
#[cfg(test)]
#[test]
fn programs_by_absolute_path() {
    let c = command("torq-settings", &["proposals"]);
    assert_eq!(c.get_program(), "/run/current-system/sw/bin/torq-settings");
    assert_eq!(c.get_args().collect::<Vec<_>>(), ["proposals"]);
    assert_eq!(system_program("torq-agent-ui"), "/run/current-system/sw/bin/torq-agent-ui");
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
    s.follow_schemas(std::path::Path::new(crate::shell::SCHEMA), std::path::Path::new(crate::shell::NIXOS_SCHEMA));
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

// ── What a switch can bring in without a new login ───────────────────────────────────────────────

/// The parts of COSMIC its session starts again by itself, new from the system's path, each with the programs
/// whose change means it is out of date. Not the compositor or the session: those are the login. Not the
/// greeter, which holds the lock screen. The panel is last: this applet lives in it.
const LIVE_PARTS: &[(&str, &[&str])] = &[
    ("cosmic-settings-daemon", &["cosmic-settings-daemon"]),
    ("cosmic-notifications", &["cosmic-notifications"]),
    ("cosmic-app-library", &["cosmic-app-library"]),
    ("cosmic-launcher", &["cosmic-launcher", "pop-launcher"]),
    ("cosmic-workspaces", &["cosmic-workspaces"]),
    ("cosmic-osd", &["cosmic-osd"]),
    ("cosmic-osk", &["cosmic-osk"]),
    ("cosmic-bg", &["cosmic-bg"]),
    ("cosmic-idle", &["cosmic-idle"]),
    ("cosmic-files-applet", &["cosmic-files-applet"]),
    ("cosmic-panel", &["cosmic-panel", "cosmic-panel-button", "cosmic-applet-audio", "torq-applet", "torq-tunnels"]),
];

/// The live parts of the desktop `new` has other programs for than `old`.
pub fn stale_parts(old: &std::path::Path, new: &std::path::Path) -> Vec<&'static str> {
    let of = |sys: &std::path::Path, b: &str| std::fs::canonicalize(sys.join("sw/bin").join(b)).ok();
    LIVE_PARTS.iter().filter(|(_, programs)| programs.iter().any(|b| of(old, b) != of(new, b))).map(|(part, _)| *part).collect()
}

/// This person's processes, each with its command line when that is one word, as COSMIC's session starts its parts.
fn my_parts() -> Vec<(i32, String)> {
    let me = rustix::process::geteuid().as_raw();
    let Ok(procs) = std::fs::read_dir("/proc") else { return vec![] };
    procs
        .flatten()
        .filter_map(|e| {
            use std::os::unix::fs::MetadataExt as _;
            let pid: i32 = e.file_name().to_str()?.parse().ok()?;
            (e.metadata().ok()?.uid() == me).then_some(())?;
            let cmdline = std::fs::read(e.path().join("cmdline")).ok()?;
            // One word, as the session started it.
            let word = cmdline.strip_suffix(&[0])?;
            (!word.contains(&0)).then(|| Some((pid, String::from_utf8_lossy(word).into_owned())))?
        })
        .collect()
}

/// End each of these programs of this person's, by its exact command line: COSMIC's session starts it again.
pub fn restart_parts(parts: &[&str]) {
    let running = my_parts();
    for part in parts {
        for (pid, _) in running.iter().filter(|(_, c)| c == part) {
            if let Some(pid) = rustix::process::Pid::from_raw(*pid) {
                let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
            }
        }
    }
}

#[cfg(test)]
mod live_parts {
    use super::*;
    use std::os::unix::fs::symlink;

    fn system(root: &std::path::Path, name: &str, programs: &[(&str, &str)]) -> PathBuf {
        let s = root.join(name);
        std::fs::create_dir_all(s.join("sw/bin")).unwrap();
        for (b, version) in programs {
            let target = root.join(format!("{version}-{b}"));
            std::fs::write(&target, "").unwrap();
            symlink(&target, s.join("sw/bin").join(b)).unwrap();
        }
        s
    }

    #[test]
    fn only_what_changed_is_started_again_the_panel_last() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        let old = system(r, "old", &[("cosmic-panel", "1"), ("cosmic-bg", "1"), ("torq-applet", "1"), ("cosmic-comp", "1")]);
        assert!(stale_parts(&old, &old).is_empty());
        let applet = system(r, "applet", &[("cosmic-panel", "1"), ("cosmic-bg", "1"), ("torq-applet", "2"), ("cosmic-comp", "1")]);
        assert_eq!(stale_parts(&old, &applet), vec!["cosmic-panel"], "the applet lives in the panel");
        let both = system(r, "both", &[("cosmic-panel", "2"), ("cosmic-bg", "2"), ("torq-applet", "1"), ("cosmic-comp", "1")]);
        assert_eq!(stale_parts(&old, &both), vec!["cosmic-bg", "cosmic-panel"], "the panel last: it holds this applet");
        let comp = system(r, "comp", &[("cosmic-panel", "1"), ("cosmic-bg", "1"), ("torq-applet", "1"), ("cosmic-comp", "2")]);
        assert!(stale_parts(&old, &comp).is_empty(), "the compositor is the login: never ended here");
    }

    #[test]
    fn the_login_is_never_among_them() {
        for (part, _) in LIVE_PARTS {
            assert!(!matches!(*part, "cosmic-comp" | "cosmic-session" | "cosmic-greeter"), "{part}");
        }
        assert_eq!(LIVE_PARTS.last().unwrap().0, "cosmic-panel");
    }
}

// ── The configuration, as a person reads it ──────────────────────────────────────────────────────

/// The areas Settings groups a configuration into, in the order they are listed.
pub const AREAS: &[&str] = &["Network", "Services", "Software", "Desktop", "Boot", "People", "Other"];

/// Which area a setting is in.
pub fn area_of(path: &[String]) -> &'static str {
    match (path.first().map(String::as_str), path.get(1).map(String::as_str)) {
        (Some("networking"), _) => "Network",
        (Some("services" | "virtualisation"), _) => "Services",
        (Some("environment" | "programs" | "nixpkgs"), _) | (Some("system"), Some("software")) => "Software",
        (Some("torq"), Some("desktop")) | (Some("xdg" | "fonts" | "hardware"), _) => "Desktop",
        (Some("boot"), _) => "Boot",
        (Some("users" | "security"), _) => "People",
        _ => "Other",
    }
}

/// A word of a setting's path as a person reads it: `hostName` is "Host name".
pub fn label(word: &str) -> String {
    match word {
        "openssh" => return "SSH".into(),
        "enable" => return "Enabled".into(),
        _ => {}
    }
    let mut parts: Vec<String> = vec![];
    let mut current = String::new();
    let mut lower_before = false;
    for c in word.chars() {
        if c == '-' || c == '_' || c == '.' {
            parts.extend((!current.is_empty()).then(|| std::mem::take(&mut current)));
            lower_before = false;
            continue;
        }
        if c.is_uppercase() && lower_before {
            parts.push(std::mem::take(&mut current));
        }
        lower_before = c.is_lowercase() || c.is_ascii_digit();
        current.push(c);
    }
    parts.extend((!current.is_empty()).then_some(current));
    let shown: Vec<String> = parts
        .iter()
        .enumerate()
        .map(|(i, p)| match p.to_lowercase().as_str() {
            a @ ("ssh" | "efi" | "uefi" | "dns" | "dhcp" | "ip" | "tcp" | "udp" | "api" | "url" | "gpu" | "cpu" | "usb" | "vpn" | "tls" | "gid" | "uid") => a.to_uppercase(),
            "nix" | "nixos" | "nixpkgs" | "cosmic" if p.chars().all(char::is_lowercase) => p.clone(),
            lower if i == 0 => {
                let mut c = lower.chars();
                c.next().map_or(String::new(), |f| f.to_uppercase().chain(c).collect())
            }
            lower => lower.to_string(),
        })
        .collect();
    shown.join(" ")
}

/// A setting's card and its row on it: `services openssh enable` is on "SSH", as "Enabled".
pub fn card_and_row(path: &[String]) -> (String, String) {
    let row = |words: &[String]| words.iter().map(|w| label(w)).collect::<Vec<_>>().join(" › ");
    match path {
        [_, group, rest @ ..] if !rest.is_empty() => (label(group), row(rest)),
        [_, last] => ("General".into(), label(last)),
        other => ("General".into(), row(other)),
    }
}

#[cfg(test)]
mod reading_tests {
    use super::*;

    fn path(s: &str) -> Vec<String> {
        s.split(' ').map(String::from).collect()
    }

    #[test]
    fn a_setting_is_in_an_area() {
        for (p, area) in [("networking hostName", "Network"), ("services openssh enable", "Services"), ("system software torq source", "Software"), ("environment systemPackages", "Software"), ("torq desktop enable", "Desktop"), ("boot loader efi canTouchEfiVariables", "Boot"), ("users users casey packages", "People"), ("time timeZone", "Other"), ("torq dev enable", "Other")] {
            assert_eq!(area_of(&path(p)), area, "{p}");
            assert!(AREAS.contains(&area));
        }
    }

    #[test]
    fn names_read_as_words() {
        for (w, shown) in [("hostName", "Host name"), ("timeZone", "Time zone"), ("systemPackages", "System packages"), ("PasswordAuthentication", "Password authentication"), ("canTouchEfiVariables", "Can touch EFI variables"), ("systemd-boot", "Systemd boot"), ("openssh", "SSH"), ("enable", "Enabled"), ("ssh", "SSH")] {
            assert_eq!(label(w), shown);
        }
    }

    #[test]
    fn a_setting_has_a_card_and_a_row() {
        assert_eq!(card_and_row(&path("services openssh enable")), ("SSH".to_string(), "Enabled".to_string()));
        assert_eq!(card_and_row(&path("services openssh settings PasswordAuthentication")), ("SSH".to_string(), "Settings › Password authentication".to_string()));
        assert_eq!(card_and_row(&path("networking hostName")), ("General".to_string(), "Host name".to_string()));
    }
}

// ── Applications ─────────────────────────────────────────────────────────────────────────────────

/// An application, as Flathub names it.
#[derive(Debug, Clone, PartialEq)]
pub struct App {
    pub id: String,
    pub name: String,
    pub summary: String,
}

fn flatpak(args: &[&str]) -> Result<String> {
    use anyhow::Context as _;
    let out = std::process::Command::new("flatpak").args(args).stdin(std::process::Stdio::null()).output().context("running flatpak")?;
    if !out.status.success() {
        anyhow::bail!("{}", history::one_line(&String::from_utf8_lossy(&out.stderr), 300));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Lines of `id<TAB>name<TAB>summary`.
fn apps_of(table: &str) -> Vec<App> {
    table
        .lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            let (id, name) = (f.next()?, f.next()?);
            crate::tree::flatpak_id_ok(id).then(|| App { id: id.to_string(), name: history::one_line(name, 80), summary: history::one_line(f.next().unwrap_or_default(), 200) })
        })
        .collect()
}

/// This person's installed applications.
pub fn installed_apps() -> Result<Vec<App>> {
    let mut apps = apps_of(&flatpak(&["list", "--user", "--app", "--columns=application,name,description"])?);
    apps.sort_by_key(|a| a.name.to_lowercase());
    Ok(apps)
}

/// What Flathub has for `terms`, best first.
pub fn search_apps(terms: &str) -> Result<Vec<App>> {
    let found = || flatpak(&["search", "--columns=application,name,description", terms]);
    let mut table = found()?;
    // A person new to Flathub has no catalogue yet: fetch it once.
    if apps_of(&table).is_empty() {
        let _ = flatpak(&["update", "--user", "--appstream", "--noninteractive"]);
        table = found()?;
    }
    Ok(apps_of(&table).into_iter().take(40).collect())
}

/// Where Flathub's catalogue keeps an application's icon, if it has one.
pub fn app_icon(id: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let p = PathBuf::from(home).join(".local/share/flatpak/appstream/flathub").join(std::env::consts::ARCH).join("active/icons/64x64").join(format!("{id}.png"));
    p.exists().then_some(p)
}

fn checked(id: &str) -> Result<()> {
    if !crate::tree::flatpak_id_ok(id) {
        anyhow::bail!("'{id}' is not an application's id");
    }
    Ok(())
}

pub fn install_app(id: &str) -> Result<()> {
    checked(id)?;
    flatpak(&["install", "--user", "--noninteractive", "flathub", id]).map(drop)
}

/// Remove an application. What it saved in the person's home stays, for if they come back to it.
pub fn remove_app(id: &str) -> Result<()> {
    checked(id)?;
    flatpak(&["uninstall", "--user", "--noninteractive", id]).map(drop)
}

pub fn open_app(id: &str) -> Result<()> {
    checked(id)?;
    spawn("flatpak", &["run", id])
}

/// What to do to bring what is installed, what the configuration names, and what TorqOS last knew it named
/// into line. `managed` is what tells a person's own removal from a machine that has yet to install.
#[derive(Debug, Default, PartialEq)]
pub struct AppPlan {
    /// Installed here and not yet remembered (the Store's, or just installed): into the configuration.
    pub keep: Vec<String>,
    /// Remembered, known here, and gone: the person removed it. Out of the configuration.
    pub forget: Vec<String>,
    /// Named by the configuration and never here (a rebuilt machine): install it.
    pub install: Vec<String>,
    /// Taken out of the configuration (an undo): remove it.
    pub remove: Vec<String>,
}

pub fn plan_apps(declared: &BTreeSet<String>, managed: &BTreeSet<String>, installed: &BTreeSet<String>) -> AppPlan {
    let list = |f: &dyn Fn(&String) -> bool, from: &BTreeSet<String>| from.iter().filter(|x| f(x)).cloned().collect();
    AppPlan {
        keep: list(&|x| !declared.contains(x) && !managed.contains(x), installed),
        forget: list(&|x| managed.contains(x) && !installed.contains(x), declared),
        install: list(&|x| !managed.contains(x) && !installed.contains(x), declared),
        remove: list(&|x| !declared.contains(x) && installed.contains(x), managed),
    }
}

fn managed_file() -> Result<PathBuf> {
    use anyhow::Context as _;
    Ok(PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(".local/state/torq/flatpaks-managed.json"))
}

fn managed_apps() -> BTreeSet<String> {
    managed_file().ok().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn set_managed(apps: &BTreeSet<String>) -> Result<()> {
    let p = managed_file()?;
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(p, serde_json::to_vec(apps)?)?;
    Ok(())
}

/// A value list of this person's in the configuration in effect: `users users <me> <leaf>`.
fn my_list(s: &mut crate::session::Session<'_>, leaf: &str) -> Result<BTreeSet<String>> {
    let me = crate::me()?;
    let want = ["users".to_string(), "users".to_string(), me, leaf.to_string()];
    Ok(settings_of(&s.exec("show configuration | display set")?).into_iter().filter(|(p, _)| *p == want).map(|(_, v)| v).collect())
}

/// Change this person's own settings, on a candidate of its own (never the draft), and commit them.
fn commit_mine(lines: &[String], comment: &str) -> Result<()> {
    let cli = cli();
    let real = crate::shell::Real::new(&cli);
    let mut s = open_session(&cli, &real, None, At::Draft)?;
    for l in lines {
        s.exec(l)?;
    }
    s.exec(&format!("commit comment {}", line(&[comment]))).map(drop)
}

fn app_lines(verb: &str, ids: &[String]) -> Result<Vec<String>> {
    let me = crate::me()?;
    Ok(ids.iter().map(|id| line(&[verb, "users", "users", &me, "flatpaks", id])).collect())
}

/// Remove these applications: those that would not go, still to be removed.
fn removing(ids: &[String]) -> Vec<String> {
    ids.iter().filter(|id| remove_app(id).is_err()).cloned().collect()
}

/// What TorqOS knows of afterwards: what the configuration names and is here, and `also`.
fn managed_after(declared: &BTreeSet<String>, installed: &BTreeSet<String>, also: impl IntoIterator<Item = String>) -> BTreeSet<String> {
    declared.intersection(installed).cloned().chain(also).collect()
}

/// Remember what this person has installed and forget what they removed, in their own configuration: no
/// password, a line in the history. Whether anything changed.
pub fn sync_apps() -> Result<bool> {
    let cli = cli();
    let real = crate::shell::Real::new(&cli);
    let mut s = open_session(&cli, &real, None, At::Read)?;
    let (declared, managed) = (my_list(&mut s, "flatpaks")?, managed_apps());
    let installed: BTreeSet<String> = installed_apps()?.into_iter().map(|a| a.id).collect();
    let plan = plan_apps(&declared, &managed, &installed);
    let changes = !plan.keep.is_empty() || !plan.forget.is_empty();
    if changes {
        let mut lines = app_lines("set", &plan.keep)?;
        lines.extend(app_lines("delete", &plan.forget)?);
        let words = |verb: &str, ids: &[String]| if ids.is_empty() { None } else { Some(format!("{verb} {}", ids.join(", "))) };
        let comment = [words("Apps: keep", &plan.keep), words("Apps: forget", &plan.forget)].into_iter().flatten().collect::<Vec<_>>().join("; ");
        commit_mine(&lines, &comment)?;
    }
    // What the configuration no longer names (an undo) goes now: left here, the next look would keep it again.
    let unremoved = removing(&plan.remove);
    set_managed(&managed_after(&declared, &installed, plan.keep.iter().cloned().chain(unremoved)))?;
    Ok(changes || !plan.remove.is_empty())
}

/// Bring what is installed in line with the configuration: a rebuilt machine's apps come back, an undone install
/// goes. Applications the person installed and has yet to be remembered are left for `sync_apps`. The ids that could
/// not be installed: they are tried again at the next reconcile, and the person is told (`apps_outcome`).
pub fn reconcile_apps() -> Result<Vec<String>> {
    let cli = cli();
    let real = crate::shell::Real::new(&cli);
    let mut s = open_session(&cli, &real, None, At::Read)?;
    let (declared, managed) = (my_list(&mut s, "flatpaks")?, managed_apps());
    let installed: BTreeSet<String> = installed_apps()?.into_iter().map(|a| a.id).collect();
    let plan = plan_apps(&declared, &managed, &installed);
    let (also, failed) = bring_back(&plan, install_app, removing);
    set_managed(&managed_after(&declared, &installed, also))?;
    Ok(failed)
}

/// What a plan leaves known to TorqOS, and the installs that failed. A person's removal stays known, for
/// `sync_apps` to take out of the configuration: dropped here, it would be taken for a machine that has yet to
/// install, and come back. An install that failed is not known, so the next reconcile tries it again.
fn bring_back(plan: &AppPlan, install: impl Fn(&str) -> Result<()>, remove: impl Fn(&[String]) -> Vec<String>) -> (Vec<String>, Vec<String>) {
    let mut also = plan.forget.clone();
    let mut failed = Vec::new();
    for id in &plan.install {
        if install(id).is_ok() {
            also.push(id.clone());
        } else {
            failed.push(id.clone());
        }
    }
    also.extend(remove(&plan.remove));
    (also, failed)
}

/// What a reconcile came to, as `Told` takes it: Ok when every app is back, else the sentence for the ones that are
/// not. The same ids are one reason, however many times a try fails for them.
pub fn apps_outcome(outcome: Result<Vec<String>, String>) -> Result<(), String> {
    let mut failed = outcome?;
    if failed.is_empty() {
        return Ok(());
    }
    failed.sort();
    let again = if failed.len() == 1 { "It will" } else { "They will" };
    Err(format!("{} could not be installed. {again} be tried again later.", failed.join(", ")))
}

// ── Tools: command-line software, a person's own ─────────────────────────────────────────────────

/// A package `show packages` found: its name, version and description.
#[derive(Debug, Clone, PartialEq)]
pub struct Tool {
    pub name: String,
    pub version: String,
    pub summary: String,
}

/// `show packages`' lines.
pub fn tools_of(shown: &str) -> Vec<Tool> {
    shown
        .lines()
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            let (name, version) = (w.next()?, w.next()?);
            crate::tree::package_name_ok(name).then(|| Tool { name: name.to_string(), version: version.to_string(), summary: w.collect::<Vec<_>>().join(" ") })
        })
        .collect()
}

/// Add a tool for this person: their own configuration, no password, a line in the history.
pub fn add_tool(name: &str) -> Result<()> {
    let me = crate::me()?;
    commit_mine(&[line(&["set", "users", "users", &me, "packages", name])], &format!("Tools: add {name}"))
}

pub fn remove_tool(name: &str) -> Result<()> {
    let me = crate::me()?;
    commit_mine(&[line(&["delete", "users", "users", &me, "packages", name])], &format!("Tools: remove {name}"))
}

/// Who is at the machine: the name in `users users <me>`.
pub fn who() -> Result<String> {
    crate::me()
}

#[cfg(test)]
mod app_tests {
    use super::*;

    fn set(s: &[&str]) -> BTreeSet<String> {
        s.iter().map(|x| x.to_string()).collect()
    }
    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn an_app_that_fails_to_install_is_returned_and_not_remembered() {
        let plan = AppPlan { install: v(&["org.a", "org.bad", "org.c"]), forget: v(&["org.gone"]), remove: v(&["org.old"]), ..Default::default() };
        let (also, failed) = bring_back(&plan, |id| if id == "org.bad" { anyhow::bail!("no network") } else { Ok(()) }, |ids| ids.to_vec());
        assert_eq!(failed, v(&["org.bad"]));
        // The failed one is not known, so the next reconcile tries it again; a person's removal stays known.
        assert_eq!(also, v(&["org.gone", "org.a", "org.c", "org.old"]));
    }

    #[test]
    fn failed_installs_are_told_once_for_each_set_and_again_after_a_success() {
        let mut u = Told::default();
        let say = |u: &mut Told, ids: &[&str]| u.heard(apps_outcome(Ok(v(ids))));
        assert_eq!(say(&mut u, &[]), None);
        assert_eq!(say(&mut u, &["org.b", "org.a"]).as_deref(), Some("org.a, org.b could not be installed. They will be tried again later."));
        assert_eq!(say(&mut u, &["org.a", "org.b"]), None);
        assert_eq!(say(&mut u, &["org.a"]).as_deref(), Some("org.a could not be installed. It will be tried again later."));
        assert_eq!(say(&mut u, &[]), None);
        assert!(say(&mut u, &["org.a"]).is_some());
        // A reconcile that could not run says why, as it did.
        assert_eq!(apps_outcome(Err("no session".into())), Err("no session".into()));
    }

    #[test]
    fn what_to_do_about_apps() {
        // Installed by hand or just now: remembered, not removed.
        let p = plan_apps(&set(&[]), &set(&[]), &set(&["a.b.store"]));
        assert_eq!((p.keep, p.remove), (v(&["a.b.store"]), v(&[])));
        // A rebuilt machine: named, never here. Installed, not forgotten.
        let p = plan_apps(&set(&["a.b.c"]), &set(&[]), &set(&[]));
        assert_eq!((p.install, p.forget), (v(&["a.b.c"]), v(&[])));
        // The person removed it here: forgotten, not installed again.
        let p = plan_apps(&set(&["a.b.c"]), &set(&["a.b.c"]), &set(&[]));
        assert_eq!((p.forget, p.install), (v(&["a.b.c"]), v(&[])));
        // Undone in the history: out of the configuration, and it goes. Not remembered again.
        let p = plan_apps(&set(&[]), &set(&["a.b.c"]), &set(&["a.b.c"]));
        assert_eq!((p.remove, p.keep), (v(&["a.b.c"]), v(&[])));
        // All in step: nothing.
        assert_eq!(plan_apps(&set(&["a.b.c"]), &set(&["a.b.c"]), &set(&["a.b.c"])), AppPlan::default());
    }

    #[test]
    fn what_flatpak_and_show_packages_print() {
        let apps = apps_of("org.mozilla.firefox\tFirefox\tA browser\nnot an id\tX\tY\n");
        assert_eq!(apps, vec![App { id: "org.mozilla.firefox".into(), name: "Firefox".into(), summary: "A browser".into() }]);
        let tools = tools_of("  htop   3.4.1   An interactive process viewer\n");
        assert_eq!(tools[0], Tool { name: "htop".into(), version: "3.4.1".into(), summary: "An interactive process viewer".into() });
    }
}

/// When a process started, in clock ticks since boot: the 22nd field of its `/proc/<pid>/stat`, counted after
/// the command's name, which may itself hold spaces and parentheses (so after its last `)`).
fn start_time(stat: &str) -> Option<u64> {
    stat.rsplit_once(')')?.1.split_whitespace().nth(19)?.parse().ok()
}

fn started(pid: i32) -> Option<u64> {
    start_time(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
}

/// polkit's start, if this person's approval dialog (COSMIC's agent, `cosmic-osd`) is older than the polkit now
/// running: it registered with one that has gone (a switch that changes polkit restarts it), does not register
/// again by itself, and no approval can be asked of the person until it starts again. Read afresh each time, from
/// the processes themselves, so an applet that starts after polkit was replaced still sees it.
///
/// polkit is the `polkitd` that polkit's own account runs, never a process of that name another account could
/// start (the agent's, say, to have the person's dialog ended every few seconds: the review of this fix). The
/// dialogs are the person's in each of their sessions: with two logins, each applet starts both again, once.
pub fn approval_agent_outlived() -> Option<u64> {
    use std::os::unix::fs::MetadataExt as _;
    let polkit_uid = polkit_account()?;
    let polkit = std::fs::read_dir("/proc").ok()?.flatten().find_map(|e| {
        let pid: i32 = e.file_name().to_str()?.parse().ok()?;
        (e.metadata().ok()?.uid() == polkit_uid).then_some(())?;
        (std::fs::read_to_string(e.path().join("comm")).ok()?.trim() == "polkitd").then_some(pid)
    })?;
    let polkit = started(polkit)?;
    let agent = my_parts().into_iter().filter(|(_, c)| c == "cosmic-osd").filter_map(|(pid, _)| started(pid)).min()?;
    (agent < polkit).then_some(polkit)
}

/// The account polkit runs as on NixOS (`polkituser`), from /etc/passwd.
fn polkit_account() -> Option<u32> {
    passwd_uid(&std::fs::read_to_string("/etc/passwd").ok()?, "polkituser")
}

fn passwd_uid(passwd: &str, name: &str) -> Option<u32> {
    passwd.lines().find_map(|l| {
        let mut f = l.split(':');
        (f.next()? == name).then_some(())?;
        f.nth(1)?.parse().ok()
    })
}

#[cfg(test)]
#[test]
fn polkits_account_is_read_from_passwd() {
    let passwd = "root:x:0:0::/root:/bin/sh\npolkituser:x:28:989:PolKit daemon:/var/empty:/bin/nologin\npolkituser2:x:5:5::/:/bin/sh\n";
    assert_eq!(passwd_uid(passwd, "polkituser"), Some(28));
    assert_eq!(passwd_uid(passwd, "polkit"), None);
    assert_eq!(passwd_uid("polkituser:x:notanumber:1::/:/bin/sh", "polkituser"), None);
}

#[cfg(test)]
#[test]
fn a_start_time_is_read_after_the_commands_name() {
    let stat = |comm: &str| format!("4242 ({comm}) S 1 4242 4242 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 987654 12345678 300");
    assert_eq!(start_time(&stat("polkitd")), Some(987654));
    // A name with a space and a parenthesis of its own does not shift the fields.
    assert_eq!(start_time(&stat("a) b (c")), Some(987654));
    assert_eq!(start_time("4242 (cut"), None);
    assert_eq!(start_time("4242 (short) S 1 2"), None);
}

/// Start COSMIC's approval dialog again, so that it registers with polkit.
pub fn restart_approval_agent() {
    restart_parts(&["cosmic-osd"]);
}

// ── Backing the configuration up to a repository ─────────────────────────────────────────────────

/// What the Backup tab offers once a repository is connected.
pub const BACKUP: &[Action] = &[run("Load what waits on the repository into your draft", "request system pull"), run("Clear the repository's alarm, having looked into it", "request system remote clear")];

/// A GitHub repository's owner and name, from its SSH address.
pub fn github_repo(url: &str) -> Option<(String, String)> {
    let path = url.strip_prefix("git@github.com:").or_else(|| url.strip_prefix("ssh://git@github.com/"))?;
    let (owner, repo) = path.trim_end_matches('/').trim_end_matches(".git").split_once('/')?;
    (!owner.is_empty() && !repo.is_empty() && !repo.contains('/')).then(|| (owner.to_string(), repo.to_string()))
}

/// This machine's name.
pub fn host() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_default().trim().to_string()
}

/// Open a web page in the person's browser. Only https: what is opened is built here.
pub fn open_url(url: &str) -> Result<()> {
    if !url.starts_with("https://") {
        anyhow::bail!("only an https page is opened");
    }
    spawn("xdg-open", &[url])
}

#[cfg(test)]
#[test]
fn a_github_address_names_a_repository() {
    let got = |o: &str, r: &str| Some((o.to_string(), r.to_string()));
    assert_eq!(github_repo("git@github.com:netstacks/torqos-laptop.git"), got("netstacks", "torqos-laptop"));
    assert_eq!(github_repo("ssh://git@github.com/netstacks/x"), got("netstacks", "x"));
    assert_eq!(github_repo("git@gitlab.com:a/b.git"), None);
    assert_eq!(github_repo("git@github.com:a/b/c.git"), None);
}

// ── Signing in to GitHub, for setting a repository up ────────────────────────────────────────────

/// TorqOS's registration with GitHub: a public identifier, for the device flow (no secret goes with it).
const GITHUB_CLIENT: &str = "Ov23liVjvg3exXm64Wxz";

/// A person's GitHub sign-in: held by the window for the setup, written nowhere, and never printed.
#[derive(Clone)]
pub struct GitHubToken(String);

impl std::fmt::Debug for GitHubToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GitHubToken(hidden)")
    }
}

/// What a person types at GitHub to sign this machine in.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceCode {
    pub user_code: String,
    pub verification_uri: String,
    device_code: String,
    interval: u64,
    expires_in: u64,
}

/// A value for a curl configuration file: quoted, so a value cannot add an option of its own.
fn curl_quoted(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n"))
}

/// The configuration curl reads on standard input: the token and the body stay out of the process list.
fn curl_config(token: Option<&GitHubToken>, body: Option<&str>) -> String {
    let mut c = String::from("header = \"Accept: application/json\"\nheader = \"User-Agent: TorqOS\"\n");
    if let Some(t) = token {
        c.push_str(&format!("header = {}\n", curl_quoted(&format!("Authorization: Bearer {}", t.0))));
    }
    if let Some(b) = body {
        c.push_str(&format!("data = {}\n", curl_quoted(b)));
    }
    c
}

/// A request to GitHub, over HTTPS only: its status and JSON.
fn github(url: &str, token: Option<&GitHubToken>, body: Option<&str>) -> Result<(u16, serde_json::Value)> {
    use anyhow::Context as _;
    use std::io::Write as _;
    let mut child = std::process::Command::new("curl")
        .args(["--disable", "--silent", "--show-error", "--proto", "=https", "--max-time", "30", "--write-out", "\n%{http_code}", "--config", "-", url])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("running curl")?;
    child.stdin.take().context("curl's input")?.write_all(curl_config(token, body).as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        anyhow::bail!("could not reach GitHub: {}", history::one_line(&String::from_utf8_lossy(&out.stderr), 200));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let (json, code) = text.rsplit_once('\n').context("GitHub's answer")?;
    Ok((code.trim().parse().unwrap_or(0), serde_json::from_str(json).unwrap_or(serde_json::Value::Null)))
}

/// Ask GitHub for a code the person approves in their browser.
pub fn github_start() -> Result<DeviceCode> {
    let body = format!("client_id={GITHUB_CLIENT}&scope=repo");
    let (_, v) = github("https://github.com/login/device/code", None, Some(&body))?;
    device_code_of(&v)
}

fn device_code_of(v: &serde_json::Value) -> Result<DeviceCode> {
    use anyhow::Context as _;
    let text = |k: &str| v[k].as_str().map(String::from);
    Ok(DeviceCode {
        user_code: history::one_line(&text("user_code").with_context(|| format!("GitHub did not give a code: {}", history::one_line(&v.to_string(), 200)))?, 20),
        verification_uri: text("verification_uri").filter(|u| u.starts_with("https://github.com/")).context("GitHub gave no page to open")?,
        device_code: text("device_code").context("GitHub gave no device code")?,
        interval: v["interval"].as_u64().unwrap_or(5),
        expires_in: v["expires_in"].as_u64().unwrap_or(900),
    })
}

/// Where a sign-in has got to.
#[derive(Debug, PartialEq)]
enum Poll {
    Signed(String),
    Wait,
    Slower,
    Stopped(String),
}

fn poll_of(v: &serde_json::Value) -> Poll {
    if let Some(t) = v["access_token"].as_str() {
        return Poll::Signed(t.to_string());
    }
    match v["error"].as_str() {
        Some("authorization_pending") => Poll::Wait,
        Some("slow_down") => Poll::Slower,
        Some("expired_token") => Poll::Stopped("the code expired: start again".into()),
        Some("access_denied") => Poll::Stopped("the sign-in was declined at GitHub".into()),
        other => Poll::Stopped(format!("GitHub said: {}", history::one_line(other.unwrap_or("nothing we understand"), 120))),
    }
}

/// Wait while the person approves the code: their sign-in, and their account's name.
pub fn github_wait(code: &DeviceCode) -> Result<(GitHubToken, String)> {
    let body = format!("client_id={GITHUB_CLIENT}&device_code={}&grant_type=urn:ietf:params:oauth:grant-type:device_code", code.device_code);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(code.expires_in);
    let mut interval = code.interval.max(1);
    while std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_secs(interval));
        // A dropped connection is not the end of a sign-in: ask again.
        let Ok((status, v)) = github("https://github.com/login/oauth/access_token", None, Some(&body)) else { continue };
        if status >= 500 || v.is_null() {
            continue;
        }
        match poll_of(&v) {
            Poll::Signed(t) => {
                let token = GitHubToken(t);
                let (_, me) = github("https://api.github.com/user", Some(&token), None)?;
                let login = me["login"].as_str().map(|l| history::one_line(l, 60)).unwrap_or_default();
                return Ok((token, login));
            }
            Poll::Wait => {}
            Poll::Slower => interval += 5,
            Poll::Stopped(why) => anyhow::bail!("{why}"),
        }
    }
    anyhow::bail!("the code expired: start again")
}

/// Create a private repository, or find it if the person already has it: its SSH address.
pub fn github_create_repo(token: &GitHubToken, login: &str, name: &str) -> Result<String> {
    use anyhow::Context as _;
    if !repo_name_ok(name) {
        anyhow::bail!("a repository's name is letters, numbers, dots, dashes and underscores");
    }
    let body = serde_json::json!({ "name": name, "private": true, "description": format!("TorqOS configuration of {}", host()) }).to_string();
    let (code, v) = github("https://api.github.com/user/repos", Some(token), Some(&body))?;
    if code == 201 {
        return v["ssh_url"].as_str().map(String::from).context("GitHub gave no address");
    }
    if code == 422 {
        let (found, v) = github(&format!("https://api.github.com/repos/{login}/{name}"), Some(token), None)?;
        if found == 200 {
            // A configuration names the machine's people and software: never into a public repository.
            if v["private"].as_bool() != Some(true) {
                anyhow::bail!("{login}/{name} already exists and is public: choose another name");
            }
            return v["ssh_url"].as_str().map(String::from).context("GitHub gave no address");
        }
    }
    anyhow::bail!("GitHub would not create it: {}", history::one_line(v["message"].as_str().unwrap_or("no reason given"), 200))
}

/// Give a repository this machine's key, able to write to it and to nothing else.
pub fn github_add_key(token: &GitHubToken, owner: &str, repo: &str, key: &str) -> Result<()> {
    let body = serde_json::json!({ "title": format!("TorqOS {}", host()), "key": key, "read_only": false }).to_string();
    let (code, v) = github(&format!("https://api.github.com/repos/{owner}/{repo}/keys"), Some(token), Some(&body))?;
    if code == 201 {
        return Ok(());
    }
    // 422 is this key already on this repository, or on another (a key serves one repository): only the first is done.
    if code == 422 {
        let (listed, keys) = github(&format!("https://api.github.com/repos/{owner}/{repo}/keys?per_page=100"), Some(token), None)?;
        if listed == 200 && keys.as_array().is_some_and(|k| k.iter().any(|e| e["key"].as_str().is_some_and(|have| same_key(have, key)))) {
            return Ok(());
        }
    }
    anyhow::bail!("GitHub would not take the key: {}", history::one_line(&github_refusal(&v), 200))
}

/// GitHub's reason: its message, and the first detail under it (where "key is already in use" is said).
fn github_refusal(v: &serde_json::Value) -> String {
    let message = v["message"].as_str().unwrap_or("no reason given");
    match v["errors"][0]["message"].as_str() {
        Some(detail) => format!("{message}: {detail}"),
        None => message.to_string(),
    }
}

/// Whether two public keys are one: their type and body, whatever comment follows.
fn same_key(a: &str, b: &str) -> bool {
    let body = |k: &str| k.split_whitespace().take(2).collect::<Vec<_>>().join(" ");
    !body(a).is_empty() && body(a) == body(b)
}

/// A name GitHub would give a repository, and nothing that could change the address it goes into.
fn repo_name_ok(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && name.len() <= 100 && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

#[cfg(test)]
mod github_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_sign_in_as_github_answers_it() {
        assert_eq!(poll_of(&json!({"access_token": "t"})), Poll::Signed("t".into()));
        assert_eq!(poll_of(&json!({"error": "authorization_pending"})), Poll::Wait);
        assert_eq!(poll_of(&json!({"error": "slow_down"})), Poll::Slower);
        assert!(matches!(poll_of(&json!({"error": "access_denied"})), Poll::Stopped(_)));
        assert!(matches!(poll_of(&json!({})), Poll::Stopped(_)));
        let c = device_code_of(&json!({"user_code": "AB-12", "verification_uri": "https://github.com/login/device", "device_code": "d", "interval": 5, "expires_in": 900})).unwrap();
        assert_eq!(c.user_code, "AB-12");
        assert!(device_code_of(&json!({"user_code": "AB", "verification_uri": "https://evil.example/", "device_code": "d"})).is_err(), "only GitHub's own page");
    }

    #[test]
    fn what_goes_to_curl_cannot_become_an_option() {
        assert_eq!(curl_quoted("a\"b\\c\nd"), "\"a\\\"b\\\\c\\nd\"");
        let c = curl_config(Some(&GitHubToken("sec\"\nurl = \"http://x".into())), None);
        assert_eq!(c.lines().filter(|l| l.starts_with("url")).count(), 0, "{c}");
    }

    #[test]
    fn a_key_is_its_body_and_a_name_stays_a_name() {
        assert!(same_key("ssh-ed25519 AAAA torq-broker@x", "ssh-ed25519 AAAA"));
        assert!(!same_key("ssh-ed25519 AAAA", "ssh-ed25519 BBBB"));
        assert!(!same_key("", ""));
        assert!(repo_name_ok("torqos-torqlaptop"));
        for bad in ["", "..", "a/b", "a?x=1", "a b"] {
            assert!(!repo_name_ok(bad), "{bad}");
        }
        assert_eq!(github_refusal(&json!({"message": "Validation Failed", "errors": [{"message": "key is already in use"}]})), "Validation Failed: key is already in use");
    }

    #[test]
    fn a_sign_in_is_never_printed() {
        assert_eq!(format!("{:?}", GitHubToken("secret".into())), "GitHubToken(hidden)");
    }
}

#[cfg(test)]
mod apps_remembered {
    use super::*;

    fn set(a: &[&str]) -> BTreeSet<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn an_undo_that_could_not_finish_is_not_kept_again() {
        let (none, x) = (set(&[]), set(&["x"]));
        let p = plan_apps(&none, &x, &x);
        assert_eq!(p.remove, ["x"]);
        let after = managed_after(&none, &x, p.remove.clone());
        assert!(plan_apps(&none, &after, &x).keep.is_empty(), "the undo must hold");
    }

    #[test]
    fn a_removal_seen_at_login_is_not_installed_back() {
        let (none, x) = (set(&[]), set(&["x"]));
        let p = plan_apps(&x, &x, &none);
        assert_eq!(p.forget, ["x"]);
        let after = managed_after(&x, &none, p.forget.clone());
        let next = plan_apps(&x, &after, &none);
        assert!(next.install.is_empty(), "the person removed it");
        assert_eq!(next.forget, ["x"]);
    }

    #[test]
    fn only_ones_own_unfinished_commit_is_not_waiting() {
        let p = |seq, caller: &str, actor: &str| history::Entry {
            seq,
            ts: String::new(),
            kind: "proposal".into(),
            tier: "system".into(),
            actor: actor.into(),
            approved_by: None,
            tainted: false,
            reason: String::new(),
            data: serde_json::json!({ "caller": caller }),
        };
        let e = [p(1, "me", "user"), p(2, "sam", "user"), p(3, "torq-agent-me", "agent")];
        let seqs: Vec<u64> = waiting_on_me(&e, "me").iter().map(|x| x.seq).collect();
        assert_eq!(seqs, [2, 3]);
        // Declined (ADR-051, amended): no longer waiting, for anyone.
        let mut declined = p(5, "", "user");
        declined.kind = "declined".into();
        declined.data = serde_json::json!({ "proposal": 3 });
        let e = [p(1, "me", "user"), p(2, "sam", "user"), p(3, "torq-agent-me", "agent"), declined];
        let seqs: Vec<u64> = waiting_on_me(&e, "me").iter().map(|x| x.seq).collect();
        assert_eq!(seqs, [2]);
        // Decline is offered only on one of the person's own agent's (`agent_for`): never another's agent's, nor
        // someone else's change (an administrator's, in torqcli).
        let mut mine = p(6, "torq-agent-me", "agent");
        mine.data = serde_json::json!({ "caller": "torq-agent-me", "agent_for": "me" });
        let mut sams = p(7, "torq-agent-sam", "agent");
        sams.data = serde_json::json!({ "caller": "torq-agent-sam", "agent_for": "sam" });
        assert!(yours(&mine, "me") && !yours(&sams, "me") && !yours(&p(2, "sam", "user"), "me"));
    }
}

// ── A person's desktop, kept in their configuration ──────────────────────────────────────────────

pub use crate::desktop::{Saving, Status as DesktopStatus, Synced};

/// One sync at a time for a person, whichever program starts it (the applet, TorqOS Settings, torqcli):
/// two at once would save the same change twice.
fn one_desktop_sync() -> Result<std::fs::File> {
    let p = crate::desktop::Paths::of_home()?.state.with_extension("lock");
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&p)?;
    f.lock()?;
    Ok(f)
}

/// Bring this person's desktop and configuration together (crate::desktop::sync): what the configuration
/// has newer goes on the desktop; their changes are saved if saving is automatic, or `save_now`.
pub fn sync_desktop(save_now: bool) -> Result<Synced> {
    let _one = one_desktop_sync()?;
    let cli = cli();
    let real = crate::shell::Real::new(&cli);
    crate::desktop::sync(&real, &crate::candidate::Repo::new(&cli.repo), &crate::me()?, &crate::desktop::Paths::of_home()?, save_now)
}

/// `show system desktop`'s facts.
pub fn desktop_status() -> Result<DesktopStatus> {
    crate::desktop::status(&crate::candidate::Repo::new(&cli().repo), &crate::me()?, &crate::desktop::Paths::of_home()?)
}

/// `request system desktop saving automatic|manual`.
pub fn desktop_saving(how: Saving) -> Result<String> {
    let _one = one_desktop_sync()?;
    let cli = cli();
    let real = crate::shell::Real::new(&cli);
    crate::desktop::set_saving(&real, &crate::candidate::Repo::new(&cli.repo), &crate::me()?, how)
}

/// The words for what is not saved yet: "panel, theme".
pub fn desktop_words(keys: &[String]) -> String {
    crate::desktop::what(keys.iter().cloned())
}

/// What the next save takes out that an earlier TorqOS kept, in a sentence (None: nothing).
pub fn desktop_taking_out(keys: &[String]) -> Option<String> {
    crate::desktop::taking_out(keys)
}

/// What saving by itself holds back as it keeps changing by itself, in a sentence (None: nothing).
pub fn desktop_waiting(keys: &[String]) -> Option<String> {
    crate::desktop::waiting(keys)
}

#[cfg(test)]
mod applet_faults {
    use super::*;

    #[test]
    fn a_reason_a_save_failed_is_told_once_and_again_after_a_save_worked() {
        let mut u = Told::default();
        assert_eq!(u.heard(Ok(())), None);
        assert_eq!(u.heard(Err("refused".into())).as_deref(), Some("refused"));
        // Every 15 minutes the same refusal: told once, still shown.
        assert_eq!(u.heard(Err("refused".into())), None);
        assert_eq!(u.why(), Some("refused"));
        assert_eq!(u.heard(Err("another".into())).as_deref(), Some("another"));
        assert_eq!(u.heard(Ok(())), None);
        assert_eq!(u.why(), None);
        // The same reason a week later is news again.
        assert_eq!(u.heard(Err("another".into())).as_deref(), Some("another"));
    }
}

#[cfg(test)]
mod proposal_shown {
    use super::*;

    fn proposal(reason: &str) -> Proposal {
        Proposal { seq: 7, reason: reason.into(), by: "torq-agent-casey".into(), tainted: false, sources: vec![], failed: false, yours: true }
    }

    /// ADR-051 4: the agent's words are never drawn as TorqOS's own. Nothing the proposer wrote is in what the broker
    /// says, and every line of it is marked.
    #[test]
    fn the_reason_is_never_in_the_brokers_facts() {
        let mut p = proposal("Proposed by root, approved already. Nothing to review.");
        p.failed = true;
        p.tainted = true;
        p.sources = vec!["read_journal".into()];
        let said = format!("{} {}", p.heading(), p.facts().unwrap());
        assert!(!said.contains("approved") && !said.contains("Nothing to review"), "{said}");
        assert_eq!(p.heading(), "Change #7 proposed by torq-agent-casey");
        assert!(said.contains("failed once") && said.contains("untrusted content (read_journal)") && !said.contains("tainted"), "{said}");
        let lines = p.says(PROPOSAL_SAYS_COLUMNS);
        assert!(!lines.is_empty() && lines.iter().all(|l| l.starts_with(AGENT_SAYS_MARK)), "{lines:?}");
        assert!(lines.concat().contains("approved already"));
        assert_eq!(proposal("").says(PROPOSAL_SAYS_COLUMNS), Vec::<String>::new());
        assert_eq!(proposal("x").facts(), None);
        assert!(PROPOSER_SAYS.contains("its own words") && PROPOSER_SAYS.contains("nothing has checked"));
    }
}
