//! `torq-agent-service <person>` (ADR-051 3, step 2): a person's agent, resident —
//! `torq-agent@<person>.service`, run as `torq-agent-<person>` in its sandbox (modules/agent.nix),
//! started by its socket (`/run/torq-agent/<person>.sock`, handed over as its standard input) the
//! first time it is asked.
//!
//! It holds the person's conversation and runs one task at a time, each its own process — TorqOS's
//! harness, `torq-doctor --propose --task` (ADR-042 6), with its own session at the broker and its own
//! record — and relays what the task says (torq-wire's `agent` words), then `done`. It reads no model's
//! output and runs no tool: the harness does. It answers only the person it serves, by the kernel's word
//! (SO_PEERCRED), whoever else could reach the socket — root included.

// No unsafe code and no dead code, ever (ADR-020); in a test build `deny`, as the doctor's.
#![forbid(unsafe_code)]
#![cfg_attr(not(test), forbid(dead_code))]
#![cfg_attr(test, deny(dead_code))]

#[path = "../clean.rs"]
mod clean;
mod memory;

use anyhow::{bail, Context, Result};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use torq_wire::agent::{Event, Request, Task, Turn, MAX_LINE};

/// The person's agent: whom it answers, how it runs a task, what it remembers, what runs now.
struct Agent {
    person: String,
    /// The person's uid: the only one answered.
    uid: u32,
    /// The harness each task runs (torq-doctor, beside this program).
    harness: PathBuf,
    home: PathBuf,
    memory: memory::Memory,
    running: Mutex<Option<Running>>,
    /// The unit's `cgroup.procs`: every process of the task is there (none outside a unit, e.g. in a test).
    procs: Option<PathBuf>,
}

/// The task running now: its process group, and the connection that asked for it.
#[derive(Clone, Copy)]
struct Running {
    group: rustix::process::Pid,
    conn: u64,
}

/// A connection's writer, shared by its reader and its task's relay: one event, one whole line.
#[derive(Clone)]
struct Out(Arc<Mutex<UnixStream>>);

impl Out {
    fn send(&self, e: &Event) {
        let mut s = self.0.lock().unwrap_or_else(|p| p.into_inner());
        // A client gone is not an error of the agent's: what it asked still runs to its end, or is stopped.
        let _ = writeln!(s, "{}", serde_json::to_string(e).unwrap_or_default()).and_then(|()| s.flush());
    }
}

/// Whether the peer of `conn` is the one with `uid`, by the kernel's word.
fn peer_is(conn: &UnixStream, uid: u32) -> Result<bool> {
    let peer = rustix::net::sockopt::socket_peercred(conn).context("asking the kernel who connected")?;
    Ok(peer.uid.as_raw() == uid)
}

/// A line read: its text, or one that was dropped (longer than `MAX_LINE` bytes, read to its end and
/// thrown away; or not UTF-8), or the end.
#[derive(Debug, PartialEq)]
enum Line {
    Text(String),
    Dropped,
    End,
}

/// One line of at most `MAX_LINE` bytes, without its newline. A longer one is read to its end and
/// dropped, so that whoever writes it is never left blocked on a pipe no one reads.
fn read_line(r: &mut impl BufRead) -> std::io::Result<Line> {
    let mut buf = Vec::new();
    let n = r.by_ref().take(MAX_LINE as u64 + 1).read_until(b'\n', &mut buf)?;
    if n == 0 {
        return Ok(Line::End);
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
    } else if buf.len() > MAX_LINE {
        loop {
            buf.clear();
            let n = r.by_ref().take(64 * 1024).read_until(b'\n', &mut buf)?;
            if n == 0 || buf.last() == Some(&b'\n') {
                return Ok(Line::Dropped);
            }
        }
    }
    Ok(String::from_utf8(buf).map(Line::Text).unwrap_or(Line::Dropped))
}

