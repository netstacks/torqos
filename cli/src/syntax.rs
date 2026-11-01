//! torqcli's commands as one model (`Op`) and one syntax (ADR-016, ADR-021): a line parses to an
//! `Op`. The session executes `Op`s; nothing else knows the syntax. A property test
//! checks that every `Op`, rendered as it is typed, parses back to itself.

use crate::session::Mode;
use crate::filter::Filter;
use anyhow::{bail, Context, Result};

/// A word with a meaning of its own after a path (`set <place> config <file>`): no list entry may be
/// named it (settings.rs, tree.rs), so a line never means two things.
pub const RESERVED: &[&str] = &["config"];

/// `| display …` after a `show`: what each view offers (CLI-DESIGN principle 5: internals only when
/// asked).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Display {
    /// A configuration as `set` commands.
    Set,
    /// With what is behind it: the files, commits, store paths.
    Detail,
    /// A comparison as the raw diff of the files (what the broker records).
    Diff,
}

impl Display {
    fn name(self) -> &'static str {
        match self {
            Display::Set => "set",
            Display::Detail => "detail",
            Display::Diff => "diff",
        }
    }
}

/// The displays a configuration, a comparison, and the commit list or alarms offer.
const OF_CONFIGURATION: &[Display] = &[Display::Set, Display::Detail];
const OF_COMPARISON: &[Display] = &[Display::Diff];
const OF_DETAIL: &[Display] = &[Display::Detail];

/// `d` if `what` offers it.
fn offered(what: &str, allowed: &[Display], d: Option<Display>) -> Result<Option<Display>> {
    match d {
        Some(d) if !allowed.contains(&d) => {
            let names: Vec<String> = allowed.iter().map(|a| format!("| display {}", a.name())).collect();
            bail!("| display {}: not for {what} (it has {})", d.name(), names.join(", "))
        }
        d => Ok(d),
    }
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
    /// `show configuration [path] [| display set|detail]`.
    ShowActive { path: Vec<String>, display: Option<Display> },
    ShowCommits { display: Option<Display> },
    ShowRollback { n: usize, compare: Option<usize>, display: Option<Display> },
    ShowAlarms { display: Option<Display> },
    ShowServices,
    /// `show system ledger [<count>]`: the ledger itself — every change, refused and failed ones too.
    ShowLedger { count: Option<usize> },
    ShowPackages { terms: Vec<String> },
    Request { words: Vec<String> },
    /// `file show|edit|delete <path>`, `file list`: the config repo's config files (in configuration
    /// mode, the candidate's; in operational mode, the active ones; `edit` and `delete` only there).
    File { action: FileAction, path: Option<String> },
    /// `save <file>`: the whole candidate, and its config files' content, to a file (saved.rs).
    Save { file: String },
    /// `load override|merge|set <file>|terminal`: a saved configuration into the candidate, all or
    /// nothing.
    Load { how: LoadHow, from: LoadFrom },
    // Configuration mode.
    /// A leaf's value or leaf-list elements: the words after `set`.
    Set { words: Vec<String> },
    /// `delete <path>`: a setting, a value of a list, or everything below a place.
    Delete { words: Vec<String> },
    /// `edit <path>`: go down to a container or list entry.
    Edit { words: Vec<String> },
    Up,
    Top,
    /// `show [path]`: the candidate.
    ShowCandidate { path: Vec<String>, display: Option<Display> },
    /// `show | compare`: what the candidate changes.
    Compare { display: Option<Display> },
    Rollback { n: usize },
    Commit(CommitOptions),
    /// `run <command>`: an operational command from configuration mode.
    Run(Box<Op>),
    /// `exit`: up one level, or out of configuration mode at the top; in operational mode, leave
    /// torqcli.
    Exit,
    /// `exit configuration-mode`: out of configuration mode from anywhere.
    End,
}

/// How `load` takes a saved configuration: as the whole candidate, onto it, or as `set` lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadHow {
    Override,
    Merge,
    Set,
}

/// Where `load` reads from: a file, or what is pasted at the terminal (`terminal`, ended by Ctrl-D;
/// a file of that name is `./terminal`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadFrom {
    File(String),
    Terminal,
}

