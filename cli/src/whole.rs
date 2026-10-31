//! The whole configuration (docs/CONFIG-IS-THE-DEVICE.md, S3c): one tree of NixOS paths over the
//! config repo's two kinds of data files. Most settings live in `nixos/<top-level>.json`
//! (settings.rs), checked against the system's own NixOS settings (nixos.rs). A few keep the files
//! TorqOS gave them before (tree.rs and the hand-made schema), for their own checks and tiers — above
//! all a person's own packages and CLI style, which are user tier: changed without a question.
//!
//! `MAP` says which, and what each was called before: those names still work (`set system host-name
//! x`), the tree always shows the NixOS one (`networking hostName`).

use crate::nixos::Tree;
use crate::settings::{self, Settings};
use crate::tree::{Config, Kind, Schema, Val};
use crate::attach::{self, Attached};
use crate::software;
use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;

/// (NixOS path, the hand-made schema's path to the file that holds it, whether the old name still
/// works as an alias). `<name>` is a list entry on both sides.
pub const MAP: &[(&str, &str, bool)] = &[
    ("networking hostName", "system host-name", true),
    ("time timeZone", "system time-zone", true),
    ("torq packages system", "system packages", true),
    ("torq cli style", "system cli style", true),
    ("torq packages user <name>", "users <name> packages", true),
    ("torq cli userStyles <name>", "users <name> cli style", true),
    // Not an alias: NixOS has its own `services` (`services printing` is NixOS's and the catalog's).
    ("torq services enabled", "services", false),
];

/// `words` with a prefix matching one side of MAP (0: NixOS, 1: hand-made) replaced by the other.
/// A NixOS path as the hand-made schema names it, if TorqOS keeps that setting in its own files.
pub fn to_hand(words: &[String]) -> Option<Vec<String>> {
    translate(words, 0, false)
}

fn translate(words: &[String], from: usize, aliases_only: bool) -> Option<Vec<String>> {
    for (nixos, hand, alias) in MAP {
        if aliases_only && !alias {
            continue;
        }
        let (a, b): (Vec<&str>, Vec<&str>) = if from == 0 {
            (nixos.split(' ').collect(), hand.split(' ').collect())
        } else {
            (hand.split(' ').collect(), nixos.split(' ').collect())
        };
        if words.len() < a.len() {
            continue;
        }
        let mut name = None;
        let ok = a.iter().zip(words).all(|(p, w)| {
            if *p == "<name>" {
                name = Some(w.clone());
                true
            } else {
                p == w
            }
        });
        if ok {
            let mut out: Vec<String> = b.iter().map(|p| if *p == "<name>" { name.clone().unwrap_or_default() } else { p.to_string() }).collect();
            out.extend_from_slice(&words[a.len()..]);
            return Some(out);
        }
    }
    None
}

/// The whole tree, given the hand-made schema and the NixOS settings tree.
pub struct Whole<'a> {
    pub schema: &'a Schema,
    pub tree: &'a Tree,
}

/// `system software torq source …`: the words after the path (None: not that setting). Only `torq`
/// has a source of its own (nixpkgs follows TorqOS's).
fn software_words(words: &[String]) -> Option<&[String]> {
    let n = software::PATH.len();
    (words.len() >= n && words[..n].iter().zip(software::PATH).all(|(a, b)| a == b)).then(|| &words[n..])
}

/// The tree's data: the files TorqOS keeps (the hand-made schema's), nixos/*.json, the config files
/// attached (config-files.json), and the software's source (flake.nix).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Data {
    pub config: Config,
    pub settings: Settings,
    pub attached: Attached,
    /// Where TorqOS comes from (`system software torq source`, software.rs); None: flake.nix is not
    /// TorqOS's fixed one (or there is none).
    pub source: Option<String>,
}

