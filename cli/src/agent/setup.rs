//! What the agent is for this person and on this machine (ADR-051 2 and 4), as the Agent page, the applet
//! and the window say it: their agent account, whether its service is here, and the model it asks —
//! local, or REMOTE and where — read from what the system's configuration put in /etc/torq/agent.json
//! (modules/ai.nix), everyone's to read. Whether a key is configured is a yes or a no: its file's name is
//! in the configuration, its contents are the agent's, never read here.

use serde::Deserialize;

/// What the system's configuration wrote for the agent (modules/ai.nix): the fields this reads.
const AGENT_JSON: &str = "/etc/torq/agent.json";

#[derive(Deserialize)]
struct Written {
    endpoint: String,
    model: String,
    #[serde(rename = "apiKeyFile", default)]
    api_key_file: Option<String>,
}

/// Where the model is.
#[derive(Debug, Clone, PartialEq)]
pub enum Place {
    /// llama-server on this machine, on its socket (the default).
    Local,
    /// Another server on this machine (`http://127.0.0.1…`): what it reads stays here.
    ThisMachine(String),
    /// Off the machine: its host. What the agent reads leaves the machine.
    Remote(String),
}

/// The model the agent asks.
#[derive(Debug, Clone, PartialEq)]
pub struct Brain {
    pub place: Place,
    /// The model's name, as its endpoint knows it.
    pub model: String,
    /// The endpoint, as configured.
    pub endpoint: String,
    /// Whether the configuration names a key file (never its contents).
    pub key: bool,
}

/// The host (and port) of an `https://` or `http://` URL.
fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    // Never a user or password in what is shown.
    crate::history::one_line(host.rsplit('@').next().unwrap_or_default(), 80)
}

/// A URL without any user or password in it, as it is shown.
fn without_user(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else { return url.to_string() };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, path) = rest.split_at(end);
    format!("{scheme}://{}{path}", authority.rsplit('@').next().unwrap_or_default())
}

/// /etc/torq/agent.json's words, read.
pub fn brain_of(json: &str) -> anyhow::Result<Brain> {
    let w: Written = serde_json::from_str(json)?;
    let place = if w.endpoint.starts_with("unix:") {
        Place::Local
    } else if w.endpoint.starts_with("https://") {
        Place::Remote(host_of(&w.endpoint))
    } else {
        Place::ThisMachine(host_of(&w.endpoint))
    };
    Ok(Brain { place, model: crate::history::one_line(&w.model, 80), endpoint: crate::history::one_line(&without_user(&w.endpoint), 200), key: w.api_key_file.is_some() })
}

/// The model this machine's agent asks: `None` when doctor mode is off (nothing configured).
pub fn brain() -> anyhow::Result<Option<Brain>> {
    match std::fs::read_to_string(AGENT_JSON) {
        Ok(json) => brain_of(&json).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(anyhow::Error::from(e).context(AGENT_JSON)),
    }
}

impl Brain {
    /// In one line: "Local model (llama-server on this machine)", "REMOTE: api.anthropic.com · claude-…".
    pub fn line(&self) -> String {
        match &self.place {
            Place::Local => "Local model (llama-server on this machine)".to_string(),
            Place::ThisMachine(h) => format!("On this machine: {h} · {}", self.model),
            Place::Remote(h) => format!("REMOTE: {h} · {}", self.model),
        }
    }

    pub fn remote(&self) -> bool {
        matches!(self.place, Place::Remote(_))
    }
}

/// The person and their agent, for the Agent page.
#[derive(Debug, Clone, PartialEq)]
pub struct Setup {
    pub me: String,
    /// Their agent account, if they have one.
    pub agent: Option<String>,
    /// Whether their agent's service is on this machine (its socket is there).
    pub service: bool,
    /// The model, where doctor mode is on.
    pub brain: Option<Brain>,
}

/// The caller's own setup.
pub fn setup() -> anyhow::Result<Setup> {
    let me = crate::me()?;
    let passwd = std::fs::read_to_string("/etc/passwd")?;
    let account = super::agent_of(&me);
    let agent = passwd.lines().any(|l| l.split(':').next() == Some(account.as_str())).then_some(account);
    let service = std::path::Path::new(&torq_wire::agent::socket(&me)).exists();
    Ok(Setup { me, agent, service, brain: brain()? })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_model_in_one_line() {
        let local = brain_of(r#"{"endpoint":"unix:/run/llama-cpp/llama.sock","model":"local","apiKeyFile":null,"journal":false,"maxSteps":8}"#).unwrap();
        assert_eq!(local.place, Place::Local);
        assert_eq!(local.line(), "Local model (llama-server on this machine)");
        assert!(!local.key && !local.remote());
        let remote = brain_of(r#"{"endpoint":"https://key:secret@api.anthropic.com/v1","model":"claude-sonnet-4-5","apiKeyFile":"/var/lib/torq-agent-alice/model.key","journal":true,"maxSteps":8}"#).unwrap();
        assert_eq!(remote.place, Place::Remote("api.anthropic.com".into()));
        assert_eq!(remote.line(), "REMOTE: api.anthropic.com · claude-sonnet-4-5");
        assert!(remote.key && remote.remote());
        assert_eq!(remote.endpoint, "https://api.anthropic.com/v1", "no password shown");
        let here = brain_of(r#"{"endpoint":"http://127.0.0.1:8099/v1","model":"fake","apiKeyFile":null}"#).unwrap();
        assert_eq!(here.place, Place::ThisMachine("127.0.0.1:8099".into()));
        assert!(!here.remote());
        assert!(brain_of("{}").is_err());
    }
}
