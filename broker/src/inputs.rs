//! What a changed flake.lock builds, checked before it is built (the audit of 2026-11-04, A2). Nix
//! looks a locked input up by its `narHash`: a lock can name a `rev` the release key signed and the
//! hash of other source already in the store, and Nix builds that source. So each input the lock
//! changes is fetched again by what names it (its `rev`), its hash recomputed and compared; and
//! TorqOS's own releases are checked to be signed by the release key, here, as root, not only by the
//! torqcli that wrote the lock. A folder (`path:`, dev mode only) is hashed again too.

use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

/// The keys a release may be signed with, as the machine has them (modules/releases.nix).
pub const SIGNERS: &str = "/etc/torq/release-signers";

/// The node a lock's root input `name` resolves to: named directly, or by a path through other inputs
/// (`follows`: `["a", "b"]` is input b of input a). None: no such input, or a path that does not resolve.
pub fn resolve(lock: &Value, name: &str) -> Option<String> {
    let root = lock["root"].as_str().unwrap_or("root");
    resolve_from(lock, root, name, 0)
}

/// Input `name` of node `node`, followed to the node it is; a path that goes round is cut off.
fn resolve_from(lock: &Value, node: &str, name: &str, depth: usize) -> Option<String> {
    if depth > 64 {
        return None;
    }
    match &lock["nodes"][node]["inputs"][name] {
        Value::String(n) => Some(n.clone()),
        Value::Array(path) => {
            let root = lock["root"].as_str().unwrap_or("root");
            path.iter().try_fold(root.to_string(), |at, step| resolve_from(lock, &at, step.as_str()?, depth + 1))
        }
        _ => None,
    }
}

/// The locked inputs `new` has that `old` does not have as they are: by node name.
pub fn changed(old: &str, new: &str) -> Result<BTreeMap<String, Value>> {
    let parse = |t: &str| -> Result<Value> { if t.trim().is_empty() { Ok(Value::Null) } else { Ok(serde_json::from_str(t)?) } };
    let (old, new) = (parse(old).context("reading the old flake.lock")?, parse(new).context("reading the new flake.lock")?);
    let mut out = BTreeMap::new();
    for (name, node) in new["nodes"].as_object().into_iter().flatten() {
        let locked = &node["locked"];
        if locked.is_null() || old["nodes"][name]["locked"] == *locked {
            continue;
        }
        out.insert(name.clone(), locked.clone());
    }
    Ok(out)
}

/// The inputs to fetch again, as `builtins.fetchTree` takes them: what locked them, but the hash, which
/// is what is checked. A folder (dev mode's `path:`) is hashed again too.
pub fn to_fetch(changes: &BTreeMap<String, Value>) -> BTreeMap<String, Value> {
    changes
        .iter()
        // A folder too: fetched again it is hashed again, and a lock naming a store path for a folder is
        // that folder's hash or refused (the audit of 0.3.0: it was skipped, whatever path it named).
        .map(|(n, l)| {
            let mut a = l.clone();
            if let Some(o) = a.as_object_mut() {
                // What is checked, and what Nix derives itself from what it fetches: given back, they
                // would have to match too, and say nothing about what is built.
                for k in ["narHash", "lastModified", "lastModifiedDate", "revCount"] {
                    o.remove(k);
                }
            }
            (n.clone(), a)
        })
        .collect()
}

/// The expression that computes, for `to_fetch` written to `file`, each one's narHash as fetched now.
/// (`nix eval` applies no `--arg` to an expression: the path is in it, as a string.)
pub fn expr(file: &Path) -> String {
    let path = serde_json::to_string(&file.to_string_lossy()).unwrap_or_default();
    format!("builtins.mapAttrs (n: a: (builtins.fetchTree a).narHash) (builtins.fromJSON (builtins.readFile {path}))")
}

/// Those whose hash, fetched again, is not the lock's.
pub fn mismatched(changes: &BTreeMap<String, Value>, fetched: &Value) -> Vec<String> {
    to_fetch(changes)
        .keys()
        .filter(|n| fetched[n.as_str()].as_str().is_none() || fetched[n.as_str()] != changes[n.as_str()]["narHash"])
        .cloned()
        .collect()
}

/// Whether the raw commit object carries an SSH signature (`-----BEGIN SSH SIGNATURE-----` in its
/// gpgsig header): OpenPGP and X.509 ones are not the release key's (the audit of 2026-11-04, A1).
pub fn ssh_signed(commit: &str) -> bool {
    let mut header = true;
    let mut sig = false;
    for line in commit.lines() {
        if line.is_empty() {
            header = false;
        }
        if header && line.starts_with("gpgsig ") {
            sig = line.trim_end() == "gpgsig -----BEGIN SSH SIGNATURE-----";
        }
    }
    sig
}

