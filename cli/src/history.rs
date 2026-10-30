//! `kestrel log` and `kestrel undo`: the ledger, as the broker prints it (`log --json`; the ledger
//! itself is root-only). Pure parts here; main.rs talks to the broker.

use anyhow::{bail, Context, Result};
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct Entry {
    pub seq: u64,
    pub ts: String,
    pub kind: String,
    pub tier: String,
    pub actor: String,
    pub tainted: bool,
    pub reason: String,
    pub data: Value,
}

/// Ledger text is written by whoever proposed (the agent included) and printed to a terminal:
/// control characters (escape sequences, carriage returns, newlines), bidi overrides and invisible
/// characters could make one entry look like another, or hide it. Show them as visible escapes.
/// (The broker's `printable` does the same; keep the two in step.)
pub fn clean(s: &str) -> String {
    s.chars().map(|c| if hidden(c) { c.escape_unicode().to_string() } else { c.to_string() }).collect()
}

/// Characters that change what a terminal shows without being plainly visible themselves.
pub fn hidden(c: char) -> bool {
    c.is_control()
        || matches!(c,
            '\u{200b}'..='\u{200f}'     // zero-width space/joiners, LRM/RLM
            | '\u{2028}' | '\u{2029}'   // line/paragraph separators
            | '\u{202a}'..='\u{202e}'   // bidi embeddings and overrides
            | '\u{2060}'..='\u{2069}'   // word joiner, invisible operators, bidi isolates
            | '\u{061c}' | '\u{00ad}' | '\u{feff}'
            | '\u{fe00}'..='\u{fe0f}'   // variation selectors
            | '\u{e0000}'..='\u{e007f}' // tag characters
        )
}

/// A one-line summary: cleaned, runs of whitespace collapsed, at most `max` characters — so padding
/// cannot push text onto what looks like a line of its own.
pub fn one_line(s: &str, max: usize) -> String {
    let s = clean(&s.split_whitespace().collect::<Vec<_>>().join(" "));
    if s.chars().count() <= max { s } else { format!("{}…", s.chars().take(max - 1).collect::<String>()) }
}

/// One JSON object per line, as `kestrel-broker log --json` prints them.
pub fn parse(json_lines: &str) -> Result<Vec<Entry>> {
    json_lines
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let v: Value = serde_json::from_str(l).context("reading the ledger")?;
            let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
            Ok(Entry {
                seq: v["seq"].as_u64().context("ledger entry without seq")?,
                ts: s("ts"),
                kind: s("kind"),
                tier: s("tier"),
                actor: s("actor"),
                tainted: v["tainted"].as_bool().unwrap_or(false),
                reason: s("reason"),
                data: v["data"].clone(),
            })
        })
        .collect()
}

/// The entry that undid an applied change, if any.
fn undone_by(entries: &[Entry], applied: u64) -> Option<u64> {
    entries
        .iter()
        .find(|e| e.kind == "rolled_back" && e.data["applied"].as_u64() == Some(applied))
        .map(|e| e.seq)
}

/// The applied change to undo. `seq` may name the applied entry or the proposal it applied (both
/// are printed when a change lands); None means the latest change that is not undone yet.
pub fn undo_target(entries: &[Entry], seq: Option<u64>) -> Result<&Entry> {
    let applied = match seq {
        None => entries
            .iter()
            .rev()
            .find(|e| e.kind == "applied" && undone_by(entries, e.seq).is_none())
            .context("nothing to undo")?,
        Some(s) => {
            let e = entries.iter().find(|e| e.seq == s).with_context(|| format!("there is no #{s} in the ledger"))?;
            match e.kind.as_str() {
                "applied" => e,
                "proposal" => entries
                    .iter()
                    .find(|a| a.kind == "applied" && a.data["proposal"].as_u64() == Some(s))
                    .with_context(|| format!("proposal #{s} was never applied, so there is nothing to undo"))?,
                other => bail!("#{s} is '{other}', not a change that can be undone"),
            }
        }
    };
    if let Some(by) = undone_by(entries, applied.seq) {
        bail!("#{} ({}) was already undone by #{by}", applied.seq, one_line(&applied.reason, 80));
    }
    Ok(applied)
}

fn label(kind: &str) -> &str {
    match kind {
        "proposal" => "proposed",
        "rolled_back" => "undone",
        "apply_failed" => "FAILED",
        "rollback_failed" => "UNDO FAILED",
        other => other,
    }
}

