//! `torq agent run [--share <dir>] [--] [<command> …]` (ADR-041, 5): a program — Claude Code, or any
//! other — run as the person's own agent account (`torq-agent-<person>`, ADR-051 3), not as the person.
//!
//! It runs as the caller's agent (through `sudo torq-agent-run`, which binds the shared directory as root and
//! then drops to `torq-agent-$SUDO_USER` for good — never an agent the caller names), in
//! the launcher's sandbox (modules/agent-run.sh): every home hidden but the one directory shared with
//! it, the people's runtime directories hidden, /tmp its own. Its home is the agent's, so a tool's
//! settings and credentials are never the person's. What it changes in the system, it proposes —
//! torqcli as the agent stops at the proposal — and a person applies (`request system apply`).
//!
//! Sharing a directory is the person's act, and visible: an ACL on what of it is theirs (and default
//! ACLs, so that files either of them makes stay both of theirs). The agent gets no way into their
//! home: torqcli opens the directory, as the person, and the launcher binds that open descriptor —
//! everything around it stays out of the agent's reach, by name or otherwise.

/// The person's own agent, as every program of theirs reaches it (ADR-051 4): its client, what a
/// conversation shows, and what the agent is here.
pub mod client;
pub mod conversation;
pub mod crash;
pub mod handoff;
pub mod launcher;
pub mod setup;
/// What the agent says, as its service streams it (torq-wire's, for the programs built on this library).
pub use torq_wire::agent::Event;

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// What every agent account's name begins with: each person's is `torq-agent-<person>` (ADR-051 3).
const AGENT_PREFIX: &str = "torq-agent-";

/// The agent account of `person`: `torq-agent-<person>`.
pub(crate) fn agent_of(person: &str) -> String {
    format!("{AGENT_PREFIX}{person}")
}

/// What each person's resident agent's own account's name begins with: `torq-resident-<person>` (ADR-051 3,
/// amended 2026-11-11). It runs the person's agent service; to torqcli it is an agent as the agent account is.
const RESIDENT_PREFIX: &str = "torq-resident-";

/// Whether `name` is named as an agent account is (`torq-agent-<someone>`, or a resident agent's own
/// `torq-resident-<someone>`). Convenience only: torqcli
/// runs unprivileged, as whoever runs it, and what it decides here is how it behaves (a commit that
/// proposes and stops, no `torq agent run` from inside one) — never what is allowed. The broker is the
/// authority: it knows an agent by its map (/etc/torq/broker.json) and the kernel's uid, not by a name.
pub(crate) fn is_agent(name: &str) -> bool {
    [AGENT_PREFIX, RESIDENT_PREFIX].iter().any(|pre| name.strip_prefix(pre).is_some_and(|p| !p.is_empty()))
}

const SUDO: &str = "/run/wrappers/bin/sudo";
const LAUNCHER: &str = "/run/current-system/sw/bin/torq-agent-run";
const SETFACL: &str = "/run/current-system/sw/bin/setfacl";

/// What `torq agent run` was asked: the directory to share, and the program (none: a shell).
#[derive(Debug, PartialEq)]
pub(crate) struct Run {
    pub share: Option<PathBuf>,
    pub command: Vec<String>,
}

/// `run [--share <dir>] [--] [<command> …]`, after `agent`.
pub(crate) fn parse(args: &[String]) -> Result<Run> {
    let usage = "usage: torq agent run [--share <dir>] [--] [<command> …]";
    let mut it = args.iter();
    if it.next().map(String::as_str) != Some("run") {
        bail!("{usage}");
    }
    let rest: Vec<&String> = it.collect();
    let (share, rest) = match rest.as_slice() {
        [flag, dir, rest @ ..] if flag.as_str() == "--share" => (Some(PathBuf::from(dir.as_str())), rest),
        [flag] if flag.as_str() == "--share" => bail!("--share needs a directory\n{usage}"),
        rest => (None, rest),
    };
    let (dashes, rest) = match rest {
        [d, rest @ ..] if d.as_str() == "--" => (true, rest),
        rest => (false, rest),
    };
    if !dashes && rest.first().is_some_and(|a| a.starts_with('-')) {
        bail!("unknown option {}\n{usage}", rest[0]);
    }
    Ok(Run { share, command: rest.iter().map(|s| s.to_string()).collect() })
}

/// The directory to share, checked: it exists, it is a directory, it is the person's own, and it is
/// inside their home but not the home itself — named as it is, every link resolved.
pub(crate) fn checked_share(dir: &Path, home: &Path, uid: u32) -> Result<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let dir = std::fs::canonicalize(dir).with_context(|| format!("{}: no such directory", dir.display()))?;
    let home = std::fs::canonicalize(home).with_context(|| format!("{}: your home", home.display()))?;
    let m = std::fs::metadata(&dir)?;
    if !m.is_dir() {
        bail!("{}: not a directory", dir.display());
    }
    if dir == home || !dir.starts_with(&home) {
        bail!("{}: share a directory inside your home ({}), not the home itself or anything outside it", dir.display(), home.display());
    }
    if m.uid() != uid {
        bail!("{}: not yours to share", dir.display());
    }
    Ok(dir)
}


