//! `?` and Tab for torqcli (ADR-016): what may come next at a point of a command line, with one
//! line of help each, from the commands of the mode and the schema. Pure: the line editor (shell.rs)
//! asks, this answers.

use crate::session::Mode;
use crate::syntax::Style;
use crate::tree::{Kind, Schema};

/// One possible next word: the word, or a `<placeholder>` for a value of a type (not insertable),
/// and its help.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    pub word: String,
    pub help: String,
}

impl Choice {
    fn new(word: &str, help: &str) -> Self {
        Choice { word: word.into(), help: help.into() }
    }
    /// A placeholder, not a word to insert.
    pub fn is_placeholder(&self) -> bool {
        self.word.starts_with('<')
    }
}

const OPERATIONAL: &[(&str, &str)] = &[
    ("configure", "Edit the configuration (a candidate, until you commit it)"),
    ("request", "Ask the machine to do something"),
    ("show", "Show information about the machine"),
    ("exit", "Leave torqcli"),
];
const CONFIGURATION: &[(&str, &str)] = &[
    ("commit", "Make the candidate the machine's configuration"),
    ("delete", "Remove a setting"),
    ("edit", "Go down to a part of the configuration"),
    ("exit", "Up one level (out of configuration mode at the top; the candidate is kept)"),
    ("quit", "Up one level (out of configuration mode at the top; the candidate is kept)"),
    ("rollback", "Load an earlier configuration into the candidate"),
    ("run", "Run an operational-mode command"),
    ("set", "Set a value"),
    ("show", "Show the candidate configuration"),
    ("switch", "Switch to the Cisco style"),
    ("top", "Back to the top of the configuration"),
    ("up", "Up one level"),
];

/// What may follow `words` (the complete words before the cursor), each starting with `partial`
/// (the word being typed, possibly empty).
#[cfg(test)]
pub fn choices(schema: &Schema, mode: Mode, words: &[String], partial: &str) -> Vec<Choice> {
    choices_at(schema, &At { mode, style: Style::Juniper, edit: vec![] }, words, partial)
}

/// Where the line is typed: the mode, the style, and where configuration mode is.
#[derive(Debug, Clone)]
pub struct At {
    pub mode: Mode,
    pub style: Style,
    pub edit: Vec<String>,
}

const CISCO_CONFIGURATION: &[(&str, &str)] = &[
    ("commit", "Make the candidate the machine's configuration"),
    ("do", "Run an operational-mode command"),
    ("end", "Leave configuration mode (an uncommitted candidate is kept)"),
    ("exit", "Up one level (out of configuration mode at the top)"),
    ("no", "Remove a setting"),
    ("rollback", "Load an earlier configuration into the candidate"),
    ("show", "Show the candidate, or what it changes"),
    ("switch", "Switch to the Juniper style"),
    ("top", "Back to the top of the configuration"),
];

/// What may follow `words` at `at`.
pub fn choices_at(schema: &Schema, at: &At, words: &[String], partial: &str) -> Vec<Choice> {
    let here = |rest: &[String]| -> Vec<String> { at.edit.iter().chain(rest).cloned().collect() };
    let first = words.first().map(String::as_str);
    let all = match (at.mode, at.style, first) {
        (Mode::Configuration, Style::Cisco, None) => {
            // A path from here, or a command.
            let mut c = tree_choices(schema, &at.edit, "set");
            c.retain(|x| x.word != "|");
            c.extend(table(CISCO_CONFIGURATION));
            c
        }
        (Mode::Configuration, Style::Cisco, Some("no")) => tree_choices(schema, &here(&words[1..]), "delete"),
        (Mode::Configuration, Style::Cisco, Some("do")) => {
            choices_at(schema, &At { mode: Mode::Operational, ..at.clone() }, &words[1..], partial)
        }
        (Mode::Configuration, Style::Cisco, Some("show")) => match &words[1..] {
            [] => table(&[("configuration", "The candidate")]),
            [w] if w == "configuration" => table(&[("diff", "What the candidate changes")]),
            [c, d, b] if c == "configuration" && d == "diff" && b == "|" => table(DISPLAY),
            [c, d, b, x] if c == "configuration" && d == "diff" && b == "|" && x == "display" => table(OF_COMPARISON),
            [c, .., b] if c == "configuration" && b == "|" => table(DISPLAY),
            [c, .., b, x] if c == "configuration" && b == "|" && x == "display" => table(OF_CONFIGURATION),
            _ => vec![],
        },
        (Mode::Configuration, Style::Cisco, Some("rollback")) => match &words[1..] {
            [] => table(&[("configuration", "Load an earlier configuration into the candidate")]),
            [w] if w == "configuration" => vec![Choice::new("<number>", "0 is the current configuration, 1 the one before, …")],
            _ => vec![],
        },
        (Mode::Configuration, Style::Cisco, Some("switch")) if words.len() == 1 => table(&[("cli", "Switch to the Juniper style")]),
        (Mode::Configuration, Style::Cisco, Some("commit")) => commit_choices(&words[1..]),
        (Mode::Configuration, Style::Cisco, Some(_)) if CISCO_CONFIGURATION.iter().any(|(w, _)| Some(*w) == first) => vec![],
        // A bare path.
        (Mode::Configuration, Style::Cisco, Some(_)) => tree_choices(schema, &here(words), "set"),
        (Mode::Configuration, Style::Juniper, Some(cmd @ ("set" | "delete" | "show" | "edit"))) => {
            let rest = &words[1..];
            if let Some(p) = pipe_choices(cmd, rest) {
                p
            } else {
                let mut c = tree_choices(schema, &here(rest), if cmd == "edit" { "show" } else { cmd });
                if cmd == "edit" {
                    c.retain(|x| x.word != "|");
                }
                c
            }
        }
        _ => choices_juniper(schema, at, words, partial),
    };
    let all = with_filters(words, all);
    all.into_iter().filter(|c| c.is_placeholder() || c.word.starts_with(partial)).collect()
}

