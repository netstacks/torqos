//! A crash, offered to the agent only when the person says so (ADR-051 4, step 4). `torq-crash-watch`, a user
//! service of the person's own, follows the journal for systemd-coredump's entries (`journalctl -f`, as the
//! person: what they may read) and keeps only their own crashes: those journald's own fields say systemd-coredump
//! logged, as the person (`_SYSTEMD_UNIT`, `_SYSTEMD_SLICE`, `_UID`, `_TRANSPORT`), and whose `COREDUMP_UID`
//! — the sender's word — is the person too. Each gives a notification,
//! **Ask the agent about this crash**; nothing is read of the crash and nothing goes anywhere until it is
//! clicked. Then its summary — `coredumpctl info` as the person, matched by its pid, owner, unit and boot (program,
//! signal, backtrace), bounded —
//! opens the agent's window, attached: the person reads it and asks, and the session that reads it is tainted.

use super::handoff::Handoff;
use serde_json::Value;

/// systemd-coredump's journal message id (sd-messages.h, SD_MESSAGE_COREDUMP).
pub const COREDUMP_MESSAGE: &str = "fc2e22bc6ee647b6b90729ab34a250b1";
/// How long `coredumpctl info` may take.
const INFO_WAIT: std::time::Duration = std::time::Duration::from_secs(20);

/// One of the person's crashes, as the journal names it.
#[derive(Debug, Clone, PartialEq)]
pub struct Crash {
    pub pid: u32,
    pub program: String,
    pub signal: String,
    /// The systemd-coredump unit that logged it: one per crash on a boot, so with `boot` it names exactly this one.
    pub unit: String,
    /// The boot it was logged in (`_BOOT_ID`, journald's own): a unit's name can come again on another boot.
    pub boot: String,
}

/// A journal field as text (journalctl's JSON gives a string, or bytes as numbers for what is not UTF-8).
fn field(v: &Value, name: &str) -> Option<String> {
    match &v[name] {
        Value::String(s) => Some(s.clone()),
        Value::Array(bytes) => Some(String::from_utf8_lossy(&bytes.iter().filter_map(|b| b.as_u64().map(|b| b as u8)).collect::<Vec<u8>>()).into_owned()),
        _ => None,
    }
}

/// The slice every systemd-coredump unit runs in.
const COREDUMP_SLICE: &str = "system-systemd\\x2dcoredump.slice";

/// Whether the entry was written by systemd-coredump itself, by the fields journald sets (a sender cannot
/// write a field beginning with `_`): its unit, from the sender's control group, is a `systemd-coredump@`
/// unit in that unit's slice of the system's — where no person's process can be — and it ran as `uid`
/// (systemd-coredump drops to the crashed process's owner before it logs). `COREDUMP_UID` and the rest are
/// the sender's words, so anyone could write those (`logger --journald`): they count only after this.
fn from_coredump(v: &Value, uid: u32) -> Option<String> {
    let unit = field(v, "_SYSTEMD_UNIT")?;
    let ok = unit.starts_with("systemd-coredump@")
        && unit.ends_with(".service")
        && unit.bytes().all(|b| b.is_ascii_alphanumeric() || b"@-_.".contains(&b))
        && field(v, "_SYSTEMD_SLICE")? == COREDUMP_SLICE
        && field(v, "_UID")?.parse::<u32>().ok()? == uid
        && field(v, "_TRANSPORT")? == "journal";
    ok.then_some(unit)
}