/// Everything under `dir` (itself included) that `uid` owns — its directories, and its other files —
/// walked without following a link (a link is not shared; what it points to is not walked).
pub(crate) fn owned(dir: &Path, uid: u32) -> Result<(Vec<PathBuf>, Vec<PathBuf>)> {
    use std::os::unix::fs::MetadataExt;
    let (mut dirs, mut files, mut todo) = (Vec::new(), Vec::new(), vec![dir.to_path_buf()]);
    while let Some(p) = todo.pop() {
        let m = std::fs::symlink_metadata(&p).with_context(|| format!("reading {}", p.display()))?;
        if m.file_type().is_symlink() || m.uid() != uid {
            continue;
        }
        if m.is_dir() {
            for e in std::fs::read_dir(&p).with_context(|| format!("reading {}", p.display()))? {
                todo.push(e?.path());
            }
            dirs.push(p);
        } else if m.is_file() {
            files.push(p);
        }
    }
    Ok((dirs, files))
}


/// Whether this machine has agent account `agent`, from /etc/passwd: refused, plainly, if not (the
/// person is not in `torq.agent.people`). The launcher checks it again, as root, against the broker's map.
fn has_agent(agent: &str) -> Result<()> {
    let passwd = std::fs::read_to_string("/etc/passwd").context("reading /etc/passwd")?;
    if !passwd.lines().any(|l| l.split(':').next() == Some(agent)) {
        bail!("you have no agent account on this machine ({agent}): `torq agent people` names who has one (the owner, by default) — adding you is a system change, an administrator's");
    }
    Ok(())
}

/// The person's agent held still (ADR-051 3, step 2): the launcher, as root, freezes their agent's
/// service (`systemctl freeze torq-agent@<person>.service`, systemd's freezer: every process of it
/// stopped where it is), refuses if any other process of the agent's runs (another `torq agent run`),
/// says `frozen`, and thaws when its input ends — when this closes it, or exits, however. A program
/// of the agent's running during the walk could swap a file it can write for a link, between the
/// walk's look and its ACL, and be granted wherever the link leads (the review of ADR-041, 5).
struct Frozen(std::process::Child);

impl Frozen {
    fn hold(launcher: &str) -> Result<Frozen> {
        use std::io::BufRead;
        use std::process::Stdio;
        let mut child = Command::new(SUDO).args(["--", launcher, "--hold-frozen"]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().context("holding your agent still (sudo torq-agent-run)")?;
        let mut said = String::new();
        let read = std::io::BufReader::new(child.stdout.take().context("the launcher's output")?).read_line(&mut said);
        if read.is_err() || said.trim() != "frozen" {
            drop(child.stdin.take());
            let _ = child.wait();
            bail!("nothing shared: your agent could not be held still while the directory is shared (the launcher said why)");
        }
        Ok(Frozen(child))
    }

    /// Thawed: the launcher's input ended, and it says whether the thaw went through.
    fn thaw(mut self) -> Result<()> {
        drop(self.0.stdin.take());
        if !self.0.wait().context("thawing your agent")?.success() {
            bail!("your agent could not be thawed: systemctl thaw it (the launcher said why)");
        }
        Ok(())
    }
}

/// setfacl with `args` on each of `paths` at once.
fn acl_many(args: &[&str], paths: &[PathBuf]) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    if !Command::new(SETFACL).args(args).arg("--").args(paths).status().context("running setfacl")?.success() {
        bail!("could not share {} (setfacl)", paths[0].display());
    }
    Ok(())
}

/// Share `dir` with `agent`: the agent may read and write what of it is the person's (`uid`, `me`);
/// what it makes there, the person may rename and delete, and (default ACLs) read and write unless
/// the agent narrows it — the agent owns what it makes. What the agent made is not the person's to set
/// ACLs on. No link is followed.
fn share_acls(dir: &Path, uid: u32, agent: &str, me: &str) -> Result<()> {
    let (dirs, files) = owned(dir, uid)?;
    for chunk in files.chunks(200) {
        acl_many(&["-m", &format!("u:{agent}:rwX")], chunk)?;
    }
    for chunk in dirs.chunks(200) {
        acl_many(&["-m", &format!("u:{agent}:rwx")], chunk)?;
        acl_many(&["-d", "-m", &format!("u:{agent}:rwX,u:{me}:rwX")], chunk)?;
    }
    Ok(())
}

