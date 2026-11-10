//! A person's agent service (ADR-051 3, step 2): `torq-agent@<person>.service` on its socket
//! `/run/torq-agent/<person>.sock`, one JSON line at a time each way. A client (`torq ask`, later the
//! panel's window) sends a `Request`; the service answers with `Event`s, streamed as they come. The
//! service runs each task as its own process, TorqOS's harness (`torq-doctor --propose --task`), which
//! is handed a `Task` on its input and writes the task's own events (`say`, `called`, `proposed`,
//! `tainted`, `error`) on its output; the service relays them and says `done` last. Every request ends
//! with a last frame, on every way it can go: `done` (an ask; and any refusal or error, exit 1),
//! `status` (status, stop) or `forgotten` (forget).
//!
//! Who is asking is never in a request: the service takes it from the kernel (SO_PEERCRED) and answers
//! only the person it serves.

use serde::{Deserialize, Serialize};

/// The directory of every person's agent socket (root's; each socket is its person's, 0600).
pub const SOCKET_DIR: &str = "/run/torq-agent";

/// Where `person`'s agent listens.
pub fn socket(person: &str) -> String {
    format!("{SOCKET_DIR}/{person}.sock")
}

/// The longest line either side reads: a request, an event (an answer is at most this long).
pub const MAX_LINE: usize = 1 << 20;

/// What a client asks. An unknown kind or field is refused, not ignored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum Request {
    /// A question or a task, with what the person attached to it (an alarm, a failed commit): the
    /// attached text is untrusted data, and taints the task's session.
    Ask {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context: Option<String>,
    },
    /// End the running task (its whole process group, and everything of it).
    Stop,
    /// Whether a task is running.
    Status,
    /// Erase the conversation the agent remembers.
    Forget,
}

/// What the service (and, for the first five, the task's process) says.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum Event {
    /// The answer, cleaned of escapes and invisible characters, as the doctor cleans it.
    Say { text: String },
    /// A tool the task ran (by its name: `read_journal`, `propose_change`, …).
    Called { tool: String },
    /// A proposal the broker recorded from this task's session: its number.
    Proposed { seq: u64 },
    /// The session read untrusted text, from this source: tainted for good.
    Tainted { why: String },
    /// Something failed; what, in words.
    Error { text: String },
    /// The ask is over: its task's exit code (killed by `stop`: 128 + the signal), or 1 when no task
    /// started (refused: the error before it says why). Every ask ends with it.
    Done { exit: i32 },
    /// The answer to `status` (and to `stop`, after it): whether a task is running.
    Status { running: bool },
    /// The conversation is forgotten.
    Forgotten,
}

impl Event {
    /// Whether the task's own process may say this; the rest are the service's alone.
    pub fn from_task(&self) -> bool {
        matches!(self, Event::Say { .. } | Event::Called { .. } | Event::Proposed { .. } | Event::Tainted { .. } | Event::Error { .. })
    }
}

/// One turn the agent remembers: what the person asked, and what it answered — kept only when the
/// answer came from a session that read nothing untrusted (else `None`: a tainted answer is never
/// carried into a later session as if it were the agent's own).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Turn {
    pub question: String,
    #[serde(default)]
    pub answer: Option<String>,
}

/// What the service hands the harness for one task, on its input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    pub question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    /// The conversation's recent turns, oldest first.
    #[serde(default)]
    pub memory: Vec<Turn>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bytes on the socket, pinned: the CLI, the panel and the service may be different builds.
    #[test]
    fn the_lines_on_the_socket() {
        let ask = Request::Ask { text: "why?".into(), context: None };
        assert_eq!(serde_json::to_string(&ask).unwrap(), r#"{"type":"ask","text":"why?"}"#);
        let with = Request::Ask { text: "this".into(), context: Some("alarm".into()) };
        assert_eq!(serde_json::to_string(&with).unwrap(), r#"{"type":"ask","text":"this","context":"alarm"}"#);
        assert_eq!(serde_json::to_string(&Request::Stop).unwrap(), r#"{"type":"stop"}"#);
        assert_eq!(serde_json::to_string(&Request::Status).unwrap(), r#"{"type":"status"}"#);
        assert_eq!(serde_json::to_string(&Request::Forget).unwrap(), r#"{"type":"forget"}"#);
        for (e, line) in [
            (Event::Say { text: "a\n".into() }, r#"{"type":"say","text":"a\n"}"#),
            (Event::Called { tool: "read_journal".into() }, r#"{"type":"called","tool":"read_journal"}"#),
            (Event::Proposed { seq: 7 }, r#"{"type":"proposed","seq":7}"#),
            (Event::Tainted { why: "context".into() }, r#"{"type":"tainted","why":"context"}"#),
            (Event::Error { text: "no".into() }, r#"{"type":"error","text":"no"}"#),
            (Event::Done { exit: 137 }, r#"{"type":"done","exit":137}"#),
            (Event::Status { running: false }, r#"{"type":"status","running":false}"#),
            (Event::Forgotten, r#"{"type":"forgotten"}"#),
        ] {
            assert_eq!(serde_json::to_string(&e).unwrap(), line);
            assert_eq!(serde_json::from_str::<Event>(line).unwrap(), e);
        }
        let t = Task { question: "q".into(), context: None, memory: vec![Turn { question: "p".into(), answer: None }] };
        assert_eq!(serde_json::to_string(&t).unwrap(), r#"{"question":"q","memory":[{"question":"p","answer":null}]}"#);
    }

    #[test]
    fn an_unknown_kind_or_field_is_refused() {
        for bad in [r#"{"type":"apply","seq":3}"#, r#"{"type":"ask","text":"x","as":"bob"}"#, r#"{"text":"x"}"#, r#""stop""#, r#"{"type":"ask"}"#] {
            assert!(serde_json::from_str::<Request>(bad).is_err(), "{bad}");
        }
        assert!(serde_json::from_str::<Task>(r#"{"question":"q","tools":["shell"]}"#).is_err());
    }

    /// Only the service says a task is done, or answers status: a task's process cannot end its own
    /// stream early, or claim another state.
    #[test]
    fn what_a_task_may_say() {
        assert!(Event::Say { text: String::new() }.from_task() && Event::Proposed { seq: 1 }.from_task());
        for e in [Event::Done { exit: 0 }, Event::Status { running: false }, Event::Forgotten] {
            assert!(!e.from_task(), "{e:?}");
        }
    }

    #[test]
    fn where_each_agent_listens() {
        assert_eq!(socket("alice"), "/run/torq-agent/alice.sock");
    }
}
