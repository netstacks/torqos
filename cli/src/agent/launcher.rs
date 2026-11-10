//! The agent in COSMIC's launcher (ADR-051 4, step 4): a pop-launcher plugin, `torq-agent-launcher`,
//! installed where pop-launcher looks for the system's plugins (`/etc/pop-launcher/plugins/torq-agent`). Typing
//! `ask <question>` offers one result, "Ask your agent: <question>"; choosing it opens the agent's window with
//! the question in its box, for the person to ask (nothing is sent by itself). pop-launcher runs it as the person, speaking JSON
//! lines on its standard input and output; it is the person's own program and hands the text only to their
//! own window.

use super::handoff::Handoff;
use serde_json::{json, Value};

/// What a query must start with: everything after it is the question.
pub const PREFIX: &str = "ask ";

#[derive(Debug, Default)]
pub struct Plugin {
    offered: Option<String>,
}

/// What a request comes to: the lines to say back, a question to open the window with, and whether to end.
#[derive(Debug, Default, PartialEq)]
pub struct Reply {
    pub say: Vec<String>,
    pub open: Option<Handoff>,
    pub end: bool,
}

impl Plugin {
    /// One request line from pop-launcher.
    pub fn handle(&mut self, line: &str) -> Reply {
        let Ok(request) = serde_json::from_str::<Value>(line) else { return Reply::default() };
        let finished = || json!("Finished").to_string();
        if let Some(query) = request.get("Search").and_then(Value::as_str) {
            let q = query.strip_prefix(PREFIX).unwrap_or_default().trim();
            if q.is_empty() {
                self.offered = None;
                return Reply { say: vec![finished()], ..Reply::default() };
            }
            let h = Handoff::new(q, None);
            let shown = crate::history::one_line(&h.question, 120);
            self.offered = Some(h.question);
            let item = json!({ "Append": {
                "id": 0,
                "name": format!("Ask your agent: {shown}"),
                "description": "TorqOS Agent: it reads and explains, and proposes what you approve",
                "icon": { "Name": "torqos" },
            }});
            return Reply { say: vec![item.to_string(), finished()], ..Reply::default() };
        }
        if request.get("Activate").is_some() {
            return match self.offered.take() {
                Some(q) => Reply { say: vec![json!("Close").to_string()], open: Some(Handoff::new(&q, None)), end: false },
                None => Reply::default(),
            };
        }
        if request == json!("Exit") {
            return Reply { end: true, ..Reply::default() };
        }
        Reply::default()
    }
}

/// The plugin's program: requests from standard input until `Exit` or its end.
pub fn run() -> i32 {
    use std::io::{BufRead, Write};
    let mut plugin = Plugin::default();
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let r = plugin.handle(&line);
        if let Some(h) = &r.open {
            if let Err(e) = super::handoff::open_window(h) {
                eprintln!("torq-agent-launcher: {e:#}");
            }
        }
        for s in &r.say {
            if writeln!(out, "{s}").and_then(|()| out.flush()).is_err() {
                return 0;
            }
        }
        if r.end {
            break;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_question_offered_then_asked() {
        let mut p = Plugin::default();
        let r = p.handle(r#"{"Search":"ask why is my wifi slow?"}"#);
        assert_eq!(r.say.len(), 2);
        let item: Value = serde_json::from_str(&r.say[0]).unwrap();
        assert_eq!(item["Append"]["name"], "Ask your agent: why is my wifi slow?");
        assert_eq!(item["Append"]["id"], 0);
        assert_eq!(r.say[1], r#""Finished""#);
        assert_eq!(r.open, None, "a search opens nothing");
        let r = p.handle(r#"{"Activate":0}"#);
        assert_eq!(r.open, Some(Handoff::new("why is my wifi slow?", None)));
        assert!(!r.open.as_ref().unwrap().taints(), "nothing attached");
        assert_eq!(r.say, [r#""Close""#]);
        assert_eq!(p.handle(r#"{"Activate":0}"#), Reply::default(), "offered once");
    }

    #[test]
    fn nothing_without_a_question() {
        let mut p = Plugin::default();
        assert_eq!(p.handle(r#"{"Search":"ask   "}"#).say, [r#""Finished""#]);
        assert_eq!(p.handle(r#"{"Activate":0}"#).open, None);
        assert_eq!(p.handle("not json"), Reply::default());
        assert!(p.handle(r#""Exit""#).end);
        assert_eq!(p.handle(r#""Interrupt""#), Reply::default());
    }

}