/// Run it: share the directory (ACLs, and the directory itself, open), then become the agent,
/// through the launcher. Does not return on success: the program takes this process's place.
pub(crate) fn run(args: &[String]) -> Result<i32> {
    use std::os::fd::AsRawFd;
    let r = parse(args)?;
    let me = crate::me()?;
    if is_agent(&me) {
        bail!("this is an agent already");
    }
    // Your own agent, and only yours: the launcher runs `torq-agent-<you>` (as sudo says who you are).
    let agent = agent_of(&me);
    has_agent(&agent)?;
    // The launcher by its own path in the store, as the sudo rule names it (sudo does not take a link
    // to it for it).
    let launcher = std::fs::canonicalize(LAUNCHER).with_context(|| format!("{LAUNCHER}: the agent's launcher is not installed"))?;
    let launcher = launcher.to_string_lossy().into_owned();
    let mut sudo: Vec<String> = Vec::new();
    let mut launch = vec![launcher];
    // Held open until the program has taken this process's place: the launcher binds it.
    let mut _shared: Option<std::fs::File> = None;
    if let Some(share) = &r.share {
        let home = PathBuf::from(std::env::var_os("HOME").context("HOME is not set: no home to share from")?);
        let uid = rustix::process::getuid().as_raw();
        let dir = checked_share(share, &home, uid)?;
        // One session of the agent's at a time, and none of its programs running while the directory is
        // walked (a link swapped in between the walk's look and its ACL would be granted wherever it
        // leads): the person's agent service held still by systemd's freezer, any other program of the
        // agent's refused — both by the launcher, as root — and thawed after, however the walk ended.
        // With its own process namespace (the launcher), nothing of a session outlives it.
        let frozen = Frozen::hold(&launch[0])?;
        let shared = share_acls(&dir, uid, &agent, &me);
        // Thawed whatever the walk did, and both said when both fail (the review of the rebased branch, C13: the
        // walk's own error was lost behind the thaw's).
        let thawed = frozen.thaw();
        match (shared, thawed) {
            (Err(e), Err(t)) => return Err(e.context(format!("and your agent could not be thawed after: {t:#}"))),
            (Err(e), Ok(())) | (Ok(()), Err(e)) => return Err(e),
            (Ok(()), Ok(())) => {}
        }
        // Opened as the person, kept open across exec (no close-on-exec), and kept by sudo (`-C`,
        // which the launcher's rule allows): the agent is handed the directory, not a way to it.
        let f = std::fs::File::open(&dir).with_context(|| format!("opening {}", dir.display()))?;
        rustix::io::fcntl_setfd(&f, rustix::io::FdFlags::empty()).context("keeping the shared directory open")?;
        let fd = f.as_raw_fd();
        sudo.extend(["-C".into(), (fd + 1).to_string()]);
        launch.extend(["--share-fd".into(), fd.to_string(), dir.to_string_lossy().into_owned()]);
        _shared = Some(f);
        eprintln!("Shared with your agent ({agent}): {} (undo: setfacl -R -P -x u:{agent},d:u:{agent} -- {})", dir.display(), dir.display());
    }
    launch.push("--".into());
    launch.extend(r.command);
    use std::os::unix::process::CommandExt;
    let e = Command::new(SUDO).args(&sudo).arg("--").args(&launch).exec();
    Err(anyhow::Error::from(e).context("starting the agent (sudo torq-agent-run)"))
}

/// What `torq ask` was asked: a question for the person's agent, or to forget the conversation.
#[derive(Debug, PartialEq)]
pub(crate) enum Asked {
    Question(String),
    Forget,
    /// No words: the question is one line read from standard input, so it needs no quoting.
    FromInput,
}

const ASK_USAGE: &str = "usage: torq ask \"<question>\" | torq ask (then type the question) | torq ask --forget — e.g. torq ask \"why is my wifi flaky?\"";

/// `ask "<question>"`, `ask -- <words…>`, `ask --forget`, or `ask` alone (the question from standard input).
pub(crate) fn parse_ask(args: &[String]) -> Result<Asked> {
    let words = match args {
        [] => return Ok(Asked::FromInput),
        [f] if f == "--forget" => return Ok(Asked::Forget),
        [d, rest @ ..] if d == "--" => rest,
        [first, ..] if first.starts_with('-') => bail!("unknown option {first}\n{ASK_USAGE}"),
        rest => rest,
    };
    let q = words.join(" ");
    if q.trim().is_empty() {
        bail!("{ASK_USAGE}");
    }
    Ok(Asked::Question(q))
}