/// A word as the harness names a tool or a taint's source: at most 64 of letters, digits, `_` and `-`.
fn word_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// What of a task's line is relayed: an event a task may say, its words checked and its text cleaned
/// again (whatever the harness did) — anything else is dropped.
fn relayed(line: &str) -> Option<Event> {
    let e: Event = serde_json::from_str(line).ok()?;
    if !e.from_task() {
        return None;
    }
    Some(match e {
        Event::Say { text } => Event::Say { text: clean::clean(&text) },
        Event::Error { text } => Event::Error { text: clean::clean(&text) },
        Event::Called { tool } if word_ok(&tool) => Event::Called { tool },
        Event::Tainted { why } if word_ok(&why) => Event::Tainted { why },
        e @ Event::Proposed { .. } => e,
        _ => return None,
    })
}

impl Agent {
    fn running(&self) -> Option<Running> {
        *self.running.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Serve one connection: the person's, or no one's. Every request's answer ends with a last frame
    /// (`done`, `status` or `forgotten`), on every way it can go — a refusal and an error included.
    fn serve(self: &Arc<Self>, conn: UnixStream, id: u64) -> Result<()> {
        let out = Out(Arc::new(Mutex::new(conn.try_clone()?)));
        let refuse = |text: String| {
            out.send(&Event::Error { text });
            out.send(&Event::Done { exit: 1 });
        };
        if !peer_is(&conn, self.uid)? {
            refuse(format!("this is {}'s agent: it answers {} alone", self.person, self.person));
            // Closed only once what it sent is read (a little of it, for at most 2 s in all): closed with
            // it unread, the kernel could reset the connection before the refusal reached the other end.
            let _ = conn.shutdown(std::net::Shutdown::Write);
            let deadline = Instant::now() + Duration::from_secs(2);
            let (mut left, mut buf) = (64 * 1024usize, [0u8; 4096]);
            while left > 0 {
                let Some(wait) = deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()) else { break };
                let _ = conn.set_read_timeout(Some(wait));
                match (&conn).read(&mut buf) {
                    Ok(n) if n > 0 => left = left.saturating_sub(n),
                    _ => break,
                }
            }
            return Ok(());
        }
        let mut r = BufReader::new(conn);
        loop {
            let line = match read_line(&mut r) {
                Ok(Line::Text(l)) => l,
                Ok(Line::End) => break,
                Ok(Line::Dropped) => {
                    refuse(format!("not a request: longer than {MAX_LINE} bytes, or not UTF-8"));
                    continue;
                }
                Err(e) => {
                    refuse(format!("reading the request: {e}"));
                    break;
                }
            };
            match serde_json::from_str::<Request>(&line) {
                Err(e) => refuse(format!("not a request: {e}")),
                Ok(Request::Ask { text, context }) => {
                    if let Err(e) = self.ask(text, context, &out, id) {
                        refuse(format!("{e:#}"));
                    }
                }
                Ok(Request::Stop) => {
                    self.stop();
                    out.send(&Event::Status { running: self.running().is_some() });
                }
                Ok(Request::Status) => out.send(&Event::Status { running: self.running().is_some() }),
                Ok(Request::Forget) => {
                    // Under the task slot's lock: a task starting now reads the memory after this.
                    let slot = self.running.lock().unwrap_or_else(|p| p.into_inner());
                    let forgot = if slot.is_some() { Err(anyhow::anyhow!("a task is running: stop it first, or wait")) } else { self.memory.forget() };
                    drop(slot);
                    match forgot {
                        Ok(()) => out.send(&Event::Forgotten),
                        Err(e) => refuse(format!("{e:#}")),
                    }
                }
            }
        }
        // Whoever asked is gone: no one hears the rest, so it is stopped.
        if self.running().is_some_and(|r| r.conn == id) {
            self.stop();
        }
        Ok(())
    }