impl Whole<'_> {
    /// Words as the hand-made schema's, if they name a file-backed setting: by its NixOS path, or by
    /// its old name. Words that match an old name are the hand-made schema's to decide: its error, if
    /// it refuses them.
    fn hand(&self, words: &[String], cfg: &Config) -> Result<Option<Vec<String>>> {
        if let Some(h) = translate(words, 0, false) {
            return Ok(Some(h));
        }
        if translate(words, 1, true).is_none() {
            return Ok(None);
        }
        self.schema.resolve_in(words, Some(cfg))?;
        Ok(Some(words.to_vec()))
    }

    /// `<place> config [<file>]`: the place a config file is (to be) attached at, and the file. None if
    /// the words are not that — and where NixOS has a setting named `config` (`containers <name>
    /// config`), they are NixOS's.
    fn attachment<'w>(&self, words: &'w [String]) -> Option<(&'w [String], Option<&'w String>)> {
        let i = words.iter().rposition(|w| w == "config")?;
        let (place, rest) = (&words[..i], &words[i + 1..]);
        // Below a set of named entries (`virtualHosts`), `config` is an entry's name.
        if rest.len() > 1 || self.tree.next(place).iter().any(|(w, _)| w == "config" || w.starts_with('<')) {
            return None;
        }
        let somewhere = place.is_empty() || self.is_place(place) || self.tree.locate(place).is_ok();
        somewhere.then(|| (place, rest.first()))
    }

    /// Every value, by NixOS path, sorted: the files TorqOS kept, nixos/*.json, the config files.
    pub fn entries(&self, d: &Data) -> Vec<(Vec<String>, Val)> {
        let mut all: Vec<(Vec<String>, Val)> = self
            .schema
            .entries(&d.config)
            .into_iter()
            .map(|(w, v)| (translate(&w, 1, false).unwrap_or(w), v))
            .collect();
        all.extend(settings::entries(&d.settings));
        all.extend(attach::entries(&d.attached));
        if let Some(s) = &d.source {
            all.push((software::PATH.map(String::from).to_vec(), Val::One(s.clone())));
        }
        all.sort_by(|a, b| a.0.cmp(&b.0));
        all
    }

    /// The config repo file each entry comes from, by NixOS path.
    pub fn files(&self, d: &Data) -> BTreeMap<Vec<String>, String> {
        let mut out = BTreeMap::new();
        for (file, held) in self.schema.files_of(&d.config, &[]) {
            for h in held {
                let w: Vec<String> = h.split(' ').map(String::from).collect();
                out.insert(translate(&w, 1, false).unwrap_or(w), file.clone());
            }
        }
        for (w, _) in settings::entries(&d.settings) {
            let file = format!("{}{}.json", settings::DIR, w[0]);
            out.insert(w, file);
        }
        for (w, _) in attach::entries(&d.attached) {
            out.insert(w, attach::FILE.to_string());
        }
        if d.source.is_some() {
            out.insert(software::PATH.map(String::from).to_vec(), "flake.nix".into());
        }
        out
    }

    /// `set <words>` (NixOS path, or an old name; `<place> config <file>` attaches a config file).
    pub fn set(&self, d: &mut Data, words: &[String]) -> Result<()> {
        if let Some(rest) = software_words(words) {
            return match rest {
                [url] => {
                    if d.source.is_none() {
                        bail!("this machine's flake.nix is not TorqOS's fixed one: its software source cannot be set here");
                    }
                    software::valid(url)?;
                    d.source = Some(url.clone());
                    Ok(())
                }
                _ => bail!("set system software torq source <source>: one source (like github:netstacks/torqos); the version is `request system update`'s"),
            };
        }
        if let Some((place, file)) = self.attachment(words) {
            let file = file.context("config <file>: which file (a .nix file of the config repo, like services/httpd.nix)")?;
            attach::valid(file)?;
            d.attached.insert(place.join(" "), file.clone());
            return Ok(());
        }
        if let Some(h) = self.hand(words, &d.config)? {
            return self.schema.set(&mut d.config, &h);
        }
        settings::set(self.tree, &mut d.settings, words).map_err(|e| self.hint(words, e))
    }

    /// `delete <words> [all]`: a setting, one value of a list, an attached config file (`<place>
    /// config`), or (with `all`) everything shown below a place — from every kind of file — or an old
    /// name's place (`delete users dev all`).
    pub fn delete(&self, d: &mut Data, words: &[String], all: bool, del: &str, typed: &[String]) -> Result<()> {
        if software_words(words).is_some() || words == ["system", "software"] || words == ["system", "software", "torq"] {
            bail!("the software's source can be changed (`set system software torq source …`), not deleted: a machine always has one");
        }
        if let Some((place, _)) = self.attachment(words).filter(|_| !all) {
            return match d.attached.remove(&place.join(" ")) {
                Some(_) => Ok(()),
                None => bail!("{}: no config file attached there", if place.is_empty() { "the top".into() } else { place.join(" ") }),
            };
        }
        // (`typed`: the words as typed, from where configuration mode is, for the hint.)
        let typed = typed.iter().map(|w| crate::syntax::quote(w)).collect::<Vec<_>>().join(" ");
        let hand = self.hand(words, &d.config).ok().flatten();
        // An old name's place (`users dev`, `system`): the hand-made schema's container or entry.
        let old_place = self.schema.resolve_in(words, Some(&d.config)).ok().filter(|t| {
            t.values.is_empty() && !t.node.is_empty() && matches!(self.schema.nodes[&t.node].kind, Kind::Container | Kind::List)
        });
        let whole = if let Some(h) = &hand {
            let t = self.schema.resolve_in(h, Some(&d.config))?;
            let kind = (!t.node.is_empty()).then(|| self.schema.nodes[&t.node].kind);
            match kind {
                None | Some(Kind::Container | Kind::List) => true,
                Some(Kind::LeafList) => t.values.is_empty(),
                Some(Kind::Leaf) => false,
            }
        } else if old_place.is_some() {
            true
        } else {
            let (_, ty, values) = self.tree.locate(words).map_err(|e| self.hint(words, e))?;
            // A place, or a whole list or set of values, goes only with `all`.
            ty.is_none() || (values.is_empty() && ty.is_some_and(|t| t.name.starts_with("listOf") || t.name.starts_with("attrsOf")))
        };
        self.check_all(whole, all, del, &typed)?;
        if !all {
            return match hand {
                Some(h) => self.schema.delete(&mut d.config, &h),
                None => settings::delete(self.tree, &mut d.settings, words),
            };
        }
        // Everything below: what TorqOS keeps in its own files, by each entry's NixOS path, the rest,
        // and the config files attached there.
        let mut gone = 0;
        let below: Vec<Vec<String>> = self.schema.entries(&d.config).into_iter().map(|(w, _)| w).collect();
        for h in below {
            let nixos = translate(&h, 1, false).unwrap_or_else(|| h.clone());
            if nixos.starts_with(words) || old_place.is_some() && h.starts_with(words) {
                self.schema.delete(&mut d.config, &h)?;
                gone += 1;
            }
        }
        if settings::entries(&d.settings).iter().any(|(w, _)| w.starts_with(words)) {
            settings::delete(self.tree, &mut d.settings, words)?;
            gone += 1;
        }
        let before = d.attached.len();
        d.attached.retain(|at, _| !at.split(' ').map(String::from).collect::<Vec<_>>().starts_with(words));
        gone += before - d.attached.len();
        if gone == 0 {
            bail!("{}: nothing set below it", words.join(" "));
        }
        Ok(())
    }

    fn check_all(&self, whole: bool, all: bool, del: &str, typed: &str) -> Result<()> {
        let typed = if typed.is_empty() { String::new() } else { format!("{typed} ") };
        if whole && !all {
            bail!("{del} {typed}deletes everything below it — say `{del} {typed}all`");
        }
        if all && !whole {
            bail!("{del} {typed}all: `all` is for a whole place or list");
        }
        Ok(())
    }

    /// Whether `words` name a place (somewhere `edit` can go, a Cisco submode), not a setting.
    pub fn is_place(&self, words: &[String]) -> bool {
        !words.is_empty() && matches!(self.tree.locate(words), Ok((_, None, _)))
    }

    /// The configuration below `path` in the brace format. An unknown path is an error, not nothing.
    pub fn show(&self, d: &Data, path: &[String]) -> Result<String> {
        let below = self.below(d, path)?;
        Ok(crate::tree::render_entries(&below))
    }

    /// `| display set` below `path`: each line whole, from the top (as Junos), so it can be pasted back.
    pub fn display_set(&self, d: &Data, path: &[String]) -> Result<String> {
        self.below(d, path)?;
        let path = self.shown_path(d, path);
        let all: Vec<(Vec<String>, Val)> = self.entries(d).into_iter().filter(|(w, _)| w.starts_with(&path)).collect();
        Ok(crate::tree::display_set_entries(&all))
    }

    /// The path entries are shown below: an old name's, by its NixOS path.
    fn shown_path(&self, d: &Data, path: &[String]) -> Vec<String> {
        translate(path, 1, true).filter(|_| self.schema.resolve_in(path, Some(&d.config)).is_ok()).unwrap_or_else(|| path.to_vec())
    }

    /// The entries below `path`, the path taken off (the brace format starts where you are).
    fn below(&self, d: &Data, path: &[String]) -> Result<Vec<(Vec<String>, Val)>> {
        let path = self.shown_path(d, path);
        let all = self.entries(d);
        let below: Vec<(Vec<String>, Val)> =
            all.into_iter().filter(|(w, _)| w.starts_with(&path)).map(|(w, v)| (w[path.len()..].to_vec(), v)).collect();
        if below.is_empty() && !path.is_empty() && self.tree.locate(&path).is_err() && translate(&path, 0, false).is_none() {
            bail!("{}: no such setting", path.join(" "));
        }
        // A setting shown by itself is its own statement.
        Ok(below.into_iter().map(|(w, v)| if w.is_empty() { (vec![path.last().cloned().unwrap_or_default()], v) } else { (w, v) }).collect())
    }

    /// What going from `old` to `new` changes, `show | compare` style, by NixOS paths.
    pub fn compare(&self, old: &Data, new: &Data) -> String {
        crate::tree::compare_entries(self.entries(old), self.entries(new))
    }

    /// A NixOS-tree error, with what the old name of the catalog is now.
    fn hint(&self, words: &[String], e: anyhow::Error) -> anyhow::Error {
        let catalog = self.schema.nodes.get("services").and_then(|n| n.values.clone()).unwrap_or_default();
        match words {
            [s, name] if s == "services" && catalog.contains_key(name) => {
                anyhow::anyhow!("{e:#} (TorqOS's catalog: `torq services enabled {name}`)")
            }
            _ => e,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &str) -> Vec<String> {
        crate::session::words(s).unwrap()
    }
    struct T {
        schema: Schema,
        tree: Tree,
    }
    fn t() -> T {
        T { schema: Schema::parse(include_str!("schema-fixture.json")).unwrap(), tree: Tree::parse(include_str!("nixos-fixture.json")).unwrap() }
    }

    #[test]
    fn one_tree_of_nixos_paths_over_both_kinds_of_files() {
        let t = t();
        let x = Whole { schema: &t.schema, tree: &t.tree };
        let mut d = Data::default();
        for line in [
            "networking hostName laptop",
            "system time-zone UTC",
            "torq packages user dev [ htop ]",
            "users dev cli style cisco",
            "virtualisation docker enable true",
            "networking firewall allowedTCPPorts [ 22 ]",
            "torq services enabled ssh",
        ] {
            x.set(&mut d, &w(line)).unwrap_or_else(|e| panic!("{line}: {e:#}"));
        }
        assert_eq!(d.config.files.len(), 4, "host.json, the user's packages and cli, services.json: {:?}", d.config.files.keys());
        assert_eq!(settings::files(&d.settings).keys().collect::<Vec<_>>(), ["nixos/networking.json", "nixos/virtualisation.json"]);
        assert_eq!(
            x.show(&d, &[]).unwrap(),
            "networking {\n    firewall {\n        allowedTCPPorts [ 22 ];\n    }\n    hostName laptop;\n}\ntime {\n    timeZone UTC;\n}\ntorq {\n    cli {\n        userStyles {\n            dev cisco;\n        }\n    }\n    packages {\n        user {\n            dev [ htop ];\n        }\n    }\n    services {\n        enabled [ ssh ];\n    }\n}\nvirtualisation {\n    docker {\n        enable true;\n    }\n}\n"
        );
        assert_eq!(x.show(&d, &w("networking hostName")).unwrap(), "hostName laptop;\n", "one setting");
        assert_eq!(x.show(&d, &w("system host-name")).unwrap(), "hostName laptop;\n", "by its old name too");
        let files = x.files(&d);
        assert_eq!(files[&w("networking hostName")], "system/host.json");
        assert_eq!(files[&w("networking firewall allowedTCPPorts")], "nixos/networking.json");
        assert_eq!(files[&w("torq packages user dev")], "users/dev/packages.json", "a person's own packages: user tier, as before");
        // `| display set` reads back to the same configuration.
        let mut d2 = Data::default();
        for line in x.display_set(&d, &[]).unwrap().lines() {
            x.set(&mut d2, &w(line.strip_prefix("set ").unwrap())).unwrap_or_else(|e| panic!("{line}: {e:#}"));
        }
        assert_eq!(d2, d);
    }

    #[test]
    fn deleting_needs_all_for_a_whole_place_and_the_catalog_says_its_new_name() {
        let t = t();
        let x = Whole { schema: &t.schema, tree: &t.tree };
        let mut d = Data::default();
        for line in ["virtualisation docker enable true", "virtualisation docker rootless enable true", "networking firewall allowedTCPPorts [ 22 80 ]", "torq packages user dev [ htop jq ]"] {
            x.set(&mut d, &w(line)).unwrap();
        }
        let e = format!("{:#}", x.delete(&mut d, &w("virtualisation docker"), false, "delete", &w("virtualisation docker")).unwrap_err());
        assert!(e.contains("say `delete virtualisation docker all`"), "{e}");
        let e = format!("{:#}", x.delete(&mut d, &w("networking firewall allowedTCPPorts"), false, "delete", &w("networking firewall allowedTCPPorts")).unwrap_err());
        assert!(e.contains("all`"), "a whole list too: {e}");
        x.delete(&mut d, &w("networking firewall allowedTCPPorts 80"), false, "delete", &w("networking firewall allowedTCPPorts 80")).unwrap();
        x.delete(&mut d, &w("virtualisation docker rootless"), true, "delete", &w("virtualisation docker rootless")).unwrap();
        x.delete(&mut d, &w("torq packages user dev jq"), false, "delete", &w("torq packages user dev jq")).unwrap();
        assert_eq!(x.display_set(&d, &[]).unwrap(), "set networking firewall allowedTCPPorts 22\nset torq packages user dev htop\nset virtualisation docker enable true\n");
        let e = format!("{:#}", x.set(&mut d, &w("services ssh")).unwrap_err());
        assert!(e.contains("`torq services enabled ssh`"), "{e}");
        assert!(x.set(&mut d, &w("services printing enable true")).is_ok(), "NixOS's own services are NixOS's");
        assert!(x.is_place(&w("virtualisation docker")) && x.is_place(&w("services httpd virtualHosts example.com")));
        assert!(!x.is_place(&w("virtualisation docker enable")));
    }

    #[test]
    fn the_map_is_the_hand_made_schemas() {
        // Every file-backed setting of the hand-made schema has its NixOS path, and that path is a
        // setting of the NixOS tree: nothing TorqOS keeps in its own files is missing from the tree.
        let t = t();
        for (key, node) in &t.schema.nodes {
            if node.file.is_none() {
                continue;
            }
            let hand: Vec<String> = key.split(' ').map(|p| if p == "<name>" { "dev".to_string() } else { p.to_string() }).collect();
            let nixos = translate(&hand, 1, false).unwrap_or_else(|| panic!("{key}: no NixOS path in MAP"));
            assert!(t.tree.locate(&nixos).is_ok(), "{key} → {}: not a NixOS setting", nixos.join(" "));
        }
    }
}

