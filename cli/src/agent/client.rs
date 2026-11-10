//! The person's own agent, as every program of theirs reaches it (ADR-051 4): `torq ask`, the panel's
//! applet, the agent's window and TorqOS Settings speak to their service (`torq-agent@<you>.service`, on
//! `/run/torq-agent/<you>.sock`) through this one piece. It connects to the caller's own socket only —
//! named by the caller's uid, and refused unless that socket is theirs — and never waits for ever: a
//! service absent, frozen (a share being set up holds it still) or silent is an error in bounded time,
//! so no window or panel thread hangs on it. It holds no secret and runs nothing: what it sends is
//! torq-wire's `agent` requests; what it hands back is the service's events, as they come.

use super::{agent_of, is_agent};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use torq_wire::agent::{socket, Event, Request, MAX_LINE};

/// How long a connection, a request and a short answer (status, stop, forget) may take. Stop waits up to
/// 5 s for a task to end before it answers; the first request may also start the service.
pub const QUICK: Duration = Duration::from_secs(15);
/// How long an answer may be silent between two of its events: the model is asked for at most 600 s a
/// round (agent/src/model.rs), so a little more than that, and the service is then not answering.
pub const QUIET: Duration = Duration::from_secs(660);

/// Why the agent could not be asked.
#[derive(Debug, Clone, PartialEq)]
pub enum Unreachable {
    /// The caller has no agent account (they are not in `torq.agent.people`).
    NoAgent(String),
    /// The caller is an agent: it has none of its own.
    IsAgent,
    /// No socket: the service is not on this machine (doctor mode off).
    NoService(String),
    /// The socket is there but not the caller's.
    NotYours(String),
    /// The service did not answer in time, or ended before it answered: what it was asked.
    Silent(String),
    /// The service refused or failed: its words.
    Refused(String),
    /// Anything else (the socket's own error).
    Broken(String),
}

impl std::fmt::Display for Unreachable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unreachable::NoAgent(agent) => write!(f, "you have no agent account on this machine ({agent}): `torq agent people` names who has one (the owner, by default) — adding you is a system change, an administrator's"),
            Unreachable::IsAgent => write!(f, "this is an agent: it has no agent of its own to ask"),
            Unreachable::NoService(path) => write!(f, "your agent's service is not on this machine ({path}): it runs where doctor mode is (torq agent doctor enable true, an administrator's change)"),
            Unreachable::NotYours(path) => write!(f, "{path}: not yours to connect to"),
            Unreachable::Silent(what) => write!(f, "your agent did not answer {what} (journalctl -u torq-agent@<you>.service, an administrator's)"),
            Unreachable::Refused(text) => write!(f, "{text}"),
            Unreachable::Broken(text) => write!(f, "connecting to your agent: {text}"),
        }
    }
}

impl std::error::Error for Unreachable {}

impl Unreachable {
    /// The panel's one line for it.
    pub fn line(&self) -> String {
        match self {
            Unreachable::NoAgent(_) | Unreachable::IsAgent => "The agent is not set up for you".into(),
            Unreachable::NoService(_) => "The agent is not set up on this machine".into(),
            Unreachable::NotYours(_) => "Not reachable: its socket is not yours".into(),
            Unreachable::Silent(_) => "Not reachable: it does not answer".into(),
            Unreachable::Refused(t) | Unreachable::Broken(t) => format!("Not reachable: {}", crate::history::one_line(t, 80)),
        }
    }

    /// Whether the person has no agent here at all (no account, or no service), rather than one that failed.
    pub fn not_set_up(&self) -> bool {
        matches!(self, Unreachable::NoAgent(_) | Unreachable::IsAgent | Unreachable::NoService(_))
    }
}

/// What `status` says.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Status {
    pub running: bool,
    /// How many turns of the conversation it remembers.
    pub turns: usize,
}

/// The caller's own agent: its socket, and whose it must be.
#[derive(Debug, Clone)]
pub struct Client {
    path: PathBuf,
    uid: u32,
    quick: Duration,
    quiet: Duration,
}

fn has_account(passwd: &str, name: &str) -> bool {
    passwd.lines().any(|l| l.split(':').next() == Some(name))
}

