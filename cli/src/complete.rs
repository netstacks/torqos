//! `?` and Tab for torqcli (ADR-016): what may come next at a point of a command line, with one
//! line of help each, from the commands of the mode and the schema. Pure: the line editor (shell.rs)
//! asks, this answers.

use crate::session::Mode;
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
    ("file", "The configuration's Nix files (list, show)"),
    ("request", "Ask the machine to do something"),
    ("show", "Show information about the machine"),
    ("exit", "Leave torqcli"),
];
const CONFIGURATION: &[(&str, &str)] = &[
    ("commit", "Make the candidate the machine's configuration"),
    ("delete", "Remove a setting"),
    ("edit", "Go down to a part of the configuration"),
    ("file", "The configuration's Nix files (list, show, edit, delete)"),
    ("load", "Load a saved configuration into the candidate (override, merge, set)"),
    ("exit", "Up one level (out of configuration mode at the top; the candidate is kept)"),
    ("quit", "Up one level (out of configuration mode at the top; the candidate is kept)"),
    ("rollback", "Load an earlier configuration into the candidate"),
    ("run", "Run an operational-mode command"),
    ("save", "Save the whole candidate, and its config files, to a file"),
    ("set", "Set a value"),
    ("show", "Show the candidate configuration"),
    ("top", "Back to the top of the configuration"),
    ("up", "Up one level"),
];

/// What may follow `words` (the complete words before the cursor), each starting with `partial`
/// (the word being typed, possibly empty).
#[cfg(test)]
pub fn choices(k: &Know, mode: Mode, words: &[String], partial: &str) -> Vec<Choice> {
    choices_at(k, &At { mode, edit: vec![], data: crate::whole::Data::default() }, words, partial)
}

/// Where the line is typed: the mode, where configuration mode is, and the configuration a line is
/// dry-run against for `<[Enter]>` (the candidate in configuration mode, the active one in
/// operational mode: Session::help_data).
#[derive(Debug, Clone)]
pub struct At {
    pub mode: Mode,
    pub edit: Vec<String>,
    pub data: crate::whole::Data,
}

/// `<[Enter]>`, offered when the words so far are already a whole command.
const ENTER: (&str, &str) = ("<[Enter]>", "Execute this command");

/// What may follow `words` at `at`: the next words, `|` after a whole `show`, and `<[Enter]>` when
/// the line could run as it is.
pub fn choices_at(k: &Know, at: &At, words: &[String], partial: &str) -> Vec<Choice> {
    let mut all = next(k, at, words, partial);
    // (After a filter's own name, its options are its own: `| grep ?` lists nothing.)
    let is_filter = |w: &String| crate::filter::FILTERS.iter().any(|(f, _)| f == w);
    if partial.is_empty() && !words.last().is_some_and(is_filter) {
        if let Ok((op, _)) = crate::syntax::parse_line(at.mode, words) {
            if crate::syntax::shows(&op) && !all.iter().any(|c| c.word == "|") {
                all.push(Choice::new("|", "Pipe the output (display, grep, wc, …)"));
            }
            if complete(k, at, &op) {
                all.insert(0, Choice::new(ENTER.0, ENTER.1));
            }
        }
    }
    all.into_iter().filter(|c| c.is_placeholder() || c.word.starts_with(partial)).collect()
}

/// The words of the command language that may follow `words` at `at` — the grammar `?` lists, and
/// that `expand` reads lines against: fixed words, and `<placeholders>` where a value stands.
fn next(k: &Know, at: &At, words: &[String], partial: &str) -> Vec<Choice> {
    let here = |rest: &[String]| -> Vec<String> { at.edit.iter().chain(rest).cloned().collect() };
    let first = words.first().map(String::as_str);
    let all = match (at.mode, first) {
        // `top <command>`: the command, as from the top.
        (Mode::Configuration, Some("top")) => return next(k, &At { edit: vec![], ..at.clone() }, &words[1..], partial),
        (Mode::Configuration, Some(cmd @ ("set" | "delete" | "show" | "edit"))) => {
            let rest = &words[1..];
            if let Some(p) = pipe_choices(cmd, rest) {
                p
            } else {
                let mut c = tree_choices(k, &here(rest), if cmd == "edit" { "show" } else { cmd });
                if cmd == "edit" {
                    c.retain(|x| x.word != "|");
                }
                c
            }
        }
        _ => choices_j(k, at, words, partial),
    };
    with_filters(words, all)
}

/// Words the grammar offers, for an error: the first dozen, then `…`.
fn listed(fixed: &[&str]) -> String {
    let shown: Vec<&str> = fixed.iter().copied().take(12).collect();
    format!("{}{}", shown.join(", "), if fixed.len() > shown.len() { ", …" } else { "" })
}