/// The crash in one line of `journalctl -o json`, if it is a coredump of `uid`'s that systemd-coredump logged.
pub fn crash_of(line: &str, uid: u32) -> Option<Crash> {
    let v: Value = serde_json::from_str(line).ok()?;
    if field(&v, "MESSAGE_ID")? != COREDUMP_MESSAGE {
        return None;
    }
    let unit = from_coredump(&v, uid)?;
    if field(&v, "COREDUMP_UID")?.parse::<u32>().ok()? != uid {
        return None;
    }
    let program = field(&v, "COREDUMP_EXE").or_else(|| field(&v, "COREDUMP_COMM")).unwrap_or_else(|| "a program".into());
    let signal = field(&v, "COREDUMP_SIGNAL_NAME").or_else(|| field(&v, "COREDUMP_SIGNAL").map(|n| format!("signal {n}"))).unwrap_or_else(|| "a signal".into());
    let boot = field(&v, "_BOOT_ID").filter(|b| b.len() == 32 && b.bytes().all(|c| c.is_ascii_hexdigit()))?;
    Some(Crash { pid: field(&v, "COREDUMP_PID")?.parse().ok()?, program: crate::history::one_line(&program, 200), signal: crate::history::one_line(&signal, 40), unit, boot })
}

/// What follows the journal for new crashes: everything the person may read, from now on.
pub fn watch_command() -> std::process::Command {
    let mut c = std::process::Command::new(crate::app::system_program("journalctl"));
    c.args(["--follow", "--lines=0", "--output=json", &format!("MESSAGE_ID={COREDUMP_MESSAGE}")]);
    c
}

/// Text put in a notification's body: escaped, as the notification spec's markup reads it.
pub fn markup(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

impl Crash {
    /// The program's name, without its path.
    pub fn name(&self) -> &str {
        self.program.rsplit('/').next().unwrap_or(&self.program)
    }

    /// The notification: a summary of fixed words (it is under TorqOS's name: the review of the rebased branch, C12), and
    /// the body with the program's name and signal (escaped: the body may be read as markup). Nothing of the
    /// crash's contents is in it.
    pub fn notification(&self) -> (String, String) {
        ("A program of yours crashed".to_string(), format!("{} ({}, {}). Your agent can look at it with you: nothing is sent unless you ask.", markup(self.name()), markup(&self.program), markup(&self.signal)))
    }

    /// `coredumpctl info` as the person, by absolute path: this crash — its pid, its owner, the boot and the
    /// systemd-coredump unit that logged it, together (a pid alone may be an earlier crash's, reused; a unit's
    /// name another boot's). Matches, not the entry's cursor: coredumpctl takes none (KNOWN-GAPS 6zzn).
    pub fn info_command(&self, uid: u32) -> std::process::Command {
        let mut c = std::process::Command::new(crate::app::system_program("coredumpctl"));
        c.args(["info", "--no-pager", &format!("COREDUMP_PID={}", self.pid), &format!("COREDUMP_UID={uid}"), &format!("_SYSTEMD_UNIT={}", self.unit), &format!("_BOOT_ID={}", self.boot)]);
        c
    }

    /// Its summary, read now (the person said to): bounded in time and size.
    pub fn info(&self, uid: u32) -> Option<String> {
        super::setup::output_within(self.info_command(uid), INFO_WAIT)
    }

    /// What opens the agent's window: a fixed question (nothing of the crash in it), and the crash — its
    /// program, signal and summary, untrusted text — attached: it taints, and the person may remove it.
    pub fn handoff(&self, info: Option<&str>) -> Handoff {
        let context = match info {
            Some(i) => format!("{} crashed with {} (pid {}).\n\ncoredumpctl info:\n{i}", self.program, self.signal, self.pid),
            None => format!("{} crashed with {} (pid {}). Its summary could not be read.", self.program, self.signal, self.pid),
        };
        Handoff::new(CRASH_QUESTION, Some(&context))
    }
}

/// What a notification daemon's signal says of the crash offer `id`: asked (its own action, on it), the activation
/// token for that ask (sent just before it, by the notification spec), closed, or nothing of it (another
/// notification's, another action, another signal).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Heard {
    Ask,
    Token,
    Closed,
    Nothing,
}

/// The signal `member` about notification `of` (with its action for `ActionInvoked`, its token for
/// `ActivationToken`), as it bears on offer `id`.
pub fn heard(member: &str, id: u32, of: u32, action: Option<&str>) -> Heard {
    match (member, action) {
        ("ActionInvoked", Some(a)) if of == id && a == ASK_ACTION => Heard::Ask,
        ("ActivationToken", Some(_)) if of == id => Heard::Token,
        ("NotificationClosed", _) if of == id => Heard::Closed,
        _ => Heard::Nothing,
    }
}