/// After a `|` in a `show`: the filters (filter.rs), which work on any show's output and, once one is
/// used, are the only pipes left. Their own options are theirs (`grep --help`).
fn with_filters(words: &[String], mut all: Vec<Choice>) -> Vec<Choice> {
    // (`run show …` and `do show …` get theirs from the operational choices.)
    if words.first().map(String::as_str) != Some("show") {
        return all;
    }
    if words.get(1).map(String::as_str) == Some("packages") {
        // A search prints as it goes: nothing follows its `|`.
        return if words.iter().any(|x| x == "|") { vec![] } else { all };
    }
    let Some(bar) = words.iter().rposition(|x| x == "|") else { return all };
    let is_filter = |x: &String| crate::filter::FILTERS.iter().any(|(f, _)| f == x);
    let after_filter = words[..bar].windows(2).any(|p| p[0] == "|" && is_filter(&p[1]));
    match words.get(bar + 1) {
        None => {
            // Each of a show's own pipes comes once (one `| display`, one `| compare`).
            let used = |p: &str| words[..bar].windows(2).any(|w| w[0] == "|" && w[1] == p);
            if used("display") {
                all.retain(|c| c.word != "display" && c.word != "compare");
            }
            if used("compare") {
                all.retain(|c| c.word != "compare");
            }
            if after_filter {
                all.clear();
            }
            all.extend(table(crate::filter::FILTERS));
        }
        Some(f) if is_filter(f) => all.clear(),
        // After a filter only filters may follow (`| display` there is refused).
        Some(_) if after_filter => all.clear(),
        Some(_) => {}
    }
    all
}

const DISPLAY: &[(&str, &str)] = &[("display", "Show it differently")];
/// What `| display` offers after a configuration, a comparison, and the commit list or alarms.
const OF_CONFIGURATION: &[(&str, &str)] = &[("detail", "With the files and commit behind it"), ("set", "As set commands")];
const OF_COMPARISON: &[(&str, &str)] = &[("diff", "As the raw diff of the files (what the broker records)")];
const OF_DETAIL: &[(&str, &str)] = &[("detail", "With the commits and store paths behind it")];

fn commit_choices(rest: &[String]) -> Vec<Choice> {
    match rest {
        [] => table(&[
            ("check", "Check the candidate evaluates, without committing"),
            ("comment", "Commit, with a comment for the log"),
            ("confirmed", "Commit, rolled back unless confirmed within minutes (default 10)"),
        ]),
        [w] if w == "comment" => vec![Choice::new("<\"text\">", "What the change is for")],
        [w] if w == "confirmed" => vec![Choice::new("<minutes>", "1 to 1440: confirm by committing again")],
        _ => vec![],
    }
}

