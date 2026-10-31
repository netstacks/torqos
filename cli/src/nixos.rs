//! The NixOS settings tree (docs/CONFIG-IS-THE-DEVICE.md): every NixOS setting of this system as a
//! path of words — `virtualisation docker enable true` for `virtualisation.docker.enable = true` —
//! read from /etc/torq/nixos-schema.json, which the system generates from its own options
//! (modules/nixos-schema.nix). Nothing here is written per setting, so the tree follows NixOS.
//!
//! Pure: it reads the schema it is given, resolves words to a setting, offers what may come next, and
//! turns a setting's value words into the JSON NixOS will be given. The options' types, when the
//! system is evaluated, stay the authority; the checks here are for early, clear errors.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

/// A type as the schema writes it: `{ name, elem?, left?, right?, values? }`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Type {
    pub name: String,
    pub elem: Option<Box<Type>>,
    pub left: Option<Box<Type>>,
    pub right: Option<Box<Type>>,
    pub values: Option<Vec<Value>>,
}

impl Type {
    /// The kind, without its arguments (`strMatching "^…$"` is a `strMatching`).
    fn kind(&self) -> &str {
        self.name.split_whitespace().next().unwrap_or("")
    }

    /// Words for `?`: what a value of this type looks like.
    pub fn describe(&self) -> String {
        match self.kind() {
            "bool" => "true or false".into(),
            "nullOr" => self.elem.as_ref().map_or("a value".into(), |e| e.describe()),
            "listOf" => format!("[ {} … ]", self.elem.as_ref().map_or("value".into(), |e| e.describe())),
            "enum" => format!("one of: {}", self.values.iter().flatten().map(plain).collect::<Vec<_>>().join(", ")),
            "package" => "a package name".into(),
            "attrsOf" | "lazyAttrsOf" => format!("<name> {}", self.elem.as_ref().map_or("value".into(), |e| e.describe())),
            "path" | "pathInStore" => "a path".into(),
            k if is_int(k) => "a number".into(),
            k if is_str(k) => "text".into(),
            other => format!("{other} (in a config file)"),
        }
    }
}

fn plain(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn is_int(k: &str) -> bool {
    matches!(k, "int" | "intBetween" | "unsignedInt" | "unsignedInt8" | "unsignedInt16" | "unsignedInt32" | "positiveInt" | "signedInt8" | "signedInt16" | "signedInt32")
}

fn is_str(k: &str) -> bool {
    matches!(k, "str" | "nonEmptyStr" | "singleLineStr" | "separatedString" | "strMatching" | "lines" | "commas" | "envVar" | "passwdEntry")
}

#[derive(Debug, Deserialize)]
struct Entry {
    loc: Vec<String>,
    #[serde(rename = "type")]
    ty: Type,
    help: String,
}

#[derive(Debug, Deserialize)]
struct File {
    version: u32,
    settings: Vec<Entry>,
    /// What the tree may never change (modules/settings-fence.nix).
    #[serde(default)]
    fence: Vec<Vec<String>>,
}

/// One point of the tree: a setting (with its type and help), what is below it, or both (a set of
/// named entries is a setting whose entries hold more settings).
#[derive(Debug, Default, Clone)]
pub struct Node {
    pub setting: Option<(Type, String)>,
    pub children: BTreeMap<String, Node>,
}

/// Where words lead in the tree.
#[derive(Debug, PartialEq)]
pub struct Resolved<'a> {
    /// The setting's path in NixOS, with entry names in place of `<name>`
    /// (`services.httpd.virtualHosts."example.com".forceSSL` → [services, httpd, virtualHosts, example.com, forceSSL]).
    pub loc: Vec<String>,
    pub ty: &'a Type,
    /// The words after the path: the value.
    pub values: Vec<String>,
}

#[derive(Clone)]
pub struct Tree {
    pub root: Node,
    fence: Vec<Vec<String>>,
}

impl Tree {
    pub fn parse(json: &str) -> Result<Tree> {
        let f: File = serde_json::from_str(json).context("reading the NixOS settings schema")?;
        if f.version != 1 {
            bail!("the NixOS settings schema is version {}; this torqcli reads version 1", f.version);
        }
        let mut root = Node::default();
        for e in f.settings {
            let mut n = &mut root;
            for w in &e.loc {
                n = n.children.entry(w.clone()).or_default();
            }
            n.setting = Some((e.ty, e.help));
        }
        Ok(Tree { root, fence: f.fence })
    }