impl Client {
    /// The caller's own agent, by the uid the kernel gives this process: its name, its agent account,
    /// its socket. Refused, plainly, for someone with no agent account, or an agent.
    pub fn mine() -> Result<Client, Unreachable> {
        let uid = rustix::process::getuid().as_raw();
        let passwd = std::fs::read_to_string("/etc/passwd").map_err(|e| Unreachable::Broken(format!("reading /etc/passwd: {e}")))?;
        let me = crate::name_of(&passwd, uid).ok_or_else(|| Unreachable::Broken(format!("uid {uid} has no name in /etc/passwd")))?;
        if is_agent(&me) {
            return Err(Unreachable::IsAgent);
        }
        let agent = agent_of(&me);
        if !has_account(&passwd, &agent) {
            return Err(Unreachable::NoAgent(agent));
        }
        Ok(Client::at(Path::new(&socket(&me)), uid))
    }

    /// The agent on `path`, which must be `uid`'s socket.
    pub fn at(path: &Path, uid: u32) -> Client {
        Client { path: path.to_path_buf(), uid, quick: QUICK, quiet: QUIET }
    }

    /// The same, with other bounds (a test's).
    pub fn bounded(self, quick: Duration, quiet: Duration) -> Client {
        Client { quick, quiet, ..self }
    }

    /// A connection to the socket, if it is the caller's: within `quick`, even when the service is frozen
    /// and its queue full.
    fn connect(&self) -> Result<UnixStream, Unreachable> {
        use rustix::io::Errno;
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        let shown = self.path.display().to_string();
        let m = match std::fs::metadata(&self.path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(Unreachable::NoService(shown)),
            Err(e) => return Err(Unreachable::Broken(format!("{shown}: {e}"))),
        };
        // Its directory is root's (/run/torq-agent, 0755): no one else can put another socket in its place.
        if !m.file_type().is_socket() || m.uid() != self.uid {
            return Err(Unreachable::NotYours(shown));
        }
        let addr = rustix::net::SocketAddrUnix::new(&self.path).map_err(|e| Unreachable::Broken(format!("{shown}: {e}")))?;
        let fd = rustix::net::socket_with(rustix::net::AddressFamily::UNIX, rustix::net::SocketType::STREAM, rustix::net::SocketFlags::CLOEXEC | rustix::net::SocketFlags::NONBLOCK, None)
            .map_err(|e| Unreachable::Broken(e.to_string()))?;
        let deadline = Instant::now() + self.quick;
        loop {
            match rustix::net::connect(&fd, &addr) {
                Ok(()) => break,
                // Its queue is full (a frozen service): again, until the deadline.
                Err(Errno::AGAIN) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
                Err(Errno::AGAIN) => return Err(Unreachable::Silent("in time: it takes no connection".into())),
                Err(Errno::NOENT) => return Err(Unreachable::NoService(shown)),
                Err(Errno::ACCESS) => return Err(Unreachable::NotYours(shown)),
                Err(Errno::CONNREFUSED) => return Err(Unreachable::Broken(format!("{shown}: nothing listens there (systemctl status torq-agent@<you>.socket)"))),
                Err(e) => return Err(Unreachable::Broken(format!("{shown}: {e}"))),
            }
        }
        rustix::io::ioctl_fionbio(&fd, false).map_err(|e| Unreachable::Broken(e.to_string()))?;
        let conn = UnixStream::from(fd);
        conn.set_write_timeout(Some(self.quick)).map_err(|e| Unreachable::Broken(e.to_string()))?;
        conn.set_read_timeout(Some(self.quick)).map_err(|e| Unreachable::Broken(e.to_string()))?;
        Ok(conn)
    }

    /// One request sent on a new connection: the connection, to read its answer from.
    fn send(&self, r: &Request) -> Result<Lines, Unreachable> {
        let mut conn = self.connect()?;
        let line = serde_json::to_string(r).map_err(|e| Unreachable::Broken(e.to_string()))?;
        writeln!(conn, "{line}").map_err(|e| Unreachable::Broken(format!("asking your agent: {e}")))?;
        Ok(Lines { r: BufReader::new(conn), deadline: Some(Instant::now() + self.quick) })
    }

