//! torq-secrets (ADR-052): a program on this machine uses a secret in an HTTPS request, and never reads it.
//!
//! It listens on 127.0.0.1:7800. A request to `/system/<secret>/<host>/<path>` (a person's own:
//! `/user/<person>/<secret>/<host>/<path>`) is asked of the broker as a use: the broker reads who is connected
//! from the kernel itself, judges the use by the secret's rule, records the agent's first use in each session,
//! and hands back the value for this one request. The daemon makes it with curl — HTTPS to that host and no other,
//! the certificate checked, no redirect followed — and gives back the answer with any form of the value in it
//! replaced. It runs as its own account, `torq-secrets`, which can read no value itself; it keeps none.

// No unsafe code and no dead code, ever (docs/DECISIONS.md ADR-020).
#![forbid(unsafe_code)]
#![forbid(dead_code)]

pub mod agent;
pub mod http;
pub mod scrub;
pub mod upstream;

use anyhow::{bail, Context, Result};
use std::io::{BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use torq_wire::secrets::PORT;
use torq_wire::socket::{Frame, Request as BrokerRequest, OK, SOCKET};

/// The most requests at once: past them, a request is turned away rather than queued.
const AT_ONCE: usize = 16;
static RUNNING: AtomicUsize = AtomicUsize::new(0);

/// `torq-secrets serve`, or its usage.
pub fn run() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("serve") => match serve() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("torq-secrets: {e:#}");
                ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!("usage: torq-secrets serve   (the secrets daemon, run by its unit: ADR-052)");
            ExitCode::from(2)
        }
    }
}

fn serve() -> Result<()> {
    use rustix::process::{set_dumpable_behavior, setrlimit, DumpableBehavior, Resource, Rlimit};
    // It holds values, one request at a time: nothing of its memory is ever read out or written to a core.
    set_dumpable_behavior(DumpableBehavior::NotDumpable).context("protecting the values")?;
    setrlimit(Resource::Core, Rlimit { current: Some(0), maximum: Some(0) }).context("protecting the values")?;
    let listener = TcpListener::bind(("127.0.0.1", PORT)).with_context(|| format!("listening on 127.0.0.1:{PORT}"))?;
    eprintln!("torq-secrets: listening on 127.0.0.1:{PORT}");
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        if RUNNING.fetch_add(1, Ordering::SeqCst) >= AT_ONCE {
            RUNNING.fetch_sub(1, Ordering::SeqCst);
            let _ = stream.write_all(&http::refusal(503, "too many requests at once: try again"));
            continue;
        }
        // A thread that cannot be had (the unit's TasksMax) is a request turned away, not a daemon that stops.
        let spawned = std::thread::Builder::new().name("request".into()).spawn(move || {
            let answer = one(&stream);
            let _ = stream.write_all(&answer);
            RUNNING.fetch_sub(1, Ordering::SeqCst);
        });
        if spawned.is_err() {
            RUNNING.fetch_sub(1, Ordering::SeqCst);
        }
    }
    Ok(())
}

/// What the broker granted.
#[derive(serde::Deserialize)]
struct Granted {
    value: String,
    header: String,
    prefix: String,
    actor: String,
}

/// Ask the broker for a use (`secret-use`): what it granted, or why not. The value is in this answer, and in
/// nothing kept.
fn ask_broker(route: &http::Route, client_port: u16, session: Option<&str>, reason: Option<&str>) -> Result<std::result::Result<Granted, String>> {
    use_at_broker(use_args(route, client_port, session, reason), true)
}

/// What the broker is asked for a use: each of the program's own words (its session, its reason) one argument with
/// its flag, so that one beginning with `-` is still that word and never a flag of its own.
fn use_args(route: &http::Route, client_port: u16, session: Option<&str>, reason: Option<&str>) -> Vec<String> {
    let mut args = vec!["--json".to_string(), "secret-use".into(), "--name".into(), route.name.clone(), "--host".into(), route.host.clone()];
    args.extend(["--client-port".into(), client_port.to_string(), "--server-port".into(), PORT.to_string()]);
    if let Some(u) = &route.user {
        args.extend(["--user".into(), u.clone()]);
    }
    if let Some(s) = session {
        args.push(format!("--session={s}"));
    }
    // The agent's own words for what the use is for: the broker cleans them, and shows them as the agent's.
    if let Some(r) = reason {
        args.push(format!("--reason={r}"));
    }
    args
}

