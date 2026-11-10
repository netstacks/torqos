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

/// One line of at most `MAX_LINE` bytes, without its newline; `None` at the end. A longer line is an error.
fn read_line(r: &mut impl BufRead) -> Result<Option<String>> {
    let mut buf = Vec::new();
    let n = r.by_ref().take(MAX_LINE as u64 + 1).read_until(b'\n', &mut buf)?;
    if n == 0 {
        return Ok(None);
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
    } else if buf.len() > MAX_LINE {
        bail!("a line longer than {MAX_LINE} bytes");
    }
    Ok(Some(String::from_utf8(buf).context("a line that is not UTF-8")?))
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

    /// Serve one connection: the person's, or no one's.
    fn serve(self: &Arc<Self>, conn: UnixStream, id: u64) -> Result<()> {
        let out = Out(Arc::new(Mutex::new(conn.try_clone()?)));
        if !peer_is(&conn, self.uid)? {
            out.send(&Event::Error { text: format!("this is {}'s agent: it answers {} alone", self.person, self.person) });
            return Ok(());
        }
        let mut r = BufReader::new(conn);
        loop {
            let line = match read_line(&mut r) {
                Ok(Some(l)) => l,
                Ok(None) => break,
                Err(e) => {
                    out.send(&Event::Error { text: format!("{e:#}") });
                    break;
                }
            };
            match serde_json::from_str::<Request>(&line) {
                Err(e) => out.send(&Event::Error { text: format!("not a request: {e}") }),
                Ok(Request::Ask { text, context }) => {
                    if let Err(e) = self.ask(text, context, &out, id) {
                        out.send(&Event::Error { text: format!("{e:#}") });
                        out.send(&Event::Done { exit: 1 });
                    }
                }
                Ok(Request::Stop) => {
                    self.stop();
                    out.send(&Event::Status { running: self.running().is_some() });
                }
                Ok(Request::Status) => out.send(&Event::Status { running: self.running().is_some() }),
                Ok(Request::Forget) => match self.running() {
                    Some(_) => out.send(&Event::Error { text: "a task is running: stop it first, or wait".into() }),
                    None => match self.memory.forget() {
                        Ok(()) => out.send(&Event::Forgotten),
                        Err(e) => out.send(&Event::Error { text: format!("{e:#}") }),
                    },
                },
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
        let task = Task { question: text.clone(), context, memory: self.memory.recent() };
        let mut slot = self.running.lock().unwrap_or_else(|p| p.into_inner());
        if slot.is_some() {
            bail!("a task is running already (one at a time): stop it, or wait");
        }
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
        std::thread::spawn(move || {
            let _ = writeln!(stdin, "{line}");
            drop(stdin);
            let (mut said, mut tainted) = (None, false);
            let mut r = BufReader::new(stdout);
            while let Ok(Some(l)) = read_line(&mut r) {
                let Some(e) = relayed(&l) else { continue };
                match &e {
                    Event::Say { text } => said = Some(text.trim().to_string()),
                    Event::Tainted { .. } => tainted = true,
                    _ => {}
                }
                out.send(&e);
            }
            let exit = match child.wait() {
                Ok(s) => s.code().unwrap_or_else(|| 128 + std::os::unix::process::ExitStatusExt::signal(&s).unwrap_or(0)),
                Err(_) => 1,
            };
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

    /// End the running task: its process group, then anything of it left in this service's own
    /// control group (a tool's program runs in a group of its own, ADR-042 5). Waits, a little, for
    /// it to be gone.
    fn stop(&self) {
        let Some(r) = self.running() else { return };
        let _ = rustix::process::kill_process_group(r.group, rustix::process::Signal::KILL);
        let unit = format!("/torq-agent@{}.service", self.person);
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.running().is_some() && Instant::now() < deadline {
            for pid in left_in_unit(&unit) {
                let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// The processes of this one's own user in this one's own control group, but itself — only when that
/// group is the person's agent's unit (`unit`, its path's end): never anything elsewhere.
fn left_in_unit(unit: &str) -> Vec<rustix::process::Pid> {
    use std::os::unix::fs::MetadataExt;
    let cgroup = |pid: &str| std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok().and_then(|c| c.lines().find_map(|l| l.strip_prefix("0::").map(String::from)));
    let Some(mine) = cgroup("self").filter(|c| c.ends_with(unit)) else { return vec![] };
    let (me, uid) = (std::process::id(), rustix::process::getuid().as_raw());
    let Ok(dir) = std::fs::read_dir("/proc") else { return vec![] };
    dir.flatten()
        .filter_map(|e| {
            let pid: u32 = e.file_name().to_str()?.parse().ok()?;
            (pid != me && e.metadata().ok()?.uid() == uid && cgroup(&pid.to_string())? == mine).then(|| rustix::process::Pid::from_raw(pid as i32)).flatten()
        })
        .collect()
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
        let a = Arc::new(Agent { person: "alice".into(), uid, harness: h, home: d.path().into(), memory: memory::Memory::new(d.path()), running: Mutex::new(None) });
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
        while let Ok(Some(l)) = read_line(&mut r) {
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
        assert_eq!(seen, vec![Event::Error { text: "this is alice's agent: it answers alice alone".into() }], "refused, and nothing run");
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
        let last: Event = serde_json::from_str(&read_line(&mut r).unwrap().unwrap()).unwrap();
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
        let e = |r: &mut BufReader<UnixStream>| serde_json::from_str::<Event>(&read_line(r).unwrap().unwrap()).unwrap();
        assert!(matches!(e(&mut r), Event::Error { text } if text.starts_with("not a request")));
        assert_eq!(e(&mut r), Event::Error { text: "ask what? (an empty question)".into() });
        assert_eq!(e(&mut r), Event::Done { exit: 1 }, "an ask always ends with done");
        assert_eq!(e(&mut r), Event::Forgotten);
    }

    #[test]
    fn a_line_too_long_is_refused() {
        let long = "x".repeat(MAX_LINE + 10);
        assert!(read_line(&mut std::io::Cursor::new(long.as_bytes())).is_err());
        assert_eq!(read_line(&mut std::io::Cursor::new(b"a\nb".as_slice())).unwrap(), Some("a".into()));
    }

    #[test]
    fn whose_uid() {
        let passwd = "root:x:0:0::/root:/bin/sh\nalice:x:1000:100::/home/alice:/bin/sh\ntorq-agent-alice:x:990:990::/var/lib/torq-agent-alice:/sbin/nologin\n";
        assert_eq!((uid_of(passwd, "alice"), uid_of(passwd, "torq-agent-alice"), uid_of(passwd, "bob")), (Some(1000), Some(990), None));
        assert!(super::agent(&["Alice".into()]).is_err() && super::agent(&[]).is_err() && super::agent(&["a;b".into()]).is_err());
    }

    /// Never a process outside the person's agent's own unit: here (a test, in no such unit), none.
    #[test]
    fn the_sweep_is_the_units_alone() {
        assert_eq!(left_in_unit("/torq-agent@alice.service"), vec![]);
    }
}
