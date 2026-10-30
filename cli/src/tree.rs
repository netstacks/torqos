//! The configuration tree (ADR-016, docs/CLI-DESIGN.md): the schema TorqOS installs as
//! /etc/torq/schema.json (modules/schema.nix), the tree's data as the config repo files it lives in,
//! `set` / `delete` in Junos syntax, and the views of it: `show configuration`, `| display set`,
//! `| compare`.
//!
//! Pure: no I/O but parsing what it is given. The modules' checks stay the authority on what is
//! valid; the checks here are for early, clear errors.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Container,
    List,
    Leaf,
    LeafList,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Node {
    pub kind: Kind,
    pub help: String,
    pub file: Option<String>,
    pub key: Option<String>,
    #[serde(default = "system")]
    pub tier: String,
    pub values: Option<BTreeMap<String, String>>,
    #[serde(rename = "type")]
    pub ty: Option<String>,
}

fn system() -> String {
    "system".into()
}

#[derive(Debug, Clone, Deserialize)]
pub struct Schema {
    pub version: u32,
    pub nodes: BTreeMap<String, Node>,
}

/// The tree's data: the config repo files it lives in, by path. A leaf's file is a JSON object of
/// strings, a leaf-list's a JSON list of strings. An absent file is an empty one.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Config {
    pub files: BTreeMap<String, Data>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Data {
    Object(BTreeMap<String, String>),
    List(Vec<String>),
}

impl Data {
    /// The file's content as TorqOS writes it: keys sorted, lists sorted and de-duplicated, one item
    /// per line (as `torq pkg` does, so diffs stay small), a final newline.
    pub fn render(&self) -> String {
        let v = match self {
            Data::Object(o) => serde_json::to_value(o),
            Data::List(l) => serde_json::to_value(l.iter().collect::<BTreeSet<_>>()),
        }
        .expect("strings serialise");
        match self {
            Data::List(l) if l.is_empty() => "[]\n".into(),
            _ => format!("{}\n", serde_json::to_string_pretty(&v).expect("strings serialise")),
        }
    }
}

/// A command's words resolved against the schema.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    /// The schema node: its key, e.g. "users <name> cli style".
    pub node: String,
    /// The words naming it, e.g. ["users", "viper", "cli", "style"].
    pub words: Vec<String>,
    /// The list entry name, if the path passes through a list (`users viper …`).
    pub name: Option<String>,
    /// The words after the node: a leaf's value, a leaf-list's elements.
    pub values: Vec<String>,
}

/// What one entry of the tree holds: a leaf's value, or a leaf-list's elements.
#[derive(Debug, Clone, PartialEq)]
enum Val {
    One(String),
    Many(Vec<String>),
}

impl Schema {
    pub fn parse(json: &str) -> Result<Schema> {
        let s: Schema = serde_json::from_str(json).context("reading the TorqOS schema")?;
        if s.version != 1 {
            bail!("this torqcli reads schema version 1, not {}", s.version);
        }
        Ok(s)
    }

    fn node(&self, key: &str) -> &Node {
        &self.nodes[key]
    }

    /// The nodes one word below `key` ("" for the top): (word, node key). Below a list, the one child
    /// is its entry, `<name>`.
    pub fn children(&self, key: &str) -> Vec<(String, String)> {
        let depth = if key.is_empty() { 0 } else { key.split(' ').count() };
        self.nodes
            .keys()
            .filter(|k| k.split(' ').count() == depth + 1 && (key.is_empty() || k.starts_with(&format!("{key} "))))
            .map(|k| (k.rsplit(' ').next().unwrap().to_string(), k.clone()))
            .collect()
    }