/// The most uses that wait here for a person at once, each a thread asking the broker again about once a second.
/// More are turned away: with the 16 that do not wait they stay well inside the unit's tasks.
const WAITING_AT_ONCE: usize = 24;
static WAITING: AtomicUsize = AtomicUsize::new(0);
/// The turn to ask the broker again about a use that waits: one at a time, so that of the few connections the
/// broker's socket gives one account, waiting uses never hold more than one, and a use that does not wait is
/// always served beside them.
static ASKING_AGAIN: std::sync::Mutex<()> = std::sync::Mutex::new(());
/// How often a waiting use asks the broker again.
const ASK_AGAIN_EVERY: std::time::Duration = std::time::Duration::from_secs(1);

/// A use that waits for a person, counted while it does. One that came in on a request's slot (`slot`) gives the
/// slot back for as long: a request that only waits must not keep those that would not wait from being served.
struct Waiting(bool);

impl Waiting {
    fn begin(slot: bool) -> Option<Waiting> {
        if WAITING.fetch_add(1, Ordering::SeqCst) >= WAITING_AT_ONCE {
            WAITING.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        if slot {
            RUNNING.fetch_sub(1, Ordering::SeqCst);
        }
        Some(Waiting(slot))
    }
}

impl Drop for Waiting {
    fn drop(&mut self) {
        WAITING.fetch_sub(1, Ordering::SeqCst);
        if self.0 {
            RUNNING.fetch_add(1, Ordering::SeqCst);
        }
    }
}

/// A use carried to its end. `ask(None)` asks the broker for it; while the broker answers that it waits for a
/// person (`{"waiting": n}`), `ask(Some(n))` asks again about that number, every `every`, for `at_most`: the
/// broker ends the wait itself well inside that. No connection is held between two askings. A try that cannot
/// reach the broker is made again: the use still waits there. `begin`: what counts the wait, or turns it away.
fn carry<G>(mut ask: impl FnMut(Option<u64>) -> Result<std::result::Result<serde_json::Value, String>>, mut begin: impl FnMut() -> Option<G>, every: std::time::Duration, at_most: std::time::Duration) -> Result<std::result::Result<serde_json::Value, String>> {
    let number = |v: &serde_json::Value| v.get("waiting").and_then(|n| n.as_u64());
    let n = match ask(None)? {
        Ok(v) => match number(&v) {
            Some(n) => n,
            None => return Ok(Ok(v)),
        },
        Err(why) => return Ok(Err(why)),
    };
    let Some(_counted) = begin() else { return Ok(Err("too many uses wait for a person at once: try again in a while".into())) };
    let until = std::time::Instant::now() + at_most;
    while std::time::Instant::now() < until {
        std::thread::sleep(every);
        match ask(Some(n)) {
            Ok(Ok(v)) if number(&v) == Some(n) => {}
            Ok(answer) => return Ok(answer),
            Err(_) => {}
        }
    }
    Ok(Err("no one answered in time".into()))
}

/// `args`, a `secret-use`, asked of the broker and carried to its end (`carry`): what it granted, or why not.
fn use_at_broker<T: serde::de::DeserializeOwned>(args: Vec<String>, slot: bool) -> Result<std::result::Result<T, String>> {
    let ask = |waiting: Option<u64>| {
        let mut args = args.clone();
        let _turn = waiting.map(|n| {
            args.push(format!("--waiting={n}"));
            // A turn another thread died holding is still a turn.
            ASKING_AGAIN.lock().unwrap_or_else(|gone| gone.into_inner())
        });
        broker::<serde_json::Value>(args)
    };
    let at_most = std::time::Duration::from_secs(torq_wire::secrets::ASK_WAIT_SECONDS + BROKER_SECONDS);
    Ok(match carry(ask, || Waiting::begin(slot), ASK_AGAIN_EVERY, at_most)? {
        Ok(v) => Ok(serde_json::from_value(v).context("the broker's answer")?),
        Err(why) => Err(why),
    })
}

/// How long the broker has to answer one request. None waits for a person: a use that does is asked about again.
const BROKER_SECONDS: u64 = 30;

/// One request of the broker's (the daemon's account may ask only `secret-use` and `secret-agent`): its answer as
/// `T`, or why it refused.
fn broker<T: serde::de::DeserializeOwned>(args: Vec<String>) -> Result<std::result::Result<T, String>> {
    use std::io::BufRead;
    let mut s = std::os::unix::net::UnixStream::connect(SOCKET).with_context(|| format!("reaching the broker at {SOCKET}"))?;

    s.set_read_timeout(Some(std::time::Duration::from_secs(BROKER_SECONDS)))?;
    writeln!(s, "{}", serde_json::to_string(&BrokerRequest { args, ..Default::default() })?)?;
    let (mut out, mut code) = (String::new(), None);
    for line in BufReader::new(s).lines() {
        match serde_json::from_str::<Frame>(&line.context("reading the broker's answer")?) {
            Ok(Frame::Out(t)) => out.push_str(&t),
            Ok(Frame::Exit(c)) => code = Some(c),
            Ok(Frame::Err(_)) => {}
            Err(_) => break,
        }
    }
    match code {
        Some(OK) => Ok(Ok(serde_json::from_str(&out).context("the broker's answer")?)),
        Some(_) => {
            let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_default();
            Ok(Err(v["error"].as_str().unwrap_or("refused").to_string()))
        }
        None => bail!("the broker hung up"),
    }
}

/// An SSH use, for a socket's key (`agent.rs`): judged by the broker by the socket's grant; the value and the host's keys.
fn ask_ssh(key: &agent::Key, grant: &str, host: &str) -> Result<std::result::Result<(String, Vec<String>), String>> {
    #[derive(serde::Deserialize)]
    struct Use {
        value: String,
        known: Vec<String>,
    }
    let mut args = vec!["--json".to_string(), "secret-use".into(), "--ssh".into(), "--name".into(), key.name.clone(), "--host".into(), host.into(), "--grant".into(), grant.into()];
    if let Some(u) = key.owner.get("person").and_then(|p| p.as_str()) {
        args.extend(["--user".into(), u.into()]);
    }
    Ok(use_at_broker::<Use>(args, false)?.map(|u| (u.value, u.known)))
}

/// `POST /ssh-agent`: an agent socket for the one connected, granted by the broker (who, read from the kernel).
fn agent_socket(req: &http::Request, client_port: u16) -> Vec<u8> {
    let mut args = vec!["--json".to_string(), "secret-agent".into(), "--client-port".into(), client_port.to_string(), "--server-port".into(), PORT.to_string()];
    if let Some(s) = req.header(torq_wire::secrets::SESSION_HEADER) {
        args.push(format!("--session={s}"));
    }
    let g: agent::Granted = match broker(args) {
        Ok(Ok(g)) => g,
        Ok(Err(why)) => return http::refusal(403, &why),
        Err(e) => return http::refusal(503, &format!("{e:#}")),
    };
    let (actor, keys) = (g.actor.clone(), g.keys.iter().map(|k| k.name.clone()).collect::<Vec<_>>());
    match agent::open(g, ask_ssh) {
        Ok(path) => {
            log(&format!("an agent socket for {actor}, with {} key(s)", keys.len()));
            let body = format!("{}\n", serde_json::json!({ "socket": path, "keys": keys, "seconds": 3600 }));
            http::response(200, "OK", &[("Content-Type".into(), "application/json".into())], body.as_bytes())
        }
        Err(e) => http::refusal(503, &format!("{e:#}")),
    }
}

/// How long a program has to send its whole request: one deadline, not a time between bytes, so a connection
/// that sends a byte at a time cannot hold its place for ever.
const REQUEST_SECONDS: u64 = 15;

/// A connection read against one deadline.
struct Deadline<'a> {
    stream: &'a TcpStream,
    until: std::time::Instant,
}

