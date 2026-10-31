//! torqcli's commands as one model (`Op`) with two syntaxes (ADR-016, "One grammar, two
//! renderings"): a line in Juniper or Cisco style parses to an `Op`, and every `Op` renders in either
//! style. The session executes `Op`s; nothing else knows a syntax. A property test checks that
//! rendering in one style and parsing in the other gives the same `Op`.

use crate::session::Mode;
use crate::filter::Filter;
use anyhow::{bail, Context, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Juniper,
    Cisco,
}

impl Style {
    pub fn name(self) -> &'static str {
        match self {
            Style::Juniper => "juniper",
            Style::Cisco => "cisco",
        }
    }
    pub fn parse(s: &str) -> Option<Style> {
        match s {
            "juniper" => Some(Style::Juniper),
            "cisco" => Some(Style::Cisco),
            _ => None,
        }
    }
}

/// Words a Cisco-style bare path could be mistaken for: no setting may be named one of these
/// (Schema::parse refuses it), so a command word is never also a path.
pub const RESERVED: &[&str] = &[
    "no", "do", "exit", "end", "top", "up", "show", "commit", "rollback", "set", "delete", "edit", "configure", "config",
    "run", "switch", "quit", "request", "all", "help", "diff",
];

/// The pipes after a `show`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pipes {
    pub compare: bool,
    pub display_set: bool,
}

/// `commit` options.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommitOptions {
    pub check: bool,
    pub confirm: Option<u32>,
    pub comment: Option<String>,
}

/// One command. Paths are relative to where configuration mode is (`edit`), except in operational
/// mode, where they are from the top.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    // Operational mode.
    Configure,
    /// `show configuration [path] [| display set]` (Cisco: `show running-config`).
    ShowActive { path: Vec<String>, pipes: Pipes },
    ShowCommits,
    ShowRollback { n: usize, compare: Option<usize> },
    ShowAlarms,
    ShowServices,
    ShowPackages { terms: Vec<String> },
    Request { words: Vec<String> },
    // Configuration mode.
    /// A leaf's value or leaf-list elements: the words after `set` (Cisco: the bare path).
    Set { words: Vec<String> },
    /// `delete <path>` (Cisco: `no <path>`); `all` deletes a whole container or list entry.
    Delete { words: Vec<String>, all: bool },
    /// Go down to a container or list entry (Cisco: its bare path, a submode).
    Edit { words: Vec<String> },
    Up,
    Top,
    /// The candidate (Cisco: `show configuration`).
    ShowCandidate { path: Vec<String>, pipes: Pipes },
    /// What the candidate changes (Juniper `show | compare`, Cisco `show configuration diff`).
    Compare,
    Rollback { n: usize },
    Commit(CommitOptions),
    /// An operational command from configuration mode (Juniper `run`, Cisco `do`).
    Run(Box<Op>),
    /// Up one level, or out of configuration mode at the top (both styles' `exit`); in operational
    /// mode, leave torqcli.
    Exit,
    /// Out of configuration mode from anywhere (Cisco `end`, Juniper `exit configuration-mode`).
    End,
    SwitchCli,
}

/// The highest rollback number (how far back `rollback` and `show system rollback` reach).
pub const MAX_ROLLBACK: usize = 10_000;

fn number(n: &str) -> Result<usize> {
    let n: usize = n.parse().with_context(|| format!("'{n}' is not a number"))?;
    if n > MAX_ROLLBACK {
        bail!("rollback numbers go up to {MAX_ROLLBACK}");
    }
    Ok(n)
}

fn split_pipes(w: &[String]) -> Result<(Vec<String>, Pipes)> {
    let mut parts = w.split(|x| x == "|");
    let head = parts.next().unwrap_or_default().to_vec();
    let mut p = Pipes::default();
    for part in parts {
        match part.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
            ["compare"] => p.compare = true,
            ["display", "set"] => p.display_set = true,
            other => bail!("unknown pipe: | {}; one of: compare, display set, or a filter: {}", other.join(" "), crate::filter::names()),
        }
    }
    Ok((head, p))
}

/// The most filters one line may chain (each runs in its own sandbox).
const MAX_FILTERS: usize = 16;