    /// Start a task, if none runs: the harness, its own process group, handed the task; its events
    /// relayed to `out` as they come, then `done`; the turn remembered.
    fn ask(self: &Arc<Self>, text: String, context: Option<String>, out: &Out, conn: u64) -> Result<()> {
        if text.trim().is_empty() {
            bail!("ask what? (an empty question)");
        }
        let mut slot = self.running.lock().unwrap_or_else(|p| p.into_inner());
        if slot.is_some() {
            bail!("a task is running already (one at a time): stop it, or wait");
        }
        // The memory read with the slot held: a `forget` before this is honoured.
        let task = Task { question: text.clone(), context, memory: self.memory.recent() };
        use std::os::unix::process::CommandExt;
        let mut child = Command::new(&self.harness)
            .args(["--propose", "--task"])
            .env_clear()
            .env("PATH", "/run/current-system/sw/bin")
            .env("HOME", &self.home)
            .env("LANG", "C.UTF-8")
            .current_dir(&self.home)
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("starting {}", self.harness.display()))?;
        *slot = Some(Running { group: rustix::process::Pid::from_child(&child), conn });
        drop(slot);
        let mut stdin = child.stdin.take().context("the task's input")?;
        let stdout = child.stdout.take().context("the task's output")?;
        let line = serde_json::to_string(&task)?;
        let (me, out) = (self.clone(), out.clone());
        // The task's events relayed as they come (a thread of their own); this one waits for the harness
        // to end, sweeps what is left of it — which also closes the output anything left could hold —
        // and only then takes the relay's end, remembers the turn, frees the slot and says `done`.
        let relay_out = out.clone();
        let relay = std::thread::spawn(move || {
            let _ = writeln!(stdin, "{line}");
            drop(stdin);
            let (mut said, mut tainted) = (None, false);
            let mut r = BufReader::new(stdout);
            loop {
                let l = match read_line(&mut r) {
                    Ok(Line::Text(l)) => l,
                    // Read to its end and thrown away: the task goes on, and is never left blocked.
                    Ok(Line::Dropped) => {
                        relay_out.send(&Event::Error { text: format!("the task said something longer than {MAX_LINE} bytes, or not text: left out") });
                        continue;
                    }
                    Ok(Line::End) | Err(_) => break,
                };
                let Some(e) = relayed(&l) else { continue };
                match &e {
                    Event::Say { text } => said = Some(text.trim().to_string()),
                    Event::Tainted { .. } => tainted = true,
                    _ => {}
                }
                relay_out.send(&e);
            }
            (said, tainted)
        });
        std::thread::spawn(move || {
            let exit = match child.wait() {
                Ok(s) => s.code().unwrap_or_else(|| 128 + std::os::unix::process::ExitStatusExt::signal(&s).unwrap_or(0)),
                Err(_) => 1,
            };
            me.sweep();
            let (said, tainted) = relay.join().unwrap_or((None, true));
            if exit == 0 {
                let answer = if tainted { None } else { said };
                if let Err(e) = me.memory.remember(Turn { question: text, answer }) {
                    eprintln!("torq-agent-service: remembering the turn: {e:#}");
                }
            }
            *me.running.lock().unwrap_or_else(|p| p.into_inner()) = None;
            out.send(&Event::Done { exit });
        });
        Ok(())
    }

    /// End the running task: its process group; then the rest of it goes as the task ends (`sweep`, by
    /// the task's waiter, before the slot is free). Waits, at most 5 s, for it to be over.
    fn stop(&self) {
        let Some(r) = self.running() else { return };
        let _ = rustix::process::kill_process_group(r.group, rustix::process::Signal::KILL);
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.running().is_some() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Nothing of a task outlives it: every other process in this service's own control group is
    /// killed, again and again until none is left (for at most 5 s) — a tool's program runs in a group of
    /// its own (ADR-042 5), and anything can make itself undumpable, and so out of sight in /proc. The
    /// unit's `cgroup.procs` lists them all, however they look; leaving the group needs a write to the
    /// cgroup files, read-only here (ProtectControlGroups). Everything in the unit but the service is the
    /// task's: one task at a time.
    fn sweep(&self) {
        let me = std::process::id();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let left = self.procs.as_deref().map(|p| left_in(p, me)).unwrap_or_default();
            if left.is_empty() {
                return;
            }
            if Instant::now() >= deadline {
                eprintln!("torq-agent-service: {} processes of the task were not gone after 5 s", left.len());
                return;
            }
            for pid in left {
                let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
/// The unit's processes but `me`, from its `cgroup.procs` (`procs`): those still alive.
fn left_in(procs: &Path, me: u32) -> Vec<rustix::process::Pid> {
    let text = std::fs::read_to_string(procs).unwrap_or_default();
    text.split_whitespace()
        .filter_map(|p| p.parse::<i32>().ok())
        .filter(|&p| p as u32 != me)
        .filter_map(rustix::process::Pid::from_raw)
        // Alive: a signal reaches it, and it is not a zombie waiting to be reaped (if /proc shows it).
        .filter(|&p| rustix::process::test_kill_process(p).is_ok() && !std::fs::read_to_string(format!("/proc/{}/stat", p.as_raw_nonzero())).is_ok_and(|s| s.rsplit_once(") ").is_some_and(|(_, rest)| rest.starts_with('Z'))))
        .collect()
}

/// This service's own unit's `cgroup.procs`, from /proc/self/cgroup — only when that group is the
/// person's agent's unit (`unit`, its path's end): it never sweeps anything elsewhere.
fn unit_procs(unit: &str) -> Option<PathBuf> {
    let cgroup = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    let path = cgroup.lines().find_map(|l| l.strip_prefix("0::"))?;
    path.ends_with(unit).then(|| PathBuf::from(format!("/sys/fs/cgroup{path}/cgroup.procs")))
}

/// The uid of account `name`, from /etc/passwd.
fn uid_of(passwd: &str, name: &str) -> Option<u32> {
    passwd.lines().find_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        (f.first() == Some(&name)).then(|| f.get(2)?.parse().ok()).flatten()
    })
}

/// The agent of `person` (its argument), as this machine says it is: the person a person (uid 1000
/// and up), this process their agent account.
fn agent(args: &[String]) -> Result<Agent> {
    let [person] = args else { bail!("usage: torq-agent-service <person> (started by torq-agent@<person>.socket)") };
    if !person.starts_with(|c: char| c.is_ascii_lowercase() || c == '_') || !person.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-') {
        bail!("{person}: not a person's name");
    }
    let passwd = std::fs::read_to_string("/etc/passwd").context("reading /etc/passwd")?;
    let uid = uid_of(&passwd, person).filter(|u| *u >= 1000).with_context(|| format!("{person} is not a person of this machine"))?;
    let account = format!("torq-agent-{person}");
    if uid_of(&passwd, &account) != Some(rustix::process::getuid().as_raw()) {
        bail!("this runs as {account}, {person}'s agent, and as no one else");
    }
    let state = std::env::var_os("STATE_DIRECTORY").context("no STATE_DIRECTORY: its memory's place (the unit's StateDirectory)")?;
    let me = std::env::current_exe().context("where this program is")?;
    Ok(Agent {
        person: person.clone(),
        uid,
        harness: me.with_file_name("torq-doctor"),
        home: PathBuf::from(format!("/var/lib/{account}")),
        memory: memory::Memory::new(Path::new(&state)),
        running: Mutex::new(None),
        procs: unit_procs(&format!("/torq-agent@{person}.service")),
    })
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = run(&args) {
        eprintln!("torq-agent-service: {e:#}");
        std::process::exit(1);
    }
}