    /// Whether a task runs, and how much the conversation keeps.
    pub fn status(&self) -> Result<Status, Unreachable> {
        let mut l = self.send(&Request::Status)?;
        loop {
            match l.next("its status")? {
                Event::Status { running, turns } => return Ok(Status { running, turns }),
                Event::Error { text } => return Err(Unreachable::Refused(crate::history::clean(&text))),
                _ => {}
            }
        }
    }

    /// End the task that runs, whoever asked for it (the person's own, from anywhere): its status after.
    pub fn stop(&self) -> Result<Status, Unreachable> {
        let mut l = self.send(&Request::Stop)?;
        loop {
            match l.next("stop")? {
                Event::Status { running, turns } => return Ok(Status { running, turns }),
                Event::Error { text } => return Err(Unreachable::Refused(crate::history::clean(&text))),
                _ => {}
            }
        }
    }

    /// Erase the conversation it remembers (refused while a task runs).
    pub fn forget(&self) -> Result<(), Unreachable> {
        let mut l = self.send(&Request::Forget)?;
        loop {
            match l.next("forget")? {
                Event::Forgotten => return Ok(()),
                Event::Error { text } => return Err(Unreachable::Refused(crate::history::clean(&text))),
                _ => {}
            }
        }
    }

    /// Ask: the answer's events, as they come, `done` last. Dropping it before then ends the task (the
    /// service stops what no one hears).
    pub fn ask(&self, text: &str, context: Option<String>) -> Result<Answer, Unreachable> {
        let mut lines = self.send(&Request::Ask { text: text.to_string(), context })?;
        // An answer has no deadline of its own: each of its events comes within `quiet` of the one before.
        lines.deadline = None;
        lines.r.get_ref().set_read_timeout(Some(self.quiet)).map_err(|e| Unreachable::Broken(e.to_string()))?;
        Ok(Answer { lines, over: false })
    }
}

/// A connection's lines, one event each, each at most `MAX_LINE` bytes; with a deadline, the whole of a
/// short answer is read by it, however slowly its bytes come.
struct Lines {
    r: BufReader<UnixStream>,
    deadline: Option<Instant>,
}

impl Lines {
    /// The next event, or why there is none: `what` was asked.
    fn next(&mut self, what: &str) -> Result<Event, Unreachable> {
        let silent = || Unreachable::Silent(format!("{what} in time"));
        let mut line = Vec::new();
        loop {
            if let Some(d) = self.deadline {
                let left = d.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err(silent());
                }
                self.r.get_ref().set_read_timeout(Some(left)).map_err(|e| Unreachable::Broken(e.to_string()))?;
            }
            let got = match self.r.fill_buf() {
                Ok(b) => b,
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => return Err(silent()),
                Err(e) => return Err(Unreachable::Broken(format!("reading your agent's answer: {e}"))),
            };
            if got.is_empty() {
                return Err(Unreachable::Silent(format!("{what}: it ended before it answered")));
            }
            let (used, whole) = match got.iter().position(|&b| b == b'\n') {
                Some(i) => (i + 1, true),
                None => (got.len(), false),
            };
            line.extend_from_slice(&got[..used]);
            self.r.consume(used);
            if line.len() > MAX_LINE + 1 {
                return Err(Unreachable::Broken(format!("your agent said something longer than {MAX_LINE} bytes")));
            }
            if whole {
                break;
            }
        }
        serde_json::from_slice(&line).map_err(|_| Unreachable::Broken("your agent said something this torq does not know".into()))
    }
}

/// An answer, streamed: each event as the service says it, ending with `done`; an error ends it too.
pub struct Answer {
    lines: Lines,
    over: bool,
}