/// The notification's action that asks the agent: only it asks (no default action: a click on the body asks nothing).
pub const ASK_ACTION: &str = "ask";

/// When the watcher gives up on an offer sent at `sent` whatever the daemon says: its expiry and a margin. Waited
/// for with a timeout, not only as signals come (step 5's second review): a daemon that never says it closed one
/// leaves nothing waiting past this.
pub fn give_up_at(sent: std::time::Instant) -> std::time::Instant {
    sent + EXPIRES + std::time::Duration::from_secs(30)
}

/// The question a crash opens the window with: the same for every crash.
pub const CRASH_QUESTION: &str = "Why did this program crash, and what can I do about it?";

/// How long one program's crashes are one offer: a program crashing over and over gives one notification.
pub const SAME_PROGRAM: std::time::Duration = std::time::Duration::from_secs(10 * 60);
/// At most this many crash notifications open at once.
pub const MAX_OPEN: usize = 3;
/// At most this many offered in a minute, whatever crashes.
pub const PER_MINUTE: usize = 5;
/// How long a crash notification stays: then it expires, and nothing of it is left waiting.
pub const EXPIRES: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// Which crashes are offered: bounded, so a crashing loop never floods the person with notifications (nor
/// the watcher with threads waiting on them).
#[derive(Debug, Default)]
pub struct Offers {
    /// Each program's last offer.
    last: std::collections::HashMap<String, std::time::Instant>,
    /// The offers of the last minute.
    recent: std::collections::VecDeque<std::time::Instant>,
    open: usize,
}

impl Offers {
    /// Whether to offer `c` now; if so, it is counted open until `closed`.
    pub fn admit(&mut self, c: &Crash, now: std::time::Instant) -> bool {
        while self.recent.front().is_some_and(|t| now.duration_since(*t) >= std::time::Duration::from_secs(60)) {
            self.recent.pop_front();
        }
        self.last.retain(|_, t| now.duration_since(*t) < SAME_PROGRAM);
        if self.open >= MAX_OPEN || self.recent.len() >= PER_MINUTE || self.last.contains_key(&c.program) {
            return false;
        }
        self.last.insert(c.program.clone(), now);
        self.recent.push_back(now);
        self.open += 1;
        true
    }