/// When a line is not a whole command but the grammar knows what may follow it, what to say: what
/// may come next. Only for a line of command words alone (`show system`): where a word stands for a
/// value (`show system rollback x`), what is wrong is the value, and the command's own error says so.
pub fn incomplete(k: &Know, at: &At, words: &[String]) -> Option<String> {
    for i in 0..words.len() {
        let c = next(k, at, &words[..i], "");
        if c.iter().any(Choice::is_placeholder) || !c.iter().any(|x| x.word == words[i]) {
            return None;
        }
    }
    let c = next(k, at, words, "");
    let fixed: Vec<&str> = c.iter().filter(|x| !x.is_placeholder() && x.word != "|").map(|x| x.word.as_str()).collect();
    (!words.is_empty() && !fixed.is_empty() && !c.iter().any(Choice::is_placeholder)).then(|| format!("`{}` is not a whole command: one of these may follow: {}", words.join(" "), listed(&fixed)))
}

/// Whether the next word is part of a configuration path (`set networking …`, `show configuration
/// networking …`, before any `|`): paths have old names and entries people name, so they are read
/// by the configuration's own check (with its hints), never against the grammar.
fn in_path(mode: Mode, words: &[String]) -> bool {
    if words.iter().any(|w| w == "|") {
        return false;
    }
    match (mode, words.first().map(String::as_str)) {
        (Mode::Configuration, Some("top")) => in_path(mode, &words[1..]),
        (Mode::Configuration, Some("run")) => in_path(Mode::Operational, &words[1..]),
        (Mode::Configuration, Some("set" | "delete" | "edit" | "show")) => true,
        (Mode::Operational, Some("show")) => words.get(1).is_some_and(|w| w == "configuration"),
        _ => false,
    }
}

/// A line's words read against the grammar (`next`), as on a network device: a word of the command
/// language that begins exactly one of the words that may stand there is that word (`sh conf | gr
/// host` is `show configuration | grep host`); one that begins several, or none, is refused, naming
/// it and what may stand there. Where a value may stand (a `<placeholder>`), the grammar says
/// nothing (after a value, a filter's own options), or a configuration path goes on (`in_path`),
/// words are taken as they are.
pub fn expand(k: &Know, at: &At, words: &[String]) -> anyhow::Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for w in words {
        // (`|` may follow any whole `show`: the parser judges it; what follows it is read again.)
        if w == "|" || in_path(at.mode, &out) {
            out.push(w.clone());
            continue;
        }
        let c = next(k, at, &out, "");
        let open = c.is_empty() || c.iter().any(Choice::is_placeholder);
        let fixed: Vec<&str> = c.iter().filter(|x| !x.is_placeholder()).map(|x| x.word.as_str()).collect();
        if open || fixed.contains(&w.as_str()) {
            out.push(w.clone());
            continue;
        }
        let after = if out.is_empty() { String::new() } else { format!(" (after `{}`)", out.join(" ")) };
        let begins: Vec<&str> = fixed.iter().copied().filter(|f| f.starts_with(w.as_str())).collect();
        match begins.as_slice() {
            [one] => out.push(one.to_string()),
            [] => {
                // (Near enough to mean something: at most 2 edits, and fewer than the word has letters.)
                let near = fixed.iter().copied().filter(|f| { let d = crate::nixos::distance(f, w); d <= 2 && d < w.chars().count() }).min_by_key(|f| crate::nixos::distance(f, w));
                let hint = near.map(|n| format!(": did you mean {n}?")).unwrap_or_default();
                anyhow::bail!("'{w}' is not a word here{after}{hint} (one of: {})", listed(&fixed));
            }
            many => anyhow::bail!("'{w}' is ambiguous{after}: {}", many.join(", ")),
        }
    }
    Ok(out)
}

/// Whether `op` would run as it is, decided as the session decides it: `set`, `delete` and `show`
/// are dry-run on a copy of the configuration the line would change or show (whole.rs, the same
/// code the session runs); `edit` needs a place, `up` somewhere to go up from.
fn complete(k: &Know, at: &At, op: &crate::syntax::Op) -> bool {
    use crate::syntax::Op;
    let whole = crate::whole::Whole { schema: &k.schema, tree: &k.tree };
    let here = |w: &[String]| -> Vec<String> { at.edit.iter().chain(w).cloned().collect() };
    match op {
        Op::Set { words } => whole.set(&mut at.data.clone(), &here(words)).is_ok(),
        Op::Delete { words } => !words.is_empty() && whole.delete(&mut at.data.clone(), &here(words)).is_ok(),
        Op::Edit { words } => !words.is_empty() && whole.is_place(&here(words)),
        Op::Up { .. } => !at.edit.is_empty(),
        Op::Top { then: Some(op) } => complete(k, &At { edit: vec![], ..at.clone() }, op),
        Op::ShowCandidate { path, .. } => whole.show(&at.data, &here(path)).is_ok(),
        Op::ShowActive { path, .. } => whole.show(&at.data, path).is_ok(),
        Op::Request { words } => {
            let w: Vec<&str> = words.iter().map(String::as_str).collect();
            let number = |n: &str| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit());
            match w.as_slice() {
                ["system", "update"] | ["system", "update", "all"] | ["system", "repair"] => true,
                ["system", "revert", n] | ["system", "software", "rollback", n] => number(n),
                _ => false,
            }
        }
        Op::Run(op) => complete(k, &At { mode: Mode::Operational, ..at.clone() }, op),
        _ => true,
    }
}

