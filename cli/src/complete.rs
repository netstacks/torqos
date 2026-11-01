//! `?` and Tab for torqcli (ADR-016, ADR-025): what may come next at a point of a command line,
//! with one line of help each — the commands from the grammar (grammar.rs), configuration paths
//! from the tree and the schema. Pure: the line editor (shell.rs) asks, this answers.

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

/// What may follow `words` at `at`: the next words, `|` after a command whose output may be piped,
/// and `<[Enter]>` when the line could run as it is.
pub fn choices_at(k: &Know, at: &At, words: &[String], partial: &str) -> Vec<Choice> {
    let mut all = next(k, at, words, partial);
    // (After a filter's own name, its options are its own: `| grep ?` lists nothing.)
    let is_filter = |w: &String| crate::filter::FILTERS.iter().any(|(f, _)| f == w);
    if partial.is_empty() && !words.last().is_some_and(is_filter) {
        if let Ok((op, _)) = crate::syntax::parse_line(at.mode, words) {
            let piped = words.iter().any(|w| w == "|");
            if (piped || crate::grammar::filtered(at.mode, words)) && !all.iter().any(|c| c.word == "|") {
                all.push(Choice::new("|", "Pipe the output (display, grep, wc, …)"));
            }
            if complete(k, at, &op) {
                all.insert(0, Choice::new(ENTER.0, ENTER.1));
            }
        }
    }
    all.into_iter().filter(|c| c.is_placeholder() || c.word.starts_with(partial)).collect()
}

/// What may follow `words` at `at`: the grammar's words and values, configuration paths from the
/// tree, and after a `|`, the pipes.
fn next(k: &Know, at: &At, words: &[String], _partial: &str) -> Vec<Choice> {
    if words.iter().any(|w| w == "|") {
        return crate::grammar::pipe_next(at.mode, words).into_iter().map(|(w, h)| Choice::new(&w, &h)).collect();
    }
    let Ok(walked) = crate::grammar::walk(at.mode, words) else { return vec![] };
    let mut out = Vec::new();
    for o in crate::grammar::offers(&walked) {
        match o {
            crate::grammar::Offer::Word(w, h) => out.push(Choice::new(w, h)),
            crate::grammar::Offer::Value(w, h) => out.push(Choice::new(&w, h)),
            crate::grammar::Offer::Path { verb, from_top } => {
                let path: Vec<String> = if from_top { walked.read.path.clone() } else { at.edit.iter().chain(&walked.read.path).cloned().collect() };
                let mut c = tree_choices(k, &path, if verb == "edit" { "show" } else { verb });
                if verb == "edit" {
                    c.retain(|x| x.word != "|");
                }
                out.extend(c);
            }
        }
    }
    out
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
        Op::Run(op) => complete(k, &At { mode: Mode::Operational, ..at.clone() }, op),
        _ => true,
    }
}