    /// Whether `loc` (entry names in place of `<name>`) is, or is below, a setting the tree may never
    /// change — as modules/settings-fence.nix decides it (a segment ending in `*` matches entry names
    /// starting with what precedes it). The system's evaluation refuses it anyway; this says so early.
    pub fn fenced(&self, loc: &[String]) -> bool {
        let seg = |pat: &String, s: &String| match pat.strip_suffix('*') {
            Some(prefix) => s.starts_with(prefix),
            None => pat == s,
        };
        // The tree holds data only: a name starting with `_` is a module directive (`_type`, `_module`).
        loc.iter().any(|s| s.starts_with('_'))
            || self.fence.iter().any(|pat| loc.len() >= pat.len() && pat.iter().zip(loc).all(|(p, s)| seg(p, s)))
    }

    /// Follow `words` from the top: names of settings, and at a set of named entries, any word as
    /// the entry's name. Stops at the first setting that takes a value; the words after it are that
    /// value. A path that ends inside the tree, with no value, names a place, not a setting.
    pub fn resolve(&self, words: &[String]) -> Result<Resolved<'_>> {
        let (mut n, mut loc) = (&self.root, Vec::new());
        for (i, w) in words.iter().enumerate() {
            if let Some((ty, _)) = &n.setting {
                if takes_value(ty) {
                    return Ok(Resolved { loc, ty, values: words[i..].to_vec() });
                }
            }
            n = match (n.children.get(w), n.children.get("<name>")) {
                (Some(c), _) => c,
                (None, Some(entry)) if w != "<name>" => entry,
                _ => bail!("{}: no such setting{}", path_text(&loc, w), did_you_mean(n, w)),
            };
            loc.push(w.clone());
        }
        match &n.setting {
            Some((ty, _)) if takes_value(ty) => Ok(Resolved { loc, ty, values: vec![] }),
            _ => bail!("{}: not a setting by itself (`?` shows what is below it)", loc.join(" ")),
        }
    }

    /// Where in `words` an entry is named (`services httpd virtualHosts example.com` → [3]).
    pub fn entry_positions(&self, words: &[String]) -> Vec<usize> {
        let (mut n, mut at) = (&self.root, Vec::new());
        for (i, w) in words.iter().enumerate() {
            n = match (n.children.get(w), n.children.get("<name>")) {
                (Some(c), _) => c,
                (None, Some(e)) => {
                    at.push(i);
                    e
                }
                _ => break,
            };
        }
        at
    }

    /// Whether the last of `words` names an entry of a set of named ones (`virtualHosts example.com`).
    pub fn is_entry(&self, words: &[String]) -> bool {
        let Some((last, parent)) = words.split_last() else { return false };
        let mut n = &self.root;
        for w in parent {
            n = match (n.children.get(w), n.children.get("<name>")) {
                (Some(c), _) => c,
                (None, Some(e)) => e,
                _ => return false,
            };
        }
        !n.children.contains_key(last) && n.children.contains_key("<name>")
    }

    /// Where `words` lead, setting or not: the path (entry names in place of `<name>`), the setting
    /// there if it takes a value, and the words after it. `delete` names places as well as settings.
    pub fn locate(&self, words: &[String]) -> Result<(Vec<String>, Option<&Type>, Vec<String>)> {
        match self.resolve(words) {
            Ok(r) => Ok((r.loc, Some(r.ty), r.values)),
            Err(e) if format!("{e}").contains("not a setting by itself") => Ok((words.to_vec(), None, vec![])),
            Err(e) => Err(e),
        }
    }

    /// What may come after `words` (for `?` and Tab): the names below, `<name>` where an entry is
    /// named, or, at a setting, what its value looks like.
    pub fn next(&self, words: &[String]) -> Vec<(String, String)> {
        let mut n = &self.root;
        for w in words {
            if let Some((ty, _)) = &n.setting {
                if takes_value(ty) {
                    return vec![];
                }
            }
            n = match (n.children.get(w), n.children.get("<name>")) {
                (Some(c), _) => c,
                (None, Some(entry)) => entry,
                _ => return vec![],
            };
        }
        if let Some((ty, help)) = &n.setting {
            if takes_value(ty) {
                return vec![(format!("<{}>", ty.describe()), help.clone())];
            }
        }
        n.children
            .iter()
            .map(|(w, c)| {
                let help = c.setting.as_ref().map(|(_, h)| h.clone()).unwrap_or_default();
                (w.clone(), help)
            })
            .collect()
    }
}

/// Whether a setting takes its value right here (rather than holding settings below it): everything
/// but a set or list of submodules, a submodule itself, and what only Nix code can say.
fn takes_value(t: &Type) -> bool {
    match t.kind() {
        "submodule" => false,
        "attrsOf" | "lazyAttrsOf" | "listOf" | "nullOr" => t.elem.as_ref().is_some_and(|e| takes_value(e)),
        _ => true,
    }
}

fn path_text(loc: &[String], w: &str) -> String {
    let mut p = loc.to_vec();
    p.push(w.to_string());
    p.join(" ")
}