#[cfg(test)]
#[test]
fn the_module_keeps_them_out_of_nixos_too() {
    // modules/settings-fence.nix's `elsewhere` must name every setting MAP keeps in TorqOS's own files
    // (but torq packages, all fenced there): the evaluation refuses them in nixos/, whoever wrote it.
    // Read where the repo is (the dev shell); the package's own build has only cli/, and skips it.
    let Ok(fence) = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../modules/settings-fence.nix")) else { return };
    for (nixos, _, _) in MAP {
        if nixos.starts_with("torq packages") {
            continue;
        }
        let words: Vec<&str> = nixos.split(' ').filter(|w| *w != "<name>").collect();
        let pat = format!("[ {} ]", words.iter().map(|w| format!("\"{w}\"")).collect::<Vec<_>>().join(" "));
        assert!(fence.contains(&pat), "settings-fence.nix's elsewhere has no {pat}");
    }
}

#[cfg(test)]
#[test]
fn review_of_s3c_one_place_one_value_and_all_means_all() {
    let (schema, tree) = (Schema::parse(include_str!("schema-fixture.json")).unwrap(), Tree::parse(include_str!("nixos-fixture.json")).unwrap());
    let x = Whole { schema: &schema, tree: &tree };
    let w = |s: &str| crate::session::words(s).unwrap();
    let mut d = Data::default();
    for line in ["networking hostName laptop", "networking firewall allowedTCPPorts [ 22 ]", "torq packages user dev [ htop ]", "users dev cli style cisco", "system time-zone UTC"] {
        x.set(&mut d, &w(line)).unwrap();
    }
    // `delete networking all`: everything shown below it, from both kinds of files.
    x.delete(&mut d, &w("networking"), true, "delete", &w("networking")).unwrap();
    assert!(!x.show(&d, &[]).unwrap().contains("networking"), "{}", x.show(&d, &[]).unwrap());
    // An old name's place deletes what it always did.
    x.delete(&mut d, &w("users dev"), true, "delete", &w("users dev")).unwrap();
    assert!(!x.show(&d, &[]).unwrap().contains("dev"));
    x.delete(&mut d, &w("system"), true, "delete", &w("system")).unwrap();
    assert_eq!(x.show(&d, &[]).unwrap(), "");
    let e = format!("{:#}", x.delete(&mut d, &w("virtualisation"), true, "delete", &w("virtualisation")).unwrap_err());
    assert!(e.contains("nothing set below it"), "{e}");
    // Text that would draw lines of its own: refused when set, and when read.
    let e = format!("{:#}", x.set(&mut d, &[w("services httpd virtualHosts a documentRoot"), vec!["/x\nKeys that will".into()]].concat()).unwrap_err());
    assert!(e.contains("multi-line text goes in a config file"), "{e}");
    let e = format!("{:#}", settings::load([("nixos/environment.json", br#"{"variables":{"X":"a\nb"}}"# as &[u8])]).unwrap_err());
    assert!(e.contains("line break or another control character"), "{e}");
    // One setting, one place: a kept one in a nixos/ file is refused, naming where it lives.
    let e = format!("{:#}", settings::load([("nixos/networking.json", br#"{"hostName":"x"}"# as &[u8])]).unwrap_err());
    assert!(e.contains("networking hostName is kept in its own file (`system host-name`)"), "{e}");
    let e = format!("{:#}", settings::load([("nixos/torq.json", br#"{"cli":{"userStyles":{"dev":"cisco"}}}"# as &[u8])]).unwrap_err());
    assert!(e.contains("torq cli userStyles is kept in its own file"), "{e}");
}