/// After a `|` in a `show`: the filters (filter.rs), which work on any show's output and, once one is
/// used, are the only pipes left. Their own options are theirs (`grep --help`).
fn with_filters(words: &[String], mut all: Vec<Choice>) -> Vec<Choice> {
    // (`run show …` gets its own from the operational choices.)
    if words.first().map(String::as_str) != Some("show") {
        return all;
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
            ("and-quit", "Commit, then leave configuration mode"),
        ]),
        [w] if w == "comment" => vec![Choice::new("<\"text\">", "What the change is for")],
        [w] if w == "confirmed" => vec![Choice::new("<minutes>", "1 to 1440: confirm by committing again")],
        _ => vec![],
    }
}

fn choices_j(k: &Know, at: &At, words: &[String], partial: &str) -> Vec<Choice> {
    let mode = at.mode;
    match (mode, words.first().map(String::as_str)) {
        (Mode::Operational, None) => table(OPERATIONAL),
        (Mode::Configuration, None) => table(CONFIGURATION),
        (Mode::Operational, Some("show")) => operational_show(k, &words[1..]),
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
        (Mode::Configuration, Some("run")) => {
            let mut c = next(k, &At { mode: Mode::Operational, ..at.clone() }, &words[1..], partial);
            // (Already in configuration mode; `exit` is configuration mode's own.)
            if words.len() == 1 {
                c.retain(|x| x.word != "configure" && x.word != "exit");
            }
            c
        }
        (Mode::Configuration, Some("commit")) => commit_choices(&words[1..]),
        (Mode::Configuration, Some("rollback")) if words.len() == 1 => {
            vec![Choice::new("<number>", "0 is the current configuration, 1 the one before, …")]
        }
        (Mode::Configuration, Some("exit")) if words.len() == 1 => {
            table(&[("configuration-mode", "Leave configuration mode from anywhere")])
        }
        (Mode::Configuration, Some("load")) if words.len() == 1 => table(&[
            ("merge", "Set the file's settings onto the candidate"),
            ("override", "The file becomes the candidate"),
            ("set", "Run the file's `set` and `delete` lines (what `| display set` writes)"),
        ]),
        (Mode::Configuration, Some("load")) if words.len() == 2 => vec![
            Choice::new("<file>", "A configuration saved with `save` (or `| display set` lines)"),
            Choice::new("terminal", "What you paste or type next, ended by Ctrl-D at a new line"),
        ],
        (Mode::Configuration, Some("save")) if words.len() == 1 => vec![Choice::new("<file>", "Where to save it (written private: 0600)")],
        (_, Some("file")) if words.len() == 1 => {
            let mut t = vec![("list", "Every Nix file of the configuration"), ("show", "Print one")];
            if mode == Mode::Configuration {
                t.extend([("delete", "Delete one, in the candidate"), ("edit", "Edit one (or a new one) in your editor, in the candidate")]);
            } else {
                t.extend([("delete", "Delete one (in configuration mode)"), ("edit", "Edit one (in configuration mode)")]);
            }
            t.sort();
            table(&t)
        }
        (_, Some("file")) if words.len() == 2 && words[1] != "list" => vec![Choice::new("<file>", "A .nix file of the config repo (file list)")],
        _ => vec![],
    }
}

fn table(t: &[(&str, &str)]) -> Vec<Choice> {
    t.iter().map(|(w, h)| Choice::new(w, h)).collect()
}

