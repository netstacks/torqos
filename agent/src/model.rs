//! The model, as an OpenAI-compatible chat endpoint (ADR-042): llama.cpp's `llama-server` on this
//! machine by default, over a unix socket; or a remote endpoint (another OpenAI-compatible server,
//! Anthropic's OpenAI-compatible API) when the owner sets one. Spoken with curl, the maintained
//! client, run by its store path with no configuration file of its own read (`-q`: the agent's home
//! is the agent's to write, and a `.curlrc` there could send the request elsewhere).

use crate::clean::clean;
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Where the model answers.
#[derive(Debug, PartialEq)]
pub enum Endpoint {
    /// `unix:/run/llama-cpp/llama.sock`: on this machine, a socket only the model's service can make.
    Unix(PathBuf),
    /// `http://127.0.0.1:8080/v1` (this machine only) or `https://…/v1`: the base URL, without its last `/`.
    Http { base: String, local: bool },
}

impl Endpoint {
    /// The endpoint the setting names. Plain http only to this machine: a key and what the tools
    /// returned are never sent in the clear.
    pub fn parse(s: &str) -> Result<Endpoint> {
        if let Some(path) = s.strip_prefix("unix:") {
            if !path.starts_with('/') {
                bail!("{s}: a unix socket by its absolute path (unix:/run/…)");
            }
            return Ok(Endpoint::Unix(PathBuf::from(path)));
        }
        if s.chars().any(|c| c.is_whitespace() || c.is_control()) {
            bail!("{s:?}: not a URL");
        }
        if let Some(rest) = s.strip_prefix("https://") {
            if rest.is_empty() {
                bail!("{s}: no host");
            }
            return Ok(Endpoint::Http { base: s.trim_end_matches('/').to_string(), local: false });
        }
        if let Some(rest) = s.strip_prefix("http://") {
            let host = rest.split('/').next().unwrap_or("");
            let name = if host.starts_with('[') { host.split(']').next().map(|h| format!("{h}]")).unwrap_or_default() } else { host.split(':').next().unwrap_or("").to_string() };
            if !["127.0.0.1", "localhost", "[::1]"].contains(&name.as_str()) {
                bail!("{s}: plain http only to this machine (127.0.0.1); another host needs https");
            }
            return Ok(Endpoint::Http { base: s.trim_end_matches('/').to_string(), local: true });
        }
        bail!("{s}: an endpoint is unix:/path, http://127.0.0.1:<port>/v1, or https://…/v1");
    }

    /// Whether what the model is shown stays on this machine.
    pub fn local(&self) -> bool {
        match self {
            Endpoint::Unix(_) => true,
            Endpoint::Http { local, .. } => *local,
        }
    }

    /// How it is named to the person.
    pub fn describe(&self) -> String {
        match self {
            Endpoint::Unix(p) => format!("unix:{}", p.display()),
            Endpoint::Http { base, .. } => base.clone(),
        }
    }

    /// One chat completion: the request's JSON in, the response's JSON out.
    pub fn chat(&self, key: Option<&str>, body: &Value) -> Result<Value> {
        let curl = option_env!("TORQ_CURL").context("this build has no curl (TORQ_CURL; agent/package.nix, `nix develop`)")?;
        let mut cmd = Command::new(curl);
        // -q first: no ~/.curlrc. No redirects followed, no proxy, http(s) only.
        cmd.args(["-q", "-sS", "--fail-with-body", "--max-time", "600", "--noproxy", "*"])
            .args(["--proto", if self.local() { "=http" } else { "=https" }])
            .args(["-H", "Content-Type: application/json", "-H", "Expect:", "--data-binary", "@-"]);
        // The key in a private file (0600, in a /tmp the launcher makes the agent's own): never in argv.
        let header = match key {
            Some(k) => {
                let mut f = tempfile::NamedTempFile::new().context("a private file for the key's header")?;
                writeln!(f, "Authorization: Bearer {}", k.trim())?;
                cmd.arg("-H").arg(format!("@{}", f.path().display()));
                Some(f)
            }
            None => None,
        };
        match self {
            Endpoint::Unix(p) => cmd.arg("--unix-socket").arg(p).arg("http://localhost/v1/chat/completions"),
            Endpoint::Http { base, .. } => cmd.arg(format!("{base}/chat/completions")),
        };
        let mut child = cmd
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("running curl")?;
        let mut stdin = child.stdin.take().context("curl's input")?;
        stdin.write_all(serde_json::to_vec(body)?.as_slice())?;
        drop(stdin);
        let out = child.wait_with_output().context("waiting for curl")?;
        drop(header);
        if !out.status.success() {
            let why = format!("{} {}", String::from_utf8_lossy(&out.stderr), String::from_utf8_lossy(&out.stdout));
            bail!("the model at {} did not answer: {}", self.describe(), crate::tools::truncate(&clean(why.trim()), 600));
        }
        serde_json::from_slice(&out.stdout).with_context(|| format!("the model at {} answered something not JSON", self.describe()))
    }
}

/// The API key, from the file the setting names (outside the repo, as passwords are): none without one.
pub fn key(file: Option<&Path>) -> Result<Option<String>> {
    let Some(f) = file else { return Ok(None) };
    let k = std::fs::read_to_string(f).with_context(|| format!("reading the model's key from {}", f.display()))?;
    let k = k.trim().to_string();
    if k.is_empty() || k.chars().any(|c| c.is_control() || c.is_whitespace()) {
        bail!("{}: one key, on one line", f.display());
    }
    Ok(Some(k))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_local_or_encrypted() {
        assert_eq!(Endpoint::parse("unix:/run/llama-cpp/llama.sock").unwrap(), Endpoint::Unix("/run/llama-cpp/llama.sock".into()));
        let local = Endpoint::parse("http://127.0.0.1:8080/v1/").unwrap();
        assert_eq!(local, Endpoint::Http { base: "http://127.0.0.1:8080/v1".into(), local: true });
        assert!(Endpoint::parse("http://[::1]:8080/v1").unwrap().local());
        let remote = Endpoint::parse("https://api.anthropic.com/v1").unwrap();
        assert!(!remote.local());
        for bad in ["http://192.168.1.5:8080/v1", "http://127.0.0.1.evil.com/v1", "http://localhost@evil.com/v1", "ftp://x", "unix:relative.sock", "https://", "https://a b"] {
            assert!(Endpoint::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_key_is_one_line() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("key");
        std::fs::write(&f, "sk-abc\n").unwrap();
        assert_eq!(key(Some(&f)).unwrap().as_deref(), Some("sk-abc"));
        std::fs::write(&f, "sk-abc\nX-Other: y\n").unwrap();
        assert!(key(Some(&f)).is_err(), "no second header smuggled in");
        assert_eq!(key(None).unwrap(), None);
    }
}