fn parse_load(w: &[String]) -> Result<Op> {
    let s: Vec<&str> = w.iter().map(String::as_str).collect();
    let how = match s.as_slice() {
        ["override", _] => LoadHow::Override,
        ["merge", _] => LoadHow::Merge,
        ["set", _] => LoadHow::Set,
        _ => bail!("load: one of: load override <file>|terminal (it becomes the candidate), load merge <file>|terminal (onto it), load set <file>|terminal (set and delete lines)"),
    };
    let from = if w[1] == "terminal" { LoadFrom::Terminal } else { LoadFrom::File(w[1].clone()) };
    Ok(Op::Load { how, from })
}

/// What `file` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileAction {
    Show,
    Edit,
    Delete,
    List,
}

fn parse_file(w: &[String], mode: Mode) -> Result<Op> {
    let s: Vec<&str> = w.iter().map(String::as_str).collect();
    let (action, path) = match s.as_slice() {
        ["show", p] => (FileAction::Show, Some(p.to_string())),
        ["edit", p] if mode == Mode::Configuration => (FileAction::Edit, Some(p.to_string())),
        ["delete", p] if mode == Mode::Configuration => (FileAction::Delete, Some(p.to_string())),
        ["list"] => (FileAction::List, None),
        ["edit" | "delete", _] => bail!("file edit and file delete change the candidate: in configuration mode"),
        _ => bail!("file: one of: file list, file show <path>, file edit <path>, file delete <path>"),
    };
    Ok(Op::File { action, path })
}

/// The highest rollback number (how far back `rollback` and `show system rollback` reach).
pub const MAX_ROLLBACK: usize = 10_000;

pub fn number(n: &str) -> Result<usize> {
    let n: usize = n.parse().with_context(|| format!("'{n}' is not a number"))?;
    if n > MAX_ROLLBACK {
        bail!("rollback numbers go up to {MAX_ROLLBACK}");
    }
    Ok(n)
}

/// A show's words and its own pipes: the words before them, whether `| compare` came, and the
/// `| display` (one).
fn split_pipes(w: &[String]) -> Result<(Vec<String>, bool, Option<Display>)> {
    let mut parts = w.split(|x| x == "|");
    let head = parts.next().unwrap_or_default().to_vec();
    let (mut compare, mut display) = (false, None);
    for part in parts {
        let d = match part.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
            ["compare"] if !compare => {
                compare = true;
                continue;
            }
            ["display", "set"] => Display::Set,
            ["display", "detail"] => Display::Detail,
            ["display", "diff"] => Display::Diff,
            other => bail!(
                "unknown pipe: | {}; one of: compare, display set|detail|diff, or a filter: {}",
                other.join(" "),
                crate::filter::names()
            ),
        };
        if let Some(was) = display.replace(d) {
            bail!("one | display at a time (| display {} and | display {})", was.name(), d.name());
        }
    }
    Ok((head, compare, display))
}