fn choices_juniper(schema: &Schema, at: &At, words: &[String], partial: &str) -> Vec<Choice> {
    let mode = at.mode;
    match (mode, words.first().map(String::as_str)) {
        (Mode::Operational, None) => table(OPERATIONAL),
        (Mode::Configuration, None) => table(CONFIGURATION),
        (Mode::Operational, Some("show")) => operational_show(at.style, &words[1..]),
        (Mode::Operational, Some("request")) => match words[1..].iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
            [] => table(&[("system", "This machine")]),
            ["system"] => table(&[
                ("repair", "Return a machine changed outside TorqOS to its last configuration"),
                ("revert", "Undo one change (a new change forward)"),
                ("software", "This machine's software"),
                ("update", "Load the newest TorqOS into the candidate"),
            ]),
            ["system", "update"] => table(&[("all", "Every input, nixpkgs too")]),
            ["system", "revert"] => vec![Choice::new("<change>", "The change number (show system ledger)")],
            ["system", "software"] => table(&[("rollback", "Load an earlier configuration's software into the candidate (a downgrade)")]),
            ["system", "software", "rollback"] => vec![Choice::new("<number>", "0 is the current configuration, 1 the one before, …")],
            _ => vec![],
        },
        (Mode::Configuration, Some("run")) => choices_at(schema, &At { mode: Mode::Operational, ..at.clone() }, &words[1..], partial),
        (Mode::Configuration, Some("commit")) => commit_choices(&words[1..]),
        (Mode::Configuration, Some("rollback")) if words.len() == 1 => {
            vec![Choice::new("<number>", "0 is the current configuration, 1 the one before, …")]
        }
        (Mode::Configuration, Some("exit")) if words.len() == 1 => {
            table(&[("configuration-mode", "Leave configuration mode from anywhere")])
        }
        (_, Some("switch")) if words.len() == 1 => table(&[("cli", "Switch between the Juniper and Cisco styles")]),
        _ => vec![],
    }
}

fn table(t: &[(&str, &str)]) -> Vec<Choice> {
    t.iter().map(|(w, h)| Choice::new(w, h)).collect()
}

fn operational_show(style: Style, rest: &[String]) -> Vec<Choice> {
    let configuration = match style {
        Style::Juniper => "configuration",
        Style::Cisco => "running-config",
    };
    let mut words: Vec<&str> = rest.iter().map(String::as_str).collect();
    if words.first() == Some(&configuration) {
        words[0] = "configuration";
    } else if words.first() == Some(&"configuration") {
        words[0] = "(not in this style)";
    }
    match words.as_slice() {
        [] => {
            let mut c = table(&[
                ("packages", "Software you can install"),
                ("services", "The services TorqOS offers, and which are on"),
                ("system", "This machine"),
            ]);
            c.insert(0, Choice::new(configuration, "The configuration in effect"));
            c
        }
        ["system"] => table(&[
            ("alarms", "Is this machine as its history says?"),
            ("commit", "The configurations so far, newest first"),
            ("ledger", "Every change, as the ledger has it (refused and failed ones too)"),
            ("rollback", "An earlier configuration, or how two differ"),
        ]),
        ["system", "rollback"] => vec![Choice::new("<number>", "0 is the current configuration, 1 the one before, …")],
        ["system", "ledger"] => vec![Choice::new("<count>", "How many entries (20)")],
        ["system", "rollback", _] => table(&[("compare", "How it differs from another one")]),
        ["system", "rollback", _, "compare"] => vec![Choice::new("<number>", "The configuration to compare with")],
        ["packages"] => table(&[("available", "Search nixpkgs")]),
        ["packages", "available", ..] => vec![Choice::new("<term>", "Words to search for")],
        ["configuration", .., "|"] | ["system", "commit" | "alarms", "|"] => table(DISPLAY),
        ["system", "rollback", _, "|"] | ["system", "rollback", _, "compare", _, "|"] => table(DISPLAY),
        ["configuration", .., "|", "display"] | ["system", "rollback", _, "|", "display"] => table(OF_CONFIGURATION),
        ["system", "commit" | "alarms", "|", "display"] => table(OF_DETAIL),
        ["system", "rollback", _, "compare", _, "|", "display"] => table(OF_COMPARISON),
        _ => vec![],
    }
}

/// After a `|` in `show`: the pipes.
fn pipe_choices(cmd: &str, rest: &[String]) -> Option<Vec<Choice>> {
    let at = rest.iter().rposition(|w| w == "|")?;
    if cmd != "show" {
        return Some(vec![]);
    }
    let compare = rest[..at].iter().any(|w| w == "compare");
    Some(match (&rest[at + 1..], compare) {
        ([], false) => table(&[("compare", "What the candidate changes"), DISPLAY[0]]),
        ([], true) => table(DISPLAY),
        ([w], false) if w == "display" => table(OF_CONFIGURATION),
        ([w], true) if w == "display" => table(OF_COMPARISON),
        _ => vec![],
    })
}