impl Iterator for Answer {
    type Item = Result<Event, Unreachable>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.over {
            return None;
        }
        let e = self.lines.next("your question");
        self.over = matches!(e, Ok(Event::Done { .. }) | Err(_));
        Some(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    fn me() -> u32 {
        rustix::process::getuid().as_raw()
    }

    /// A fake service on a socket in a temporary directory: to each connection, after its first line,
    /// the lines `say` gives for it (none: it stays silent, holding the connection). Each request read is
    /// kept, for the test to look at.
    fn service(say: impl Fn(&str) -> Vec<String> + Send + 'static) -> (tempfile::TempDir, PathBuf, std::sync::mpsc::Receiver<String>) {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("me.sock");
        let l = UnixListener::bind(&path).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for conn in l.incoming() {
                let Ok(conn) = conn else { return };
                let mut r = BufReader::new(conn.try_clone().unwrap());
                let mut first = String::new();
                r.read_line(&mut first).unwrap();
                let _ = tx.send(first.trim_end().to_string());
                let lines = say(first.trim_end());
                let mut w = conn;
                if lines.is_empty() {
                    std::thread::sleep(Duration::from_secs(30));
                }
                for l in lines {
                    let _ = writeln!(w, "{l}");
                }
            }
        });
        (d, path, rx)
    }

    fn quick(path: &Path) -> Client {
        Client::at(path, me()).bounded(Duration::from_millis(500), Duration::from_millis(500))
    }

    /// An answer streamed: each event of the task, `done` last, then nothing; the request as the wire says.
    #[test]
    fn an_answer_streamed() {
        let (_d, path, asked) = service(|_| {
            [
                r#"{"type":"called","tool":"read_journal"}"#,
                r#"{"type":"tainted","why":"read_journal"}"#,
                r#"{"type":"say","text":"fine"}"#,
                r#"{"type":"proposed","seq":7}"#,
                r#"{"type":"done","exit":0}"#,
                r#"{"type":"say","text":"after done"}"#,
            ]
            .map(String::from)
            .to_vec()
        });
        let got: Vec<_> = quick(&path).ask("why?", None).unwrap().collect::<Result<_, _>>().unwrap();
        assert_eq!(
            got,
            vec![
                Event::Called { tool: "read_journal".into() },
                Event::Tainted { why: "read_journal".into() },
                Event::Say { text: "fine".into() },
                Event::Proposed { seq: 7 },
                Event::Done { exit: 0 },
            ],
            "nothing after done"
        );
        assert_eq!(asked.recv().unwrap(), r#"{"type":"ask","text":"why?"}"#);
    }

    #[test]
    fn status_stop_and_forget() {
        let (_d, path, asked) = service(|req| match req {
            r#"{"type":"status"}"# => vec![r#"{"type":"status","running":true,"turns":4}"#.into()],
            r#"{"type":"stop"}"# => vec![r#"{"type":"status","running":false,"turns":4}"#.into()],
            _ => vec![r#"{"type":"error","text":"a task is running: stop it first\u001b[2J"}"#.into(), r#"{"type":"done","exit":1}"#.into()],
        });
        let c = quick(&path);
        assert_eq!(c.status().unwrap(), Status { running: true, turns: 4 });
        assert_eq!(c.stop().unwrap(), Status { running: false, turns: 4 });
        let refused = c.forget().unwrap_err();
        assert!(matches!(&refused, Unreachable::Refused(t) if t.starts_with("a task is running") && !t.contains('\u{1b}')), "{refused:?}");
        let sent: Vec<String> = asked.try_iter().collect();
        assert_eq!(sent, [r#"{"type":"status"}"#, r#"{"type":"stop"}"#, r#"{"type":"forget"}"#]);
    }

    /// A service that never answers: every call ends, in its bound.
    #[test]
    fn a_silent_service_times_out() {
        let (_d, path, _asked) = service(|_| vec![]);
        let c = quick(&path);
        let t = Instant::now();
        assert!(matches!(c.status(), Err(Unreachable::Silent(_))));
        let mut a = c.ask("hello", None).unwrap();
        assert!(matches!(a.next(), Some(Err(Unreachable::Silent(_)))));
        assert!(a.next().is_none(), "over at its error");
        assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
    }

    /// A service that ends without its last word: an error, not an answer.
    #[test]
    fn a_service_that_ends_early() {
        let (_d, path, _asked) = service(|_| vec![r#"{"type":"say","text":"half"}"#.into()]);
        let got: Vec<_> = quick(&path).ask("q", None).unwrap().collect();
        assert_eq!(got[0], Ok(Event::Say { text: "half".into() }));
        assert!(matches!(got[1], Err(Unreachable::Silent(_))) && got.len() == 2, "{got:?}");
    }

    /// Only the caller's own socket: one of another uid is refused before anything is sent; no socket is
    /// no service; a file that is not a socket is not theirs either.
    #[test]
    fn only_ones_own_socket() {
        let (d, path, asked) = service(|_| vec![r#"{"type":"status","running":false,"turns":0}"#.into()]);
        let other = Client::at(&path, me() + 1).bounded(Duration::from_millis(500), Duration::from_millis(500));
        assert_eq!(other.status().unwrap_err(), Unreachable::NotYours(path.display().to_string()));
        assert!(other.ask("hi", None).is_err());
        assert!(asked.recv_timeout(Duration::from_millis(200)).is_err(), "nothing was sent");
        assert!(matches!(quick(&d.path().join("none.sock")).status(), Err(Unreachable::NoService(_))));
        std::fs::write(d.path().join("file"), "").unwrap();
        assert!(matches!(quick(&d.path().join("file")).status(), Err(Unreachable::NotYours(_))));
        assert!(quick(&path).status().unwrap() == Status { running: false, turns: 0 }, "one's own is asked");
    }

    /// A socket no one listens on any more: an error at once, not a wait.
    #[test]
    fn nothing_listening() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("gone.sock");
        drop(UnixListener::bind(&path).unwrap());
        assert!(matches!(quick(&path).status(), Err(Unreachable::Broken(t)) if t.contains("nothing listens")));
    }

    /// A service whose queue is full (frozen, never accepting): connecting gives up at its bound.
    #[test]
    fn a_frozen_service_is_given_up_on() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("frozen.sock");
        let fd = rustix::net::socket(rustix::net::AddressFamily::UNIX, rustix::net::SocketType::STREAM, None).unwrap();
        rustix::net::bind(&fd, &rustix::net::SocketAddrUnix::new(&path).unwrap()).unwrap();
        rustix::net::listen(&fd, 0).unwrap();
        // Fill its queue: connections no one accepts.
        let mut held = vec![];
        for _ in 0..8 {
            let s = rustix::net::socket_with(rustix::net::AddressFamily::UNIX, rustix::net::SocketType::STREAM, rustix::net::SocketFlags::NONBLOCK, None).unwrap();
            if rustix::net::connect(&s, &rustix::net::SocketAddrUnix::new(&path).unwrap()).is_err() {
                break;
            }
            held.push(s);
        }
        let t = Instant::now();
        assert!(matches!(quick(&path).status(), Err(Unreachable::Silent(_))));
        assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
    }

    #[test]
    fn whose_name_is_this_uid() {
        let passwd = "root:x:0:0::/root:/bin/sh\nalice:x:1000:100::/home/alice:/bin/sh\ntorq-agent-alice:x:990:990::/var/lib/torq-agent-alice:/sbin/nologin\n";
        assert_eq!(crate::name_of(passwd, 1000).as_deref(), Some("alice"));
        assert_eq!(crate::name_of(passwd, 1001), None);
        assert!(has_account(passwd, "torq-agent-alice") && !has_account(passwd, "torq-agent-bob"));
    }

    #[test]
    fn what_the_panel_says_of_each() {
        assert_eq!(Unreachable::NoAgent("torq-agent-me".into()).line(), "The agent is not set up for you");
        assert!(Unreachable::NoService("/x".into()).not_set_up() && !Unreachable::Silent("x".into()).not_set_up());
        assert!(!Unreachable::Refused("a\nb\u{1b}".into()).line().contains(['\n', '\u{1b}']));
    }

    /// A service that trickles its answer a byte at a time, never ending the line: one deadline for the
    /// whole short answer, not one for each byte.
    #[test]
    fn a_trickling_service_is_given_up_on() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("slow.sock");
        let l = UnixListener::bind(&path).unwrap();
        std::thread::spawn(move || {
            if let Ok((mut c, _)) = l.accept() {
                for _ in 0..200 {
                    if c.write_all(b" ").is_err() {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        });
        let t = Instant::now();
        assert!(matches!(quick(&path).status(), Err(Unreachable::Silent(_))));
        assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
    }
}
