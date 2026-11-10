//! What the panel's applet and the agent's window show of a conversation (ADR-051 4): each question,
//! its answer as it comes, the tools it called, whether its session read untrusted text, what it
//! proposed, and how it ended. Kept here, tested, so the two programs only draw it. Everything the agent
//! says is cleaned again (nothing in it can draw on a screen), and bounded.

use super::client::{Status, Unreachable};
use torq_wire::agent::Event;

/// The most of an answer kept (the harness cuts its own at 64 000 characters).
pub const MAX_ANSWER: usize = 64_000;
/// The most turns a window keeps on screen.
pub const MAX_TURNS: usize = 50;
/// The most tools, sources and errors kept of one turn.
const MAX_ITEMS: usize = 64;

/// How a turn ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Ended {
    /// Still being answered.
    Working,
    Answered,
    /// Stopped, by the person (or the service).
    Stopped,
    /// It failed: the task's code, or no answer at all.
    Failed,
}

/// One question and what came of it.
#[derive(Debug, Clone, PartialEq)]
pub struct Turn {
    pub question: String,
    pub answer: String,
    /// The tools it called, in order.
    pub called: Vec<String>,
    /// Why its session is tainted: each source of untrusted text it read (none: untainted).
    pub tainted: Vec<String>,
    /// The proposals it made, by number.
    pub proposed: Vec<u64>,
    pub errors: Vec<String>,
    pub ended: Ended,
}

/// A text the agent said, as it is shown: each line cleaned, at most `max` characters.
pub fn shown(text: &str, max: usize) -> String {
    let lines: Vec<String> = text.lines().map(crate::history::clean).collect();
    let all = lines.join("\n");
    if all.chars().count() <= max {
        all
    } else {
        format!("{}…", all.chars().take(max.saturating_sub(1)).collect::<String>())
    }
}

fn push_bounded<T>(v: &mut Vec<T>, x: T) {
    if v.len() < MAX_ITEMS {
        v.push(x);
    }
}

/// The turns of this window's conversation, oldest first.
#[derive(Debug, Default, Clone)]
pub struct Conversation {
    turns: std::collections::VecDeque<Turn>,
}

impl Conversation {
    /// A question asked: a turn, working.
    pub fn asked(&mut self, question: &str) {
        if self.turns.len() >= MAX_TURNS {
            self.turns.pop_front();
        }
        self.turns.push_back(Turn {
            question: shown(question, MAX_ANSWER),
            answer: String::new(),
            called: vec![],
            tainted: vec![],
            proposed: vec![],
            errors: vec![],
            ended: Ended::Working,
        });
    }

    /// What the agent said of the turn being answered.
    pub fn heard(&mut self, e: Event) {
        let Some(t) = self.turns.back_mut().filter(|t| t.ended == Ended::Working) else { return };
        match e {
            Event::Say { text } => {
                if !t.answer.is_empty() {
                    t.answer.push('\n');
                }
                t.answer.push_str(&text);
                t.answer = shown(&t.answer, MAX_ANSWER);
            }
            Event::Called { tool } => push_bounded(&mut t.called, crate::history::one_line(&tool, 64)),
            Event::Tainted { why } => push_bounded(&mut t.tainted, crate::history::one_line(&why, 64)),
            Event::Proposed { seq } => push_bounded(&mut t.proposed, seq),
            Event::Error { text } => push_bounded(&mut t.errors, shown(&text, 2000)),
            // Killed by `stop` (its signal's code, 128 + 9) is stopped, not failed.
            Event::Done { exit } => {
                t.ended = match exit {
                    0 if t.errors.is_empty() => Ended::Answered,
                    137 => Ended::Stopped,
                    _ => Ended::Failed,
                }
            }
            Event::Status { .. } | Event::Forgotten => {}
        }
    }

    /// The turn being answered could not be: why (the service gone, silent, refused).
    pub fn lost(&mut self, why: &str) {
        if let Some(t) = self.turns.back_mut().filter(|t| t.ended == Ended::Working) {
            push_bounded(&mut t.errors, shown(why, 2000));
            t.ended = Ended::Failed;
        }
    }

    /// Whether a question of this window's is being answered.
    pub fn working(&self) -> bool {
        self.turns.back().is_some_and(|t| t.ended == Ended::Working)
    }