impl std::io::Read for Deadline<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = self.until.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "the request took too long"));
        }
        self.stream.set_read_timeout(Some(left))?;
        (&mut &*self.stream).read(buf)
    }
}

/// One connection's answer.
fn one(stream: &TcpStream) -> Vec<u8> {
    let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(upstream::SECONDS as u64)));
    let Ok(peer) = stream.peer_addr() else { return http::refusal(400, "no peer") };
    let until = std::time::Instant::now() + std::time::Duration::from_secs(REQUEST_SECONDS);
    let req = match http::read(&mut BufReader::new(Deadline { stream, until })) {
        Ok(r) => r,
        Err(why) => return http::refusal(400, &why),
    };
    if let Err(why) = http::not_a_browser(&req) {
        log(&format!("refused a browser's request: {why}"));
        return http::refusal(403, &why);
    }
    // An agent socket for SSH (agent.rs): asked as a use is, so that the broker reads who from the kernel.
    if req.target == "/ssh-agent" {
        if req.method != "POST" {
            return http::refusal(400, "an agent socket is asked for with POST /ssh-agent");
        }
        return agent_socket(&req, peer.port());
    }
    let route = match http::route(&req.target) {
        Ok(r) => r,
        Err(why) => return http::refusal(400, &why),
    };
    let granted = match ask_broker(&route, peer.port(), req.header(torq_wire::secrets::SESSION_HEADER), req.header(torq_wire::secrets::REASON_HEADER)) {
        Ok(Ok(g)) => g,
        Ok(Err(why)) => {
            log(&format!("refused: {} {} for {}: {why}", req.method, route.host, whose(&route)));
            return http::refusal(403, &why);
        }
        Err(e) => return http::refusal(503, &format!("{e:#}")),
    };
    // A value that could end a header line would add headers of its own: never sent.
    if granted.value.chars().any(|c| c == '\r' || c == '\n' || c == '\0') || granted.prefix.chars().any(|c| c.is_control()) {
        return http::refusal(502, &format!("{} holds a line break: it cannot go in a header", whose(&route)));
    }
    let headers = http::forwarded(&req, &granted.header);
    let header_value = format!("{}{}", granted.prefix, granted.value);
    let forms = scrub::forms(&granted.value);
    let sent = upstream::send(&route, &req, &headers, (&granted.header, &header_value));
    drop(header_value);
    let clean = |s: &str| -> (String, bool) {
        let (b, f) = scrub::scrub(s.as_bytes(), &forms);
        (String::from_utf8_lossy(&b).into_owned(), f)
    };
    let path = route.rest.split('?').next().unwrap_or("/");
    let answer = match sent {
        Err(e) => {
            log(&format!("{} used {} for {} https://{}{path}: failed", granted.actor, whose(&route), req.method, route.host));
            // Scrubbed whole, then cut: a value across the cut is not left half there.
            let why: String = clean(&format!("{e:#}")).0.chars().filter(|c| !c.is_control()).take(300).collect();
            return http::refusal(502, &why);
        }
        Ok(a) => a,
    };
    if answer.headers.iter().any(|(k, v)| k.eq_ignore_ascii_case("content-encoding") && !v.eq_ignore_ascii_case("identity")) {
        return http::refusal(502, "the host answered compressed, though asked not to: a compressed answer cannot be checked for the secret");
    }
    // HTTP's own between hops, and what would let a web page read the answer (CORS): never passed on. A HEAD's
    // length is the host's, as HEAD says what a GET would be; any other's is the daemon's own.
    const HOP: &[&str] = &["connection", "keep-alive", "transfer-encoding", "content-length", "trailer", "upgrade", "proxy-authenticate"];
    let head = req.method == "HEAD";
    let mut found = false;
    let mut out_headers = Vec::new();
    for (k, v) in &answer.headers {
        let low = k.to_ascii_lowercase();
        let keep_length = head && low == "content-length";
        if (HOP.contains(&low.as_str()) && !keep_length) || low.starts_with("access-control-") || !http::token_ok(k) || v.chars().any(|c| c.is_control() && c != '\t') {
            continue;
        }
        let (k, f1) = clean(k);
        let (v, f2) = clean(v);
        found |= f1 || f2;
        out_headers.push((k, v));
    }
    let (body, f) = scrub::scrub(&answer.body, &forms);
    found |= f;
    let (reason, f) = clean(&answer.reason);
    found |= f;
    let reason: String = reason.chars().filter(|c| !c.is_control()).collect();
    log(&format!(
        "{} used {} for {} https://{}{path}: {}{}",
        granted.actor,
        whose(&route),
        req.method,
        route.host,
        answer.status,
        if found { " (the host sent the secret back: replaced)" } else { "" }
    ));
    if head {
        return http::head_response(answer.status, &reason, &out_headers);
    }
    http::response(answer.status, &reason, &out_headers, &body)
}

