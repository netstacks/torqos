//! `torq-broker serve`: one request on the socket (ADR-040, `torq_broker::socket`).
//!
//! Its standard input and output are the connection (systemd, `Accept=yes`). It learns who is at
//! the other end from the kernel, reads the request, decides what that account may do, checks a
//! password where one is needed, and then runs the broker — this same program, as root — with the
//! request's arguments and the identity it decided. What the broker prints comes back as frames;
//! a caller that hangs up stops nothing: a change already started finishes and is ledgered.

use anyhow::{bail, Context, Result};
use serde_json::json;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use torq_broker::config::{self, Settings};
use torq_broker::ledger::Ledger;
use torq_broker::policy::Tier;
use torq_broker::socket::{self, Caller, Change, Frame, Gate, Request};

/// The connection's output: a frame per line. Written while anyone listens; once the caller has
/// gone, nothing more is sent, and nothing stops because of it.
#[derive(Clone)]
struct Reply(Arc<Mutex<Option<std::io::Stdout>>>);

impl Reply {
    fn send(&self, f: &Frame) {
        let Ok(mut out) = self.0.lock() else { return };
        let gone = match out.as_mut() {
            Some(o) => serde_json::to_string(f).map_or(true, |line| writeln!(o, "{line}").and_then(|_| o.flush()).is_err()),
            None => false,
        };
        if gone {
            *out = None;
        }
    }
}

/// Serve the one request; the exit code it ended with (also sent, last).
pub fn serve() -> i32 {
    let reply = Reply(Arc::new(Mutex::new(Some(std::io::stdout()))));
    let code = match answer(&reply) {
        Ok(code) => code,
        Err(Refusal { json, code, why }) => {
            // A caller that asked for JSON reads its error as JSON (`{"error": …}`), as the broker
            // writes it; a person reads text.
            if json {
                reply.send(&Frame::Out(format!("{}\n", json!({ "error": why, "password": code == socket::NEED_PASSWORD }))));
            } else {
                reply.send(&Frame::Err(format!("Error: {why}\n")));
            }
            code
        }
    };
    reply.send(&Frame::Exit(code));
    code
}

struct Refusal {
    json: bool,
    code: i32,
    why: String,
}

fn answer(reply: &Reply) -> std::result::Result<i32, Refusal> {
    let refuse = |json: bool, code: i32| move |e: anyhow::Error| Refusal { json, code, why: format!("{e:#}") };
    if !config::running_as_root() {
        return Err(refuse(false, 1)(anyhow::anyhow!("serve runs as root, from torq-broker.socket")));
    }
    let uid = rustix::net::sockopt::socket_peercred(std::io::stdin())
        .context("this is not a connection to the broker's socket (no SO_PEERCRED)")
        .map_err(refuse(false, 1))?
        .uid
        .as_raw();
    // A caller that sends nothing, or stops reading, holds nothing for long (the audit of ADR-040,
    // its 3 and 4): the request must come within the time, and a reply no one reads is dropped —
    // the broker's own output is still read, so a change never waits on its caller.
    let limit = Some(std::time::Duration::from_secs(socket::TIMEOUT_SECONDS));
    rustix::net::sockopt::set_socket_timeout(std::io::stdin(), rustix::net::sockopt::Timeout::Recv, limit)
        .and_then(|()| rustix::net::sockopt::set_socket_timeout(std::io::stdout(), rustix::net::sockopt::Timeout::Send, limit))
        .context("setting the connection's time limits")
        .map_err(refuse(false, 1))?;
    let req = read_request().map_err(refuse(false, 2))?;
    let json = req.args.iter().any(|a| a == "--json");
    let refused = refuse(json, 1);
    if uid == 0 {
        return Err(refused(anyhow::anyhow!("root runs torq-broker itself, not through the socket")));
    }
    let read = |p: &str| std::fs::read_to_string(p).with_context(|| format!("reading {p}"));
    let who = socket::caller(uid, &read("/etc/passwd").map_err(&refused)?, &read("/etc/group").map_err(&refused)?).map_err(&refused)?;
    socket::check_args(&req.args).map_err(&refused)?;
    let settings = Settings::read(Path::new(config::FIXED), false).map_err(&refused)?.context("no /etc/torq/broker.json").map_err(&refused)?;

    // The request's files, private to root, gone when this is.
    let scratch = Scratch::new(&settings.state_dir).map_err(&refused)?;
    let diff = req.diff.as_deref().map(|d| scratch.write("change.diff", d)).transpose().map_err(&refused)?;
    let hash = req.hash.as_deref().map(|h| scratch.write("hash", h)).transpose().map_err(&refused)?;
    let cmd = req.args.iter().find(|a| !a.starts_with('-')).cloned().unwrap_or_default();
    let diff_s = diff.as_ref().map(|p| p.to_string_lossy().into_owned());
    let hash_s = hash.as_ref().map(|p| p.to_string_lossy().into_owned());
    let args = socket::with_identity(&req.args, &cmd, &who, diff_s.as_deref(), hash_s.as_deref()).map_err(&refused)?;

    // The arguments as the broker will read them: what they name, for the gate.
    let m = crate::command_line()
        .try_get_matches_from(std::iter::once("torq-broker".to_string()).chain(args.iter().cloned()))
        .map_err(|e| Refusal { json, code: 2, why: e.render().to_string().trim().to_string() })?;
    let (name, sub) = m.subcommand().ok_or_else(|| refused(anyhow::anyhow!("no command")))?;
    if name != cmd {
        return Err(refused(anyhow::anyhow!("the command is {name}, not {cmd}")));
    }
    let seq = sub.try_get_one::<u64>("seq").ok().flatten().copied();
    let user = sub.try_get_one::<String>("user").ok().flatten().cloned();
    let change = match (name, seq) {
        ("apply" | "revert", Some(seq)) => change(&settings.state_dir, seq, if name == "apply" { "proposal" } else { "applied" }),
        _ => None,
    };
    // A commit confirmed holds everyone else's changes back while it waits: its own case.
    let confirmed = name == "apply" && sub.try_get_one::<u32>("confirm").ok().flatten().is_some();

    match socket::gate(&who, if confirmed { "apply-confirmed" } else { name }, change.as_ref(), user.as_deref()) {
        Gate::Open => {}
        Gate::Refused(why) => return Err(refused(anyhow::anyhow!(why))),
        Gate::Password => match req.password.as_deref() {
            None => return Err(Refusal { json, code: socket::NEED_PASSWORD, why: format!("this needs {}'s password", who.name) }),
            Some(p) => {
                // One check at a time per account: guesses sent at once wait their turn, and each
                // wrong one costs PAM's delay before the next is tried (its 1).
                let _turn = one_at_a_time(who.uid).map_err(&refused)?;
                if !socket::password_ok(&who.name, p) {
                    return Err(Refusal { json, code: socket::NEED_PASSWORD, why: format!("that is not {}'s password", who.name) });
                }
            }
        },
    }
    drop(req);
    Ok(run(reply, &who, &args))
}

