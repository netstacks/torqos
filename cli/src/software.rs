//! `system software`: where this machine's TorqOS comes from (docs/CONFIG-IS-THE-DEVICE.md, S4b).
//! The source is the one `torq.url = "…";` line of TorqOS's fixed flake.nix; changing it rewrites
//! that line and, in the same candidate, locks the new source (`flake.lock`), so a person approves
//! both together. The version is the lock's: `request system update`, never `set`.
//!
//! Pure: it reads and rewrites the flake.nix text it is given.

use anyhow::{bail, Result};

/// The setting's path in the tree.
pub const PATH: [&str; 4] = ["system", "software", "torq", "source"];

/// The source a fixed flake.nix names (its single `torq.url = "…";` line), if it is one.
pub fn source_of(flake: &str) -> Option<String> {
    let mut found = flake.lines().filter_map(|l| l.trim().strip_prefix("torq.url = \"")?.strip_suffix("\";"));
    let one = found.next()?;
    (found.next().is_none() && valid(one).is_ok()).then(|| one.to_string())
}

/// `flake` with its source set to `url` (indentation kept; nothing else changes).
pub fn with_source(flake: &str, url: &str) -> Result<String> {
    valid(url)?;
    if source_of(flake).is_none() {
        bail!("this machine's flake.nix is not TorqOS's fixed one (one `torq.url = \"…\";` line): its source cannot be set here");
    }
    // Only the line source_of read (the same test), each line's ending kept as it was.
    let is_source = |l: &str| l.trim().strip_prefix("torq.url = \"").and_then(|r| r.strip_suffix("\";")).is_some_and(|s| valid(s).is_ok());
    Ok(flake
        .split_inclusive('\n')
        .map(|l| {
            let body = l.trim_end_matches(['\n', '\r']);
            if !is_source(body) {
                return l.to_string();
            }
            let indent = &body[..body.len() - body.trim_start().len()];
            format!("{indent}torq.url = \"{url}\";{}", &l[body.len()..])
        })
        .collect())
}

/// Whether `url` may be a source: a flake reference of a kind TorqOS can be fetched from, written
/// with plain URL characters only (it is put inside a Nix string: no quote, backslash or `${`).
/// Whether `url` is a folder someone can write to (`path:`, outside the read-only store): whoever
/// can write there decides what the next build installs, so it is for developing TorqOS only (dev
/// mode). A path in the store cannot change — an image's own TorqOS (docs/INSTALL.md) — and is
/// any machine's.
pub fn writable_folder(url: &str) -> bool {
    url.starts_with("path:") && !(url.starts_with("path:/nix/store/") && !url.contains(".."))
}

pub fn valid(url: &str) -> Result<()> {
    let plain = !url.is_empty() && url.chars().all(|c| c.is_ascii_alphanumeric() || ":/._~?&=%+@-".contains(c));
    let kinds = ["github:", "gitlab:", "sourcehut:", "git+https://", "https://", "path:/"];
    if !plain || !kinds.iter().any(|k| url.starts_with(k) && url.len() > k.len()) || url.contains("..") {
        bail!(
            "'{url}' is not a TorqOS source: one of github:<owner>/<repo>[/<ref>], gitlab:…, sourcehut:…, \
             git+https://…, https://… (a tarball) or path:/<absolute folder>"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FLAKE: &str = "{\n  inputs = {\n    # The TorqOS distribution.\n    torq.url = \"github:netstacks/torqos\";\n    nixpkgs.follows = \"torq/nixpkgs\";\n  };\n}\n";

    #[test]
    fn only_a_folder_that_can_change_is_dev_only() {
        assert!(writable_folder("path:/home/viper/torq") && writable_folder("path:/nix/store/../home/x"));
        assert!(!writable_folder("path:/nix/store/abc-source") && !writable_folder("github:netstacks/torqos"));
    }

    #[test]
    fn the_source_is_the_one_line() {
        assert_eq!(source_of(FLAKE).as_deref(), Some("github:netstacks/torqos"));
        let moved = with_source(FLAKE, "path:/home/dev/torq").unwrap();
        assert_eq!(moved, FLAKE.replace("github:netstacks/torqos", "path:/home/dev/torq"), "only that line changes");
        assert_eq!(source_of(&moved).as_deref(), Some("path:/home/dev/torq"));
        // Not TorqOS's fixed flake.nix: no source (two lines, none, or one that is not a source).
        assert_eq!(source_of(&format!("{FLAKE}    torq.url = \"github:a/b\";\n")), None);
        assert_eq!(source_of("{ }"), None);
        assert_eq!(source_of(&FLAKE.replace("github:netstacks/torqos", "${evil}")), None);
        assert!(with_source("{ }", "github:a/b").is_err());
        // Only that line (not a commented copy), and the file's line endings kept.
        let both = FLAKE.replace("    nixpkgs", "    # torq.url = \"github:old/one\"; was\n    nixpkgs");
        assert_eq!(with_source(&both, "github:a/b").unwrap(), both.replace("netstacks/torqos", "a/b"));
        let crlf = FLAKE.replace('\n', "\r\n");
        assert_eq!(with_source(&crlf, "github:a/b").unwrap(), crlf.replace("netstacks/torqos", "a/b"));
    }

    #[test]
    fn only_plain_sources() {
        for ok in ["github:netstacks/torqos", "github:netstacks/torqos/v0.1.0", "gitlab:a/b", "git+https://example.com/torq.git?ref=main", "https://example.com/torq-0.1.tar.gz", "path:/home/viper/torq"] {
            valid(ok).unwrap_or_else(|e| panic!("{ok}: {e:#}"));
        }
        for bad in ["", "github:", "path:relative", "path:/a/../b", "file:///etc", "github:a/b\";x", "github:a/${b}", "github:a/b c", "github:a\\b", "/home/x", "git+ssh://h/r", "github:a/b\n"] {
            assert!(valid(bad).is_err(), "{bad:?}");
        }
    }
}