    /// Resolve the words of `set …` / `delete …` / `show configuration …` to a node. Stops at a leaf
    /// or leaf-list (the rest are its values) or when the words run out (a container or list).
    pub fn resolve(&self, words: &[String]) -> Result<Target> {
        let (mut key, mut path, mut name) = (String::new(), Vec::new(), None);
        let mut i = 0;
        while i < words.len() {
            let w = &words[i];
            if !key.is_empty() && self.node(&key).kind == Kind::List {
                let list = self.node(&key);
                check_name(&key, list, w)?;
                key = format!("{key} <name>");
                name = Some(w.clone());
            } else {
                let next = if key.is_empty() { w.clone() } else { format!("{key} {w}") };
                if !self.nodes.contains_key(&next) {
                    let options: Vec<String> = self.children(&key).into_iter().map(|(w, _)| w).collect();
                    let at = if key.is_empty() { "the top".to_string() } else { format!("'{}'", path.join(" ")) };
                    bail!("syntax error: '{w}' is not a setting at {at}; one of: {}", options.join(", "));
                }
                key = next;
            }
            path.push(w.clone());
            i += 1;
            if matches!(self.node(&key).kind, Kind::Leaf | Kind::LeafList) {
                break;
            }
        }
        Ok(Target { node: key, words: path, name, values: words[i..].to_vec() })
    }

    /// The repo file a node's data is in, for a list entry `name`.
    fn file_of(&self, key: &str, name: Option<&str>) -> Option<String> {
        let f = self.node(key).file.as_ref()?;
        Some(match name {
            Some(n) => f.replace("<name>", n),
            None => f.clone(),
        })
    }