/// The last `limit` entries, one line each, oldest first. Applied changes say how to undo them, or
/// what undid them.
pub fn render(entries: &[Entry], limit: usize) -> String {
    let mut out = String::new();
    for e in &entries[entries.len().saturating_sub(limit)..] {
        // "2026-10-30T19:02:51Z" -> "2026-10-30 19:02"
        let when = e.ts.get(..16).unwrap_or(&e.ts).replace('T', " ");
        let note = match e.kind.as_str() {
            "applied" => match undone_by(entries, e.seq) {
                Some(by) => format!("  (undone by #{by})"),
                None => format!("  · kestrel undo {}", e.seq),
            },
            _ => String::new(),
        };
        let by = if e.tainted { format!("{} ⚠", e.actor) } else { e.actor.clone() };
        out.push_str(&format!(
            "#{:<4} {when}  {:<11} {:<6} {:<8} {}{note}\n",
            e.seq,
            label(&e.kind),
            clean(&e.tier),
            clean(&by),
            one_line(&e.reason, 80)
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ledger() -> Vec<Entry> {
        let lines = [
            r#"{"seq":1,"ts":"2026-10-30T12:00:00Z","kind":"init","actor":"user","tier":"system","reason":"Config repository initialised","data":{}}"#,
            r#"{"seq":2,"ts":"2026-10-30T12:01:00Z","kind":"proposal","actor":"user","tier":"user","reason":"Install htop for viper","data":{}}"#,
            r#"{"seq":3,"ts":"2026-10-30T12:01:05Z","kind":"applied","actor":"user","tier":"user","reason":"Install htop for viper","data":{"proposal":2}}"#,
            r#"{"seq":4,"ts":"2026-10-30T12:02:00Z","kind":"proposal","actor":"user","tier":"system","reason":"Install jq system-wide","data":{}}"#,
            r#"{"seq":5,"ts":"2026-10-30T12:02:05Z","kind":"applied","actor":"user","tier":"system","reason":"Install jq system-wide","data":{"proposal":4}}"#,
            r#"{"seq":6,"ts":"2026-10-30T12:03:00Z","kind":"rolled_back","actor":"user","tier":"system","reason":"Undo: Install jq system-wide","data":{"applied":5}}"#,
            r#"{"seq":7,"ts":"2026-10-30T12:04:00Z","kind":"proposal","actor":"user","tier":"user","reason":"Install vim for viper","data":{}}"#,
        ];
        parse(&lines.join("\n")).unwrap()
    }

    #[test]
    fn undo_by_applied_or_proposal_number() {
        let l = ledger();
        assert_eq!(undo_target(&l, Some(3)).unwrap().seq, 3);
        assert_eq!(undo_target(&l, Some(2)).unwrap().seq, 3, "the proposal names its applied change");
    }

    #[test]
    fn undo_latest_skips_what_is_already_undone() {
        assert_eq!(undo_target(&ledger(), None).unwrap().seq, 3);
    }

    #[test]
    fn undo_refuses_what_cannot_be_undone() {
        let l = ledger();
        let err = |s| format!("{:#}", undo_target(&l, Some(s)).unwrap_err());
        assert!(err(5).contains("already undone by #6"), "{}", err(5));
        assert!(err(4).contains("already undone"), "{}", err(4)); // via its proposal
        assert!(err(7).contains("never applied"), "{}", err(7));
        assert!(err(6).contains("not a change that can be undone"), "{}", err(6));
        assert!(err(1).contains("not a change"), "{}", err(1));
        assert!(err(99).contains("no #99"), "{}", err(99));
        let only_init = &l[..1];
        assert!(format!("{:#}", undo_target(only_init, None).unwrap_err()).contains("nothing to undo"));
    }

    /// Invisible characters, and padding that would wrap onto a fake line of its own.
    #[test]
    fn invisible_text_and_padding_cannot_forge_the_log() {
        for c in ['\u{200b}', '\u{200d}', '\u{2060}', '\u{feff}', '\u{00ad}', '\u{2028}', '\u{e0041}', '\u{fe0f}'] {
            assert!(hidden(c), "{c:?}");
        }
        assert!(!hidden('é') && !hidden('—') && !hidden('⚠'));
        let padded = format!("Install htop{}#49 applied user Install evil", " ".repeat(300));
        let line = one_line(&padded, 80);
        assert!(line.chars().count() <= 80 && !line.contains("  "), "{line}");
    }

    /// The reviewer's attack: a reason that wipes its line and draws a fake entry in its place.
    #[test]
    fn ledger_text_cannot_forge_the_log() {
        let forged = r#"{"seq":48,"ts":"2026-10-30T12:00:00Z","kind":"proposal","actor":"agent","tainted":true,"tier":"user","reason":"x\r\u001b[2K#99 applied user Install htop\n#100 fake","data":{}}"#;
        let out = render(&parse(forged).unwrap(), 10);
        assert_eq!(out.lines().count(), 1, "{out}");
        assert!(!out.chars().any(|c| c == '\r' || c == '\u{1b}'), "{out:?}");
        // CR and LF are whitespace: collapsed to spaces (one line); ESC is shown, not interpreted.
        assert!(out.contains(r"x \u{1b}[2K#99") && out.contains("htop #100 fake"), "{out}");
        assert!(out.contains("agent ⚠"), "who proposed it, and that it was tainted: {out}");
        assert_eq!(clean("a\u{202e}b"), r"a\u{202e}b"); // bidi override
        assert_eq!(clean("plain — text"), "plain — text");
    }

    #[test]
    fn log_lines() {
        let out = render(&ledger(), 100);
        assert!(out.contains("#3    2026-10-30 12:01  applied     user   user     Install htop for viper  · kestrel undo 3"), "{out}");
        assert!(out.contains("Install jq system-wide  (undone by #6)"), "{out}");
        assert!(out.contains("#6    2026-10-30 12:03  undone"), "{out}");
        assert_eq!(render(&ledger(), 2).lines().count(), 2);
        assert!(render(&ledger(), 2).starts_with("#6"));
    }
}