    /// An offer's notification is gone (clicked, closed or expired).
    pub fn closed(&mut self) {
        self.open = self.open.saturating_sub(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// An entry as systemd-coredump writes it (its trusted fields as journald sets them), with `with` over it.
    fn entry(uid: u32, pid: &str, unit: &str, with: Value) -> String {
        let mut v = json!({
            "MESSAGE_ID": COREDUMP_MESSAGE, "COREDUMP_UID": uid.to_string(), "COREDUMP_PID": pid,
            "COREDUMP_EXE": "/run/current-system/sw/bin/bash", "COREDUMP_SIGNAL_NAME": "SIGSEGV",
            "_UID": uid.to_string(), "_TRANSPORT": "journal", "_COMM": "systemd-coredum",
            "_SYSTEMD_UNIT": unit, "_SYSTEMD_SLICE": COREDUMP_SLICE, "_BOOT_ID": BOOT,
        });
        if let (Value::Object(base), Value::Object(over)) = (&mut v, with) {
            base.extend(over);
        }
        v.to_string()
    }

    const UNIT: &str = "systemd-coredump@16-61455-4242_4242-0.service";
    const BOOT: &str = "0123456789abcdef0123456789abcdef";

    fn line(uid: u32) -> String {
        entry(uid, "4242", UNIT, json!({}))
    }

    #[test]
    fn only_ones_own_crashes() {
        assert_eq!(crash_of(&line(1000), 1000), Some(Crash { pid: 4242, program: "/run/current-system/sw/bin/bash".into(), signal: "SIGSEGV".into(), unit: UNIT.into(), boot: BOOT.into() }));
        assert_eq!(crash_of(&line(1001), 1000), None, "someone else's crash");
        assert_eq!(crash_of(&line(1000).replace(COREDUMP_MESSAGE, "0123"), 1000), None, "not a coredump");
        assert_eq!(crash_of("junk", 1000), None);
        let bytes = entry(1000, "7", UNIT, json!({ "COREDUMP_EXE": null, "COREDUMP_SIGNAL_NAME": null, "COREDUMP_COMM": [115, 104], "COREDUMP_SIGNAL": "11" }));
        assert_eq!(crash_of(&bytes, 1000), Some(Crash { pid: 7, program: "sh".into(), signal: "signal 11".into(), unit: UNIT.into(), boot: BOOT.into() }));
    }

    /// What anyone can write to the journal (`logger --journald`, sd_journal_send) is not a crash: only what
    /// systemd-coredump's own unit logged, as the person.
    #[test]
    fn a_forged_entry_is_no_crash() {
        let forged = [
            // A person's own program, in their session: its unit and slice are journald's word, not theirs.
            entry(1000, "4242", "app-forge.scope", json!({ "_SYSTEMD_SLICE": "app.slice" })),
            // A unit named like it, in another slice.
            entry(1000, "4242", UNIT, json!({ "_SYSTEMD_SLICE": "user-1000.slice" })),
            // Logged by another uid (root forging the person's crash, or another person's).
            entry(1000, "4242", UNIT, json!({ "_UID": "0" })),
            entry(1000, "4242", UNIT, json!({ "_UID": "1001" })),
            // No trusted fields at all.
            json!({ "MESSAGE_ID": COREDUMP_MESSAGE, "COREDUMP_UID": "1000", "COREDUMP_PID": "4242" }).to_string(),
            // Each trusted check on its own: another transport (syslog, stdout), a unit that is not a service, no boot.
            entry(1000, "4242", UNIT, json!({ "_TRANSPORT": "syslog" })),
            entry(1000, "4242", UNIT, json!({ "_TRANSPORT": "stdout" })),
            entry(1000, "4242", "systemd-coredump@16-61455-4242_4242-0.socket", json!({})),
            entry(1000, "4242", "systemd-coredump@16-61455-4242_4242-0.service.d", json!({})),
            entry(1000, "4242", UNIT, json!({ "_BOOT_ID": null })),
            // Every trusted field right, its COREDUMP_UID another's: the sender's word, which must be the person too.
            entry(1000, "4242", UNIT, json!({ "COREDUMP_UID": "1001" })),
            // A unit name that is not one.
            entry(1000, "4242", "systemd-coredump@x;rm.service", json!({})),
        ];
        for f in forged {
            assert_eq!(crash_of(&f, 1000), None, "{f}");
        }
    }

    /// Exactly the crash offered is read: a pid reused by a later crash is another unit's.
    #[test]
    fn exactly_the_offered_crash() {
        let first = crash_of(&line(1000), 1000).unwrap();
        let again = crash_of(&entry(1000, "4242", "systemd-coredump@17-61500-4242_4242-0.service", json!({})), 1000).unwrap();
        let args = |c: &Crash| c.info_command(1000).get_args().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>();
        assert_eq!(args(&first), ["info", "--no-pager", "COREDUMP_PID=4242", "COREDUMP_UID=1000", &format!("_SYSTEMD_UNIT={UNIT}"), &format!("_BOOT_ID={BOOT}")]);
        let later = crash_of(&entry(1000, "4242", UNIT, json!({ "_BOOT_ID": "fedcba9876543210fedcba9876543210" })), 1000).unwrap();
        assert_ne!(args(&first), args(&later), "the same unit's name, another boot");
        assert_ne!(args(&first), args(&again), "the same pid, another crash");
        assert_eq!(first.info_command(1000).get_program(), "/run/current-system/sw/bin/coredumpctl");
    }

    /// The notification says nothing of the crash's contents and escapes what it says; the question is fixed,
    /// the program only in what is attached (bounded, tainting); the person asks.
    #[test]
    fn offered_then_attached() {
        let c = crash_of(&entry(1000, "4242", UNIT, json!({ "COREDUMP_EXE": "/home/me/<b>x</b>&y" })), 1000).unwrap();
        let (_, body) = c.notification();
        assert!(body.contains("&lt;b&gt;x&lt;/b&gt;&amp;y") && !body.contains("<b>"), "{body}");
        let c = crash_of(&line(1000), 1000).unwrap();
        let (summary, body) = c.notification();
        assert_eq!(summary, "A program of yours crashed", "fixed words: the program's name is only in the body (C12)");
        assert!(body.starts_with("bash (/run/current-system/sw/bin/bash, SIGSEGV)"), "{body}");
        assert!(body.contains("SIGSEGV") && body.contains("nothing is sent unless you ask"), "{body}");
        let h = c.handoff(Some(&"frame\n".repeat(10_000)));
        assert!(h.taints());
        assert_eq!(h.question, CRASH_QUESTION, "nothing of the crash in the question");
        assert!(h.context.as_deref().unwrap().contains("/run/current-system/sw/bin/bash crashed with SIGSEGV"));
        assert!(h.context.as_deref().unwrap().chars().count() <= super::super::handoff::MAX_CONTEXT);
        assert!(c.handoff(None).context.unwrap().contains("could not be read"));
        assert_eq!(watch_command().get_program(), "/run/current-system/sw/bin/journalctl");
    }

    /// One program crashing over and over is one offer; at most MAX_OPEN open, PER_MINUTE a minute.
    #[test]
    fn offers_are_bounded() {
        let crash = |exe: &str| crash_of(&entry(1000, "1", UNIT, json!({ "COREDUMP_EXE": exe })), 1000).unwrap();
        let t = std::time::Instant::now();
        let mut o = Offers::default();
        assert!(o.admit(&crash("/bin/a"), t));
        assert!(!o.admit(&crash("/bin/a"), t + std::time::Duration::from_secs(5)), "the same program again: merged");
        assert!(o.admit(&crash("/bin/b"), t) && o.admit(&crash("/bin/c"), t));
        assert!(!o.admit(&crash("/bin/d"), t), "MAX_OPEN open already");
        o.closed();
        assert!(o.admit(&crash("/bin/d"), t), "one closed: room again");
        o.closed();
        o.closed();
        o.closed();
        assert!(o.admit(&crash("/bin/e"), t));
        o.closed();
        assert!(!o.admit(&crash("/bin/f"), t), "PER_MINUTE in a minute");
        assert!(o.admit(&crash("/bin/f"), t + std::time::Duration::from_secs(61)), "a minute later");
        assert!(o.admit(&crash("/bin/a"), t + SAME_PROGRAM + std::time::Duration::from_secs(1)), "the same program, much later: offered again");
    }

    /// Only the offer's own action on it asks; only its own close ends it; nothing else counts (step 5's second
    /// review: `notify()`'s checks).
    #[test]
    fn only_its_own_ask_asks() {
        assert_eq!(heard("ActionInvoked", 7, 7, Some(ASK_ACTION)), Heard::Ask);
        assert_eq!(heard("ActionInvoked", 7, 8, Some(ASK_ACTION)), Heard::Nothing, "another notification's");
        assert_eq!(heard("ActionInvoked", 7, 7, Some("default")), Heard::Nothing, "another action (the body clicked)");
        assert_eq!(heard("ActionInvoked", 7, 7, None), Heard::Nothing);
        assert_eq!(heard("NotificationClosed", 7, 7, None), Heard::Closed);
        assert_eq!(heard("NotificationClosed", 7, 9, None), Heard::Nothing, "another's close");
        assert_eq!(heard("ActivationToken", 7, 7, Some("tok")), Heard::Token, "its own ask's token (finding 11)");
        assert_eq!(heard("ActivationToken", 7, 8, Some("tok")), Heard::Nothing, "another's token");
        assert_eq!(heard("ActivationToken", 7, 7, None), Heard::Nothing);
        let t = std::time::Instant::now();
        assert_eq!(give_up_at(t) - t, EXPIRES + std::time::Duration::from_secs(30));
    }
}