    pub fn turns(&self) -> impl DoubleEndedIterator<Item = &Turn> {
        self.turns.iter()
    }

    pub fn last(&self) -> Option<&Turn> {
        self.turns.back()
    }

    /// Every proposal made in this conversation, newest first, and whether its session was tainted.
    pub fn proposals(&self) -> Vec<(u64, bool)> {
        self.turns.iter().rev().flat_map(|t| t.proposed.iter().rev().map(|&s| (s, !t.tainted.is_empty()))).collect()
    }

    /// The window's own record emptied, as the agent's memory is by `forget`.
    pub fn clear(&mut self) {
        self.turns.clear();
    }
}

impl Turn {
    /// How the turn stands, in words.
    pub fn state(&self) -> String {
        let mut s = match self.ended {
            Ended::Working => "Working…".to_string(),
            Ended::Answered => "Answered".to_string(),
            Ended::Stopped => "Stopped".to_string(),
            Ended::Failed => "Did not finish".to_string(),
        };
        if !self.called.is_empty() {
            s.push_str(&format!(" · called {}", self.called.join(", ")));
        }
        s
    }

    /// Said plainly when the session read untrusted text: what it proposes is marked so, and asked for one
    /// by one.
    pub fn taint(&self) -> Option<String> {
        (!self.tainted.is_empty()).then(|| format!("Tainted: this session read untrusted text ({}). What it proposes is marked so.", self.tainted.join(", ")))
    }
}

/// How the agent's mark stands in the panel: quiet when idle, the accent while it works, the warning when
/// something is wrong.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mood {
    Quiet,
    Working,
    Trouble,
}

/// The agent at a glance — its one line, and its mark — from what its service said (`None`: not running,
/// so idle and not asked, since asking would start it) and from this program's own last turn.
pub fn glance(reach: &Result<Option<Status>, Unreachable>, here: Option<&Turn>) -> (String, Mood) {
    if here.is_some_and(|t| t.ended == Ended::Working) {
        return ("Working…".into(), Mood::Working);
    }
    let failed = here.is_some_and(|t| t.ended == Ended::Failed);
    match reach {
        Ok(Some(s)) if s.running => ("Working…".into(), Mood::Working),
        Ok(Some(s)) => {
            let kept = match s.turns {
                0 => "remembers nothing yet".to_string(),
                1 => "remembers 1 turn".to_string(),
                n => format!("remembers {n} turns"),
            };
            (format!("Idle · {kept}"), if failed { Mood::Trouble } else { Mood::Quiet })
        }
        Ok(None) => ("Idle".into(), if failed { Mood::Trouble } else { Mood::Quiet }),
        Err(e) if e.not_set_up() => (e.line(), Mood::Quiet),
        Err(e) => (e.line(), Mood::Trouble),
    }
}

/// What became of a proposal, by the ledger.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Fate {
    /// Waiting for a person (as TorqOS Settings' Proposals page lists it).
    Waiting,
    Applied,
    /// Its apply failed.
    Failed,
    /// No proposal of this person's agent has that number: no Review for it.
    NotYours,
}

/// Each of `seqs`' fate in the ledger's `entries`, where `agent` is the person's agent account: a number
/// that is not a proposal of that agent's is `NotYours`, whatever the service said.
pub fn fates(entries: &[crate::history::Entry], agent: &str, seqs: &[u64]) -> Vec<(u64, Fate)> {
    let waiting = crate::history::waiting_proposals(entries);
    seqs.iter()
        .map(|&seq| {
            let mine = entries.iter().any(|e| e.seq == seq && e.kind == "proposal" && e.data["agent"].as_str() == Some(agent));
            let ended = |kind: &str| entries.iter().any(|e| e.kind == kind && e.data["proposal"].as_u64() == Some(seq));
            let fate = if !mine {
                Fate::NotYours
            } else if waiting.iter().any(|e| e.seq == seq) {
                Fate::Waiting
            } else if ended("applied") {
                Fate::Applied
            } else {
                Fate::Failed
            };
            (seq, fate)
        })
        .collect()
}