/// The pipes after a show that takes no more words (`show system commit [| display detail]`).
fn view_pipes(what: &str, w: &[String], allowed: &[Display]) -> Result<Option<Display>> {
    let (rest, compare, display) = split_pipes(w)?;
    if let Some(x) = rest.first() {
        bail!("{what}: unexpected '{x}'");
    }
    if compare {
        bail!("| compare is for the candidate, in configuration mode");
    }
    offered(what, allowed, display)
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
pub fn shows(op: &Op) -> bool {
    match op {
        Op::ShowActive { .. } | Op::ShowCommits { .. } | Op::ShowRollback { .. } | Op::ShowAlarms { .. } | Op::ShowServices | Op::ShowLedger { .. } => true,
        Op::ShowCandidate { .. } | Op::Compare { .. } => true,
        Op::Run(op) => shows(op),
        _ => false,
    }
}

/// Parse one line: the command (`parse`) and the filters after it.
pub fn parse_line(mode: Mode, w: &[String]) -> Result<(Op, Vec<Filter>)> {
    let (head, filters) = split_filters(w)?;
    let op = parse(mode, &head)?;
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

/// The operational commands (`configure` and `config` both enter configuration mode).
fn parse_operational(w: &[String]) -> Result<Op> {
    let s: Vec<&str> = w.iter().map(String::as_str).collect();
    Ok(match s.as_slice() {
        ["configure" | "config"] | ["configure", "terminal"] => Op::Configure,
        ["exit" | "quit"] => Op::Exit,
        ["show", "configuration", ..] => {
            let (path, compare, display) = split_pipes(&w[2..])?;
            if compare {
                bail!("| compare is for the candidate, in configuration mode");
            }
            Op::ShowActive { path, display: offered("a configuration", OF_CONFIGURATION, display)? }
        }
        ["show", "system", "commit", ..] => Op::ShowCommits { display: view_pipes("show system commit", &w[3..], OF_DETAIL)? },
        ["show", "system", "rollback", ..] => {
            let bar = w.iter().position(|x| x == "|").unwrap_or(w.len());
            let (n, compare) = match &s[3..bar] {
                [n] => (number(n)?, None),
                [n, "compare", m] => (number(n)?, Some(number(m)?)),
                _ => bail!("show system rollback <n> [compare <m>]"),
            };
            let (what, allowed) = if compare.is_some() { ("a comparison", OF_COMPARISON) } else { ("a configuration", OF_CONFIGURATION) };
            Op::ShowRollback { n, compare, display: view_pipes(what, &w[bar..], allowed)? }
        }
        ["show", "system", "alarms", ..] => Op::ShowAlarms { display: view_pipes("show system alarms", &w[3..], OF_DETAIL)? },
        ["show", "services"] => Op::ShowServices,
        ["show", "system", "ledger"] => Op::ShowLedger { count: None },
        ["show", "system", "ledger", n] => Op::ShowLedger { count: Some(n.parse().ok().filter(|n| (1..=10_000).contains(n)).with_context(|| format!("show system ledger <count>: 1 to 10000, not '{n}'"))?) },
        ["show", "packages", "available", terms @ ..] if !terms.is_empty() => Op::ShowPackages { terms: w[3..].to_vec() },
        ["request", ..] => Op::Request { words: w[1..].to_vec() },
        ["file", ..] => parse_file(&w[1..], Mode::Operational)?,
        [] => bail!("empty command"),
        [other, ..] => bail!("unknown command: {other}; one of: configure, show configuration, show system …, show services, request, file, exit"),
    })
}

/// Parse one line's words in `mode`.
pub fn parse(mode: Mode, w: &[String]) -> Result<Op> {
    if w.is_empty() {
        bail!("empty command");
    }
    if mode == Mode::Operational {
        return parse_operational(w);
    }
    let s: Vec<&str> = w.iter().map(String::as_str).collect();
    Ok(match s.as_slice() {
        ["set", ..] => Op::Set { words: w[1..].to_vec() },
        ["delete", ..] => Op::Delete { words: w[1..].to_vec() },
        ["edit", ..] => Op::Edit { words: w[1..].to_vec() },
        ["up"] => Op::Up,
        ["top"] => Op::Top,
        ["show", ..] => {
            let (path, compare, display) = split_pipes(&w[1..])?;
            match (compare, path.is_empty()) {
                (true, true) => Op::Compare { display: offered("a comparison", OF_COMPARISON, display)? },
                (true, false) => bail!("| compare compares the whole candidate: `show | compare`"),
                (false, _) => Op::ShowCandidate { path, display: offered("a configuration", OF_CONFIGURATION, display)? },
            }
        }
        ["save", f] => Op::Save { file: f.to_string() },
        ["save", ..] => bail!("save <file>: one file (the whole candidate is saved, from the top)"),
        ["load", ..] => parse_load(&w[1..])?,
        ["file", ..] => parse_file(&w[1..], mode)?,
        ["rollback"] => Op::Rollback { n: 0 },
        ["rollback", n] => Op::Rollback { n: number(n)? },
        ["commit", ..] => Op::Commit(parse_commit(&w[1..])?),
        ["run", ..] => Op::Run(Box::new(parse_operational(&w[1..])?)),
        ["exit", "configuration-mode"] => Op::End,
        ["exit" | "quit"] => Op::Exit,
        [other, ..] => bail!("unknown command: {other}; one of: set, delete, edit, up, top, show, commit, rollback, run, save, load, file, exit"),
        [] => unreachable!(),
    })
}

// Rendering (Op → text) is what the round-trip test checks the parser against; nothing else prints
// an Op yet, so it is compiled for the tests only.

#[cfg(test)]
fn display_text(d: &Option<Display>) -> String {
    d.map(|d| format!(" | display {}", d.name())).unwrap_or_default()
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

/// How `op` is typed. The inverse of `parse` (tested).
#[cfg(test)]
pub fn render(op: &Op) -> String {
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
    match op {
        Op::Configure => "configure".into(),
        Op::ShowActive { path, display } => format!("{}{}", sep("show configuration", path), display_text(display)),
        Op::ShowCommits { display } => format!("show system commit{}", display_text(display)),
        Op::ShowRollback { n, compare: None, display } => format!("show system rollback {n}{}", display_text(display)),
        Op::ShowRollback { n, compare: Some(m), display } => format!("show system rollback {n} compare {m}{}", display_text(display)),
        Op::ShowAlarms { display } => format!("show system alarms{}", display_text(display)),
        Op::ShowServices => "show services".into(),
        Op::ShowLedger { count: None } => "show system ledger".into(),
        Op::ShowLedger { count: Some(n) } => format!("show system ledger {n}"),
        Op::ShowPackages { terms } => sep("show packages available", terms),
        Op::Request { words } => sep("request", words),
        Op::Save { file } => format!("save {}", quote(file)),
        Op::Load { how, from } => {
            let h = match how {
                LoadHow::Override => "override",
                LoadHow::Merge => "merge",
                LoadHow::Set => "set",
            };
            match from {
                LoadFrom::File(f) => format!("load {h} {}", quote(f)),
                LoadFrom::Terminal => format!("load {h} terminal"),
            }
        }
        Op::File { action, path } => {
            let a = match action {
                FileAction::Show => "show",
                FileAction::Edit => "edit",
                FileAction::Delete => "delete",
                FileAction::List => "list",
            };
            sep(&format!("file {a}"), &path.iter().cloned().collect::<Vec<_>>())
        }
        Op::Set { words } => sep("set", words),
        Op::Delete { words } => sep("delete", words),
        Op::Edit { words } => sep("edit", words),
        Op::Up => "up".into(),
        Op::Top => "top".into(),
        Op::ShowCandidate { path, display } => format!("{}{}", sep("show", path), display_text(display)),
        Op::Compare { display } => format!("show | compare{}", display_text(display)),
        Op::Rollback { n } => format!("rollback {n}"),
        Op::Commit(o) => commit(o),
        Op::Run(op) => format!("run {}", render(op)),
        Op::Exit => "exit".into(),
        Op::End => "exit configuration-mode".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &str) -> Vec<String> {
        crate::session::words(s).unwrap()
    }
    fn p(mode: Mode, line: &str) -> Op {
        parse(mode, &w(line)).unwrap_or_else(|e| panic!("{line}: {e:#}"))
    }

    /// Every operation, in both modes it exists in.
    fn ops() -> Vec<(Mode, Op)> {
        let v = |s: &str| w(s);
        let (set, detail, diff) = (Some(Display::Set), Some(Display::Detail), Some(Display::Diff));
        vec![
            (Mode::Operational, Op::Configure),
            (Mode::Operational, Op::ShowActive { path: vec![], display: None }),
            (Mode::Operational, Op::ShowActive { path: v("users dev"), display: set }),
            (Mode::Operational, Op::ShowActive { path: v("system"), display: detail }),
            (Mode::Operational, Op::ShowCommits { display: None }),
            (Mode::Operational, Op::ShowCommits { display: detail }),
            (Mode::Operational, Op::ShowRollback { n: 3, compare: None, display: None }),
            (Mode::Operational, Op::ShowRollback { n: 3, compare: None, display: detail }),
            (Mode::Operational, Op::ShowRollback { n: 0, compare: Some(2), display: None }),
            (Mode::Operational, Op::ShowRollback { n: 0, compare: Some(2), display: diff }),
            (Mode::Operational, Op::ShowAlarms { display: None }),
            (Mode::Operational, Op::ShowAlarms { display: detail }),
            (Mode::Operational, Op::ShowServices),
            (Mode::Operational, Op::ShowLedger { count: None }),
            (Mode::Operational, Op::ShowLedger { count: Some(5) }),
            (Mode::Operational, Op::ShowPackages { terms: v("rip grep") }),
            (Mode::Operational, Op::Request { words: v("system repair") }),
            (Mode::Operational, Op::Exit),
            (Mode::Configuration, Op::Set { words: v("system host-name torq") }),
            (Mode::Configuration, Op::Set { words: v("system packages [ htop vim ]") }),
            (Mode::Configuration, Op::Set { words: v("users dev packages jq") }),
            (Mode::Configuration, Op::Delete { words: v("system packages htop") }),
            (Mode::Configuration, Op::Delete { words: v("system") }),
            (Mode::Configuration, Op::Delete { words: vec![] }),
            (Mode::Configuration, Op::Edit { words: v("system") }),
            (Mode::Configuration, Op::Edit { words: v("users dev") }),
            (Mode::Configuration, Op::Up),
            (Mode::Configuration, Op::Top),
            (Mode::Configuration, Op::ShowCandidate { path: vec![], display: None }),
            (Mode::Configuration, Op::ShowCandidate { path: v("system"), display: set }),
            (Mode::Configuration, Op::ShowCandidate { path: vec![], display: detail }),
            (Mode::Configuration, Op::Compare { display: None }),
            (Mode::Configuration, Op::Compare { display: diff }),
            (Mode::Configuration, Op::Rollback { n: 2 }),
            (Mode::Configuration, Op::Save { file: "/home/dev/my device.conf".into() }),
            (Mode::Configuration, Op::Load { how: LoadHow::Override, from: LoadFrom::File("a.conf".into()) }),
            (Mode::Configuration, Op::Load { how: LoadHow::Merge, from: LoadFrom::File("a.conf".into()) }),
            (Mode::Configuration, Op::Load { how: LoadHow::Set, from: LoadFrom::File("a.set".into()) }),
            (Mode::Configuration, Op::Load { how: LoadHow::Merge, from: LoadFrom::Terminal }),
            (Mode::Configuration, Op::File { action: FileAction::Edit, path: Some("custom/a.nix".into()) }),
            (Mode::Configuration, Op::File { action: FileAction::List, path: None }),
            (Mode::Operational, Op::File { action: FileAction::Show, path: Some("a.nix".into()) }),
            (Mode::Configuration, Op::Commit(CommitOptions::default())),
            (Mode::Configuration, Op::Commit(CommitOptions { check: true, ..Default::default() })),
            (Mode::Configuration, Op::Commit(CommitOptions { check: false, confirm: Some(5), comment: Some("ssh; keys | only \"x\"".into()) })),
            (Mode::Configuration, Op::Run(Box::new(Op::ShowAlarms { display: None }))),
            (Mode::Configuration, Op::Run(Box::new(Op::ShowActive { path: v("system"), display: None }))),
            (Mode::Configuration, Op::Run(Box::new(Op::ShowRollback { n: 1, compare: Some(0), display: diff }))),
            (Mode::Configuration, Op::Exit),
            (Mode::Configuration, Op::End),
        ]
    }

    /// Every operation, rendered as it is typed, parses back to itself.
    #[test]
    fn every_op_round_trips() {
        for (mode, op) in ops() {
            let line = render(&op);
            assert_eq!(p(mode, &line), op, "{line:?}");
        }
    }

    /// One syntax: a line is a command, never a bare path; the other syntax's words are unknown.
    #[test]
    fn commands_only() {
        for (mode, line, want) in [
            (Mode::Configuration, "system host-name x", "unknown command: system"),
            (Mode::Configuration, "no system packages jq", "unknown command: no"),
            (Mode::Configuration, "do show system alarms", "unknown command: do"),
            (Mode::Configuration, "end", "unknown command: end"),
            (Mode::Configuration, "switch cli", "unknown command: switch"),
            (Mode::Operational, "show running-config", "unknown command: show"),
            (Mode::Operational, "switch cli", "unknown command: switch"),
        ] {
            let e = format!("{:#}", parse(mode, &w(line)).unwrap_err());
            assert!(e.contains(want), "{line}: {e}");
        }
    }

    fn pl(mode: Mode, line: &str) -> Result<(Op, Vec<Filter>)> {
        parse_line(mode, &w(line))
    }
    fn err(mode: Mode, line: &str) -> String {
        format!("{:#}", pl(mode, line).unwrap_err())
    }
    fn f(tool: &str, args: &str) -> Filter {
        Filter { tool: tool.into(), args: w(args) }
    }

    /// Every show, with filters after it, parses back to itself and its filters.
    #[test]
    fn filters_follow_every_show() {
        let filters = vec![f("grep", "-Ei \"host|name\""), f("sort", "-r"), f("wc", "-l")];
        let mut n = 0;
        for (mode, op) in ops().into_iter().filter(|(_, op)| shows(op)) {
            let line = format!("{} | grep -Ei \"host|name\" | sort -r | wc -l", render(&op));
            assert_eq!(pl(mode, &line).unwrap_or_else(|e| panic!("{line}: {e:#}")), (op.clone(), filters.clone()), "{line}");
            n += 1;
        }
        assert!(n >= 8, "the shows are all there ({n})");
    }

    #[test]
    fn each_view_has_its_own_displays() {
        let (o, c) = (Mode::Operational, Mode::Configuration);
        for (mode, line, want) in [
            (o, "show system commit | display set", "| display set: not for show system commit (it has | display detail)"),
            (o, "show system alarms | display diff", "not for show system alarms"),
            (o, "show configuration | display diff", "not for a configuration (it has | display set, | display detail)"),
            (o, "show system rollback 1 compare 0 | display set", "not for a comparison (it has | display diff)"),
            (o, "show configuration | display set | display detail", "one | display at a time"),
            (o, "show system commit extra", "show system commit: unexpected 'extra'"),
            (o, "show system rollback 1 compare", "show system rollback <n> [compare <m>]"),
            (c, "show | compare | display set", "not for a comparison"),
            (c, "show | display diff", "not for a configuration"),
        ] {
            let e = err(mode, line);
            assert!(e.contains(want), "{line}: {e}");
        }
    }

    #[test]
    fn filters_come_after_the_commands_own_pipes() {
        let (o, c) = (Mode::Operational, Mode::Configuration);
        assert_eq!(pl(o, "show configuration | grep 'a b'").unwrap().1, [f("grep", "\"a b\"")]);
        assert_eq!(pl(o, "show configuration | grep \"a|b\"").unwrap().1, [f("grep", "\"a|b\"")], "a quoted | is the pattern's");
        assert_eq!(pl(c, "show | compare | grep x").unwrap(), (Op::Compare { display: None }, vec![f("grep", "x")]));
        let (op, fs) = pl(c, "show system | display set | wc -l").unwrap();
        assert_eq!((op, fs), (Op::ShowCandidate { path: w("system"), display: Some(Display::Set) }, vec![f("wc", "-l")]));
        assert_eq!(pl(c, "run show system alarms | wc -l").unwrap().0, Op::Run(Box::new(Op::ShowAlarms { display: None })));
        assert_eq!(pl(c, "show").unwrap().1, [], "no pipe, no filter");
        for (line, want) in [
            ("show configuration | grep a | display set", "| display set must come before | grep"),
            ("show configuration | grep x |", "a | needs something after it"),
            ("show configuration |", "a | needs something after it"),
            ("show configuration | sed s/a/b/", "unknown pipe: | sed s/a/b/; one of: compare, display set|detail|diff, or a filter: cut, grep"),
            ("show configuration | sh -c x", "unknown pipe: | sh -c x"),
            ("show packages available rg | grep x", "cannot be filtered"),
        ] {
            let e = err(o, line);
            assert!(e.contains(want), "{line}: {e}");
        }
        for line in ["set system host-name a | wc -l", "commit | grep x", "rollback 1 | head", "run show packages available x | wc -l"] {
            let e = err(c, line);
            assert!(e.contains("filtered"), "{line}: {e}");
        }
        let many = format!("show configuration{}", " | wc -l".repeat(MAX_FILTERS));
        assert!(pl(o, &many).is_ok());
        let e = err(o, &format!("{many} | wc -l"));
        assert!(e.contains("at most 16 filters"), "{e}");
    }
}