/// Split the filters off the end of a line: the rest is the command and its own pipes. A filter
/// works on output text, so only another filter may follow it.
fn split_filters(w: &[String]) -> Result<(Vec<String>, Vec<Filter>)> {
    let bars: Vec<usize> = w.iter().enumerate().filter(|(_, x)| *x == "|").map(|(i, _)| i).collect();
    let (mut head_end, mut filters) = (w.len(), Vec::new());
    for (n, &bar) in bars.iter().enumerate() {
        let seg = &w[bar + 1..bars.get(n + 1).copied().unwrap_or(w.len())];
        if seg.is_empty() {
            bail!("a | needs something after it: compare, display set, or a filter: {}", crate::filter::names());
        }
        match crate::filter::parse(seg) {
            Some(f) => {
                head_end = head_end.min(bar);
                filters.push(f);
                if filters.len() > MAX_FILTERS {
                    bail!("at most {MAX_FILTERS} filters on one line");
                }
            }
            None if head_end < w.len() => {
                bail!("| {} must come before | {}: filters work on the output, last", seg.join(" "), w[head_end + 1])
            }
            None => {}
        }
    }
    Ok((w[..head_end].to_vec(), filters))
}

/// Whether `op` prints output a filter can work on.
fn shows(op: &Op) -> bool {
    match op {
        Op::ShowActive { .. } | Op::ShowCommits | Op::ShowRollback { .. } | Op::ShowAlarms | Op::ShowServices => true,
        Op::ShowCandidate { .. } | Op::Compare => true,
        Op::Run(op) => shows(op),
        _ => false,
    }
}

/// Parse one line: the command (`parse`) and the filters after it.
pub fn parse_line(style: Style, mode: Mode, w: &[String], is_place: &dyn Fn(&[String]) -> bool) -> Result<(Op, Vec<Filter>)> {
    let (head, filters) = split_filters(w)?;
    let op = parse(style, mode, &head, is_place)?;
    if !filters.is_empty() && !shows(&op) {
        let tool = &w[head.len() + 1];
        let packages = |o: &Op| matches!(o, Op::ShowPackages { .. });
        if packages(&op) || matches!(&op, Op::Run(o) if packages(o)) {
            bail!("| {tool}: show packages available prints as it searches and cannot be filtered; add search terms instead");
        }
        bail!("| {tool}: only the output of a show command can be filtered");
    }
    Ok((op, filters))
}

pub fn parse_commit(w: &[String]) -> Result<CommitOptions> {
    let usage = || anyhow::anyhow!("commit: one of: commit, commit check, commit [confirmed [<minutes>]] [comment \"…\"]");
    if w.len() == 1 && w[0] == "check" {
        return Ok(CommitOptions { check: true, ..Default::default() });
    }
    let (mut confirm, mut comment, mut i) = (None, None, 0);
    while i < w.len() {
        match w[i].as_str() {
            "confirmed" if confirm.is_none() => {
                let mut m = 10;
                if let Some(n) = w.get(i + 1).filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())) {
                    m = n.parse().map_err(|_| anyhow::anyhow!("commit confirmed: '{n}' minutes is too many"))?;
                    i += 1;
                }
                if !(1..=1440).contains(&m) {
                    bail!("commit confirmed takes 1 to 1440 minutes, not {m}");
                }
                confirm = Some(m);
            }
            "comment" if comment.is_none() => {
                comment = Some(w.get(i + 1).ok_or_else(usage)?.clone());
                i += 1;
            }
            _ => return Err(usage()),
        }
        i += 1;
    }
    Ok(CommitOptions { check: false, confirm, comment })
}

/// The operational commands, the same words in both styles except `show running-config` (Cisco)
/// and `configure` / `config` (both accepted in both).
fn parse_operational(style: Style, w: &[String]) -> Result<Op> {
    let s: Vec<&str> = w.iter().map(String::as_str).collect();
    Ok(match s.as_slice() {
        ["configure" | "config"] | ["configure", "terminal"] => Op::Configure,
        ["exit" | "quit"] => Op::Exit,
        ["switch", "cli"] => Op::SwitchCli,
        ["show", "configuration", ..] if style == Style::Juniper => {
            let (path, pipes) = split_pipes(&w[2..])?;
            if pipes.compare {
                bail!("| compare is for the candidate, in configuration mode");
            }
            Op::ShowActive { path, pipes }
        }
        ["show", "running-config", ..] if style == Style::Cisco => {
            let (path, pipes) = split_pipes(&w[2..])?;
            if pipes.compare {
                bail!("| compare is for the candidate: `show configuration diff` in configuration mode");
            }
            Op::ShowActive { path, pipes }
        }
        ["show", "system", "commit"] => Op::ShowCommits,
        ["show", "system", "rollback", n] => Op::ShowRollback { n: number(n)?, compare: None },
        ["show", "system", "rollback", n, "compare", m] => Op::ShowRollback { n: number(n)?, compare: Some(number(m)?) },
        ["show", "system", "alarms"] => Op::ShowAlarms,
        ["show", "services"] => Op::ShowServices,
        ["show", "packages", "available", terms @ ..] if !terms.is_empty() => Op::ShowPackages { terms: w[3..].to_vec() },
        ["request", ..] => Op::Request { words: w[1..].to_vec() },
        [] => bail!("empty command"),
        [other, ..] => {
            let show = if style == Style::Cisco { "show running-config" } else { "show configuration" };
            bail!("unknown command: {other}; one of: configure, {show}, show system …, show services, request, switch cli, exit")
        }
    })
}

