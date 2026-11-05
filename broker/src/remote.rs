//! The config repo's remote (ADR-036): where the machine's configuration and its ledger also live,
//! off the machine.
//!
//! After every change the broker pushes the configuration (`main`) and the ledger — as
//! `refs/torq/ledger`, a chain of commits each holding the ledger as it was — never forced: with the
//! remote refusing to rewrite (branch protection), what was pushed stays. The ledger here must then
//! always be the remote's and more: anything else is a rewritten local history (`verify`, `status`).
//! Pulling is a proposal, never an apply (`request system pull`, later).
//!
//! Over SSH only, with a key of the machine's own (a deploy key, one repository), in the broker's
//! state directory: root's, never the agent's. The remote's host key is pinned on first contact.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Whether `url` is a remote the broker pushes to: SSH, as `ssh://[user@]host[:port]/path` or
/// `user@host:path`. Nothing else — a git URL can name a command to run (`ext::`), a local path, or
/// a transport git would ask a helper for; as root, none of them.
pub fn url_ok(url: &str) -> bool {
    let safe = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "._-/~".contains(c));
    let host_ok = |h: &str| !h.is_empty() && !h.starts_with('-') && h.chars().all(|c| c.is_ascii_alphanumeric() || ".-".contains(c));
    let user_ok = |u: &str| !u.is_empty() && !u.starts_with('-') && u.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c));
    if let Some(rest) = url.strip_prefix("ssh://") {
        let Some((authority, path)) = rest.split_once('/') else { return false };
        let host_port = authority.split_once('@').map_or(authority, |(u, h)| if user_ok(u) { h } else { "" });
        let host = match host_port.split_once(':') {
            Some((h, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => h,
            Some(_) => return false,
            None => host_port,
        };
        return host_ok(host) && safe(path) && !path.contains("..");
    }
    let Some((user, rest)) = url.split_once('@') else { return false };
    let Some((host, path)) = rest.split_once(':') else { return false };
    user_ok(user) && host_ok(host) && safe(path) && !path.contains("..") && !path.starts_with('-')
}

/// The remote's place in the broker's state: its key, the host key pinned, the last push.
pub struct Remote {
    pub url: String,
    dir: PathBuf,
}

/// The last push, as the broker recorded it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Pushed {
    /// When it was tried (UTC, RFC 3339).
    pub at: String,
    /// Whether it went through.
    pub ok: bool,
    /// Why not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// How many ledger entries the remote had after it, if it went through.
    #[serde(default)]
    pub entries: usize,
    /// What the remote's ledger said of this one, before the push (a push is not made if it differs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<Anchor>,
}

/// What the remote's ledger says of this one.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum Anchor {
    /// The remote has the first `remote` of the `local` entries, exactly: the rest are not pushed yet.
    Agrees { remote: usize, local: usize },
    /// The remote has none yet.
    Empty,
    /// The remote's ledger is not the start of this one: this one was rewritten, or cut short.
    Differs { why: String },
    /// It could not be asked.
    Unreachable { error: String },
}

impl Remote {
    pub fn new(url: &str, state_dir: &Path) -> Result<Self> {
        if !url_ok(url) {
            bail!("the remote must be an SSH URL (ssh://host/path or user@host:path), not {url:?}");
        }
        Ok(Remote { url: url.to_string(), dir: state_dir.join("remote") })
    }

    pub fn key(&self) -> PathBuf {
        self.dir.join("key")
    }

    /// The machine's key for the remote — made the first time, kept after: its public half, to add
    /// to the repository as a deploy key (with write access).
    pub fn public_key(&self, host: &str) -> Result<String> {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&self.dir).with_context(|| format!("creating {}", self.dir.display()))?;
        if !self.key().exists() {
            let comment = format!("torq-broker@{host}");
            let ok = crate::config::command("ssh-keygen")
                .args(["-q", "-t", "ed25519", "-N", "", "-C", &comment, "-f"])
                .arg(self.key())
                .status()
                .context("running ssh-keygen")?
                .success();
            if !ok {
                bail!("ssh-keygen could not make the remote's key");
            }
        }
        let p = self.dir.join("key.pub");
        Ok(std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?.trim().to_string())
    }

    /// How git reaches the remote: no ssh configuration of anyone's, this key and no other, the host key
    /// pinned (accepted the first
    /// time, held to after), never a question, and a dead connection given up.
    pub fn ssh_command(&self) -> String {
        format!(
            "ssh -F /dev/null -i {} -o IdentitiesOnly=yes -o IdentityAgent=none -o UserKnownHostsFile={} -o StrictHostKeyChecking=accept-new \
             -o BatchMode=yes -o ConnectTimeout=10 -o ServerAliveInterval=10 -o ServerAliveCountMax=3",
            self.key().display(),
            self.dir.join("known_hosts").display()
        )
    }

    pub fn last(&self) -> Option<Pushed> {
        serde_json::from_slice(&std::fs::read(self.dir.join("last.json")).ok()?).ok()
    }

    pub fn record(&self, p: &Pushed) -> Result<()> {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&self.dir).with_context(|| format!("creating {}", self.dir.display()))?;
        let f = self.dir.join("last.json");
        let tmp = self.dir.join("last.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(p)?)?;
        std::fs::rename(&tmp, &f).with_context(|| format!("writing {}", f.display()))
    }
}