/// The next word of a tree path (`set system host-name …`).
fn tree_choices(schema: &Schema, rest: &[String], cmd: &str) -> Vec<Choice> {
    let t = match schema.resolve(rest) {
        Ok(t) => t,
        Err(_) => return vec![],
    };
    let node = (!t.node.is_empty()).then(|| &schema.nodes[&t.node]);
    match node.map(|n| n.kind) {
        None | Some(Kind::Container) => {
            let mut c: Vec<Choice> = schema.children(&t.node).into_iter().map(|(w, k)| Choice::new(&w, &schema.nodes[&k].help)).collect();
            if cmd == "show" {
                c.push(Choice::new("|", "Pipe the output (compare, display set, grep, wc, …)"));
            }
            c
        }
        Some(Kind::List) => {
            let n = node.unwrap();
            let mut c: Vec<Choice> = n.values.iter().flatten().map(|(v, h)| Choice::new(v, h)).collect();
            if c.is_empty() {
                c.push(Choice::new("<name>", &n.help));
            }
            c
        }
        Some(Kind::Leaf | Kind::LeafList) if cmd == "show" => vec![],
        // A leaf takes one value; once it has it, nothing follows. A leaf-list takes more.
        Some(Kind::Leaf) if !t.values.is_empty() => vec![],
        Some(Kind::Leaf | Kind::LeafList) if cmd == "delete" && node.unwrap().kind == Kind::Leaf => vec![],
        Some(_) => {
            let n = node.unwrap();
            match &n.values {
                Some(v) => v.iter().map(|(v, h)| Choice::new(v, h)).collect(),
                None => vec![Choice::new(&format!("<{}>", n.ty.as_deref().unwrap_or("value")), &n.help)],
            }
        }
    }
}

/// `?`: the choices, Junos style.
pub fn render(choices: &[Choice]) -> String {
    if choices.is_empty() {
        return "No completions here.\n".into();
    }
    let width = choices.iter().map(|c| c.word.chars().count()).max().unwrap_or(0).min(24);
    let mut out = String::from("Possible completions:\n");
    for c in choices.iter().take(60) {
        out.push_str(&format!("  {:<width$}  {}\n", c.word, c.help));
    }
    if choices.len() > 60 {
        out.push_str(&format!("  … and {} more: type more of the word\n", choices.len() - 60));
    }
    out
}