/// Parse one line's words in `style` and `mode`. `is_place(words)`: whether the words name a
/// container or list entry (somewhere to go), not a setting — what a Cisco-style bare path means
/// depends on it.
pub fn parse(style: Style, mode: Mode, w: &[String], is_place: &dyn Fn(&[String]) -> bool) -> Result<Op> {
    if w.is_empty() {
        bail!("empty command");
    }
    if mode == Mode::Operational {
        return parse_operational(style, w);
    }
    let s: Vec<&str> = w.iter().map(String::as_str).collect();
    match style {
        Style::Juniper => Ok(match s.as_slice() {
            ["set", ..] => Op::Set { words: w[1..].to_vec() },
            ["delete", .., "all"] => Op::Delete { words: w[1..w.len() - 1].to_vec(), all: true },
            ["delete", ..] => Op::Delete { words: w[1..].to_vec(), all: false },
            ["edit", ..] => Op::Edit { words: w[1..].to_vec() },
            ["up"] => Op::Up,
            ["top"] => Op::Top,
            ["show", "|", "compare"] => Op::Compare,
            ["show", ..] => {
                let (path, pipes) = split_pipes(&w[1..])?;
                if pipes.compare {
                    bail!("| compare compares the whole candidate: `show | compare`");
                }
                Op::ShowCandidate { path, pipes }
            }
            ["rollback"] => Op::Rollback { n: 0 },
            ["rollback", n] => Op::Rollback { n: number(n)? },
            ["commit", ..] => Op::Commit(parse_commit(&w[1..])?),
            ["run", ..] => Op::Run(Box::new(parse_operational(style, &w[1..])?)),
            ["exit", "configuration-mode"] => Op::End,
            ["exit" | "quit"] => Op::Exit,
            ["switch", "cli"] => Op::SwitchCli,
            [other, ..] => bail!("unknown command: {other}; one of: set, delete, edit, up, top, show, commit, rollback, run, exit"),
            [] => unreachable!(),
        }),
        Style::Cisco => Ok(match s.as_slice() {
            ["no", .., "all"] => Op::Delete { words: w[1..w.len() - 1].to_vec(), all: true },
            ["no", ..] => Op::Delete { words: w[1..].to_vec(), all: false },
            ["exit"] => Op::Exit,
            ["top"] => Op::Top,
            ["end"] => Op::End,
            ["show", "configuration", "diff"] => Op::Compare,
            ["show", "configuration", ..] => {
                let (path, pipes) = split_pipes(&w[2..])?;
                if pipes.compare {
                    bail!("the comparison is `show configuration diff`");
                }
                Op::ShowCandidate { path, pipes }
            }
            ["rollback", "configuration"] => Op::Rollback { n: 0 },
            ["rollback", "configuration", n] => Op::Rollback { n: number(n)? },
            ["commit", ..] => Op::Commit(parse_commit(&w[1..])?),
            ["do", ..] => Op::Run(Box::new(parse_operational(style, &w[1..])?)),
            ["switch", "cli"] => Op::SwitchCli,
            [first, ..] if RESERVED.contains(first) => {
                bail!("unknown command: {first}; in Cisco style: <path> [value], no <path>, exit, end, top, show configuration [diff], commit, rollback configuration <n>, do <command>")
            }
            // A bare path: a container or list entry is somewhere to go (a submode); anything else
            // sets a value.
            _ if is_place(w) => Op::Edit { words: w.to_vec() },
            _ => Op::Set { words: w.to_vec() },
        }),
    }
}

// Rendering (Op → text) is what the round-trip test checks the parser against; nothing else prints
// an Op yet, so it is compiled for the tests only.