/// Compare the remote's ledger (its text) with this machine's: it must be this one's first lines,
/// exactly.
pub fn anchor(remote: Option<&str>, local: &str) -> Anchor {
    let Some(remote) = remote else { return Anchor::Empty };
    let r: Vec<&str> = remote.lines().filter(|l| !l.trim().is_empty()).collect();
    let l: Vec<&str> = local.lines().filter(|l| !l.trim().is_empty()).collect();
    if r.len() > l.len() {
        return Anchor::Differs { why: format!("the remote has {} entries, this machine {}: entries here were removed", r.len(), l.len()) };
    }
    if let Some(i) = r.iter().zip(&l).position(|(a, b)| a != b) {
        return Anchor::Differs { why: format!("entry {} differs from the remote's: the ledger here was rewritten", i + 1) };
    }
    Anchor::Agrees { remote: r.len(), local: l.len() }
}

/// The remote's ledgers, each as it was pushed (oldest first), against this machine's: each must
/// start with the one before — what was pushed is never taken back, not even by a push of root's —
/// and this one must start with the newest.
pub fn anchor_history(history: &[String], local: &str) -> Anchor {
    for (i, w) in history.windows(2).enumerate() {
        if let Anchor::Differs { why } = anchor(Some(&w[0]), &w[1]) {
            return Anchor::Differs { why: format!("the remote's ledger was rewritten at its push {}: {why}", i + 2) };
        }
    }
    anchor(history.last().map(String::as_str), local)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_ssh_urls() {
        for ok in ["git@github.com:netstacks/torq5-config.git", "ssh://git@git.example.org/srv/config.git", "ssh://git@host:2222/c.git", "ssh://host/srv/c", "deploy@10.0.0.2:repos/m1"] {
            assert!(url_ok(ok), "{ok}");
        }
        for bad in [
            "ext::sh -c touch% /tmp/pwned", "file:///srv/config", "/srv/config", "https://github.com/x/y", "http://x/y",
            "fd::3", "git@-oProxyCommand=sh:x", "ssh://-oProxyCommand=sh/x", "git@host:--upload-pack=sh", "git@host:../etc",
            "ssh://git@host:22x/y", "ssh://user;id@host/x", "git@host:a b", "", "ssh://host", "git@host:",
        ] {
            assert!(!url_ok(bad), "{bad}");
        }
    }

    /// A push of root's that replaces the ledger is new on the remote, not a rewrite of what it had:
    /// the history shows it.
    #[test]
    fn a_rewrite_pushed_on_top_is_seen_in_the_history() {
        let h = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(anchor_history(&h(&["a\n", "a\nb\n"]), "a\nb\nc\n"), Anchor::Agrees { remote: 2, local: 3 });
        assert!(matches!(anchor_history(&h(&["a\nb\n", "a\nX\n"]), "a\nX\n"), Anchor::Differs { .. }));
        assert_eq!(anchor_history(&[], "a\n"), Anchor::Empty);
    }

    #[test]
    fn the_remote_must_be_the_start_of_the_ledger() {
        let local = "a\nb\nc\n";
        assert_eq!(anchor(None, local), Anchor::Empty);
        assert_eq!(anchor(Some("a\nb\n"), local), Anchor::Agrees { remote: 2, local: 3 });
        assert_eq!(anchor(Some(local), local), Anchor::Agrees { remote: 3, local: 3 });
        assert!(matches!(anchor(Some("a\nX\n"), local), Anchor::Differs { .. }), "rewritten");
        assert!(matches!(anchor(Some("a\nb\nc\nd\n"), local), Anchor::Differs { .. }), "cut short here");
        assert!(matches!(anchor(Some("a\nb\nc\n"), "a\nb\n"), Anchor::Differs { .. }));
    }
}