    /// Check a value for a leaf or leaf-list element.
    fn check_value(&self, key: &str, value: &str) -> Result<()> {
        let n = self.node(key);
        let what = key.replace("<name>", "…");
        if let Some(values) = &n.values {
            if !values.contains_key(value) {
                let mut near: Vec<&String> = values.keys().filter(|v| v.to_lowercase().contains(&value.to_lowercase())).take(5).collect();
                near.sort();
                let hint = if near.is_empty() { String::new() } else { format!(" (did you mean: {}?)", near.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")) };
                bail!("'{value}' is not a valid {what}{hint}");
            }
            return Ok(());
        }
        let ok = match n.ty.as_deref() {
            Some("package") => package_name_ok(value),
            Some("host-name") => host_name_ok(value),
            Some("user-name") => user_name_ok(value),
            _ => !value.is_empty(),
        };
        if !ok {
            bail!("'{value}' is not a valid {what}");
        }
        Ok(())
    }

    /// `set <words>`: give a leaf a value, or add elements to a leaf-list.
    pub fn set(&self, cfg: &mut Config, words: &[String]) -> Result<()> {
        let t = self.resolve(words)?;
        let n = self.node(&t.node);
        let file = self.file_of(&t.node, t.name.as_deref());
        match (n.kind, file) {
            (Kind::Leaf, Some(file)) => {
                let [value] = t.values.as_slice() else {
                    bail!("set {}: needs exactly one value", t.words.join(" "));
                };
                self.check_value(&t.node, value)?;
                let key = n.key.clone().context("schema: a leaf without a key")?;
                match cfg.files.entry(file.clone()).or_insert_with(|| Data::Object(BTreeMap::new())) {
                    Data::Object(o) => {
                        o.insert(key, value.clone());
                    }
                    Data::List(_) => bail!("{file} holds a list, not settings"),
                }
            }
            (Kind::LeafList, Some(file)) => {
                let values = list_values(&t.values)?;
                if values.is_empty() {
                    bail!("set {}: needs a value", t.words.join(" "));
                }
                for v in &values {
                    self.check_value(&t.node, v)?;
                }
                match cfg.files.entry(file.clone()).or_insert_with(|| Data::List(Vec::new())) {
                    Data::List(l) => {
                        l.extend(values);
                        l.sort();
                        l.dedup();
                    }
                    Data::Object(_) => bail!("{file} holds settings, not a list"),
                }
            }
            _ => {
                let options: Vec<String> = self.children(&t.node).into_iter().map(|(w, _)| w).collect();
                bail!("set {}: incomplete; one of: {}", t.words.join(" "), options.join(", "));
            }
        }
        Ok(())
    }

    /// `delete <words>`: remove a leaf, elements of a leaf-list (all of them without values), or
    /// everything below a container or list entry.
    pub fn delete(&self, cfg: &mut Config, words: &[String]) -> Result<()> {
        let t = self.resolve(words)?;
        let n = self.node(&t.node);
        let gone = |what: &str| anyhow::anyhow!("delete {}: {what} is not set", t.words.join(" "));
        match n.kind {
            Kind::Leaf => {
                if !t.values.is_empty() {
                    bail!("delete {}: takes no value", t.words.join(" "));
                }
                let file = self.file_of(&t.node, t.name.as_deref()).context("schema: a leaf without a file")?;
                let key = n.key.as_deref().context("schema: a leaf without a key")?;
                let Some(Data::Object(o)) = cfg.files.get_mut(&file) else { return Err(gone(key)) };
                o.remove(key).ok_or_else(|| gone(key))?;
                if o.is_empty() {
                    cfg.files.remove(&file);
                }
            }
            Kind::LeafList => {
                let file = self.file_of(&t.node, t.name.as_deref()).context("schema: a leaf-list without a file")?;
                let values = list_values(&t.values)?;
                let Some(Data::List(l)) = cfg.files.get_mut(&file) else { return Err(gone("it")) };
                if values.is_empty() {
                    cfg.files.remove(&file);
                } else {
                    for v in &values {
                        let at = l.iter().position(|x| x == v).ok_or_else(|| gone(v))?;
                        l.remove(at);
                    }
                    if l.is_empty() {
                        cfg.files.remove(&file);
                    }
                }
            }
            Kind::Container | Kind::List => {
                if !t.values.is_empty() {
                    bail!("delete {}: unexpected '{}'", t.words.join(" "), t.values[0]);
                }
                let below: Vec<(String, Option<String>)> = self
                    .instances(cfg)
                    .into_iter()
                    .filter(|(key, name, _)| under(key, name.as_deref(), &t))
                    .map(|(key, name, _)| (key, name))
                    .collect();
                if below.is_empty() {
                    return Err(gone("nothing below it"));
                }
                for (key, name) in below {
                    let file = self.file_of(&key, name.as_deref()).expect("instances have files");
                    match (self.node(&key).kind, cfg.files.get_mut(&file)) {
                        (Kind::Leaf, Some(Data::Object(o))) => {
                            o.remove(self.node(&key).key.as_deref().unwrap_or_default());
                            if o.is_empty() {
                                cfg.files.remove(&file);
                            }
                        }
                        _ => {
                            cfg.files.remove(&file);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Every set leaf and leaf-list in `cfg`: (node key, list entry name, value).
    fn instances(&self, cfg: &Config) -> Vec<(String, Option<String>, Val)> {
        let mut out = Vec::new();
        for (key, n) in &self.nodes {
            let Some(pattern) = &n.file else { continue };
            for (file, data) in &cfg.files {
                let Some(name) = matches_file(pattern, file) else { continue };
                let val = match (n.kind, data) {
                    (Kind::Leaf, Data::Object(o)) => match o.get(n.key.as_deref().unwrap_or_default()) {
                        Some(v) => Val::One(v.clone()),
                        None => continue,
                    },
                    (Kind::LeafList, Data::List(l)) if !l.is_empty() => {
                        let mut l = l.clone();
                        l.sort();
                        l.dedup();
                        Val::Many(l)
                    }
                    _ => continue,
                };
                out.push((key.clone(), name, val));
            }
        }
        out
    }

    /// The tree's entries as (words, value), in display order.
    fn entries(&self, cfg: &Config) -> Vec<(Vec<String>, Val)> {
        let mut out: Vec<(Vec<String>, Val)> = self
            .instances(cfg)
            .into_iter()
            .map(|(key, name, val)| {
                let words = key
                    .split(' ')
                    .map(|w| if w == "<name>" { name.clone().unwrap_or_default() } else { w.to_string() })
                    .collect();
                (words, val)
            })
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// `show configuration [<words>]`, Junos style: containers in braces, `leaf value;`,
    /// `leaf-list [ a b ];`.
    pub fn show(&self, cfg: &Config, prefix: &[String]) -> String {
        let entries: Vec<(Vec<String>, Val)> = self
            .entries(cfg)
            .into_iter()
            .filter(|(w, _)| w.starts_with(prefix))
            .map(|(w, v)| (w[prefix.len()..].to_vec(), v))
            .collect();
        let mut out = String::new();
        render(&entries, 0, &mut out);
        out
    }

    /// `show configuration | display set`: one `set` command per value.
    pub fn display_set(&self, cfg: &Config) -> String {
        let mut out = String::new();
        for (words, val) in self.entries(cfg) {
            let path = words.iter().map(|w| quote(w)).collect::<Vec<_>>().join(" ");
            match val {
                Val::One(v) => out.push_str(&format!("set {path} {}\n", quote(&v))),
                Val::Many(vs) => {
                    for v in vs {
                        out.push_str(&format!("set {path} {}\n", quote(&v)));
                    }
                }
            }
        }
        out
    }

    /// `show | compare`: what `new` changes from `old`, Junos style — under `[edit <container>]`,
    /// `-` lines for what goes and `+` lines for what comes.
    pub fn compare(&self, old: &Config, new: &Config) -> String {
        let (a, b): (BTreeMap<_, _>, BTreeMap<_, _>) = (self.entries(old).into_iter().collect(), self.entries(new).into_iter().collect());
        let paths: BTreeSet<&Vec<String>> = a.keys().chain(b.keys()).collect();
        let mut out = String::new();
        let mut section: Option<Vec<String>> = None;
        for p in paths {
            let (was, now) = (a.get(p), b.get(p));
            if was == now {
                continue;
            }
            let parent = p[..p.len() - 1].to_vec();
            if section.as_ref() != Some(&parent) {
                out.push_str(&if parent.is_empty() { "[edit]\n".to_string() } else { format!("[edit {}]\n", parent.join(" ")) });
                section = Some(parent);
            }
            let leaf = p.last().unwrap();
            if let Some(v) = was {
                out.push_str(&format!("-   {};\n", statement(leaf, v)));
            }
            if let Some(v) = now {
                out.push_str(&format!("+   {};\n", statement(leaf, v)));
            }
        }
        out
    }
}

/// `name` names a list entry of `list` (at `key`): its type, and one of its values if it has them.
fn check_name(key: &str, list: &Node, name: &str) -> Result<()> {
    let shaped = match list.ty.as_deref() {
        Some("user-name") => user_name_ok(name),
        _ => !name.is_empty() && !name.contains(['/', ' ']) && name != "." && name != "..",
    };
    if !shaped {
        bail!("'{name}' is not a valid name in {key}");
    }
    if let Some(values) = &list.values {
        if !values.contains_key(name) {
            let known = values.keys().cloned().collect::<Vec<_>>().join(", ");
            bail!("'{name}' is not one of the {key} on this machine ({known})");
        }
    }
    Ok(())
}

/// The list entry name if `file` is an instance of the schema's file `pattern` (`users/<name>/x`).
fn matches_file(pattern: &str, file: &str) -> Option<Option<String>> {
    match pattern.split_once("<name>") {
        None => (pattern == file).then_some(None),
        Some((pre, post)) => {
            let name = file.strip_prefix(pre)?.strip_suffix(post)?;
            (!name.is_empty() && !name.contains('/')).then(|| Some(name.to_string()))
        }
    }
}

/// Whether node `key` (for list entry `name`) lies below the target's container or list entry.
fn under(key: &str, name: Option<&str>, t: &Target) -> bool {
    let inside = key.starts_with(&format!("{} ", t.node));
    inside && (t.name.is_none() || name == t.name.as_deref())
}

/// A leaf-list's values: `a b` or `[ a b ]`.
fn list_values(words: &[String]) -> Result<Vec<String>> {
    match words.first().map(String::as_str) {
        Some("[") => {
            if words.last().map(String::as_str) != Some("]") || words.len() < 2 {
                bail!("missing ']'");
            }
            Ok(words[1..words.len() - 1].to_vec())
        }
        _ => Ok(words.to_vec()),
    }
}

fn statement(leaf: &str, v: &Val) -> String {
    match v {
        Val::One(v) => format!("{leaf} {}", quote(v)),
        Val::Many(vs) => format!("{leaf} [ {} ]", vs.iter().map(|v| quote(v)).collect::<Vec<_>>().join(" ")),
    }
}

fn render(entries: &[(Vec<String>, Val)], depth: usize, out: &mut String) {
    let indent = "    ".repeat(depth);
    let mut i = 0;
    while i < entries.len() {
        let (words, val) = &entries[i];
        if words.len() == 1 {
            out.push_str(&format!("{indent}{};\n", statement(&words[0], val)));
            i += 1;
            continue;
        }
        let head = &words[0];
        let j = entries[i..].iter().position(|(w, _)| w.len() == 1 || &w[0] != head).map_or(entries.len(), |p| i + p);
        let group: Vec<(Vec<String>, Val)> = entries[i..j].iter().map(|(w, v)| (w[1..].to_vec(), v.clone())).collect();
        out.push_str(&format!("{indent}{} {{\n", quote(head)));
        render(&group, depth + 1, out);
        out.push_str(&format!("{indent}}}\n"));
        i = j;
    }
}

/// A word as Junos shows it: quoted if it has spaces or characters the syntax uses.
fn quote(w: &str) -> String {
    if w.is_empty() || w.contains([' ', ';', '{', '}', '[', ']', '"', '#']) {
        format!("\"{}\"", w.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        w.to_string()
    }
}

/// The form of a package name, as the broker and modules/packages.nix accept it: a top-level
/// nixpkgs attribute, or one level under a package set.
pub fn package_name_ok(name: &str) -> bool {
    let segment = |s: &str| {
        let mut c = s.chars();
        c.next().is_some_and(|f| f.is_ascii_alphabetic() || f == '_')
            && c.all(|x| x.is_ascii_alphanumeric() || "_'+-".contains(x))
    };
    let parts: Vec<&str> = name.split('.').collect();
    parts.len() <= 2 && parts.iter().all(|p| segment(p))
}

/// RFC 1123, one label (modules/host-rules.nix).
pub fn host_name_ok(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=63).contains(&b.len())
        && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'-')
        && b[0] != b'-'
        && b[b.len() - 1] != b'-'
}

/// A user's login name as the broker treats it as user tier: `[a-z_][a-z0-9_-]*`.
pub fn user_name_ok(s: &str) -> bool {
    let mut c = s.chars();
    c.next().is_some_and(|f| f.is_ascii_lowercase() || f == '_')
        && c.all(|x| x.is_ascii_lowercase() || x.is_ascii_digit() || x == '_' || x == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real schema, as the modules produce it for a machine whose one user is `dev`
    /// (tests/tree.nix checks this copy is current). Regenerate after a schema change:
    ///   nix eval --impure --raw --expr 'let f = builtins.getFlake (toString ./.); in (f.lib.mkSystem {
    ///     system = "aarch64-linux"; configDir = ./tests/tree/cli-good; modules = [ { torq.user = "dev"; } ];
    ///   }).config.environment.etc."torq/schema.json".text' | jq -S . > cli/src/schema-fixture.json
    fn schema() -> Schema {
        Schema::parse(include_str!("schema-fixture.json")).unwrap()
    }
    fn w(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }
    fn err(r: Result<()>) -> String {
        format!("{:#}", r.unwrap_err())
    }
    fn configured() -> Config {
        let (s, mut c) = (schema(), Config::default());
        for cmd in [
            "system host-name torq",
            "system time-zone Europe/Berlin",
            "system packages [ ripgrep htop ]",
            "services ssh",
            "users dev packages jq",
            "users dev cli style cisco",
        ] {
            s.set(&mut c, &w(cmd)).unwrap();
        }
        c
    }

    #[test]
    fn resolves_paths_through_the_schema() {
        let s = schema();
        let t = s.resolve(&w("users dev cli style cisco")).unwrap();
        assert_eq!((t.node.as_str(), t.name.as_deref(), t.values.clone()), ("users <name> cli style", Some("dev"), w("cisco")));
        assert_eq!(t.words, w("users dev cli style"));
        let t = s.resolve(&w("system")).unwrap();
        assert_eq!((t.node.as_str(), t.values.len()), ("system", 0));
        let e = format!("{:#}", s.resolve(&w("system hostname x")).unwrap_err());
        assert!(e.contains("'hostname' is not a setting at 'system'") && e.contains("host-name"), "{e}");
        // List entries: only the machine's users, and only well-formed names, before any path is built.
        for (bad, why) in [("nobody", "not one of the users"), ("Bad.Name", "not a valid name"), ("../etc", "not a valid name"), ("cli", "not one of the users")] {
            let e = format!("{:#}", s.resolve(&w(&format!("users {bad} packages jq"))).unwrap_err());
            assert!(e.contains(why), "{bad}: {e}");
        }
    }

    #[test]
    fn set_writes_one_key_or_list_of_one_file() {
        let c = configured();
        assert_eq!(c.files["system/host.json"], Data::Object([("host-name".into(), "torq".into()), ("time-zone".into(), "Europe/Berlin".into())].into()));
        assert_eq!(c.files["system/packages.json"], Data::List(w("htop ripgrep")));
        assert_eq!(c.files["services.json"], Data::List(w("ssh")));
        assert_eq!(c.files["users/dev/packages.json"], Data::List(w("jq")));
        assert_eq!(c.files["users/dev/cli.json"], Data::Object([("style".into(), "cisco".into())].into()));
        assert_eq!(c.files.len(), 5);
    }

    #[test]
    fn set_checks_values() {
        let (s, mut c) = (schema(), Config::default());
        let e = err(s.set(&mut c, &w("system time-zone Europe/Berln")));
        assert!(e.contains("not a valid system time-zone"), "{e}");
        let e = err(s.set(&mut c, &w("system time-zone berlin")));
        assert!(e.contains("did you mean: Europe/Berlin"), "{e}");
        for bad in ["system host-name my_laptop", "system host-name -x", "system packages ../x", "system packages a.b.c", "services telnet", "system cli style vim"] {
            assert!(err(s.set(&mut c, &w(bad))).contains("is not a valid"), "{bad}");
        }
        assert!(err(s.set(&mut c, &w("system host-name a b"))).contains("exactly one value"));
        assert!(err(s.set(&mut c, &w("system packages"))).contains("needs a value"));
        assert!(err(s.set(&mut c, &w("system packages [ htop"))).contains("missing ']'"));
        let e = err(s.set(&mut c, &w("system cli")));
        assert!(e.contains("incomplete; one of: style"), "{e}");
        assert_eq!(c, Config::default(), "nothing invalid was written");
    }

    #[test]
    fn delete_removes_exactly_what_it_names() {
        let s = schema();
        let mut c = configured();
        s.delete(&mut c, &w("system time-zone")).unwrap();
        assert_eq!(c.files["system/host.json"], Data::Object([("host-name".into(), "torq".into())].into()));
        s.delete(&mut c, &w("system host-name")).unwrap();
        assert!(!c.files.contains_key("system/host.json"), "an empty object is no file");
        s.delete(&mut c, &w("system packages htop")).unwrap();
        assert_eq!(c.files["system/packages.json"], Data::List(w("ripgrep")));
        s.delete(&mut c, &w("system packages")).unwrap();
        assert!(!c.files.contains_key("system/packages.json"));
        // A list entry: everything below it, each in its own file.
        s.delete(&mut c, &w("users dev")).unwrap();
        assert!(c.files.keys().all(|f| !f.starts_with("users/")), "{:?}", c.files.keys());
        assert_eq!(c.files.keys().collect::<Vec<_>>(), ["services.json"]);
        // Not set: an error, as in Junos ("statement not found").
        for gone in ["system host-name", "system packages", "system packages vim", "users dev", "system"] {
            assert!(err(s.delete(&mut c, &w(gone))).contains("is not set"), "{gone}");
        }
        assert!(err(s.delete(&mut c, &w("system host-name torq"))).contains("takes no value"));
    }

    #[test]
    fn delete_of_a_container_takes_everything_below_it() {
        let (s, mut c) = (schema(), configured());
        s.delete(&mut c, &w("system")).unwrap();
        assert_eq!(c.files.keys().collect::<Vec<_>>(), ["services.json", "users/dev/cli.json", "users/dev/packages.json"]);
        let mut c = configured();
        s.delete(&mut c, &w("users")).unwrap();
        assert!(c.files.keys().all(|f| !f.starts_with("users/")));
    }

    #[test]
    fn show_configuration_is_junos_style() {
        let (s, c) = (schema(), configured());
        assert_eq!(
            s.show(&c, &[]),
            "services [ ssh ];\n\
             system {\n    host-name torq;\n    packages [ htop ripgrep ];\n    time-zone Europe/Berlin;\n}\n\
             users {\n    dev {\n        cli {\n            style cisco;\n        }\n        packages [ jq ];\n    }\n}\n"
        );
        assert_eq!(s.show(&c, &w("users dev")), "cli {\n    style cisco;\n}\npackages [ jq ];\n");
        assert_eq!(s.show(&Config::default(), &[]), "");
    }

    #[test]
    fn display_set_is_one_command_per_value() {
        let (s, c) = (schema(), configured());
        assert_eq!(
            s.display_set(&c),
            "set services ssh\nset system host-name torq\nset system packages htop\nset system packages ripgrep\n\
             set system time-zone Europe/Berlin\nset users dev cli style cisco\nset users dev packages jq\n"
        );
        // ...and reading it back gives the same configuration.
        let mut again = Config::default();
        for line in s.display_set(&c).lines() {
            s.set(&mut again, &w(line.strip_prefix("set ").unwrap())).unwrap();
        }
        assert_eq!(again, c);
    }

    #[test]
    fn compare_shows_what_changes_under_its_container() {
        let s = schema();
        let old = configured();
        let mut new = old.clone();
        s.set(&mut new, &w("system host-name laptop")).unwrap();
        s.set(&mut new, &w("system packages vim")).unwrap();
        s.delete(&mut new, &w("users dev cli")).unwrap();
        assert_eq!(
            s.compare(&old, &new),
            "[edit system]\n-   host-name torq;\n+   host-name laptop;\n-   packages [ htop ripgrep ];\n+   packages [ htop ripgrep vim ];\n\
             [edit users dev cli]\n-   style cisco;\n"
        );
        assert_eq!(s.compare(&old, &old), "");
    }

    #[test]
    fn files_are_written_as_torqos_writes_them() {
        assert_eq!(Data::List(w("ripgrep htop htop")).render(), "[\n  \"htop\",\n  \"ripgrep\"\n]\n");
        assert_eq!(Data::List(vec![]).render(), "[]\n");
        let o = Data::Object([("time-zone".into(), "UTC".into()), ("host-name".into(), "a".into())].into());
        assert_eq!(o.render(), "{\n  \"host-name\": \"a\",\n  \"time-zone\": \"UTC\"\n}\n");
    }

    /// Deleting one user's settings leaves everyone else's alone.
    #[test]
    fn delete_of_a_list_entry_is_only_that_entry() {
        let mut s = schema();
        s.nodes.get_mut("users").unwrap().values.as_mut().unwrap().insert("ana".into(), String::new());
        let mut c = configured();
        s.set(&mut c, &w("users ana packages vim")).unwrap();
        s.set(&mut c, &w("users ana cli style juniper")).unwrap();
        s.delete(&mut c, &w("users dev")).unwrap();
        let users: Vec<&String> = c.files.keys().filter(|f| f.starts_with("users/")).collect();
        assert_eq!(users, ["users/ana/cli.json", "users/ana/packages.json"]);
    }

    #[test]
    fn value_rules_match_the_modules() {
        for ok in ["htop", "python3Packages.numpy", "g++", "xorg'"] {
            assert!(package_name_ok(ok), "{ok}");
        }
        for bad in ["", "a.b.c", "1password", ".x", "x.", "a b", "../x"] {
            assert!(!package_name_ok(bad), "{bad}");
        }
        assert!(host_name_ok("torq-1") && host_name_ok(&"a".repeat(63)));
        for bad in ["", "-a", "a-", "a.b", "my_laptop", &"a".repeat(64)] {
            assert!(!host_name_ok(bad), "{bad}");
        }
        assert!(user_name_ok("dev") && user_name_ok("_svc") && !user_name_ok("Dev") && !user_name_ok("a.b"));
    }
}