/// One line read as the question (`ask` alone): whatever it holds, no shell reads it. At most 8 KiB: a longer
/// one is refused, never cut unsaid (C13).
fn question_from<R: std::io::BufRead>(r: &mut R) -> Result<String> {
    use std::io::Read;
    const MAX: u64 = 8 * 1024;
    let mut line = String::new();
    std::io::BufRead::read_line(&mut r.take(MAX), &mut line).context("reading your question")?;
    if line.len() as u64 >= MAX && !line.ends_with('\n') {
        bail!("your question is longer than {MAX} bytes: ask it shorter");
    }
    let q = line.trim();
    if q.is_empty() {
        bail!("{ASK_USAGE}");
    }
    Ok(q.to_string())
}

/// What a key does while an answer comes.
#[derive(Debug, PartialEq)]
enum KeyDoes {
    Nothing,
    /// The first Ctrl-C: ask the agent to stop.
    Stop,
    /// A second: leave, whatever the agent does.
    Leave,
}

/// How long after a Ctrl-C the agent has to end its task before `torq ask` leaves without it.
const GIVE_UP: std::time::Duration = std::time::Duration::from_secs(5);

/// Ctrl-C asks the agent to stop; a second Ctrl-C, or no end within `GIVE_UP` of the first, gets the
/// person out (a hung or frozen service never holds the terminal).
#[derive(Default)]
struct Interrupt {
    stopped_at: Option<std::time::Instant>,
}

impl Interrupt {
    fn key(&mut self, b: u8, now: std::time::Instant) -> KeyDoes {
        match (b, self.stopped_at) {
            (0x03, None) => {
                self.stopped_at = Some(now);
                KeyDoes::Stop
            }
            (0x03, Some(_)) => KeyDoes::Leave,
            _ => KeyDoes::Nothing,
        }
    }

    fn given_up(&self, now: std::time::Instant) -> bool {
        self.stopped_at.is_some_and(|t| now.duration_since(t) >= GIVE_UP)
    }
}

/// Ctrl-C, while an answer comes: the terminal's keys read as keys (no signal, no echo), the first
/// Ctrl-C a `stop` sent, a second — or no end within `GIVE_UP` — the terminal as it was and exit 130;
/// the terminal as it was, too, when this ends, and when a signal would end it (`Signals`).
struct Keys {
    was: rustix::termios::Termios,
    /// Whether the person stopped the task (Ctrl-C): `torq ask` then ends 130, as when they leave.
    stopped: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// The signals that would end `torq ask` with the terminal still in its key mode (the review of the rebased
/// branch, B8): blocked before any thread starts, so that every later thread has them blocked too, and read from a
/// signalfd instead. One that comes puts the terminal back and ends as its signal would (128 + its number).
struct Signals(nix::sys::signalfd::SignalFd);

impl Signals {
    fn hold() -> Option<Signals> {
        use nix::sys::signal::{SigSet, Signal};
        let mut set = SigSet::empty();
        for s in [Signal::SIGTERM, Signal::SIGHUP, Signal::SIGINT, Signal::SIGQUIT] {
            set.add(s);
        }
        set.thread_block().ok()?;
        match nix::sys::signalfd::SignalFd::new(&set) {
            Ok(fd) => Some(Signals(fd)),
            // Held and nothing to read them from: let go again, so they end it as they would.
            Err(_) => {
                let _ = set.thread_unblock();
                None
            }
        }
    }