/// A proposal as a turn shows it: its words, and whether it has a Review (only one of the person's own
/// agent's, waiting).
#[derive(Debug, Clone, PartialEq)]
pub struct ProposalRow {
    pub seq: u64,
    pub text: String,
    pub review: bool,
}

impl Turn {
    /// The proposals this turn made, each with its fate (not yet looked up: said so, no Review), and marked
    /// tainted when the turn's session read untrusted text.
    pub fn proposal_rows(&self, fates: &[(u64, Fate)]) -> Vec<ProposalRow> {
        let mark = if self.tainted.is_empty() { "" } else { " (tainted)" };
        self.proposed
            .iter()
            .map(|&seq| {
                let fate = fates.iter().find(|(s, _)| *s == seq).map(|(_, f)| *f);
                let said = match fate {
                    None => "looking it up…",
                    Some(Fate::Waiting) => "waiting for you to apply or refuse it",
                    Some(Fate::Applied) => "applied",
                    Some(Fate::Failed) => "did not apply",
                    Some(Fate::NotYours) => "not your agent's proposal",
                };
                ProposalRow { seq, text: format!("Proposed #{seq}{mark}: {said}"), review: fate == Some(Fate::Waiting) }
            })
            .collect()
    }
}

/// Whether the agent's memory was erased since the last look (by Forget, from anywhere): it never shrinks
/// otherwise — it keeps its last turns, and drops the oldest as it adds one. A window then stops showing
/// the forgotten conversation as current.
pub fn forgotten(before: Option<usize>, now: usize) -> bool {
    before.is_some_and(|b| now < b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_turn_as_it_comes() {
        let mut c = Conversation::default();
        assert!(!c.working());
        c.asked("why\u{1b}[2J?");
        assert!(c.working());
        c.heard(Event::Called { tool: "read_journal".into() });
        c.heard(Event::Tainted { why: "read_journal".into() });
        c.heard(Event::Say { text: "line one\r\nrewrite\u{202e}".into() });
        c.heard(Event::Proposed { seq: 12 });
        c.heard(Event::Done { exit: 0 });
        let t = c.last().unwrap();
        assert!(!c.working() && t.ended == Ended::Answered);
        assert!(!t.question.contains('\u{1b}') && !t.answer.contains(['\r', '\u{202e}']), "{t:?}");
        assert!(t.answer.starts_with("line one\nrewrite"), "{:?}", t.answer);
        assert_eq!(t.state(), "Answered · called read_journal");
        assert!(t.taint().unwrap().contains("untrusted text (read_journal)"));
        assert_eq!(c.proposals(), vec![(12, true)]);
        // Nothing more lands on a turn that is over.
        c.heard(Event::Say { text: "late".into() });
        assert!(!c.last().unwrap().answer.contains("late"));
    }

    #[test]
    fn how_a_turn_ends() {
        let mut c = Conversation::default();
        let end = |c: &mut Conversation, events: Vec<Event>| {
            c.asked("q");
            events.into_iter().for_each(|e| c.heard(e));
            c.last().unwrap().ended.clone()
        };
        assert_eq!(end(&mut c, vec![Event::Done { exit: 137 }]), Ended::Stopped);
        assert_eq!(end(&mut c, vec![Event::Done { exit: 1 }]), Ended::Failed);
        assert_eq!(end(&mut c, vec![Event::Error { text: "a task is running already".into() }, Event::Done { exit: 0 }]), Ended::Failed);
        c.asked("q");
        c.lost("your agent did not answer in time");
        assert_eq!(c.last().unwrap().ended, Ended::Failed);
        assert_eq!(c.last().unwrap().errors, ["your agent did not answer in time"]);
        assert!(c.last().unwrap().taint().is_none());
    }

    #[test]
    fn bounded() {
        let mut c = Conversation::default();
        for i in 0..MAX_TURNS + 5 {
            c.asked(&format!("q{i}"));
            c.heard(Event::Proposed { seq: i as u64 });
            c.heard(Event::Done { exit: 0 });
        }
        assert_eq!(c.turns().count(), MAX_TURNS);
        assert_eq!(c.turns().next().unwrap().question, "q5", "the oldest go first");
        assert_eq!(c.proposals()[0], (MAX_TURNS as u64 + 4, false), "newest first");
        c.asked("long");
        c.heard(Event::Say { text: "x".repeat(MAX_ANSWER) });
        c.heard(Event::Say { text: "more".into() });
        assert_eq!(c.last().unwrap().answer.chars().count(), MAX_ANSWER);
        for _ in 0..200 {
            c.heard(Event::Called { tool: "t".into() });
        }
        assert_eq!(c.last().unwrap().called.len(), MAX_ITEMS);
        c.clear();
        assert_eq!(c.turns().count(), 0);
    }

    /// The panel's line and mark: idle quiet, working in the accent, trouble in the warning colour; an agent
    /// not set up is said, quietly.
    #[test]
    fn at_a_glance() {
        let idle = |turns| Ok(Some(Status { running: false, turns }));
        assert_eq!(glance(&idle(0), None), ("Idle · remembers nothing yet".into(), Mood::Quiet));
        assert_eq!(glance(&idle(3), None), ("Idle · remembers 3 turns".into(), Mood::Quiet));
        assert_eq!(glance(&Ok(None), None), ("Idle".into(), Mood::Quiet), "not running: not asked");
        assert_eq!(glance(&Ok(Some(Status { running: true, turns: 1 })), None).1, Mood::Working, "a task from anywhere: working");
        assert_eq!(glance(&Err(Unreachable::NoAgent("torq-agent-me".into())), None), ("The agent is not set up for you".into(), Mood::Quiet));
        assert_eq!(glance(&Err(Unreachable::Silent("x".into())), None).1, Mood::Trouble);
        let mut c = Conversation::default();
        c.asked("q");
        assert_eq!(glance(&idle(0), c.last()), ("Working…".into(), Mood::Working), "its own question, before the service says so");
        c.lost("gone");
        assert_eq!(glance(&idle(0), c.last()).1, Mood::Trouble, "its last question failed");
    }

    fn entry(seq: u64, kind: &str, data: serde_json::Value) -> crate::history::Entry {
        crate::history::Entry { seq, ts: String::new(), kind: kind.into(), tier: String::new(), actor: "agent".into(), approved_by: None, tainted: false, reason: String::new(), data }
    }

    /// A proposal's row: its fate from the ledger, Review only for one of the person's own agent's that
    /// waits, and the taint mark wherever the turn's session read untrusted text.
    #[test]
    fn proposals_as_a_turn_shows_them() {
        use serde_json::json;
        let entries = vec![
            entry(1, "proposal", json!({"agent": "torq-agent-me"})),
            entry(2, "proposal", json!({"agent": "torq-agent-me"})),
            entry(3, "applied", json!({"proposal": 2})),
            entry(4, "proposal", json!({"agent": "torq-agent-bob"})),
            entry(5, "proposal", json!({"agent": "torq-agent-me"})),
            entry(6, "apply_failed", json!({"proposal": 5})),
            entry(7, "applied", json!({})),
        ];
        let f = fates(&entries, "torq-agent-me", &[1, 2, 4, 5, 7, 99]);
        assert_eq!(f, vec![(1, Fate::Waiting), (2, Fate::Applied), (4, Fate::NotYours), (5, Fate::Failed), (7, Fate::NotYours), (99, Fate::NotYours)]);
        let mut c = Conversation::default();
        c.asked("q");
        for seq in [1, 2, 4, 8] {
            c.heard(Event::Proposed { seq });
        }
        let rows = c.last().unwrap().proposal_rows(&f);
        assert_eq!(rows.iter().map(|r| (r.seq, r.review)).collect::<Vec<_>>(), [(1, true), (2, false), (4, false), (8, false)]);
        assert_eq!(rows[0].text, "Proposed #1: waiting for you to apply or refuse it");
        assert_eq!(rows[3].text, "Proposed #8: looking it up…");
        c.heard(Event::Tainted { why: "read_journal".into() });
        let rows = c.last().unwrap().proposal_rows(&f);
        assert!(rows.iter().all(|r| r.text.contains("(tainted)")), "{rows:?}");
    }

    #[test]
    fn forgotten_elsewhere() {
        assert!(forgotten(Some(3), 0) && forgotten(Some(8), 1));
        assert!(!forgotten(None, 0), "never seen: nothing to say");
        assert!(!forgotten(Some(8), 8) && !forgotten(Some(2), 3));
    }
}