#[cfg(test)]
fn pipes_text(p: &Pipes) -> String {
    let mut s = String::new();
    if p.compare {
        s.push_str(" | compare");
    }
    if p.display_set {
        s.push_str(" | display set");
    }
    s
}

#[cfg(test)]
fn join(w: &[String]) -> String {
    w.iter().map(|x| quote(x)).collect::<Vec<_>>().join(" ")
}

/// A word as it must be typed: quoted if it has a space or a character the line syntax uses.
pub fn quote(w: &str) -> String {
    if w.is_empty() || w.chars().any(|c| c.is_whitespace() || "\"'|;\\".contains(c)) {
        format!("\"{}\"", w.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        w.to_string()
    }
}

#[cfg(test)]
fn sep(prefix: &str, w: &[String]) -> String {
    if w.is_empty() { prefix.to_string() } else { format!("{prefix} {}", join(w)) }
}

/// How `op` is typed in `style`. The inverse of `parse` (tested).
#[cfg(test)]
pub fn render(style: Style, op: &Op) -> String {
    let commit = |o: &CommitOptions| {
        let mut s = String::from("commit");
        if o.check {
            s.push_str(" check");
        }
        if let Some(m) = o.confirm {
            s.push_str(&format!(" confirmed {m}"));
        }
        if let Some(c) = &o.comment {
            s.push_str(&format!(" comment {}", quote(c)));
        }
        s
    };
    match (style, op) {
        (_, Op::Configure) => "configure".into(),
        (Style::Juniper, Op::ShowActive { path, pipes }) => format!("{}{}", sep("show configuration", path), pipes_text(pipes)),
        (Style::Cisco, Op::ShowActive { path, pipes }) => format!("{}{}", sep("show running-config", path), pipes_text(pipes)),
        (_, Op::ShowCommits) => "show system commit".into(),
        (_, Op::ShowRollback { n, compare: None }) => format!("show system rollback {n}"),
        (_, Op::ShowRollback { n, compare: Some(m) }) => format!("show system rollback {n} compare {m}"),
        (_, Op::ShowAlarms) => "show system alarms".into(),
        (_, Op::ShowServices) => "show services".into(),
        (_, Op::ShowPackages { terms }) => sep("show packages available", terms),
        (_, Op::Request { words }) => sep("request", words),
        (Style::Juniper, Op::Set { words }) => sep("set", words),
        (Style::Cisco, Op::Set { words }) => join(words),
        (Style::Juniper, Op::Delete { words, all }) => format!("{}{}", sep("delete", words), if *all { " all" } else { "" }),
        (Style::Cisco, Op::Delete { words, all }) => format!("{}{}", sep("no", words), if *all { " all" } else { "" }),
        (Style::Juniper, Op::Edit { words }) => sep("edit", words),
        (Style::Cisco, Op::Edit { words }) => join(words),
        (Style::Juniper, Op::Up) => "up".into(),
        (Style::Cisco, Op::Up) => "exit".into(),
        (_, Op::Top) => "top".into(),
        (Style::Juniper, Op::ShowCandidate { path, pipes }) => format!("{}{}", sep("show", path), pipes_text(pipes)),
        (Style::Cisco, Op::ShowCandidate { path, pipes }) => format!("{}{}", sep("show configuration", path), pipes_text(pipes)),
        (Style::Juniper, Op::Compare) => "show | compare".into(),
        (Style::Cisco, Op::Compare) => "show configuration diff".into(),
        (Style::Juniper, Op::Rollback { n }) => format!("rollback {n}"),
        (Style::Cisco, Op::Rollback { n }) => format!("rollback configuration {n}"),
        (_, Op::Commit(o)) => commit(o),
        (Style::Juniper, Op::Run(op)) => format!("run {}", render(style, op)),
        (Style::Cisco, Op::Run(op)) => format!("do {}", render(style, op)),
        (_, Op::Exit) => "exit".into(),
        (Style::Juniper, Op::End) => "exit configuration-mode".into(),
        (Style::Cisco, Op::End) => "end".into(),
        (_, Op::SwitchCli) => "switch cli".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &str) -> Vec<String> {
        crate::session::words(s).unwrap()
    }
    /// The fixture schema's places (containers and list entries), as the session decides it.
    fn is_place(words: &[String]) -> bool {
        let s = crate::tree::Schema::parse(include_str!("schema-fixture.json")).unwrap();
        s.resolve(words).is_ok_and(|t| t.values.is_empty() && !t.node.is_empty() && matches!(s.nodes[&t.node].kind, crate::tree::Kind::Container | crate::tree::Kind::List))
    }
    fn p(style: Style, mode: Mode, line: &str) -> Op {
        parse(style, mode, &w(line), &is_place).unwrap_or_else(|e| panic!("{line}: {e:#}"))
    }

    /// Every operation, in both modes it exists in.
    fn ops() -> Vec<(Mode, Op)> {
        let v = |s: &str| w(s);
        let pipes = Pipes { compare: false, display_set: true };
        vec![
            (Mode::Operational, Op::Configure),
            (Mode::Operational, Op::ShowActive { path: vec![], pipes: Pipes::default() }),
            (Mode::Operational, Op::ShowActive { path: v("users dev"), pipes: pipes.clone() }),
            (Mode::Operational, Op::ShowCommits),
            (Mode::Operational, Op::ShowRollback { n: 3, compare: None }),
            (Mode::Operational, Op::ShowRollback { n: 0, compare: Some(2) }),
            (Mode::Operational, Op::ShowAlarms),
            (Mode::Operational, Op::ShowServices),
            (Mode::Operational, Op::ShowPackages { terms: v("rip grep") }),
            (Mode::Operational, Op::Request { words: v("system repair") }),
            (Mode::Operational, Op::Exit),
            (Mode::Operational, Op::SwitchCli),
            (Mode::Configuration, Op::Set { words: v("system host-name torq") }),
            (Mode::Configuration, Op::Set { words: v("system packages [ htop vim ]") }),
            (Mode::Configuration, Op::Set { words: v("users dev cli style cisco") }),
            (Mode::Configuration, Op::Delete { words: v("system packages htop"), all: false }),
            (Mode::Configuration, Op::Delete { words: v("system"), all: true }),
            (Mode::Configuration, Op::Edit { words: v("system") }),
            (Mode::Configuration, Op::Edit { words: v("users dev") }),
            (Mode::Configuration, Op::Top),
            (Mode::Configuration, Op::ShowCandidate { path: vec![], pipes: Pipes::default() }),
            (Mode::Configuration, Op::ShowCandidate { path: v("system"), pipes: pipes.clone() }),
            (Mode::Configuration, Op::Compare),
            (Mode::Configuration, Op::Rollback { n: 2 }),
            (Mode::Configuration, Op::Commit(CommitOptions::default())),
            (Mode::Configuration, Op::Commit(CommitOptions { check: true, ..Default::default() })),
            (Mode::Configuration, Op::Commit(CommitOptions { check: false, confirm: Some(5), comment: Some("ssh; keys | only \"x\"".into()) })),
            (Mode::Configuration, Op::Run(Box::new(Op::ShowAlarms))),
            (Mode::Configuration, Op::Run(Box::new(Op::ShowActive { path: v("system"), pipes: Pipes::default() }))),
            (Mode::Configuration, Op::Exit),
            (Mode::Configuration, Op::End),
            (Mode::Configuration, Op::SwitchCli),
        ]
    }

    /// One grammar, two renderings: every operation rendered in either style parses back to itself,
    /// so the two styles say exactly the same things.
    #[test]
    fn every_op_round_trips_in_both_styles() {
        for (mode, op) in ops() {
            for style in [Style::Juniper, Style::Cisco] {
                let line = render(style, &op);
                assert_eq!(p(style, mode, &line), op, "{} {line:?}", style.name());
            }
        }
        // Juniper's `up` has no Cisco word of its own (Cisco's `exit` goes up, or out at the top).
        assert_eq!(p(Style::Juniper, Mode::Configuration, "up"), Op::Up);
    }

    #[test]
    fn the_same_change_in_each_style() {
        for (j, c) in [
            ("set system packages jq", "system packages jq"),
            ("delete system packages jq", "no system packages jq"),
            ("edit system", "system"),
            ("show | compare", "show configuration diff"),
            ("rollback 1", "rollback configuration 1"),
            ("run show system alarms", "do show system alarms"),
            ("exit configuration-mode", "end"),
        ] {
            assert_eq!(p(Style::Juniper, Mode::Configuration, j), p(Style::Cisco, Mode::Configuration, c), "{j} / {c}");
        }
        assert_eq!(p(Style::Juniper, Mode::Operational, "show configuration system"), p(Style::Cisco, Mode::Operational, "show running-config system"));
    }

    /// In Cisco style a line's first word is a command or a path, never both: the reserved words
    /// are refused as paths (and Schema::parse refuses a setting named one of them).
    #[test]
    fn command_words_are_never_paths() {
        for line in ["set system host-name x", "edit system", "delete system packages", "up", "show system", "run show system alarms"] {
            let e = format!("{:#}", parse(Style::Cisco, Mode::Configuration, &w(line), &is_place).unwrap_err());
            assert!(e.contains("in Cisco style"), "{line}: {e}");
        }
        let e = format!("{:#}", parse(Style::Juniper, Mode::Configuration, &w("system host-name x"), &is_place).unwrap_err());
        assert!(e.contains("unknown command: system"), "{e}");
    }

    fn pl(style: Style, mode: Mode, line: &str) -> Result<(Op, Vec<Filter>)> {
        parse_line(style, mode, &w(line), &is_place)
    }
    fn err(style: Style, mode: Mode, line: &str) -> String {
        format!("{:#}", pl(style, mode, line).unwrap_err())
    }
    fn f(tool: &str, args: &str) -> Filter {
        Filter { tool: tool.into(), args: w(args) }
    }

    /// Every show, with filters after it, in either style parses back to itself and its filters.
    #[test]
    fn filters_follow_every_show_in_both_styles() {
        let filters = vec![f("grep", "-Ei \"host|name\""), f("sort", "-r"), f("wc", "-l")];
        let mut n = 0;
        for (mode, op) in ops().into_iter().filter(|(_, op)| shows(op)) {
            for style in [Style::Juniper, Style::Cisco] {
                let line = format!("{} | grep -Ei \"host|name\" | sort -r | wc -l", render(style, &op));
                assert_eq!(pl(style, mode, &line).unwrap_or_else(|e| panic!("{line}: {e:#}")), (op.clone(), filters.clone()), "{line}");
                n += 1;
            }
        }
        assert!(n >= 16, "the shows are all there ({n})");
    }

    #[test]
    fn filters_come_after_the_commands_own_pipes() {
        let (j, o, c) = (Style::Juniper, Mode::Operational, Mode::Configuration);
        assert_eq!(pl(j, o, "show configuration | grep 'a b'").unwrap().1, [f("grep", "\"a b\"")]);
        assert_eq!(pl(j, o, "show configuration | grep \"a|b\"").unwrap().1, [f("grep", "\"a|b\"")], "a quoted | is the pattern's");
        assert_eq!(pl(j, c, "show | compare | grep x").unwrap(), (Op::Compare, vec![f("grep", "x")]));
        let (op, fs) = pl(j, c, "show system | display set | wc -l").unwrap();
        assert_eq!((op, fs), (Op::ShowCandidate { path: w("system"), pipes: Pipes { compare: false, display_set: true } }, vec![f("wc", "-l")]));
        assert_eq!(pl(Style::Cisco, c, "show configuration diff | grep x").unwrap().0, Op::Compare);
        assert_eq!(pl(Style::Cisco, c, "do show system alarms | wc -l").unwrap().0, Op::Run(Box::new(Op::ShowAlarms)));
        assert_eq!(pl(j, c, "show").unwrap().1, [], "no pipe, no filter");
        for (line, want) in [
            ("show configuration | grep a | display set", "| display set must come before | grep"),
            ("show configuration | grep x |", "a | needs something after it"),
            ("show configuration |", "a | needs something after it"),
            ("show configuration | sed s/a/b/", "unknown pipe: | sed s/a/b/; one of: compare, display set, or a filter: cut, grep"),
            ("show configuration | sh -c x", "unknown pipe: | sh -c x"),
            ("show packages available rg | grep x", "cannot be filtered"),
        ] {
            let e = err(j, o, line);
            assert!(e.contains(want), "{line}: {e}");
        }
        for line in ["set system host-name a | wc -l", "commit | grep x", "rollback 1 | head", "run show packages available x | wc -l"] {
            let e = err(j, c, line);
            assert!(e.contains("filtered"), "{line}: {e}");
        }
        let e = err(Style::Cisco, c, "system host-name a | grep a");
        assert!(e.contains("only the output of a show command"), "{e}");
        let many = format!("show configuration{}", " | wc -l".repeat(MAX_FILTERS));
        assert!(pl(j, o, &many).is_ok());
        let e = err(j, o, &format!("{many} | wc -l"));
        assert!(e.contains("at most 16 filters"), "{e}");
    }
}