    /// The next of them (blocking), its number.
    fn next(&self) -> Option<i32> {
        self.0.read_signal().ok().flatten().map(|i| i.ssi_signo as i32)
    }
}

/// Held and then not watched (the terminal could not be read): let go again, so the signals end it as they would.
impl Drop for Signals {
    fn drop(&mut self) {
        use nix::sys::signal::{SigSet, Signal};
        let mut set = SigSet::empty();
        for s in [Signal::SIGTERM, Signal::SIGHUP, Signal::SIGINT, Signal::SIGQUIT] {
            set.add(s);
        }
        // Nothing to do if it fails: the signals stay held, read by no one, as they were.
        let _ = set.thread_unblock();
    }
}

impl Keys {
    fn watch(agent: client::Client, signals: Option<Signals>) -> Option<Keys> {
        use rustix::termios::{tcgetattr, tcsetattr, LocalModes, OptionalActions, SpecialCodeIndex};
        let stdin = std::io::stdin();
        let was = tcgetattr(&stdin).ok()?;
        let mut keys = was.clone();
        keys.local_modes.remove(LocalModes::ICANON | LocalModes::ECHO | LocalModes::ISIG);
        keys.special_codes[SpecialCodeIndex::VMIN] = 1;
        keys.special_codes[SpecialCodeIndex::VTIME] = 0;
        tcsetattr(&stdin, OptionalActions::Now, &keys).ok()?;
        if let Some(signals) = signals {
            let restore = was.clone();
            std::thread::spawn(move || {
                if let Some(n) = signals.next() {
                    // Putting it back may fail (the terminal gone, as at a hangup): then there is nothing to put back.
                    let _ = rustix::termios::tcsetattr(std::io::stdin(), rustix::termios::OptionalActions::Now, &restore);
                    // Said if it can be: at a hangup the terminal is gone, and eprintln! would panic this thread and
                    // leave torq ask running, the signal taken. Whatever the write does, it ends.
                    let _ = std::io::Write::write_fmt(&mut std::io::stderr(), format_args!("agent: ended by signal {n}, without its answer (the agent stops the task as the connection ends)\n"));
                    std::process::exit(128 + n);
                }
            });
        }
        let restore = was.clone();
        let stopped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let said_stop = stopped.clone();
        std::thread::spawn(move || {
            use rustix::event::{poll, PollFd, PollFlags, Timespec};
            let (mut i, mut open) = (Interrupt::default(), true);
            loop {
                let stdin = std::io::stdin();
                let mut fds = [PollFd::new(&stdin, PollFlags::IN)];
                let ready = open && poll(&mut fds, Some(&Timespec { tv_sec: 0, tv_nsec: 200_000_000 })).is_ok_and(|n| n > 0);
                if !open {
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
                let mut does = KeyDoes::Nothing;
                if ready {
                    let mut b = [0u8; 1];
                    match rustix::io::read(&stdin, &mut b) {
                        Ok(1) => does = i.key(b[0], std::time::Instant::now()),
                        _ => open = false,
                    }
                }
                if does == KeyDoes::Stop {
                    eprintln!("agent: stopping (Ctrl-C again to leave now)");
                    said_stop.store(true, std::sync::atomic::Ordering::Relaxed);
                    // On a thread of its own: the service answers a stop once the task is over, and a second
                    // Ctrl-C must be read meanwhile. A stop not taken is said (B8).
                    let agent = agent.clone();
                    std::thread::spawn(move || {
                        if let Err(e) = agent.stop() {
                            eprintln!("agent: the stop was not taken: {e:#}");
                        }
                    });
                }
                if does == KeyDoes::Leave || i.given_up(std::time::Instant::now()) {
                    let _ = rustix::termios::tcsetattr(&stdin, rustix::termios::OptionalActions::Now, &restore);
                    eprintln!("agent: left without its answer (it may still be stopping)");
                    std::process::exit(130);
                }
            }
        });
        Some(Keys { was, stopped })
    }
}

impl Drop for Keys {
    fn drop(&mut self) {
        let _ = rustix::termios::tcsetattr(std::io::stdin(), rustix::termios::OptionalActions::Now, &self.was);
    }
}

/// How the person reads an event: the answer and the proposals on standard output, the steps on the
/// way on standard error, every word of the agent's cleaned of what could fool a terminal. `Some`
/// (the exit code) when it is the last — an error is the last, too, when `error_ends` (for `forget`).
fn show(e: torq_wire::agent::Event, error_ends: bool, failed: &mut bool, out: &mut dyn std::io::Write, err: &mut dyn std::io::Write) -> std::io::Result<Option<i32>> {
    use crate::history::clean;
    use torq_wire::agent::Event;
    match e {
        Event::Say { text } => {
            for line in text.lines() {
                writeln!(out, "{}", clean(line))?;
            }
        }
        Event::Called { tool } => writeln!(err, "agent: {}", clean(&tool))?,
        Event::Tainted { why } => writeln!(err, "agent: this session read untrusted text ({}): anything it proposes is marked tainted", clean(&why))?,
        Event::Proposed { seq } => writeln!(out, "Proposed #{seq}: a person applies it — `show system proposals`, `request system apply {seq}`")?,
        Event::Error { text } => {
            *failed = true;
            writeln!(err, "agent: {}", clean(&text))?;
            if error_ends {
                return Ok(Some(1));
            }
        }
        Event::Done { exit } => return Ok(Some(if exit == 0 && !*failed { 0 } else { 1 })),
        Event::Forgotten => {
            writeln!(out, "Your agent has forgotten the conversation.")?;
            return Ok(Some(0));
        }
        Event::Status { .. } => {}
    }
    Ok(None)
}

/// `torq ask "<question>"` (ADR-051 4, the terminal's way to the agent): the person's own agent —
/// its service, `torq-agent@<you>.service`, on its socket — asked through the client every UI shares
/// (`client::Client`), the answer streamed as it comes, with the number of anything it proposed; Ctrl-C
/// stops it. `--forget`: the conversation it keeps is erased.
pub(crate) fn ask(args: &[String]) -> Result<i32> {
    use torq_wire::agent::Event;
    let asked = parse_ask(args)?;
    let agent = client::Client::mine()?;
    let text = match asked {
        Asked::Forget => {
            return match agent.forget() {
                Ok(()) => show(Event::Forgotten, true, &mut false, &mut std::io::stdout(), &mut std::io::stderr()).map(|c| c.unwrap_or(0)).map_err(Into::into),
                Err(client::Unreachable::Refused(text)) => show(Event::Error { text }, true, &mut false, &mut std::io::stdout(), &mut std::io::stderr()).map(|c| c.unwrap_or(1)).map_err(Into::into),
                Err(e) => Err(e.into()),
            };
        }
        Asked::Question(text) => text,
        Asked::FromInput => {
            use std::io::IsTerminal;
            if std::io::stdin().is_terminal() {
                eprint!("Ask your agent: ");
            }
            question_from(&mut std::io::stdin().lock())?
        }
    };
    // Held before any thread starts (a thread started earlier would take them, and end with the terminal raw), and
    // only now: at the question's prompt, Ctrl-C ends torq ask as it always does. Until the key thread reads them (just
    // below) they are held unread for as long as agent.ask takes to return its stream: bounded, its connect and its
    // request each within the client's QUICK (15 s).
    use std::io::IsTerminal;
    let signals = std::io::stdin().is_terminal().then(Signals::hold).flatten();
    let answer = agent.ask(&text, None)?;
    let keys = if std::io::stdin().is_terminal() { Keys::watch(agent.clone(), signals) } else { None };
    // Stopped by the person (Ctrl-C): 130, as when they leave, whatever the task's own end (B8).
    let stopped = || keys.as_ref().is_some_and(|k| k.stopped.load(std::sync::atomic::Ordering::Relaxed));
    let mut failed = false;
    for e in answer {
        // An error, then the end: said already, so only the code (as the service closed without its `done`).
        let e = match e {
            Err(_) if failed => return Ok(if stopped() { 130 } else { 1 }),
            e => e?,
        };
        if let Some(code) = show(e, false, &mut failed, &mut std::io::stdout(), &mut std::io::stderr())? {
            return Ok(if stopped() { 130 } else { code });
        }
    }
    bail!("your agent's service ended before it answered (journalctl -u {}, an administrator's)", agent.unit())
}

/// `torq doctor "<question>"` (ADR-042): the agent, in doctor mode, asked one question — run as the
/// agent's account through `torq agent run` (no directory shared), so it reads only what the agent
/// may, and proposes nothing. As the agent already (a tool run as it), the doctor itself.
///
/// `torq agent propose "<ask>"` (`propose`; ADR-042, 6): the same, with one more tool — it may
/// propose a change within what the agent may (torq.agent.mayPropose), never apply one: a person
/// applies it with `request system apply <n>`.
pub(crate) fn doctor_mode(args: &[String], propose: bool) -> Result<i32> {
    const DOCTOR: &str = "/run/current-system/sw/bin/torq-doctor";
    let question = args.join(" ");
    if question.trim().is_empty() {
        bail!(if propose {
            "usage: torq agent propose \"<what you want changed>\" — e.g. torq agent propose \"install htop for me\""
        } else {
            "usage: torq doctor \"<question>\" — e.g. torq doctor \"why is my wifi flaky?\""
        });
    }
    if !Path::new(DOCTOR).exists() {
        bail!("doctor mode is off on this machine: configure; set torq agent doctor enable true; commit (ADR-042)");
    }
    let mut doctor: Vec<String> = vec![DOCTOR.into()];
    if propose {
        doctor.push("--propose".into());
    }
    doctor.extend(["--".into(), question]);
    if is_agent(&crate::me()?) {
        use std::os::unix::process::CommandExt;
        let e = Command::new(&doctor[0]).args(&doctor[1..]).exec();
        return Err(anyhow::Error::from(e).context("starting the doctor"));
    }
    run(&[vec!["run".to_string(), "--".to_string()], doctor].concat())
}

/// `torq break-glass "<reason>"` (ADR-035): a root shell, after the record. The launcher, through
/// sudo (the person's password), writes the ledger entry, pushes it, and only then opens the shell.
pub(crate) fn break_glass(args: &[String]) -> Result<i32> {
    const LAUNCHER: &str = "/run/current-system/sw/bin/torq-break-glass";
    let reason = args.join(" ");
    if reason.trim().is_empty() {
        bail!("usage: torq break-glass \"<why>\" — the reason goes in the ledger, before the shell opens");
    }
    if is_agent(&crate::me()?) {
        bail!("the agent has no root shell: it proposes, and a person applies");
    }
    let launcher = std::fs::canonicalize(LAUNCHER).with_context(|| format!("{LAUNCHER}: not on this machine"))?;
    use std::os::unix::process::CommandExt;
    let e = Command::new(SUDO).arg("--").arg(launcher).arg(&reason).exec();
    Err(anyhow::Error::from(e).context("starting break-glass (sudo)"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    /// ADR-051 3: each person's agent is `torq-agent-<person>`; the old single account, a bare prefix
    /// and a person are not named as one (torqcli's convenience; the broker's map is the authority).
    #[test]
    fn whose_agent_by_name() {
        assert_eq!(agent_of("alice"), "torq-agent-alice");
        assert!(is_agent("torq-agent-alice") && is_agent(&agent_of("bob")) && is_agent("torq-resident-alice"));
        for not in ["torq-agent", "torq-agent-", "torq-resident-", "torq-resident", "alice", "xtorq-agent-alice"] {
            assert!(!is_agent(not), "{not}");
        }
    }

    #[test]
    fn what_it_is_asked() {
        assert_eq!(parse(&a("run")).unwrap(), Run { share: None, command: vec![] });
        assert_eq!(parse(&a("run claude")).unwrap(), Run { share: None, command: a("claude") });
        assert_eq!(parse(&a("run --share /home/v/p -- claude --resume")).unwrap(), Run { share: Some("/home/v/p".into()), command: a("claude --resume") });
        assert_eq!(parse(&a("run -- -x")).unwrap().command, a("-x"), "after --, anything");
        assert!(parse(&a("run --share")).is_err());
        assert!(parse(&a("run --shar /x")).is_err(), "no other option");
        assert!(parse(&a("start")).is_err());
    }

    #[test]
    fn the_doctor_needs_a_question() {
        for q in [vec![], vec!["  ".to_string()]] {
            assert!(format!("{:#}", doctor_mode(&q, false).unwrap_err()).starts_with("usage: torq doctor"));
            assert!(format!("{:#}", doctor_mode(&q, true).unwrap_err()).starts_with("usage: torq agent propose"));
        }
    }

    #[test]
    fn only_a_directory_of_ones_own_inside_ones_home() {
        use std::os::unix::fs::MetadataExt;
        let d = tempfile::tempdir().unwrap();
        let home = d.path().join("home");
        let project = home.join("src/project");
        std::fs::create_dir_all(&project).unwrap();
        let uid = std::fs::metadata(&home).unwrap().uid();
        assert_eq!(checked_share(&project, &home, uid).unwrap(), std::fs::canonicalize(&project).unwrap());
        assert!(checked_share(&home, &home, uid).is_err(), "not the home itself");
        assert!(checked_share(d.path(), &home, uid).is_err(), "not outside it");
        assert!(checked_share(&project, &home, uid + 1).is_err(), "not someone else's");
        std::fs::write(home.join("file"), "").unwrap();
        assert!(checked_share(&home.join("file"), &home, uid).is_err(), "not a file");
        // A link out of the home is where it leads, and refused there.
        std::os::unix::fs::symlink(d.path(), home.join("out")).unwrap();
        assert!(checked_share(&home.join("out"), &home, uid).is_err());
    }

    #[test]
    fn the_walk_takes_only_ones_own_and_follows_no_link() {
        use std::os::unix::fs::MetadataExt;
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("p");
        std::fs::create_dir_all(p.join("sub")).unwrap();
        std::fs::write(p.join("sub/f"), "").unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "").unwrap();
        std::os::unix::fs::symlink(outside.path(), p.join("link")).unwrap();
        let uid = std::fs::metadata(&p).unwrap().uid();
        let (dirs, files) = owned(&p, uid).unwrap();
        assert_eq!(dirs.len(), 2, "{dirs:?}");
        assert_eq!(files, [p.join("sub/f")]);
        assert_eq!(owned(&p, uid + 1).unwrap(), (vec![], vec![]), "someone else's: nothing");
    }


    #[test]
    fn what_torq_ask_is_asked() {
        assert_eq!(parse_ask(&a("why is it slow?")).unwrap(), Asked::Question("why is it slow?".into()));
        assert_eq!(parse_ask(&a("--forget")).unwrap(), Asked::Forget);
        assert_eq!(parse_ask(&a("-- --forget me")).unwrap(), Asked::Question("--forget me".into()), "after --, words");
        assert_eq!(parse_ask(&a("forget --forget")).unwrap(), Asked::Question("forget --forget".into()));
        assert_eq!(parse_ask(&[]).unwrap(), Asked::FromInput, "no words: the question from standard input");
        let mut typed = std::io::Cursor::new("why is my wifi slow? it's $HOME * \"odd\"\nnext line\n");
        assert_eq!(question_from(&mut typed).unwrap(), "why is my wifi slow? it's $HOME * \"odd\"", "one line, as typed");
        assert!(question_from(&mut std::io::Cursor::new("\n")).is_err(), "nothing typed");
        for bad in ["--", "--stop", "-x why"] {
            assert!(parse_ask(&a(bad)).is_err(), "{bad:?}");
        }
        assert!(format!("{:#}", parse_ask(&a("--stop")).unwrap_err()).contains("unknown option --stop"));
    }

    /// The answer and the proposals on standard output, the steps on standard error, nothing of the
    /// agent's able to fool a terminal; the end, and its code, at `done`.
    #[test]
    fn how_the_answer_is_shown() {
        use torq_wire::agent::Event;
        let (mut out, mut err, mut failed) = (Vec::new(), Vec::new(), false);
        let mut s = |e| show(e, false, &mut failed, &mut out, &mut err).unwrap();
        assert_eq!(s(Event::Called { tool: "read_journal".into() }), None);
        assert_eq!(s(Event::Tainted { why: "read_journal".into() }), None);
        assert_eq!(s(Event::Say { text: "line one\nrewrite\rd \u{1b}[2J\u{202e}".into() }), None);
        assert_eq!(s(Event::Proposed { seq: 12 }), None);
        assert_eq!(s(Event::Done { exit: 0 }), Some(0));
        let (out, err) = (String::from_utf8(out).unwrap(), String::from_utf8(err).unwrap());
        assert!(out.starts_with("line one\n") && out.contains("Proposed #12") && out.contains("request system apply 12"), "{out}");
        assert!(!out.contains('\u{1b}') && !out.contains('\r') && !out.contains('\u{202e}'), "{out:?}");
        assert!(err.contains("agent: read_journal") && err.contains("untrusted text (read_journal)"), "{err}");
        // An error, then done: failed, whatever the code.
        let (mut failed, mut sink) = (false, Vec::new());
        assert_eq!(show(Event::Error { text: "a task is running already".into() }, false, &mut failed, &mut Vec::new(), &mut sink).unwrap(), None);
        assert_eq!(show(Event::Done { exit: 0 }, false, &mut failed, &mut Vec::new(), &mut Vec::new()).unwrap(), Some(1));
        assert_eq!(show(Event::Done { exit: 137 }, false, &mut false, &mut Vec::new(), &mut Vec::new()).unwrap(), Some(1), "stopped");
    }

    /// `forget` ends at its error: refused while a task runs, `torq ask --forget` never waits.
    #[test]
    fn forget_ends_at_an_error() {
        use torq_wire::agent::Event;
        let refused = || Event::Error { text: "a task is running: stop it first, or wait".into() };
        assert_eq!(show(refused(), true, &mut false, &mut Vec::new(), &mut Vec::new()).unwrap(), Some(1));
        assert_eq!(show(refused(), false, &mut false, &mut Vec::new(), &mut Vec::new()).unwrap(), None, "an ask goes on to its done");
        assert_eq!(show(Event::Forgotten, true, &mut false, &mut Vec::new(), &mut Vec::new()).unwrap(), Some(0));
    }

    /// Ctrl-C asks the agent to stop; a second, or no end within GIVE_UP of the first, leaves.
    #[test]
    fn ctrl_c_stops_then_leaves() {
        let t = std::time::Instant::now();
        let mut i = Interrupt::default();
        assert!(!i.given_up(t + GIVE_UP * 10), "nothing asked yet: never given up");
        assert_eq!(i.key(b'x', t), KeyDoes::Nothing);
        assert_eq!(i.key(0x03, t), KeyDoes::Stop);
        assert!(!i.given_up(t + GIVE_UP / 2));
        assert!(i.given_up(t + GIVE_UP), "no end in time: leave");
        assert_eq!(i.key(b'y', t), KeyDoes::Nothing);
        assert_eq!(i.key(0x03, t), KeyDoes::Leave, "the second Ctrl-C");
    }

    /// The signals that would end `torq ask` with the terminal raw are held and read instead (the review of the
    /// rebased branch, B8): a SIGTERM sent to this thread is read from the signalfd as its number, and the thread is
    /// let go of them after (Drop). Sent to this thread alone, so the test's other threads are untouched.
    #[test]
    fn a_signal_is_read_not_taken() {
        use nix::sys::signal::{raise, SigSet, Signal};
        let s = Signals::hold().expect("held");
        raise(Signal::SIGTERM).unwrap();
        assert_eq!(s.next(), Some(15));
        drop(s);
        assert!(!SigSet::thread_get_mask().unwrap().contains(Signal::SIGTERM), "let go again");
    }

    /// A question read from input past 8 KiB is refused, not cut unsaid; one within is taken whole (C13). And the
    /// client names the person's own unit, not a placeholder.
    #[test]
    fn a_long_question_is_refused_and_the_unit_named() {
        let long = format!("{}\n", "x".repeat(9 * 1024));
        assert!(format!("{:#}", question_from(&mut long.as_bytes()).unwrap_err()).contains("longer than 8192 bytes"));
        let fits = format!("{}\n", "y".repeat(8 * 1024 - 1));
        assert_eq!(question_from(&mut fits.as_bytes()).unwrap().len(), 8 * 1024 - 1);
        let c = client::Client::at(std::path::Path::new("/run/torq-agent/alice.sock"), 1000);
        assert_eq!(c.unit(), "torq-agent@alice.service");
    }
}
