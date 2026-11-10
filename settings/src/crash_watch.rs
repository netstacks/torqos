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
use torq::agent::crash::{self, Crash};
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
    loop {
        match crash::watch_command().stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).spawn() {
            Ok(mut journal) => {
                if let Some(out) = journal.stdout.take() {
                    for line in std::io::BufReader::new(out).lines() {
                        let Ok(line) = line else { break };
                        if let Some(c) = crash::crash_of(&line, uid) {
                            offer(c);
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

/// The notification, on a thread of its own that waits for it to be clicked or closed. Only to a person who
/// has an agent here: to anyone else it would offer nothing.
fn offer(c: Crash) {
    if Client::mine().is_err() {
        return;
    }
    eprintln!("torq-crash-watch: offered: {} ({}, pid {})", c.program, c.signal, c.pid);
    std::thread::spawn(move || {
        if let Err(e) = notify(&c) {
            eprintln!("torq-crash-watch: the notification: {e}");
        }
    });
}

fn notify(c: &Crash) -> zbus::Result<()> {
    let (summary, body) = c.notification();
    let connection = zbus::blocking::Connection::session()?;
    let bus = zbus::blocking::Proxy::new(&connection, "org.freedesktop.Notifications", "/org/freedesktop/Notifications", "org.freedesktop.Notifications")?;
    // Only the notification daemon's own signals: the proxy listens to the name's owner.
    let signals = bus.receive_all_signals()?;
    let hints = std::collections::HashMap::<&str, zbus::zvariant::Value>::new();
    let id: u32 = bus.call("Notify", &("TorqOS", 0u32, "torqos", summary.as_str(), body.as_str(), vec!["default", "Ask the agent about this crash", ASK, "Ask the agent about this crash"], hints, 0i32))?;
    for signal in signals {
        let header = signal.header();
        let member = header.member().map(|m| m.to_string()).unwrap_or_default();
        if member == "ActionInvoked" {
            let (of, _action): (u32, String) = signal.body().deserialize()?;
            if of == id {
                // The person said so: only now is the crash read, and attached.
                eprintln!("torq-crash-watch: asked about pid {}", c.pid);
                if let Err(e) = handoff::open_window(&c.handoff(c.info().as_deref())) {
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
    }
    Ok(())
}