/// The turn to check `uid`'s password: an exclusive lock on its file in faillock's directory
/// (root's alone), held until dropped.
fn one_at_a_time(uid: u32) -> Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let p = Path::new(socket::FAILLOCK_DIR).join(format!(".turn-{uid}"));
    let f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(&p)
        .with_context(|| format!("opening {}", p.display()))?;
    f.lock().context("waiting for the turn to check the password")?;
    Ok(f)
}

/// The change `seq` is, if it is a `kind` entry: its tier and the files it touches.
fn change(state_dir: &Path, seq: u64, kind: &str) -> Option<Change> {
    let e = Ledger::open(state_dir.join("ledger.jsonl")).get(seq).ok()?;
    if e.body.kind != kind {
        return None;
    }
    let tier: Tier = serde_json::from_value(json!(e.body.tier)).ok()?;
    let files = e.body.data["files"].as_array()?.iter().filter_map(|f| f.as_str().map(String::from)).collect();
    Some(Change { tier, files })
}

/// One JSON line, no longer than the limit.
fn read_request() -> Result<Request> {
    let mut line = String::new();
    BufReader::new(std::io::stdin().take(socket::MAX_REQUEST)).read_line(&mut line).context("reading the request")?;
    if !line.ends_with('\n') {
        bail!("the request is one JSON line, at most {} bytes", socket::MAX_REQUEST);
    }
    serde_json::from_str(&line).context("the request is not one the broker reads")
}

/// The broker itself, as root, with `args`: its output to the caller as it comes, its exit code.
fn run(reply: &Reply, who: &Caller, args: &[String]) -> i32 {
    let child = Command::new("/proc/self/exe")
        .args(args)
        // Nothing of serve's environment: the broker reads its settings from broker.json.
        .env_clear()
        .env("TORQ_VIA", "socket")
        // Who asked, as the kernel said: in every entry, inside what the hash covers (its 6).
        .env("TORQ_CALLER", &who.name)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            reply.send(&Frame::Err(format!("Error: running the broker for {}: {e}\n", who.name)));
            return 1;
        }
    };
    let forward = |from: Option<Box<dyn Read + Send>>, frame: fn(String) -> Frame| {
        let reply = reply.clone();
        std::thread::spawn(move || {
            let Some(from) = from else { return };
            let mut r = BufReader::new(from);
            let mut line = Vec::new();
            while r.read_until(b'\n', &mut line).is_ok_and(|n| n > 0) {
                reply.send(&frame(String::from_utf8_lossy(&line).into_owned()));
                line.clear();
            }
        })
    };
    let out = forward(child.stdout.take().map(|o| Box::new(o) as Box<dyn Read + Send>), Frame::Out);
    let err = forward(child.stderr.take().map(|e| Box::new(e) as Box<dyn Read + Send>), Frame::Err);
    let _ = out.join();
    let _ = err.join();
    match child.wait() {
        Ok(s) => s.code().unwrap_or(1),
        Err(_) => 1,
    }
}

/// A directory for one request's files: in the broker's state directory (root's, 0700), its own
/// name, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(state_dir: &Path) -> Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        let dir = state_dir.join(format!("request-{}", std::process::id()));
        // A leftover of a server that died, with this pid: not ours to trust.
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::DirBuilder::new().mode(0o700).create(&dir).with_context(|| format!("creating {}", dir.display()))?;
        Ok(Scratch(dir))
    }

    fn write(&self, name: &str, text: &str) -> Result<PathBuf> {
        use std::os::unix::fs::OpenOptionsExt;
        let p = self.0.join(name);
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&p).with_context(|| format!("writing {}", p.display()))?;
        f.write_all(text.as_bytes())?;
        Ok(p)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
