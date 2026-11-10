//! `torq-crash-watch`: a crash of the person's, offered to their agent only when they say so (ADR-051 4,
//! step 4). A user service of the person's own (`torq-crash-watch.service`, with their graphical session):
//! it follows the journal for systemd-coredump's entries, as the person, and keeps their own crashes only.
//! Each gives a notification, **Ask the agent about this crash**; nothing of the crash is read, and nothing
//! goes anywhere, until it is clicked. Then `coredumpctl info` (as the person) is read and attached, and the
//! agent's window opens with it: the person reads it and asks. No privilege; no secret; the logic is
//! torqcli's library (`torq::agent::crash`).

// No unsafe code and no dead code, ever: `forbid` cannot be undone by any `#[allow]` below it,
// so the compiler is the guard (docs/DECISIONS.md ADR-020).
#![forbid(unsafe_code)]
#![forbid(dead_code)]

use std::io::BufRead;
use std::os::unix::fs::MetadataExt;
use torq::agent::client::Client;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use torq::agent::crash::{self, Crash, Offers};
use torq::agent::handoff;

/// The notification's action that asks the agent.
const ASK: &str = "ask";

fn main() {
    // Whose crashes: this process's own uid (its /proc entry's owner).
    let uid = match std::fs::metadata("/proc/self") {
        Ok(m) => m.uid(),
        Err(e) => {
            eprintln!("torq-crash-watch: who am I: {e}");
            std::process::exit(1);
        }
    };
    let offers = Arc::new(Mutex::new(Offers::default()));
    loop {
        match crash::watch_command().stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).spawn() {
            Ok(mut journal) => {
                if let Some(out) = journal.stdout.take() {
                    for line in std::io::BufReader::new(out).lines() {
                        let Ok(line) = line else { break };
                        if let Some(c) = crash::crash_of(&line, uid) {
                            offer(c, uid, &offers);
                        }
                    }
                }
                let _ = journal.wait();
            }
            Err(e) => eprintln!("torq-crash-watch: following the journal: {e}"),
        }
        // The journal's follower ended (journald restarted): again, a moment later.
        std::thread::sleep(std::time::Duration::from_secs(5));
    }
}

/// The notification, on a thread of its own that waits for it to be clicked, closed or expire. Only to a
/// person who has an agent here (to anyone else it would offer nothing), and bounded (`Offers`).
fn offer(c: Crash, uid: u32, offers: &Arc<Mutex<Offers>>) {
    if Client::mine().is_err() {
        return;
    }
    let admitted = offers.lock().map(|mut o| o.admit(&c, Instant::now())).unwrap_or(false);
    if !admitted {
        eprintln!("torq-crash-watch: not offered again yet: {} ({}, pid {})", c.program, c.signal, c.pid);
        return;
    }
    eprintln!("torq-crash-watch: offered: {} ({}, pid {})", c.program, c.signal, c.pid);
    let offers = offers.clone();
    std::thread::spawn(move || {
        if let Err(e) = notify(&c, uid) {
            eprintln!("torq-crash-watch: the notification: {e}");
        }
        if let Ok(mut o) = offers.lock() {
            o.closed();
        }
    });
}

fn notify(c: &Crash, uid: u32) -> zbus::Result<()> {
    let (summary, body) = c.notification();
    let connection = zbus::blocking::Connection::session()?;
    let bus = zbus::blocking::Proxy::new(&connection, "org.freedesktop.Notifications", "/org/freedesktop/Notifications", "org.freedesktop.Notifications")?;
    // Only the notification daemon's own signals: the proxy listens to the name's owner.
    let signals = bus.receive_all_signals()?;
    let hints = std::collections::HashMap::<&str, zbus::zvariant::Value>::new();
    let expires = i32::try_from(crash::EXPIRES.as_millis()).unwrap_or(i32::MAX);
    let id: u32 = bus.call("Notify", &("TorqOS", 0u32, "torqos", summary.as_str(), body.as_str(), vec![ASK, "Ask the agent about this crash"], hints, expires))?;
    let deadline = Instant::now() + crash::EXPIRES + std::time::Duration::from_secs(30);
    for signal in signals {
        let header = signal.header();
        let member = header.member().map(|m| m.to_string()).unwrap_or_default();
        if member == "ActionInvoked" {
            let (of, action): (u32, String) = signal.body().deserialize()?;
            // Its own action on its own notification: nothing else asks.
            if of == id && action == ASK {
                // The person said so: only now is the crash read, and attached.
                eprintln!("torq-crash-watch: asked about pid {}", c.pid);
                if let Err(e) = handoff::open_window(&c.handoff(c.info(uid).as_deref())) {
                    eprintln!("torq-crash-watch: opening the agent's window: {e:#}");
                }
                break;
            }
        } else if member == "NotificationClosed" {
            let (of, _why): (u32, u32) = signal.body().deserialize()?;
            if of == id {
                break;
            }
        }
        // A daemon that never says it closed: given up on after its expiry (checked as signals come).
        if Instant::now() > deadline {
            break;
        }
    }
    Ok(())
}