fn run(args: &[String]) -> Result<()> {
    let agent = Arc::new(agent(args)?);
    // The socket systemd listens on, as standard input (StandardInput=socket, Accept=no).
    let listener = UnixListener::from(std::io::stdin().as_fd().try_clone_to_owned().context("the socket (standard input)")?);
    serve(agent, listener)
}

/// Each connection in a thread of its own.
fn serve(agent: Arc<Agent>, listener: UnixListener) -> Result<()> {
    let mut id = 0u64;
    loop {
        let (conn, _) = listener.accept().context("waiting for the person (is standard input the socket?)")?;
        id += 1;
        let (agent, n) = (agent.clone(), id);
        std::thread::spawn(move || {
            if let Err(e) = agent.serve(conn, n) {
                eprintln!("torq-agent-service: {e:#}");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// An agent whose harness is a script, serving a socket in a directory of the test's.
    fn agent(harness: &str, uid: u32) -> (Arc<Agent>, PathBuf, tempfile::TempDir) {
        let d = tempfile::tempdir().unwrap();
        let h = d.path().join("harness");
        // The harness is run with PATH the machine's: the test's own, for its script.
        std::fs::write(&h, format!("#!/bin/sh\nPATH={}\n{harness}\n", std::env::var("PATH").unwrap_or_default())).unwrap();
        std::fs::set_permissions(&h, std::fs::Permissions::from_mode(0o755)).unwrap();
        let a = Arc::new(Agent { person: "alice".into(), uid, harness: h, home: d.path().into(), memory: memory::Memory::new(d.path()), running: Mutex::new(None), procs: Some(d.path().join("procs")) });
        let sock = d.path().join("s.sock");
        let l = UnixListener::bind(&sock).unwrap();
        let served = a.clone();
        std::thread::spawn(move || serve(served, l));
        (a, sock, d)
    }

    fn me() -> u32 {
        rustix::process::getuid().as_raw()
    }

    /// Send `requests`, read events until `until` says stop (or the connection ends).
    fn talk(sock: &Path, requests: &[Request], until: impl Fn(&Event) -> bool) -> Vec<Event> {
        let mut c = UnixStream::connect(sock).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        for q in requests {
            writeln!(c, "{}", serde_json::to_string(q).unwrap()).unwrap();
        }
        let mut r = BufReader::new(c.try_clone().unwrap());
        let mut seen = vec![];
        while let Ok(Line::Text(l)) = read_line(&mut r) {
            let e: Event = serde_json::from_str(&l).unwrap();
            let end = until(&e);
            seen.push(e);
            if end {
                break;
            }
        }
        seen
    }

    fn ask(text: &str) -> Request {
        Request::Ask { text: text.into(), context: None }
    }

    fn done(e: &Event) -> bool {
        matches!(e, Event::Done { .. })
    }

    /// SO_PEERCRED: the kernel's uid of whoever connected — answered if it is the person's, refused if not.
    #[test]
    fn only_the_person_the_kernel_names() {
        let (a, b) = UnixStream::pair().unwrap();
        assert!(peer_is(&a, me()).unwrap() && peer_is(&b, me()).unwrap());
        assert!(!peer_is(&a, me() + 1).unwrap());
        let (_a, sock, _d) = agent("echo never", me() + 1);
        let seen = talk(&sock, &[ask("hi")], |_| false);
        assert_eq!(seen, vec![Event::Error { text: "this is alice's agent: it answers alice alone".into() }, Event::Done { exit: 1 }], "refused, and nothing run");
    }

    /// What the harness says is relayed, cleaned; `done` last; the turn remembered — its answer only
    /// from an untainted session; and the next task is handed the conversation.
    #[test]
    fn a_task_streamed_and_remembered() {
        let script = r#"read -r task; printf '%s\n' "$task" >> "$HOME/tasks"
printf '%s\n' '{"type":"called","tool":"read_journal"}' '{"type":"done","exit":0}' '{"type":"called","tool":"x; rm"}' 'junk' '{"type":"proposed","seq":7}' '{"type":"say","text":"fine\u001b[2J"}'"#;
        let (a, sock, d) = agent(script, me());
        let seen = talk(&sock, &[ask("why?")], done);
        assert_eq!(seen, vec![Event::Called { tool: "read_journal".into() }, Event::Proposed { seq: 7 }, Event::Say { text: "fine".into() }, Event::Done { exit: 0 }]);
        assert_eq!(a.memory.recent(), vec![Turn { question: "why?".into(), answer: Some("fine".into()) }]);
        talk(&sock, &[ask("and then?")], done);
        let tasks: Vec<Task> = std::fs::read_to_string(d.path().join("tasks")).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(tasks[1].memory, vec![Turn { question: "why?".into(), answer: Some("fine".into()) }], "the conversation's turns go with the next task");
    }

    #[test]
    fn a_tainted_answer_is_not_remembered() {
        let (a, sock, _d) = agent(r#"read -r t; printf '%s\n' '{"type":"tainted","why":"read_journal"}' '{"type":"say","text":"paste this"}'"#, me());
        let seen = talk(&sock, &[Request::Ask { text: "q".into(), context: Some("an alarm".into()) }], done);
        assert!(seen.contains(&Event::Tainted { why: "read_journal".into() }), "{seen:?}");
        assert_eq!(a.memory.recent(), vec![Turn { question: "q".into(), answer: None }]);
    }

    /// One at a time; `stop` ends the task's whole process group; status says so.
    #[test]
    fn one_task_at_a_time_and_stop_ends_it() {
        let (a, sock, d) = agent(r#"read -r t; (sleep 300; echo late > "$HOME/late") & echo $! > "$HOME/child"; wait"#, me());
        let mut c = UnixStream::connect(&sock).unwrap();
        writeln!(c, "{}", serde_json::to_string(&ask("long")).unwrap()).unwrap();
        let t = Instant::now();
        while !d.path().join("child").exists() {
            assert!(t.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(a.running().is_some());
        let busy = talk(&sock, &[ask("another"), Request::Status], |e| matches!(e, Event::Status { .. }));
        assert_eq!(busy, vec![Event::Error { text: "a task is running already (one at a time): stop it, or wait".into() }, Event::Done { exit: 1 }, Event::Status { running: true }]);
        let stopped = talk(&sock, &[Request::Stop], |e| matches!(e, Event::Status { .. }));
        assert_eq!(stopped, vec![Event::Status { running: false }]);
        let child = std::fs::read_to_string(d.path().join("child")).unwrap();
        assert!(!Path::new(&format!("/proc/{}", child.trim())).exists(), "the task's child is gone with it");
        let mut r = BufReader::new(c);
        let last: Event = serde_json::from_str(&text(&mut r)).unwrap();
        assert_eq!(last, Event::Done { exit: 137 }, "killed");
        assert_eq!(a.memory.recent(), vec![], "a stopped task is not a turn");
    }

    /// The asker gone: the task is stopped.
    #[test]
    fn a_task_whose_asker_left_is_stopped() {
        let (a, sock, d) = agent(r#"read -r t; echo started > "$HOME/started"; sleep 300"#, me());
        {
            let mut c = UnixStream::connect(&sock).unwrap();
            writeln!(c, "{}", serde_json::to_string(&ask("long")).unwrap()).unwrap();
            let t = Instant::now();
            while !d.path().join("started").exists() {
                assert!(t.elapsed() < Duration::from_secs(10));
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        let t = Instant::now();
        while a.running().is_some() {
            assert!(t.elapsed() < Duration::from_secs(10), "still running");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn requests_it_does_not_know_are_refused() {
        let (_a, sock, _d) = agent("true", me());
        let mut c = UnixStream::connect(&sock).unwrap();
        writeln!(c, r#"{{"type":"apply","seq":1}}"#).unwrap();
        writeln!(c, r#"{{"type":"ask","text":"   "}}"#).unwrap();
        writeln!(c, r#"{{"type":"forget"}}"#).unwrap();
        let mut r = BufReader::new(c);
        let e = |r: &mut BufReader<UnixStream>| serde_json::from_str::<Event>(&text(r)).unwrap();
        assert!(matches!(e(&mut r), Event::Error { text } if text.starts_with("not a request")));
        assert_eq!(e(&mut r), Event::Done { exit: 1 }, "every request ends with a last frame");
        assert_eq!(e(&mut r), Event::Error { text: "ask what? (an empty question)".into() });
        assert_eq!(e(&mut r), Event::Done { exit: 1 }, "an ask always ends with done");
        assert_eq!(e(&mut r), Event::Forgotten);
    }

    /// A line too long is read to its end and dropped: whoever wrote it goes on.
    #[test]
    fn a_line_too_long_is_dropped_whole() {
        let long = format!("{}\nnext\n", "x".repeat(MAX_LINE * 2 + 10));
        let mut c = std::io::Cursor::new(long.as_bytes());
        assert_eq!(read_line(&mut c).unwrap(), Line::Dropped);
        assert_eq!(read_line(&mut c).unwrap(), Line::Text("next".into()));
        assert_eq!(read_line(&mut c).unwrap(), Line::End);
        assert_eq!(read_line(&mut std::io::Cursor::new(b"\xff\nb\n".as_slice())).unwrap(), Line::Dropped);
    }

    /// A task that says something over the bound (an answer of 2 MB): dropped, said so, and the task runs
    /// on to its end — never left blocked on a pipe no one reads.
    #[test]
    fn a_task_saying_too_much_is_not_left_hanging() {
        let (_a, sock, _d) = agent(r#"read -r t; printf '{"type":"say","text":"'; head -c 2000000 /dev/zero | tr '\0' x; printf '"}\n'; printf '%s\n' '{"type":"say","text":"after"}'"#, me());
        let seen = talk(&sock, &[ask("long")], done);
        assert_eq!(seen.len(), 3, "{:?}", seen.iter().map(|e| format!("{e:?}").chars().take(80).collect::<String>()).collect::<Vec<_>>());
        assert!(matches!(&seen[0], Event::Error { text } if text.contains("left out")));
        assert_eq!(seen[1..], [Event::Say { text: "after".into() }, Event::Done { exit: 0 }]);
    }

    /// `forget` while a task runs: refused, and ended (no client waits); after it, done.
    #[test]
    fn forget_while_a_task_runs_ends_too() {
        let (a, sock, d) = agent(r#"read -r t; echo started > "$HOME/started"; sleep 300"#, me());
        a.memory.remember(Turn { question: "kept".into(), answer: None }).unwrap();
        let mut c = UnixStream::connect(&sock).unwrap();
        writeln!(c, "{}", serde_json::to_string(&ask("long")).unwrap()).unwrap();
        wait_for(&d.path().join("started"));
        let seen = talk(&sock, &[Request::Forget], |e| matches!(e, Event::Done { .. } | Event::Forgotten));
        assert_eq!(seen, [Event::Error { text: "a task is running: stop it first, or wait".into() }, Event::Done { exit: 1 }]);
        assert_eq!(a.memory.recent().len(), 1, "not forgotten while it ran");
        talk(&sock, &[Request::Stop], |e| matches!(e, Event::Status { .. }));
        assert_eq!(talk(&sock, &[Request::Forget], |e| matches!(e, Event::Forgotten)), [Event::Forgotten]);
        assert_eq!(a.memory.recent(), vec![]);
    }

    fn wait_for(f: &Path) {
        let t = Instant::now();
        while !f.exists() {
            assert!(t.elapsed() < Duration::from_secs(10), "{} never came", f.display());
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Gone: no signal reaches it, or it is a zombie (dead, waiting to be reaped).
    fn gone(pid: i32) -> bool {
        let p = rustix::process::Pid::from_raw(pid).unwrap();
        rustix::process::test_kill_process(p).is_err() || std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| s.rsplit_once(") ").is_some_and(|(_, r)| r.starts_with('Z')))
    }

    /// Not part of the suite: run by the next test as a process of the task that has made itself
    /// undumpable (so /proc shows it as root's, and hides it from the service's ProtectProc), then waits.
    #[test]
    #[ignore]
    fn undumpable_helper() {
        if std::env::var_os("TORQ_TEST_UNDUMPABLE").is_some() {
            rustix::process::set_dumpable_behavior(rustix::process::DumpableBehavior::NotDumpable).unwrap();
            std::thread::sleep(Duration::from_secs(300));
        }
    }

    /// `stop` ends all of the task: its group, a process in a group of its own, and one that made itself
    /// undumpable — every process the unit's `cgroup.procs` lists (here a file the harness writes) — even
    /// once the harness itself is gone.
    #[test]
    fn stop_ends_every_process_of_the_task() {
        let helper = std::env::current_exe().unwrap();
        let script = format!(
            r#"read -r t; set -m
sleep 300 & echo $! > "$HOME/own-group"
TORQ_TEST_UNDUMPABLE=1 {} --exact tests::undumpable_helper --ignored --test-threads=1 >/dev/null 2>&1 & echo $! > "$HOME/undumpable"
echo $$ $(cat "$HOME/own-group") $(cat "$HOME/undumpable") > "$HOME/procs"
echo ready > "$HOME/ready"; wait"#,
            helper.display()
        );
        let (a, sock, d) = agent(&script, me());
        let mut c = UnixStream::connect(&sock).unwrap();
        writeln!(c, "{}", serde_json::to_string(&ask("long")).unwrap()).unwrap();
        wait_for(&d.path().join("ready"));
        let pid = |f: &str| std::fs::read_to_string(d.path().join(f)).unwrap().trim().parse::<i32>().unwrap();
        let (own, undumpable) = (pid("own-group"), pid("undumpable"));
        // Its own group: not the harness's, so the group's kill does not reach it.
        let pgid = |p: i32| std::fs::read_to_string(format!("/proc/{p}/stat")).ok().and_then(|s| s.rsplit_once(") ").and_then(|(_, r)| r.split(' ').nth(2).map(String::from)));
        let harness: i32 = std::fs::read_to_string(d.path().join("procs")).unwrap().split_whitespace().next().unwrap().parse().unwrap();
        assert!(pgid(own).is_some() && pgid(own) != pgid(harness), "a group of its own");
        // Undumpable: /proc shows its files as root's, not ours.
        let t = Instant::now();
        loop {
            use std::os::unix::fs::MetadataExt;
            if std::fs::metadata(format!("/proc/{undumpable}/cgroup")).is_ok_and(|m| m.uid() != me()) {
                break;
            }
            assert!(t.elapsed() < Duration::from_secs(10), "the helper never made itself undumpable");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(talk(&sock, &[Request::Stop], |e| matches!(e, Event::Status { .. })), [Event::Status { running: false }]);
        assert!(gone(own), "the process in a group of its own is gone");
        assert!(gone(undumpable), "the undumpable process is gone");
        let mut r = BufReader::new(c);
        assert_eq!(serde_json::from_str::<Event>(&text(&mut r)).unwrap(), Event::Done { exit: 137 });
        assert!(a.running().is_none());
    }


    /// A task that ends by itself leaves nothing behind: a process it left in a group of its own,
    /// holding its output open, and an undumpable one, are gone by `done` — and `done` comes.
    #[test]
    fn nothing_of_a_task_outlives_it() {
        let helper = std::env::current_exe().unwrap();
        let script = format!(
            r#"read -r t; set -m
sleep 300 & echo $! > "$HOME/own-group"
TORQ_TEST_UNDUMPABLE=1 {} --exact tests::undumpable_helper --ignored --test-threads=1 >/dev/null 2>&1 </dev/null & echo $! > "$HOME/undumpable"
echo $$ $(cat "$HOME/own-group") $(cat "$HOME/undumpable") > "$HOME/procs"
printf '%s\n' '{{"type":"say","text":"bye"}}'"#,
            helper.display()
        );
        let (_a, sock, d) = agent(&script, me());
        let seen = talk(&sock, &[ask("go")], done);
        assert_eq!(seen, [Event::Say { text: "bye".into() }, Event::Done { exit: 0 }]);
        let pid = |f: &str| std::fs::read_to_string(d.path().join(f)).unwrap().trim().parse::<i32>().unwrap();
        assert!(gone(pid("own-group")) && gone(pid("undumpable")), "left behind");
    }

    /// The refusal's wait for what was sent is one deadline: a peer that keeps sending is cut off.
    #[test]
    fn a_refused_peer_is_cut_off_in_time() {
        let (_a, sock, _d) = agent("echo never", me() + 1);
        let mut c = UnixStream::connect(&sock).unwrap();
        let t = Instant::now();
        let w = c.try_clone().unwrap();
        std::thread::spawn(move || {
            let mut w = w;
            while writeln!(w, "{{\"type\":\"status\"}}").is_ok() {
                std::thread::sleep(Duration::from_millis(100));
            }
        });
        c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut all = String::new();
        let _ = c.read_to_string(&mut all);
        assert!(all.contains("answers alice alone"), "{all}");
        assert!(t.elapsed() < Duration::from_secs(4), "closed after {:?}", t.elapsed());
    }

    fn text(r: &mut impl BufRead) -> String {
        match read_line(r).unwrap() {
            Line::Text(l) => l,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn whose_uid() {
        let passwd = "root:x:0:0::/root:/bin/sh\nalice:x:1000:100::/home/alice:/bin/sh\ntorq-agent-alice:x:990:990::/var/lib/torq-agent-alice:/sbin/nologin\n";
        assert_eq!((uid_of(passwd, "alice"), uid_of(passwd, "torq-agent-alice"), uid_of(passwd, "bob")), (Some(1000), Some(990), None));
        assert!(super::agent(&["Alice".into()]).is_err() && super::agent(&[]).is_err() && super::agent(&["a;b".into()]).is_err());
    }

    /// Never a process outside the person's agent's own unit: here (a test, in no such unit), no sweep.
    #[test]
    fn the_sweep_is_the_units_alone() {
        assert_eq!(unit_procs("/torq-agent@alice.service"), None);
    }
}
