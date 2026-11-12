//! The conversation the agent remembers (ADR-051 9, phase 1's memory): its recent turns, in one file
//! in the agent service's own state directory (`/var/lib/torq-agent-memory/<person>`, the agent's,
//! 0700) — not in the agent's home, which every program run as the agent (`torq agent run`) can
//! write, and hidden from them (modules/agent-run.sh). Bounded: the last `MAX_TURNS` turns, each
//! question and answer cut to `MAX_CHARS`; a file larger than `MAX_FILE`, or not one this wrote, is
//! taken as no memory. Erasable: `forget` removes it (`torq ask --forget`), and with it each task's own record
//! (`torq_agent::session::records_dir`, the last `RECORDS_KEPT` kept).
//!
//! A turn keeps the agent's answer only when the session it came from read nothing untrusted: a
//! tainted answer is never carried into a later session as the agent's own words.

use anyhow::{Context, Result};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use torq_wire::agent::Turn;

/// How many turns are remembered.
pub const MAX_TURNS: usize = 8;
/// The most of a question or an answer that is kept (the rest is cut off).
pub const MAX_CHARS: usize = 2_000;
/// The largest memory file read; anything larger is not this program's, and is no memory.
pub const MAX_FILE: u64 = 256 * 1024;

pub struct Memory {
    file: PathBuf,
}

/// `s`, at most `MAX_CHARS` characters of its beginning.
fn cut(s: &str) -> String {
    if s.chars().count() <= MAX_CHARS {
        return s.to_string();
    }
    let mut c: String = s.chars().take(MAX_CHARS - 1).collect();
    c.push('…');
    c
}

impl Memory {
    pub fn new(dir: &Path) -> Memory {
        Memory { file: dir.join("conversation.json") }
    }

    /// The turns remembered, oldest first: none if there is no memory, or it is not what this wrote.
    pub fn recent(&self) -> Vec<Turn> {
        let read = || -> Result<Vec<Turn>> {
            use std::os::unix::fs::OpenOptionsExt;
            let f = match std::fs::OpenOptions::new().read(true).custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32).open(&self.file) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
                f => f?,
            };
            let mut text = String::new();
            f.take(MAX_FILE + 1).read_to_string(&mut text)?;
            anyhow::ensure!(text.len() as u64 <= MAX_FILE, "larger than {MAX_FILE} bytes");
            let turns: Vec<Turn> = serde_json::from_str(&text)?;
            Ok(bounded(turns))
        };
        read().unwrap_or_else(|e| {
            eprintln!("torq-agent-service: {}: not read ({e:#}); remembering nothing before", self.file.display());
            vec![]
        })
    }

    /// Remember `turn`, the newest; the oldest beyond `MAX_TURNS` are forgotten.
    pub fn remember(&self, turn: Turn) -> Result<()> {
        let mut turns = self.recent();
        turns.push(turn);
        self.write(&bounded(turns))
    }

    /// Forget everything.
    pub fn forget(&self) -> Result<()> {
        match std::fs::remove_file(&self.file) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e).with_context(|| format!("removing {}", self.file.display())),
            _ => Ok(()),
        }
    }

    /// Written whole, then put in place: a crash leaves the old memory or the new, never half of one.
    fn write(&self, turns: &[Turn]) -> Result<()> {
        use std::os::unix::fs::OpenOptionsExt;
        let new = self.file.with_extension("json.new");
        let _ = std::fs::remove_file(&new);
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(&new)
            .with_context(|| format!("writing {}", new.display()))?;
        f.write_all(&serde_json::to_vec(turns)?)?;
        f.sync_all()?;
        std::fs::rename(&new, &self.file).with_context(|| format!("writing {}", self.file.display()))
    }
}

/// The last `MAX_TURNS` of `turns`, each cut to `MAX_CHARS`.
fn bounded(turns: Vec<Turn>) -> Vec<Turn> {
    let skip = turns.len().saturating_sub(MAX_TURNS);
    turns.into_iter().skip(skip).map(|t| Turn { question: cut(&t.question), answer: t.answer.as_deref().map(cut) }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(q: &str, a: Option<&str>) -> Turn {
        Turn { question: q.into(), answer: a.map(String::from) }
    }

    #[test]
    fn the_last_turns_and_no_more() {
        let d = tempfile::tempdir().unwrap();
        let m = Memory::new(d.path());
        assert_eq!(m.recent(), vec![], "none at first");
        for i in 0..MAX_TURNS + 3 {
            m.remember(turn(&format!("q{i}"), Some("a"))).unwrap();
        }
        let r = m.recent();
        assert_eq!(r.len(), MAX_TURNS);
        assert_eq!(r[0].question, "q3", "the oldest forgotten");
        assert_eq!(r[MAX_TURNS - 1].question, format!("q{}", MAX_TURNS + 2));
    }

    #[test]
    fn each_turn_cut_and_the_whole_within_its_bound() {
        let d = tempfile::tempdir().unwrap();
        let m = Memory::new(d.path());
        // The widest characters there are (four bytes each), and quotes JSON writes as two.
        let big = "𝄞\"".repeat(MAX_CHARS);
        for _ in 0..MAX_TURNS {
            m.remember(turn(&big, Some(&big))).unwrap();
        }
        let r = m.recent();
        assert_eq!(r.len(), MAX_TURNS, "a full memory is still read back");
        assert!(r.iter().all(|t| t.question.chars().count() == MAX_CHARS && t.answer.as_ref().unwrap().chars().count() == MAX_CHARS));
        assert!(std::fs::metadata(d.path().join("conversation.json")).unwrap().len() <= MAX_FILE);
    }

    #[test]
    fn what_is_not_its_own_is_no_memory() {
        let d = tempfile::tempdir().unwrap();
        let m = Memory::new(d.path());
        let f = d.path().join("conversation.json");
        std::fs::write(&f, "not json").unwrap();
        assert_eq!(m.recent(), vec![]);
        std::fs::write(&f, format!("[{}]", vec![r#"{"question":"q","answer":null}"#; 20_000].join(","))).unwrap();
        assert!(std::fs::metadata(&f).unwrap().len() > MAX_FILE);
        assert_eq!(m.recent(), vec![], "larger than its bound: not read");
        std::fs::remove_file(&f).unwrap();
        let elsewhere = d.path().join("elsewhere.json");
        std::fs::write(&elsewhere, r#"[{"question":"planted","answer":"x"}]"#).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &f).unwrap();
        assert_eq!(m.recent(), vec![], "never through a link");
    }

    #[test]
    fn forgotten() {
        let d = tempfile::tempdir().unwrap();
        let m = Memory::new(d.path());
        m.remember(turn("q", None)).unwrap();
        m.forget().unwrap();
        assert_eq!(m.recent(), vec![]);
        m.forget().unwrap();
    }
}