/// The arguments for git, with nothing of anyone's configuration, that check `rev` is signed by a key
/// in `signers`.
pub fn git_config(signers: &Path) -> Vec<String> {
    [
        "-c", "core.hooksPath=/dev/null", "-c", "gpg.format=ssh",
        "-c", "gpg.openpgp.program=false", "-c", "gpg.x509.program=false",
    ]
    .iter()
    .map(|s| s.to_string())
    .chain(["-c".to_string(), format!("gpg.ssh.allowedSignersFile={}", signers.display())])
    .collect()
}

/// A rev: forty hex digits.
pub fn rev_ok(rev: &str) -> bool {
    rev.len() == 40 && rev.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn lock(nixpkgs_hash: &str, torq: Value) -> String {
        json!({ "nodes": {
            "nixpkgs": { "locked": { "type": "github", "owner": "NixOS", "repo": "nixpkgs", "rev": "a".repeat(40), "narHash": nixpkgs_hash } },
            "torq": { "locked": torq },
            "root": { "inputs": { "torq": "torq" } }
        }, "root": "root", "version": 7 })
        .to_string()
    }

    #[test]
    fn a_root_input_resolves_to_its_node_however_named() {
        let l = json!({"root": "root", "nodes": {
            "root": {"inputs": {"torq": "torq_2", "x": "x", "via": ["x", "y"], "loop": ["loop"], "gone": ["x", "nope"]}},
            "torq_2": {}, "x": {"inputs": {"y": "z"}}, "z": {}
        }});
        assert_eq!(resolve(&l, "torq").as_deref(), Some("torq_2"));
        assert_eq!(resolve(&l, "via").as_deref(), Some("z"), "a follows path");
        assert_eq!(resolve(&l, "none"), None);
        assert_eq!(resolve(&l, "gone"), None, "a path that does not resolve");
        assert_eq!(resolve(&l, "loop"), None, "a path that goes round");
    }

    #[test]
    fn what_changed_and_what_is_fetched_again() {
        let torq = json!({ "type": "git", "url": "https://github.com/netstacks/torqos-releases", "rev": "b".repeat(40), "narHash": "sha256-t" });
        let old = lock("sha256-n", torq.clone());
        assert!(changed(&old, &old).unwrap().is_empty());
        let new = lock("sha256-n2", torq.clone());
        let c = changed(&old, &new).unwrap();
        assert_eq!(c.keys().collect::<Vec<_>>(), ["nixpkgs"]);
        let f = to_fetch(&c);
        assert!(f["nixpkgs"].get("narHash").is_none() && f["nixpkgs"]["rev"] == "a".repeat(40));
        assert_eq!(mismatched(&c, &json!({ "nixpkgs": "sha256-n2" })), Vec::<String>::new());
        assert_eq!(mismatched(&c, &json!({ "nixpkgs": "sha256-older" })), ["nixpkgs"]);
        assert_eq!(mismatched(&c, &json!({})), ["nixpkgs"], "not fetched is not agreed");
        // A folder, dev mode's: hashed again too, so a lock naming a store path for it is checked.
        let dev = lock("sha256-n", json!({ "type": "path", "path": "/home/x/torq", "narHash": "sha256-p" }));
        let c = changed(&old, &dev).unwrap();
        assert_eq!(to_fetch(&c).keys().collect::<Vec<_>>(), ["torq"]);
        assert_eq!(mismatched(&c, &json!({ "torq": "sha256-p" })), Vec::<String>::new());
        assert_eq!(mismatched(&c, &json!({})), ["torq"], "a folder not hashed again is not agreed");
        // A new machine's first lock: everything is new.
        assert_eq!(changed("", &old).unwrap().len(), 2);
    }

    #[test]
    fn the_releases_and_their_signature() {
        // Which inputs are the releases: torq_wire::releases, the broker's rule and torqcli's (tested there).
        assert!(ssh_signed("tree t\ngpgsig -----BEGIN SSH SIGNATURE-----\n x\n -----END SSH SIGNATURE-----\n\nmsg\n"));
        assert!(!ssh_signed("tree t\ngpgsig -----BEGIN PGP SIGNATURE-----\n x\n\nmsg\n"));
        assert!(!ssh_signed("tree t\n\ngpgsig -----BEGIN SSH SIGNATURE-----\n"), "in the message is not a signature");
        assert!(rev_ok(&"c".repeat(40)) && !rev_ok("main") && !rev_ok(&"g".repeat(40)));
    }
}
