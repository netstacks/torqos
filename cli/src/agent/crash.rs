//! A crash, offered to the agent only when the person says so (ADR-051 4, step 4). `torq-crash-watch`, a user
//! service of the person's own, follows the journal for systemd-coredump's entries (`journalctl -f`, as the
//! person: what they may read) and keeps only their own crashes (`COREDUMP_UID`). Each gives a notification,
//! **Ask the agent about this crash**; nothing is read of the crash and nothing goes anywhere until it is
//! clicked. Then its summary — `coredumpctl info <pid>` as the person (program, signal, backtrace), bounded —
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
}

/// A journal field as text (journalctl's JSON gives a string, or bytes as numbers for what is not UTF-8).
fn field(v: &Value, name: &str) -> Option<String> {
    match &v[name] {
        Value::String(s) => Some(s.clone()),
        Value::Array(bytes) => Some(String::from_utf8_lossy(&bytes.iter().filter_map(|b| b.as_u64().map(|b| b as u8)).collect::<Vec<u8>>()).into_owned()),
        _ => None,
    }
}

/// The crash in one line of `journalctl -o json`, if it is a coredump of `uid`'s.
pub fn crash_of(line: &str, uid: u32) -> Option<Crash> {
    let v: Value = serde_json::from_str(line).ok()?;
    if field(&v, "MESSAGE_ID")? != COREDUMP_MESSAGE || field(&v, "COREDUMP_UID")?.parse::<u32>().ok()? != uid {
        return None;
    }
    let program = field(&v, "COREDUMP_EXE").or_else(|| field(&v, "COREDUMP_COMM")).unwrap_or_else(|| "a program".into());
    let signal = field(&v, "COREDUMP_SIGNAL_NAME").or_else(|| field(&v, "COREDUMP_SIGNAL").map(|n| format!("signal {n}"))).unwrap_or_else(|| "a signal".into());
    Some(Crash { pid: field(&v, "COREDUMP_PID")?.parse().ok()?, program: crate::history::one_line(&program, 200), signal: crate::history::one_line(&signal, 40) })
}

/// What follows the journal for new crashes: everything the person may read, from now on.
pub fn watch_command() -> std::process::Command {
    let mut c = std::process::Command::new(crate::app::system_program("journalctl"));
    c.args(["--follow", "--lines=0", "--output=json", &format!("MESSAGE_ID={COREDUMP_MESSAGE}")]);
    c
}

impl Crash {
    /// The program's name, without its path.
    fn name(&self) -> &str {
        self.program.rsplit('/').next().unwrap_or(&self.program)
    }

    /// The notification: its summary and body. Nothing of the crash's contents is in it.
    pub fn notification(&self) -> (String, String) {
        (format!("{} crashed", self.name()), format!("{} ({}). Your agent can look at it with you: nothing is sent unless you ask.", self.program, self.signal))
    }

    /// `coredumpctl info <pid>` as the person, by absolute path.
    pub fn info_command(&self) -> std::process::Command {
        let mut c = std::process::Command::new(crate::app::system_program("coredumpctl"));
        c.args(["info", "--no-pager", &self.pid.to_string()]);
        c
    }

    /// Its summary, read now (the person said to): bounded in time and size.
    pub fn info(&self) -> Option<String> {
        super::setup::output_within(self.info_command(), INFO_WAIT)
    }

    /// What opens the agent's window: a question, and the crash's summary attached (untrusted text: it taints).
    pub fn handoff(&self, info: Option<&str>) -> Handoff {
        let context = match info {
            Some(i) => format!("{} crashed with {} (pid {}).\n\ncoredumpctl info:\n{i}", self.program, self.signal, self.pid),
            None => format!("{} crashed with {} (pid {}). Its summary could not be read.", self.program, self.signal, self.pid),
        };
        Handoff::new(&format!("Why did {} crash, and what can I do about it?", self.name()), Some(&context))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(uid: u32) -> String {
        serde_json::json!({
            "MESSAGE_ID": COREDUMP_MESSAGE, "COREDUMP_UID": uid.to_string(), "COREDUMP_PID": "4242",
            "COREDUMP_EXE": "/run/current-system/sw/bin/bash", "COREDUMP_SIGNAL_NAME": "SIGSEGV", "MESSAGE": "Process 4242 (sh) of user 1000 dumped core."
        })
        .to_string()
    }

    #[test]
    fn only_ones_own_crashes() {
        assert_eq!(crash_of(&line(1000), 1000), Some(Crash { pid: 4242, program: "/run/current-system/sw/bin/bash".into(), signal: "SIGSEGV".into() }));
        assert_eq!(crash_of(&line(1001), 1000), None, "someone else's crash");
        assert_eq!(crash_of(&line(1000).replace(COREDUMP_MESSAGE, "0123"), 1000), None, "not a coredump");
        assert_eq!(crash_of("junk", 1000), None);
        let bytes = serde_json::json!({ "MESSAGE_ID": COREDUMP_MESSAGE, "COREDUMP_UID": "1000", "COREDUMP_PID": "7", "COREDUMP_COMM": [115, 104], "COREDUMP_SIGNAL": "11" }).to_string();
        assert_eq!(crash_of(&bytes, 1000), Some(Crash { pid: 7, program: "sh".into(), signal: "signal 11".into() }));
    }

    /// The notification says nothing of the crash's contents; the handoff attaches its summary, bounded and
    /// tainting, and the person asks.
    #[test]
    fn offered_then_attached() {
        let c = crash_of(&line(1000), 1000).unwrap();
        let (summary, body) = c.notification();
        assert_eq!(summary, "bash crashed");
        assert!(body.contains("SIGSEGV") && body.contains("nothing is sent unless you ask"), "{body}");
        let h = c.handoff(Some(&"frame\n".repeat(10_000)));
        assert!(h.taints() && !h.ask_now, "attached, and the person asks");
        assert!(h.question.contains("bash crash"));
        assert!(h.context.as_deref().unwrap().chars().count() <= super::super::handoff::MAX_CONTEXT);
        assert!(c.handoff(None).context.unwrap().contains("could not be read"));
        assert_eq!(c.info_command().get_program(), "/run/current-system/sw/bin/coredumpctl");
        assert_eq!(c.info_command().get_args().collect::<Vec<_>>(), ["info", "--no-pager", "4242"]);
        assert_eq!(watch_command().get_program(), "/run/current-system/sw/bin/journalctl");
    }
}