fn whose(r: &http::Route) -> String {
    match &r.user {
        None => format!("the system's secret {}", r.name),
        Some(u) => format!("{u}'s secret {}", r.name),
    }
}

/// Every use, to the journal (the daemon's log, ADR-052 3): who, which secret, what, and how it went. Never a value.
pub(crate) fn log(line: &str) {
    eprintln!("{}", line.chars().filter(|c| !c.is_control()).collect::<String>());
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;

    /// A use that waits is asked about again under its number until the broker says how it ended; what the broker
    /// says then is the answer, a value or a refusal. A try that does not reach the broker is made again.
    #[test]
    fn a_waiting_use_is_asked_about_again_until_it_ends() {
        let script = |answers: Vec<Result<std::result::Result<serde_json::Value, String>>>| {
            let mut answers = answers.into_iter();
            let mut asked = Vec::new();
            let out = carry(
                |n| {
                    asked.push(n);
                    answers.next().unwrap_or_else(|| Ok(Ok(json!({ "waiting": 7 }))))
                },
                || Some(()),
                Duration::from_millis(1),
                Duration::from_millis(200),
            );
            (out.unwrap(), asked)
        };
        let waiting = || Ok(Ok(json!({ "waiting": 7 })));
        let (out, asked) = script(vec![Ok(Ok(json!({ "value": "v" })))]);
        assert_eq!((out, asked), (Ok(json!({ "value": "v" })), vec![None]), "a use that does not wait is asked once");
        let (out, asked) = script(vec![waiting(), waiting(), Err(anyhow::anyhow!("the broker is busy")), waiting(), Ok(Ok(json!({ "value": "v" })))]);
        assert_eq!((out, asked), (Ok(json!({ "value": "v" })), vec![None, Some(7), Some(7), Some(7), Some(7)]));
        let (out, _) = script(vec![waiting(), Ok(Err("refused: sam denied this use".into()))]);
        assert_eq!(out, Err("refused: sam denied this use".into()));
        let (out, _) = script(vec![Ok(Err("refused: sam may not use it".into()))]);
        assert_eq!(out, Err("refused: sam may not use it".into()));
        // The broker ends a wait itself; should it never, the daemon does not ask for ever.
        let t = std::time::Instant::now();
        let (out, asked) = script(vec![]);
        assert_eq!(out, Err("no one answered in time".into()));
        assert!(asked.len() > 2 && asked[1..].iter().all(|n| *n == Some(7)) && t.elapsed() < Duration::from_secs(5), "{asked:?}");
        // A use that cannot be counted among the waiting is turned away, and not asked about again.
        let mut asked = 0;
        let out = carry(
            |_| {
                asked += 1;
                waiting()
            },
            || None::<()>,
            Duration::from_millis(1),
            Duration::from_millis(50),
        );
        assert!(out.unwrap().unwrap_err().contains("too many uses wait") && asked == 1);
    }

    /// Waiting uses are counted apart from the requests being served, and give their request's slot back while
    /// they wait: a request that only waits keeps none that would not wait from being served. At most so many wait.
    #[test]
    fn a_waiting_use_gives_its_slot_back() {
        // Each waiting use came in on a slot of its own; two more requests are being served.
        RUNNING.store(WAITING_AT_ONCE + 2, Ordering::SeqCst);
        let held: Vec<Waiting> = (0..WAITING_AT_ONCE).map(|_| Waiting::begin(true).expect("room")).collect();
        assert_eq!(RUNNING.load(Ordering::SeqCst), 2, "each gave its slot back");
        assert!(Waiting::begin(true).is_none() && Waiting::begin(false).is_none(), "and no more wait");
        drop(held);
        assert_eq!((RUNNING.load(Ordering::SeqCst), WAITING.load(Ordering::SeqCst)), (WAITING_AT_ONCE + 2, 0), "taken again when the wait is over");
        RUNNING.store(0, Ordering::SeqCst);
    }

    /// A program's own words go to the broker each as one argument with its flag: a reason or a session that
    /// begins with a dash is still that word, not a flag, and does not fail the use.
    #[test]
    fn a_reason_beginning_with_a_dash_is_one_argument() {
        let route = http::route("/user/dev/gh/api.example.test/x").unwrap();
        let args = use_args(&route, 4242, Some("--json"), Some("-rf / --user=root"));
        assert_eq!(args, ["--json", "secret-use", "--name", "gh", "--host", "api.example.test", "--client-port", "4242", "--server-port", "7800", "--user", "dev", "--session=--json", "--reason=-rf / --user=root"]);
        assert!(!use_args(&route, 4242, None, None).iter().any(|a| a.starts_with("--session") || a.starts_with("--reason")));
    }
}
