//! What the panel's applet and the agent's window show of a conversation (ADR-051 4): each question,
//! its answer as it comes, the tools it called, whether its session read untrusted text, what it
//! proposed, and how it ended. Kept here, tested, so the two programs only draw it. Everything the agent
//! says is cleaned again (nothing in it can draw on a screen), and bounded.

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
}