/// After a `|` in a `show`: the filters (filter.rs), which work on any show's output and, once one is
/// used, are the only pipes left. Their own options are theirs (`grep --help`).
/// What a line is read with: the hand-made schema, and NixOS's tree.
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
    if let Some(hand) = crate::whole::hand_words(rest) {
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

/// `?`: what may follow, or, where nothing may, why — in the words Enter would use (the same
/// grammar: `expand`), laid out to fit a window `fit` wide.
pub fn answer(k: &Know, at: &At, words: &[String], partial: &str, fit: usize) -> String {
    let c = choices_at(k, at, words, partial);
    if !c.is_empty() {
        return render(&c, fit);
    }
    let mut line = words.to_vec();
    if !partial.is_empty() {
        line.push(partial.to_string());
    }
    let why = match crate::syntax::parse_line(at.mode, &line) {
        Err(e) => format!("{e:#}"),
        Ok(_) => format!("Nothing more is known to follow `{}`: Enter runs it, and says what is wrong with it, if anything.", line.join(" ")),
    };
    format!("{}\n", crate::ui::hang("", &why, Some(fit), fit))
}

/// The choices, laid out to fit a window `fit` wide (ui.rs): each help wrapped under the help
/// column, never back to the left edge.
fn render(choices: &[Choice], fit: usize) -> String {
    let rows: Vec<(String, String)> = choices.iter().take(60).map(|c| (c.word.clone(), c.help.clone())).collect();
    let mut out = format!("Possible completions:\n{}", table(&rows, fit));
    if choices.len() > 60 {
        out.push_str(&format!("  … and {} more: type more of the word\n", choices.len() - 60));
    }
    out
}

/// Words and their help as `?` and `help` lay them out, to fit a window `fit` wide (ui.rs): each
/// help a sentence beside its word, wrapped under the help column (or under the word in a narrow
/// window), never back to the left edge.
pub fn table(rows: &[(String, String)], fit: usize) -> String {
    let width = rows.iter().map(|(w, _)| w.chars().count()).max().unwrap_or(0).min(COLUMN);
    let mut out = String::new();
    for (word, help) in rows {
        let help = summary(help);
        if help.is_empty() {
            out.push_str(&format!("  {word}\n"));
        } else if word.chars().count() > width && fit.saturating_sub(width + 4) >= crate::ui::NARROWEST {
            // A word wider than the column: its help on the next line, in the help column (where the
            // window leaves no room there, ui::hang puts it under the word).
            out.push_str(&format!("  {word}\n"));
            out.push_str(&crate::ui::hang(&" ".repeat(width + 4), &help, Some(fit), HELP_LINES));
        } else {
            out.push_str(&crate::ui::hang(&format!("  {word:<width$}  "), &help, Some(fit), HELP_LINES));
        }
    }
    out
}

/// The widest a word column gets; a wider word has its help on the next line.
const COLUMN: usize = 24;

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
        assert_eq!(words_of(&choices(&s, Mode::Operational, &[], "")), ["configure", "exit", "file", "help", "quit", "request", "show"]);
        assert_eq!(words_of(&choices(&s, Mode::Operational, &w("show"), "")), ["configuration", "packages", "services", "system"]);
        assert_eq!(words_of(&choices(&s, Mode::Operational, &w("show system"), "")), ["alarms", "commit", "ledger", "rollback"]);
        assert_eq!(words_of(&choices(&s, Mode::Operational, &w("show system rollback 2"), "")), ["<[Enter]>", "compare", "|"]);
        assert_eq!(words_of(&choices(&s, Mode::Operational, &w("request system"), "")), ["password", "repair", "revert", "software", "update"]);
        assert_eq!(words_of(&choices(&s, Mode::Configuration, &[], "s")), ["save", "set", "show"]);
        assert_eq!(words_of(&choices(&s, Mode::Configuration, &w("run"), "")), ["file", "help", "request", "show"], "not configure or exit: already here");
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
        assert_eq!(words_of(&choices(&s, Mode::Operational, &w("file"), "")), ["list", "show"], "edit and delete are configuration mode's (and say so when typed)");
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
        assert_eq!(words_of(&choices(&s, Mode::Operational, &w("show packages x |"), "")), filters, "a search is a show like any other");
        assert!(choices(&s, Mode::Configuration, &w("set system |"), "").is_empty(), "only a show");
    }

    #[test]
    fn requests_for_the_software() {
        let s = schema();
        assert_eq!(words_of(&choices(&s, Mode::Operational, &w("request system"), "")), ["password", "repair", "revert", "software", "update"]);
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
        assert_eq!(c(k, "commit"), ["<[Enter]>", "and-quit", "check", "comment", "confirmed"]);
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
    /// names a wrong word; configuration paths, values and a filter's own words are taken as they are.
    #[test]
    fn the_command_language_abbreviates() {
        let (o, k) = (Mode::Operational, Mode::Configuration);
        let x = |mode, line: &str| crate::grammar::read_line(mode, &w(line)).map(|(_, _, words)| words).map_err(|e| format!("{e:#}"));
        assert_eq!(x(o, "sho sys ala").unwrap(), w("show system alarms"));
        assert_eq!(x(o, "sh conf | display set | grep host").unwrap(), w("show configuration | display set | grep host"));
        assert_eq!(x(o, "sh conf netw").unwrap(), w("show configuration netw"), "a path is the configuration's to read");
        assert_eq!(x(k, "se netw hostName x").unwrap(), w("set netw hostName x"));
        let why = vec!["comm".to_string(), "comm".into(), "why not".into()];
        assert_eq!(crate::grammar::read_line(k, &why).unwrap().2, vec!["commit".to_string(), "comment".into(), "why not".into()], "a value stays");
        assert_eq!(x(k, "ru sh sys ala").unwrap(), w("run show system alarms"));
        assert_eq!(x(k, "to se time timeZone UTC").unwrap(), w("top set time timeZone UTC"));
        assert!(x(k, "s").unwrap_err().contains("'s' is ambiguous: save, set, show"));
        assert_eq!(x(o, "quit").unwrap(), w("quit"), "quit, as exit");
        let e = x(o, "show system foo").unwrap_err();
        assert!(e.contains("'foo' is not a word here (after `show system`)") && e.contains("alarms, commit, ledger, rollback"), "{e}");
        assert!(x(o, "show configuraton").unwrap_err().contains("did you mean configuration?"));
        // A line that is not whole says what may follow, what value it needs, or that nothing may.
        assert_eq!(x(o, "show system").unwrap_err(), "not a whole command: one of these may follow: alarms, commit, ledger, rollback");
        assert_eq!(x(o, "request system revert").unwrap_err(), "needs <change> (the change's number, from show system ledger)");
        assert_eq!(x(o, "show system alarms extra").unwrap_err(), "'extra' is not a word here: nothing may follow `show system alarms`");
        assert_eq!(x(o, "show system rollback x").unwrap_err(), "'x' is not a number (configuration numbers: 0, 1, …)");
    }

    /// The whole command language, walked as a person pressing `?` after every word would (a sample
    /// value where a value goes): every word `?` offers is one the line takes, and wherever `?` offers
    /// `<[Enter]>`, Enter runs the line.
    #[test]
    fn question_mark_and_enter_agree_everywhere() {
        let s = schema();
        let sample = |p: &str| match p {
            "<number>" | "<count>" | "<change>" | "<minutes>" | "<n>" => "1".to_string(),
            "<user>" => "dev".into(),
            "<file>" => "services/x.nix".into(),
            _ => "x".into(),
        };
        let mut checked = 0;
        for mode in [Mode::Operational, Mode::Configuration] {
            let at = At { mode, edit: vec![], data: crate::whole::Data::default() };
            let mut todo: Vec<Vec<String>> = vec![vec![]];
            while let Some(line) = todo.pop() {
                // Configuration paths are the tree's (tested with it), past their first word.
                let walked = crate::grammar::walk(mode, &line).unwrap_or_else(|e| panic!("{mode:?} `{}`: offered, then refused: {e}", line.join(" ")));
                if walked.node().is_some_and(|n| n.word == crate::grammar::Word::Path) || line.len() > 7 {
                    continue;
                }
                let offered = choices_at(&s, &at, &line, "");
                if !line.is_empty() {
                    checked += 1;
                    if offered.iter().any(|c| c.word == ENTER.0) {
                        let parsed = crate::syntax::parse_line(mode, &line);
                        assert!(parsed.is_ok(), "{mode:?} `{}`: `?` offers <[Enter]>, Enter refuses: {:#}", line.join(" "), parsed.unwrap_err());
                    }
                }
                for c in offered.iter().filter(|c| c.word != ENTER.0 && c.word != "|") {
                    let word = if c.is_placeholder() { sample(&c.word) } else { c.word.clone() };
                    // A value that may repeat (search words): once is enough.
                    if line.last() == Some(&word) && c.is_placeholder() {
                        continue;
                    }
                    todo.push([line.clone(), vec![word]].concat());
                }
            }
        }
        assert!(checked > 60, "the walk reached {checked} lines");
    }

    #[test]
    fn question_mark_lists_with_help() {
        let s = schema();
        let out = render(&choices(&s, Mode::Configuration, &w("load"), ""), 80);
        assert!(out.starts_with("Possible completions:\n  merge     Set the file's settings onto the candidate\n"), "{out}");
        let out = render(&choices(&s, Mode::Configuration, &w("set time timeZone"), ""), 80);
        assert!(out.contains("… and 537 more"), "{out}");
    }

    /// Where nothing may follow, `?` says why, as Enter would; a setting's old name completes as its
    /// NixOS path does.
    #[test]
    fn question_mark_says_why_nothing_follows() {
        let s = schema();
        let at = |mode| At { mode, edit: vec![], data: crate::whole::Data::default() };
        let out = answer(&s, &at(Mode::Operational), &w("show system packages"), "", 80);
        let out = out.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(out.contains("'packages' is not a word here (after `show system`)") && out.contains("alarms, commit, ledger, rollback"), "{out}");
        let out = answer(&s, &at(Mode::Operational), &w("show system"), "pac", 80);
        assert!(out.contains("'pac' is not a word here"), "the word being typed too: {out}");
        let out = answer(&s, &at(Mode::Configuration), &w("set system host-name a.b"), "", 80);
        assert!(out.contains("Enter runs it"), "{out}");
        assert!(!out.contains("Possible completions") && out.lines().all(|l| crate::ui::width(l) <= 80));
        let narrow = answer(&s, &at(Mode::Operational), &w("show system packages"), "", 30);
        assert!(narrow.lines().all(|l| crate::ui::width(l) <= 30), "{narrow}");
        // The old name, as its NixOS path.
        for line in ["set system packages", "set torq packages system"] {
            assert_eq!(words_of(&choices(&s, Mode::Configuration, &w(line), "")), ["<package>"], "{line}");
        }
        assert_eq!(words_of(&choices(&s, Mode::Configuration, &w("set system packages"), "b")), ["<package>"], "while typing a name");
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
        // A word wider than the column: its help on the next line, in the same column as the others.
        let wide = vec![Choice::new("usePredictableInterfaceNamesTooLong", long), Choice::new("a", "b")];
        let out = render(&wide, 60);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[1], "  usePredictableInterfaceNamesTooLong", "{out}");
        assert!(lines[2].starts_with(&format!("{}This", " ".repeat(28))), "{out}");
        assert!(lines.last().is_some_and(|l| l.starts_with(&format!("  {:<24}  b", "a"))), "the same column: {out}");
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
        // Room beside the column (50 - 28 ≥ 20): the help column; less: under the word, indented 4.
        let out = render(&[Choice::new(&"w".repeat(40), long), Choice::new("a", "b")], 50);
        assert!(out.contains(&format!("\n  {}\n{}This", "w".repeat(40), " ".repeat(28))), "{out}");
        let out = render(&[Choice::new(&"w".repeat(40), long), Choice::new("a", "b")], 45);
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