/// Tab: the words to offer, as (shown, inserted) — placeholders are not words to insert, and a word
/// that is the only one gets its space. (The editor completes their common prefix.)
pub fn tab(choices: &[Choice]) -> Vec<(String, String)> {
    let words: Vec<&Choice> = choices.iter().filter(|c| !c.is_placeholder()).collect();
    let one = words.len() == 1;
    words.into_iter().map(|c| (c.word.clone(), if one { format!("{} ", c.word) } else { c.word.clone() })).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> Schema {
        Schema::parse(include_str!("schema-fixture.json")).unwrap()
    }
    fn w(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }
    fn words_of(c: &[Choice]) -> Vec<&str> {
        c.iter().map(|c| c.word.as_str()).collect()
    }

    #[test]
    fn commands_of_each_mode() {
        let s = schema();
        assert_eq!(words_of(&choices(&s, Mode::Operational, &[], "")), ["configure", "request", "show", "exit"]);
        assert_eq!(words_of(&choices(&s, Mode::Operational, &w("show"), "")), ["configuration", "packages", "services", "system"]);
        assert_eq!(words_of(&choices(&s, Mode::Operational, &w("show system"), "")), ["alarms", "commit", "ledger", "rollback"]);
        assert_eq!(words_of(&choices(&s, Mode::Operational, &w("show system rollback 2"), "")), ["compare"]);
        assert_eq!(words_of(&choices(&s, Mode::Operational, &w("request system"), "")), ["repair", "revert", "software", "update"]);
        assert_eq!(words_of(&choices(&s, Mode::Configuration, &[], "s")), ["set", "show", "switch"]);
        assert_eq!(words_of(&choices(&s, Mode::Configuration, &w("run"), "")), ["configure", "request", "show", "exit"]);
        assert_eq!(words_of(&choices(&s, Mode::Configuration, &w("commit"), "c")), ["check", "comment", "confirmed"]);
    }

    #[test]
    fn the_tree_from_the_schema() {
        let s = schema();
        let c = choices(&s, Mode::Configuration, &w("set"), "");
        assert_eq!(words_of(&c), ["services", "system", "users"]);
        assert_eq!(c[1].help, "This machine");
        assert_eq!(words_of(&choices(&s, Mode::Configuration, &w("set system"), "")), ["cli", "host-name", "packages", "time-zone"]);
        assert_eq!(words_of(&choices(&s, Mode::Configuration, &w("set users"), "")), ["dev"], "the machine's users");
        assert_eq!(words_of(&choices(&s, Mode::Configuration, &w("set users dev cli style"), "")), ["cisco", "juniper"]);
        assert_eq!(words_of(&choices(&s, Mode::Configuration, &w("set system host-name"), "")), ["<host-name>"]);
        assert_eq!(words_of(&choices(&s, Mode::Configuration, &w("set system packages htop"), "")), ["<package>"], "a list takes more");
        assert!(choices(&s, Mode::Configuration, &w("set system host-name torq"), "").is_empty(), "a leaf has its value");
        assert!(choices(&s, Mode::Configuration, &w("delete system host-name"), "").is_empty());
        assert_eq!(words_of(&choices(&s, Mode::Configuration, &w("set system time-zone"), "Europe/Be")), ["Europe/Belfast", "Europe/Belgrade", "Europe/Berlin"]);
        assert!(choices(&s, Mode::Configuration, &w("set nonsense"), "").is_empty());
    }

    #[test]
    fn pipes_after_show() {
        let s = schema();
        assert!(words_of(&choices(&s, Mode::Configuration, &w("show"), "")).contains(&"|"));
        assert_eq!(words_of(&choices(&s, Mode::Configuration, &w("show |"), "")), ["compare", "display", "cut", "grep", "head", "sort", "tail", "tr", "uniq", "wc"]);
        assert_eq!(words_of(&choices(&s, Mode::Configuration, &w("show system | display"), "")), ["detail", "set"]);
        assert_eq!(words_of(&choices(&s, Mode::Configuration, &w("show | compare |"), "")), ["display", "cut", "grep", "head", "sort", "tail", "tr", "uniq", "wc"]);
        assert_eq!(words_of(&choices(&s, Mode::Configuration, &w("show | compare | display"), "")), ["diff"]);
        let filters = ["cut", "grep", "head", "sort", "tail", "tr", "uniq", "wc"];
        assert_eq!(words_of(&choices(&s, Mode::Configuration, &w("show | display set |"), "")), filters, "one display");
        assert_eq!(words_of(&choices(&s, Mode::Configuration, &w("show | compare | display diff |"), "")), filters);
        assert_eq!(words_of(&choices(&s, Mode::Operational, &w("show configuration | display set |"), "")), filters);
        assert_eq!(words_of(&choices(&s, Mode::Operational, &w("show system commit | display"), "")), ["detail"]);
        assert_eq!(words_of(&choices(&s, Mode::Operational, &w("show system rollback 2 | display"), "")), ["detail", "set"]);
        assert_eq!(words_of(&choices(&s, Mode::Operational, &w("show system rollback 2 compare 0 | display"), "")), ["diff"]);
        let cisco = At { mode: Mode::Configuration, style: Style::Cisco, edit: vec![] };
        assert_eq!(words_of(&choices_at(&s, &cisco, &w("show configuration diff | display"), "")), ["diff"]);
        assert_eq!(words_of(&choices_at(&s, &cisco, &w("show configuration system | display"), "")), ["detail", "set"]);
        assert_eq!(words_of(&choices(&s, Mode::Operational, &w("show configuration |"), "")), ["display", "cut", "grep", "head", "sort", "tail", "tr", "uniq", "wc"]);
    }

    #[test]
    fn filters_after_any_show() {
        let s = schema();
        let filters = ["cut", "grep", "head", "sort", "tail", "tr", "uniq", "wc"];
        let mut with_display = vec!["display"];
        with_display.extend(filters);
        assert_eq!(words_of(&choices(&s, Mode::Operational, &w("show system commit |"), "")), with_display);
        assert_eq!(words_of(&choices(&s, Mode::Configuration, &w("run show system alarms |"), "")), with_display);
        assert_eq!(words_of(&choices(&s, Mode::Configuration, &w("show | grep x |"), "")), filters, "after a filter, only filters");
        assert_eq!(words_of(&choices(&s, Mode::Operational, &w("show configuration |"), "g")), ["grep"]);
        assert!(choices(&s, Mode::Operational, &w("show configuration | grep"), "").is_empty(), "grep's options are grep's");
        assert!(choices(&s, Mode::Configuration, &w("show | grep x | display"), "").is_empty(), "after a filter, no display");
        assert!(choices(&s, Mode::Operational, &w("show packages available x |"), "").is_empty(), "a search cannot be filtered");
        assert!(choices(&s, Mode::Configuration, &w("set system |"), "").is_empty(), "only a show");
        let cisco = At { mode: Mode::Configuration, style: Style::Cisco, edit: vec![] };
        assert_eq!(words_of(&choices_at(&s, &cisco, &w("show configuration |"), "")), with_display);
        assert_eq!(words_of(&choices_at(&s, &cisco, &w("do show running-config |"), "")), with_display);
    }

    #[test]
    fn requests_for_the_software() {
        let s = schema();
        assert_eq!(words_of(&choices(&s, Mode::Operational, &w("request system"), "")), ["repair", "revert", "software", "update"]);
        assert_eq!(words_of(&choices(&s, Mode::Operational, &w("request system update"), "")), ["all"]);
        assert_eq!(words_of(&choices(&s, Mode::Operational, &w("request system revert"), "")), ["<change>"]);
        assert_eq!(words_of(&choices(&s, Mode::Operational, &w("show system ledger"), "")), ["<count>"]);
        assert_eq!(words_of(&choices(&s, Mode::Operational, &w("request system software"), "")), ["rollback"]);
        assert_eq!(words_of(&choices(&s, Mode::Configuration, &w("run request system software rollback"), "")), ["<number>"]);
        assert!(choices(&s, Mode::Operational, &w("request system update all"), "").is_empty());
    }

    #[test]
    fn tab_offers_words_not_placeholders() {
        let s = schema();
        let t = |words: &str, partial: &str| tab(&choices(&s, Mode::Configuration, &w(words), partial));
        assert_eq!(t("", "co"), [("commit".to_string(), "commit ".to_string())], "the only one: with its space");
        assert_eq!(t("set system", "t"), [("time-zone".to_string(), "time-zone ".to_string())]);
        let berlin = t("set system time-zone", "Europe/Be");
        assert_eq!(berlin.len(), 3);
        assert!(berlin.iter().all(|(shown, inserted)| shown == inserted), "several: no space yet");
        assert!(t("set system host-name", "").is_empty(), "a placeholder is not inserted");
    }

    /// Cisco style: in configuration mode the first word is a path from where you are, or a
    /// command; `no` takes a path; operational `show running-config`.
    #[test]
    fn cisco_style_and_where_you_are() {
        let s = schema();
        let at = |mode, edit: &str| At { mode, style: Style::Cisco, edit: w(edit) };
        let top = choices_at(&s, &at(Mode::Configuration, ""), &[], "");
        let words = words_of(&top);
        assert!(words.contains(&"system") && words.contains(&"no") && words.contains(&"end") && !words.contains(&"set"), "{words:?}");
        assert_eq!(words_of(&choices_at(&s, &at(Mode::Configuration, "system"), &[], "h")), ["host-name"], "from where you are");
        assert_eq!(words_of(&choices_at(&s, &at(Mode::Configuration, ""), &w("no system"), "t")), ["time-zone"]);
        assert_eq!(words_of(&choices_at(&s, &at(Mode::Configuration, ""), &w("users dev cli style"), "")), ["cisco", "juniper"]);
        assert_eq!(words_of(&choices_at(&s, &at(Mode::Configuration, ""), &w("show configuration"), "")), ["diff"]);
        assert_eq!(words_of(&choices_at(&s, &at(Mode::Operational, ""), &w("show"), "r")), ["running-config"]);
        let j = At { mode: Mode::Configuration, style: Style::Juniper, edit: w("users dev") };
        assert_eq!(words_of(&choices_at(&s, &j, &w("set"), "")), ["cli", "packages"], "Juniper, from where you are");
    }

    #[test]
    fn question_mark_lists_with_help() {
        let s = schema();
        let out = render(&choices(&s, Mode::Configuration, &w("set system cli style"), ""));
        assert!(out.starts_with("Possible completions:\n  cisco    IOS-style syntax"), "{out}");
        let out = render(&choices(&s, Mode::Configuration, &w("set system time-zone"), ""));
        assert!(out.contains("… and 537 more"), "{out}");
    }
}