/// A close name at this point, for a misspelling (`virtualisation dokcer` → docker).
fn did_you_mean(n: &Node, w: &str) -> String {
    let close = n.children.keys().filter(|k| *k != "<name>").min_by_key(|k| distance(k, w));
    match close {
        Some(k) if distance(k, w) <= 2 => format!(" (did you mean {k}?)"),
        _ => String::new(),
    }
}

fn distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            cur.push((prev[j] + usize::from(ca != cb)).min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// A setting's value words as the JSON NixOS will get, checked against its type: `true`/`false`
/// (no words: true), numbers, text, one of an enum's values, `[ a b ]` for a list, package names,
/// and for a set of plain values, `<name> <value>` (`environment variables EDITOR vim`). What only
/// Nix can say (functions, free-form sets) is refused: it belongs in a config file.
pub fn value(ty: &Type, words: &[String]) -> Result<Value> {
    let one = |what: &str| -> Result<&String> {
        match words {
            [w] => Ok(w),
            [] => bail!("needs {what}"),
            _ => bail!("takes one value ({what}); in \"quotes\" if it has spaces"),
        }
    };
    Ok(match ty.kind() {
        "bool" => match words {
            [] => Value::Bool(true),
            [w] if w == "true" => Value::Bool(true),
            [w] if w == "false" => Value::Bool(false),
            _ => bail!("true or false"),
        },
        "nullOr" => value(ty.elem.as_deref().context("a nullOr without its type")?, words)?,
        "enum" => {
            let w = one("one of its values")?;
            let ok = ty.values.iter().flatten().find(|v| plain(v) == *w).cloned();
            ok.with_context(|| format!("'{w}' is not one of: {}", ty.values.iter().flatten().map(plain).collect::<Vec<_>>().join(", ")))?
        }
        "listOf" => {
            let elem = ty.elem.as_deref().context("a list without its type")?;
            let items: &[String] = match words {
                [first, inner @ .., last] if first == "[" && last == "]" => inner,
                [w] if w == "[" || w == "]" => bail!("a list is [ a b … ]"),
                other => other,
            };
            Value::Array(items.iter().map(|i| value(elem, std::slice::from_ref(i))).collect::<Result<_>>()?)
        }
        "attrsOf" | "lazyAttrsOf" => {
            let elem = ty.elem.as_deref().context("a set without its type")?;
            match words {
                [name, rest @ ..] if !rest.is_empty() => {
                    Value::Object([(name.clone(), value(elem, rest)?)].into_iter().collect())
                }
                _ => bail!("<name> <value>"),
            }
        }
        "either" => {
            let (l, r) = (ty.left.as_deref().context("either")?, ty.right.as_deref().context("either")?);
            match value(l, words) {
                Ok(v) => v,
                Err(_) => value(r, words)?,
            }
        }
        k if is_int(k) => {
            let w = one("a number")?;
            let n: i64 = w.parse().with_context(|| format!("'{w}' is not a number"))?;
            match k {
                "unsignedInt16" if !(0..=65535).contains(&n) => bail!("{n}: 0 to 65535"),
                "unsignedInt" | "unsignedInt8" | "unsignedInt32" if n < 0 => bail!("{n}: not below 0"),
                "positiveInt" if n < 1 => bail!("{n}: 1 or more"),
                _ => {}
            }
            Value::from(n)
        }
        "package" => {
            let w = one("a package name")?;
            if !crate::tree::package_name_ok(w) {
                bail!("'{w}' is not a package name");
            }
            Value::String(w.clone())
        }
        "path" | "pathInStore" => Value::String(one("a path")?.clone()),
        k if is_str(k) => Value::String(one("text")?.clone()),
        other => bail!("a {other} is not a value here: put it in a config file"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> Tree {
        Tree::parse(include_str!("nixos-fixture.json")).unwrap()
    }
    fn w(s: &str) -> Vec<String> {
        crate::session::words(s).unwrap()
    }
    fn set(t: &Tree, line: &str) -> Result<(Vec<String>, Value)> {
        let r = t.resolve(&w(line))?;
        Ok((r.loc.clone(), value(r.ty, &r.values)?))
    }

    #[test]
    fn nixos_paths_one_to_one() {
        let t = tree();
        assert_eq!(set(&t, "virtualisation docker enable true").unwrap(), (w("virtualisation docker enable"), Value::Bool(true)));
        assert_eq!(set(&t, "virtualisation docker enable").unwrap().1, Value::Bool(true), "a bare switch is on");
        assert_eq!(set(&t, "services httpd virtualHosts example.com forceSSL true").unwrap(), (w("services httpd virtualHosts example.com forceSSL"), Value::Bool(true)));
        assert_eq!(set(&t, "services httpd virtualHosts example.com documentRoot /var/www").unwrap().1, Value::from("/var/www"));
        assert_eq!(set(&t, "networking firewall allowedTCPPorts [ 80 443 ]").unwrap().1, serde_json::json!([80, 443]));
        assert_eq!(set(&t, "environment systemPackages [ htop git ]").unwrap().1, serde_json::json!(["htop", "git"]));
        assert_eq!(set(&t, "services openssh settings LogLevel VERBOSE").unwrap().1, Value::from("VERBOSE"));
        assert_eq!(set(&t, "services openssh settings PasswordAuthentication false").unwrap().1, Value::Bool(false));
        assert_eq!(set(&t, "time timeZone Europe/Berlin").unwrap().1, Value::from("Europe/Berlin"));
        assert_eq!(set(&t, "environment variables EDITOR vim").unwrap(), (w("environment variables"), serde_json::json!({"EDITOR": "vim"})));
        assert_eq!(set(&t, "torq dev enable false").unwrap().1, Value::Bool(false), "TorqOS's own settings are there too");
    }

    #[test]
    fn values_are_checked_against_nixos_types() {
        let t = tree();
        for (line, want) in [
            ("virtualisation docker enable yes", "true or false"),
            ("networking firewall allowedTCPPorts [ 80 70000 ]", "0 to 65535"),
            ("networking firewall allowedTCPPorts [ http ]", "not a number"),
            ("services openssh settings LogLevel LOUD", "'LOUD' is not one of: QUIET"),
            ("environment systemPackages [ \"rm -rf\" ]", "not a package name"),
            ("time timeZone", "needs text"),
            ("virtualisation dokcer enable", "no such setting (did you mean docker?)"),
            ("services httpd virtualHosts", "not a setting by itself"),
            ("boot kernel sysctl", "not a setting by itself"),
        ] {
            let e = format!("{:#}", set(&t, line).unwrap_err());
            assert!(e.contains(want), "{line}: {e}");
        }
    }

    #[test]
    fn what_may_come_next() {
        let t = tree();
        let words = |v: Vec<(String, String)>| v.into_iter().map(|(w, _)| w).collect::<Vec<_>>();
        assert_eq!(words(t.next(&w("virtualisation docker"))), ["enable", "rootless"]);
        assert_eq!(words(t.next(&w("services httpd virtualHosts"))), ["<name>"], "an entry is named");
        assert_eq!(words(t.next(&w("services httpd virtualHosts example.com"))), ["documentRoot", "forceSSL", "serverAliases"]);
        assert_eq!(words(t.next(&w("networking firewall allowedTCPPorts"))), ["<[ a number … ]>"]);
        assert_eq!(words(t.next(&w("services openssh settings LogLevel"))), ["<one of: QUIET, FATAL, ERROR, INFO, VERBOSE, DEBUG, DEBUG1, DEBUG2, DEBUG3>"]);
        let help = t.next(&w("virtualisation docker"));
        assert!(help[0].1.starts_with("This option enables docker"), "{help:?}");
        assert!(t.next(&w("virtualisation docker enable true")).is_empty());
    }

    #[test]
    fn the_whole_schema_reads() {
        // The real file this system generates, when there is one (a TorqOS machine): every setting
        // must load, and the common ones resolve.
        let path = std::env::var("TORQ_NIXOS_SCHEMA").unwrap_or_else(|_| "/etc/torq/nixos-schema.json".into());
        let Ok(json) = std::fs::read_to_string(&path) else { return };
        let started = std::time::Instant::now();
        let t = Tree::parse(&json).unwrap();
        eprintln!("parsed {path} in {:?}", started.elapsed());
        assert!(t.resolve(&w("virtualisation docker enable true")).is_ok());
        assert!(t.resolve(&w("services openssh settings PasswordAuthentication false")).is_ok());
    }
}

#[cfg(test)]
#[test]
fn the_fence_is_the_modules() {
    let t = Tree::parse(include_str!("nixos-fixture.json")).unwrap();
    let w = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
    for fenced in [
        "torq broker rebuildCommand", "torq broker", "environment etc torq/broker.json text", "environment etc torq",
        "systemd services torq-broker-init enable", "systemd units torq-broker-init.service text", "nixpkgs config allowUnfree",
        "_module args", "torq packages user", "system build nixos-rebuild", "nix package", "systemd package",
        "services httpd virtualHosts _x",
    ] {
        assert!(t.fenced(&w(fenced)), "{fenced}");
    }
    for open in ["torq dev enable", "environment etc motd-torq/x text", "systemd services myapp enable", "torq", "environment etc"] {
        assert!(!t.fenced(&w(open)), "{open}");
    }
}