fn operational_show(k: &Know, rest: &[String]) -> Vec<Choice> {
    let words: Vec<&str> = rest.iter().map(String::as_str).collect();
    match words.as_slice() {
        [] => table(&[
            ("configuration", "The configuration in effect"),
            ("packages", "Software you can install"),
            ("services", "The services TorqOS offers, and which are on"),
            ("system", "This machine"),
        ]),
        // The places below, as `show` in configuration mode has them (no `| compare` here).
        ["configuration", path @ ..] if !path.contains(&"|") => {
            let mut c = tree_choices(k, &rest[1..], "show");
            for x in c.iter_mut().filter(|x| x.word == "|") {
                x.help = "Pipe the output (display set, grep, wc, …)".into();
            }
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
        ["packages", "available", terms @ ..] if !terms.contains(&"|") => vec![Choice::new("<term>", "Words to search for")],
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

/// What `?` and Tab know: the hand-made schema (the values of the settings TorqOS keeps in its own
/// files: time zones, user names) and every NixOS setting of the system (the tree's paths).
pub struct Know<'a> {
    pub schema: &'a Schema,
    pub tree: &'a crate::nixos::Tree,
}

/// The next word of a tree path (`set virtualisation docker …`): from the NixOS tree, and for a
/// setting TorqOS keeps in its own files, its values from the hand-made schema.
fn tree_choices(k: &Know, rest: &[String], cmd: &str) -> Vec<Choice> {
    // Where TorqOS comes from (software.rs): its one setting, `system software torq source`.
    let sw: Vec<&str> = rest.iter().map(String::as_str).collect();
    match sw.as_slice() {
        ["system", "software"] => return vec![Choice::new("torq", "TorqOS: where it comes from (its version: `request system update`)")],
        ["system", "software", "torq"] => return vec![Choice::new("source", "Where TorqOS comes from: github:<owner>/<repo>[/<ref>], path:/<folder>, …")],
        ["system", "software", "torq", "source"] if cmd != "show" => return vec![Choice::new("<source>", "github:netstacks/torqos, a tag (…/v0.1.0), or path:/<folder> while developing")],
        _ => {}
    }
    if let Some(hand) = crate::whole::to_hand(rest) {
        let mut c = hand_tree_choices(&k.schema, &hand, cmd);
        if !c.is_empty() {
            if sw.as_slice() == ["system"] {
                c.push(Choice::new("software", "Where this machine's TorqOS comes from"));
                c.sort_by(|a, b| (a.word == "|").cmp(&(b.word == "|")).then(a.word.cmp(&b.word)));
            }
            return c;
        }
    }
    let at_place = rest.is_empty() || matches!(k.tree.locate(rest), Ok((_, None, _)));
    if cmd == "show" && !at_place {
        return vec![];
    }
    // A config file, attached at a place (attach.rs) — unless NixOS has its own `config` there.
    if let [place @ .., last] = rest {
        if last == "config" && (place.is_empty() || matches!(k.tree.locate(place), Ok((_, None, _)))) && !k.tree.next(place).iter().any(|(w, _)| w == "config" || w.starts_with('<')) {
            return vec![Choice::new("<file>", "A .nix file of the config repo (like services/httpd.nix); `file edit <file>` writes it")];
        }
    }
    // Only what may be set: not NixOS's module internals (`_module`), nor what the fence keeps out of
    // the tree (TorqOS's own, the broker's) — `set` refuses those anyway.
    let mut c: Vec<Choice> = k
        .tree
        .next(rest)
        .into_iter()
        .filter(|(w, _)| {
            let at: Vec<String> = [rest, &[w.clone()]].concat();
            // (A place whose every setting is fenced is not offered either: `torq cli`.)
            let below = k.tree.next(&at);
            w.starts_with('<') || !w.starts_with('_') && !k.tree.fenced(&at) && (below.is_empty() || !below.iter().all(|(b, _)| k.tree.fenced(&[at.clone(), vec![b.clone()]].concat())))
        })
        .map(|(w, h)| Choice::new(&w, &h))
        .collect();
    if cmd == "show" {
        c.push(Choice::new("|", "Pipe the output (compare, display set, grep, wc, …)"));
    }
    if at_place && cmd != "show" && !c.iter().any(|x| x.word == "config" || x.is_placeholder()) {
        c.push(Choice::new("config", "Attach a NixOS file here, for what is not a value"));
        c.sort_by(|a, b| a.word.cmp(&b.word));
    }
    c
}

/// The next word of a path of the hand-made schema (`system host-name …`).
fn hand_tree_choices(schema: &Schema, rest: &[String], cmd: &str) -> Vec<Choice> {
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

/// `?`: the choices, laid out to fit a window `fit` wide (ui.rs): each help wrapped under the help
/// column, never back to the left edge.
pub fn render(choices: &[Choice], fit: usize) -> String {
    if choices.is_empty() {
        return "No completions here.\n".into();
    }
    let width = choices.iter().map(|c| c.word.chars().count()).max().unwrap_or(0).min(24);
    let mut out = String::from("Possible completions:\n");
    for c in choices.iter().take(60) {
        // A table: the word, then its help as a sentence beside it (ui::hang: wrapped under the help
        // column, or under the word in a narrow window).
        let help = summary(&c.help);
        if help.is_empty() {
            out.push_str(&format!("  {}\n", c.word));
        } else {
            out.push_str(&crate::ui::hang(&format!("  {:<width$}  ", c.word), &help, Some(fit), HELP_LINES));
        }
    }
    if choices.len() > 60 {
        out.push_str(&format!("  … and {} more: type more of the word\n", choices.len() - 60));
    }
    out
}

/// At most this many lines of help per choice; more ends in `…`.
const HELP_LINES: usize = 3;

/// A help as `?` shows it: NixOS's documentation markup taken out (`{command}`x`` → x,
/// `[text](url)` → text, backticks), spaces collapsed, and only its first sentence. NixOS's help
/// arrives cut at 160 bytes (modules/nixos-schema.nix): text cut mid-sentence ends in `…`.
fn summary(help: &str) -> String {
    let text = plain(help);
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some(end) = sentence_end(&text) {
        return text[..end].to_string();
    }
    if help.len() >= 160 {
        // Cut by the schema: end at the last whole word.
        let cut = text.rfind(' ').unwrap_or(text.len());
        return format!("{}…", text[..cut].trim_end_matches([',', ';', ':']));
    }
    text
}

/// Where the first sentence ends (after its `.`), if another follows: `. ` and a capital, not after
/// an abbreviation.
fn sentence_end(text: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(i) = text[from..].find(". ").map(|i| from + i) {
        let next = text[i + 2..].chars().next();
        let word = text[..i].rsplit([' ', '(', '"', '[']).next().unwrap_or("");
        if next.is_some_and(char::is_uppercase) && !["e.g", "i.e", "etc", "vs", "cf"].contains(&word.to_lowercase().as_str()) {
            return Some(i + 1);
        }
        from = i + 2;
    }
    None
}

/// The roles NixOS's option documentation uses (`{option}`networking.hostName``).
const ROLES: &[&str] = &["option", "command", "env", "file", "var", "manpage"];

/// NixOS's documentation markdown as plain text: `{role}`x`` → x, `[text](target)` → text (an empty
/// text → the target, without `#opt-`), backticks dropped.
fn plain(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(c) = rest.chars().next() {
        if c == '{' {
            // `{command}` immediately before a backtick: one of NixOS's roles, not text (`${HOME}` is).
            if let Some(end) = rest.find('}') {
                let role = &rest[1..end];
                if ROLES.contains(&role) && rest[end + 1..].starts_with('`') {
                    rest = &rest[end + 1..];
                    continue;
                }
            }
        }
        if c == '[' {
            if let Some(close) = rest.find("](") {
                if let Some(end) = rest[close..].find(')').map(|e| close + e) {
                    let text = &rest[1..close];
                    if !text.contains(['[', ']']) {
                        let target = &rest[close + 2..end];
                        out.push_str(if text.is_empty() { target.trim_start_matches("#opt-") } else { text });
                        rest = &rest[end + 1..];
                        continue;
                    }
                }
            }
        }
        if c != '`' {
            out.push(c);
        }
        rest = &rest[c.len_utf8()..];
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

    fn schema() -> Know<'static> {
        static FIXTURE: std::sync::OnceLock<(Schema, crate::nixos::Tree)> = std::sync::OnceLock::new();
        let (schema, tree) = FIXTURE.get_or_init(|| (Schema::parse(include_str!("schema-fixture.json")).unwrap(), crate::nixos::Tree::parse(include_str!("nixos-fixture.json")).unwrap()));
        Know { schema, tree }
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
        assert_eq!(words_of(&choices(&s, Mode::Operational, &[], "")), ["configure", "file", "request", "show", "exit"]);
        assert_eq!(words_of(&choices(&s, Mode::Operational, &w("show"), "")), ["configuration", "packages", "services", "system"]);
        assert_eq!(words_of(&choices(&s, Mode::Operational, &w("show system"), "")), ["alarms", "commit", "ledger", "rollback"]);
        assert_eq!(words_of(&choices(&s, Mode::Operational, &w("show system rollback 2"), "")), ["<[Enter]>", "compare", "|"]);
        assert_eq!(words_of(&choices(&s, Mode::Operational, &w("request system"), "")), ["repair", "revert", "software", "update"]);
        assert_eq!(words_of(&choices(&s, Mode::Configuration, &[], "s")), ["save", "set", "show"]);
        assert_eq!(words_of(&choices(&s, Mode::Configuration, &w("run"), "")), ["file", "request", "show"], "not configure or exit: already here");
        assert_eq!(words_of(&choices(&s, Mode::Configuration, &w("commit"), "c")), ["check", "comment", "confirmed"]);
    }

    #[test]
    fn the_tree_from_the_schema() {
        let s = schema();
        let c = choices(&s, Mode::Configuration, &w("set"), "");
        assert_eq!(words_of(&c), ["boot", "config", "environment", "fileSystems", "networking", "services", "specialisation", "system", "systemd", "time", "torq", "users", "virtualisation"], "NixOS's top level");
        assert_eq!(words_of(&choices(&s, Mode::Configuration, &w("set virtualisation docker"), "")), ["config", "enable", "rootless"]);
        assert!(choices(&s, Mode::Configuration, &w("set virtualisation docker"), "")[1].help.starts_with("This option enables docker"), "NixOS's own help");
        assert_eq!(words_of(&choices(&s, Mode::Configuration, &w("set services httpd virtualHosts"), "")), ["<name>"]);
        // (An empty list is a value the session takes, so the line would run as it is.)
        assert_eq!(words_of(&choices(&s, Mode::Configuration, &w("set networking firewall allowedTCPPorts"), "")), ["<[Enter]>", "<[ a number … ]>"]);
        // The settings TorqOS keeps in its own files: their values, as before.
        assert_eq!(words_of(&choices(&s, Mode::Configuration, &w("set networking hostName"), "")), ["<host-name>"]);
        assert_eq!(words_of(&choices(&s, Mode::Configuration, &w("set networking hostName torq"), "")), ["<[Enter]>"], "a leaf has its value: nothing more");
        assert_eq!(words_of(&choices(&s, Mode::Configuration, &w("set time timeZone"), "Europe/Be")), ["Europe/Belfast", "Europe/Belgrade", "Europe/Berlin"]);
        assert!(choices(&s, Mode::Configuration, &w("set nonsense"), "").is_empty());
        // Config files: `config` at a place, then a file.
        assert_eq!(words_of(&choices(&s, Mode::Configuration, &w("set services httpd"), "c")), ["config"]);
        assert_eq!(words_of(&choices(&s, Mode::Configuration, &w("set services httpd config"), "")), ["<file>"]);
        assert!(choices(&s, Mode::Configuration, &w("show services httpd"), "c").is_empty(), "show has no config to attach");
        assert_eq!(words_of(&choices(&s, Mode::Configuration, &w("file"), "")), ["delete", "edit", "list", "show"]);
        assert_eq!(words_of(&choices(&s, Mode::Configuration, &w("load"), "")), ["merge", "override", "set"]);
        assert_eq!(words_of(&choices(&s, Mode::Configuration, &[], "l")), ["load"]);
        assert_eq!(words_of(&choices(&s, Mode::Operational, &w("file"), "")), ["delete", "edit", "list", "show"], "edit and delete are there, said to be configuration mode's");
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
        assert_eq!(words_of(&choices(&s, Mode::Operational, &w("show packages available x |"), "")), filters, "a search is a show like any other");
        assert!(choices(&s, Mode::Configuration, &w("set system |"), "").is_empty(), "only a show");
    }

    #[test]
    fn requests_for_the_software() {
        let s = schema();
        assert_eq!(words_of(&choices(&s, Mode::Operational, &w("request system"), "")), ["repair", "revert", "software", "update"]);
        assert_eq!(words_of(&choices(&s, Mode::Operational, &w("request system update"), "")), ["<[Enter]>", "all"]);
        assert_eq!(words_of(&choices(&s, Mode::Operational, &w("request system revert"), "")), ["<change>"]);
        assert_eq!(words_of(&choices(&s, Mode::Operational, &w("show system ledger"), "")), ["<[Enter]>", "<count>", "|"]);
        assert_eq!(words_of(&choices(&s, Mode::Operational, &w("request system software"), "")), ["rollback"]);
        assert_eq!(words_of(&choices(&s, Mode::Configuration, &w("run request system software rollback"), "")), ["<number>"]);
        assert_eq!(words_of(&choices(&s, Mode::Operational, &w("request system update all"), "")), ["<[Enter]>"]);
        assert!(choices(&s, Mode::Operational, &w("request system revert"), "").iter().all(|c| c.word != "<[Enter]>"), "a change number is missing");
    }

    #[test]
    fn tab_offers_words_not_placeholders() {
        let s = schema();
        let t = |words: &str, partial: &str| tab(&choices(&s, Mode::Configuration, &w(words), partial));
        assert_eq!(t("", "co"), [("commit".to_string(), "commit ".to_string())], "the only one: with its space");
        assert_eq!(t("set time", "t"), [("timeZone".to_string(), "timeZone ".to_string())]);
        let berlin = t("set time timeZone", "Europe/Be");
        assert_eq!(berlin.len(), 3);
        assert!(berlin.iter().all(|(shown, inserted)| shown == inserted), "several: no space yet");
        assert!(t("set networking hostName", "").is_empty(), "a placeholder is not inserted");
    }

    /// As Junos has it: `<[Enter]>` when the line would run as it is (dry-run as the session runs it,
    /// on the candidate), `|` after any whole `show`, and `show configuration` lists the places below.
    #[test]
    fn enter_pipes_and_places() {
        let s = schema();
        // A candidate with something in it: what `delete` may take.
        let mut data = crate::whole::Data::default();
        let x = crate::whole::Whole { schema: &s.schema, tree: &s.tree };
        for line in ["networking hostName torq", "system packages [ htop ]", "virtualisation docker enable true", "services httpd config httpd.nix"] {
            x.set(&mut data, &w(line)).unwrap_or_else(|e| panic!("{line}: {e:#}"));
        }
        let at = |mode, edit: &str| At { mode, edit: w(edit), data: data.clone() };
        let c = |mode, line: &str| words_of(&choices_at(&s, &at(mode, ""), &w(line), "")).iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let enter = |mode, line: &str| c(mode, line).first().is_some_and(|x| x == "<[Enter]>");
        let (o, k) = (Mode::Operational, Mode::Configuration);
        let top = c(o, "show configuration");
        assert_eq!((top.first().map(String::as_str), top.last().map(String::as_str)), (Some("<[Enter]>"), Some("|")), "{top:?}");
        assert!(top.iter().any(|x| x == "networking") && top.iter().any(|x| x == "virtualisation"), "the places below: {top:?}");
        let net = c(o, "show configuration networking");
        assert!(net[0] == "<[Enter]>" && net.iter().any(|x| x == "hostName") && net.last().is_some_and(|x| x == "|"), "{net:?}");
        assert!(!enter(o, "show configuration nonsense") && !enter(k, "show nonsense"), "refused: no such setting");
        assert_eq!(c(o, "show system commit"), ["<[Enter]>", "|"]);
        assert_eq!(c(o, "show services"), ["<[Enter]>", "|"]);
        assert!(!enter(o, "show"), "show alone is not a command");
        assert_eq!(c(k, "commit"), ["<[Enter]>", "check", "comment", "confirmed", "and-quit"]);
        assert_eq!(c(k, "rollback"), ["<[Enter]>", "<number>"]);
        assert!(enter(k, "show") && c(k, "show").last().is_some_and(|x| x == "|"));
        // Pipes after pipes: each whole show takes another; a filter's own options are its own.
        assert!(enter(k, "show | compare") && c(k, "show | compare").contains(&"|".to_string()));
        assert!(enter(k, "show | display set") && c(k, "show | display set").contains(&"|".to_string()));
        assert!(enter(k, "show | grep x") && c(k, "show | grep x").contains(&"|".to_string()), "{:?}", c(k, "show | grep x"));
        assert!(c(k, "show | grep").is_empty(), "grep's options are grep's");
        // delete: what the session runs — a setting, a value, a whole list or place, by either name; what
        // is not set runs too (a warning, nothing deleted).
        for line in ["delete networking hostName", "delete system host-name", "delete networking", "delete system packages htop", "delete system packages", "delete torq packages system", "delete services httpd config", "delete virtualisation docker enable", "delete time timeZone", "delete virtualisation config"] {
            assert!(enter(k, line), "{line}: {:?}", c(k, line));
        }
        for line in ["delete nonsense", "delete networking all", "delete"] {
            assert!(!enter(k, line), "{line}: refused (or asks first), so no <[Enter]>");
        }
        assert!(enter(k, "edit networking") && !enter(k, "edit") && !enter(k, "edit networking hostName"));
        assert!(!enter(k, "set") && !enter(k, "set virtualisation docker"), "a place is not a value");
        assert_eq!(c(k, "set virtualisation docker enable false"), ["<[Enter]>"]);
        assert!(!enter(k, "up"), "already at the top");
        assert!(words_of(&choices_at(&s, &at(k, "networking"), &w("up"), ""))[0] == "<[Enter]>");
        assert_eq!(c(k, "run show system alarms"), ["<[Enter]>", "|"]);
        // From where configuration mode is.
        let docker = at(k, "virtualisation docker");
        assert_eq!(words_of(&choices_at(&s, &docker, &w("set"), "")), ["config", "enable", "rootless"], "from where you are");
        assert_eq!(words_of(&choices_at(&s, &docker, &w("set enable true"), "")), ["<[Enter]>"]);
        // Tab never inserts `<[Enter]>`.
        assert!(tab(&choices(&s, k, &w("commit"), "")).iter().all(|(t, _)| t != "<[Enter]>"));
    }

    /// Lines read against the grammar, as on a network device: the command language abbreviates and
    /// names a wrong word; configuration paths and values are taken as they are.
    #[test]
    fn the_command_language_abbreviates() {
        let s = schema();
        let at = |mode| At { mode, edit: vec![], data: crate::whole::Data::default() };
        let (o, k) = (Mode::Operational, Mode::Configuration);
        let x = |mode, line: &str| expand(&s, &at(mode), &w(line)).map_err(|e| format!("{e:#}"));
        assert_eq!(x(o, "sho sys ala").unwrap(), w("show system alarms"));
        assert_eq!(x(o, "sh conf | disp set | gr host").unwrap(), w("show configuration | display set | grep host"), "pipes too, a filter's own words not");
        assert_eq!(x(o, "sh conf netw").unwrap(), w("show configuration netw"), "a path is the configuration's to read");
        assert_eq!(x(k, "se netw hostName x").unwrap(), w("set netw hostName x"));
        let why = vec!["comm".to_string(), "comm".into(), "why not".into()];
        assert_eq!(expand(&s, &at(k), &why).unwrap(), vec!["commit".to_string(), "comment".into(), "why not".into()], "a value stays");
        assert_eq!(x(k, "ru sh sys ala").unwrap(), w("run show system alarms"));
        assert_eq!(x(k, "to se time timeZone UTC").unwrap(), w("top set time timeZone UTC"));
        assert!(x(k, "s").unwrap_err().contains("'s' is ambiguous: save, set, show"));
        let e = x(o, "show system foo").unwrap_err();
        assert!(e.contains("'foo' is not a word here (after `show system`)") && e.contains("alarms, commit, ledger, rollback"), "{e}");
        assert!(x(o, "show configuraton").unwrap_err().contains("did you mean configuration?"));
        assert!(!x(k, "set torq ?").unwrap_or_default().is_empty(), "a path word is never refused here");
        // A line of command words that is not whole says what may follow; a wrong value does not.
        assert!(incomplete(&s, &at(o), &w("show system")).is_some_and(|m| m.contains("alarms, commit, ledger, rollback")));
        assert_eq!(incomplete(&s, &at(o), &w("show system rollback x")), None);
        assert_eq!(incomplete(&s, &at(o), &w("show system alarms")), None, "whole: nothing follows");
    }

    #[test]
    fn question_mark_lists_with_help() {
        let s = schema();
        let out = render(&choices(&s, Mode::Configuration, &w("load"), ""), 80);
        assert!(out.starts_with("Possible completions:\n  merge     Set the file's settings onto the candidate\n"), "{out}");
        let out = render(&choices(&s, Mode::Configuration, &w("set time timeZone"), ""), 80);
        assert!(out.contains("… and 537 more"), "{out}");
    }

    #[test]
    fn long_help_wraps_under_the_help_column() {
        let long = "This option allows you to define interfaces encapsulating IPv6 packets within IPv4 packets. Each attribute set specifies one interface.";
        let choices = vec![Choice::new("sits", long), Choice::new("vlans", "Short."), Choice::new("ifstate", "")];
        let out = render(&choices, 50);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[1], "  sits     This option allows you to define", "{out}");
        assert_eq!(lines[2], "           interfaces encapsulating IPv6 packets", "{out}");
        assert_eq!(lines[3], "           within IPv4 packets.", "only the first sentence: {out}");
        assert_eq!(lines[4], "  vlans    Short.");
        assert_eq!(lines[5], "  ifstate");
        assert!(out.lines().all(|l| l.chars().count() <= 50), "{out}");
        // A word wider than the column: its help starts after it, and wraps under where it started.
        let wide = vec![Choice::new("usePredictableInterfaceNamesTooLong", long), Choice::new("a", "b")];
        let out = render(&wide, 60);
        assert!(out.lines().nth(2).is_some_and(|l| l.starts_with(&" ".repeat(39)) && !l.starts_with(&" ".repeat(40))), "{out}");
        assert!(out.lines().all(|l| l.chars().count() <= 60), "{out}");
        // Too long for three lines: the third ends in `…`.
        let out = render(&[Choice::new("a", &"abcd ".repeat(40))], 30);
        assert_eq!(out.lines().count(), 1 + 3, "{out}");
        assert!(out.lines().last().is_some_and(|l| l.ends_with('…')), "{out}");
    }

    #[test]
    fn a_narrow_window_puts_help_under_its_word() {
        let long = "This option allows you to define interfaces encapsulating IPv6 packets within IPv4 packets.";
        let choices = vec![Choice::new("usePredictableInterfaceNames", long), Choice::new("sits", "Short."), Choice::new("ifstate", "")];
        for fit in [20, 30] {
            let out = render(&choices, fit);
            let lines: Vec<&str> = out.lines().collect();
            assert_eq!(lines[1], "  usePredictableInterfaceNames", "{out}");
            assert!(lines[2].starts_with("    This") && !lines[2].starts_with("     "), "{out}");
            assert!(out.contains("\n  sits\n    Short.\n  ifstate\n"), "{out}");
            assert!(out.lines().filter(|l| l.starts_with("    ")).all(|l| l.chars().count() <= fit), "{fit}: {out}");
        }
        // A short help that fits beside its word stays beside it.
        assert!(render(&choices, 40).contains(&format!("\n  {:<24}  Short.\n", "sits")));
        let out = render(&[Choice::new(&"w".repeat(40), long), Choice::new("a", "b")], 50);
        assert!(out.contains(&format!("\n  {}\n    This", "w".repeat(40))), "{out}");
    }

    #[test]
    fn help_without_nixos_markup_and_cut_text_marked() {
        assert_eq!(summary("Interfaces for which to start {command}`wpa_supplicant`. The supplicant is used."), "Interfaces for which to start wpa_supplicant.");
        assert_eq!(summary("Whether to assign [predictable names to network interfaces](https://x.org/a/). If enabled, in"), "Whether to assign predictable names to network interfaces.");
        assert_eq!(summary("See [](#opt-networking.useDHCP) for more."), "See networking.useDHCP for more.");
        assert_eq!(summary("The list of nameservers.  It can be left empty."), "The list of nameservers.");
        assert_eq!(summary("Use a tool, e.g. Docker, for this. And more."), "Use a tool, e.g. Docker, for this.");
        assert_eq!(summary("Keeps {braces} and [brackets] that are not markup"), "Keeps {braces} and [brackets] that are not markup");
        assert_eq!(summary("Set to `${HOME}`, or `%{name}`."), "Set to ${HOME}, or %{name}.", "not a role: kept");
        assert_eq!(summary("Extra flags (e.g. Docker) here. More."), "Extra flags (e.g. Docker) here.");
        let cut = "word ".repeat(32);
        assert_eq!(cut.chars().count(), 160);
        let s = summary(&cut);
        assert!(s.ends_with("word…") && !s.contains("  "), "{s}");
    }
}
