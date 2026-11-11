//! `torq-mcp --link` (ADR-051 5 and 7, step 5): TorqOS's MCP server and its skill linked into Claude Code's
//! settings in this account's home — run by `torq agent run`'s launcher at every start, **as the agent**,
//! after it has dropped to the agent account and before the program (modules/agent-run.sh): nothing is
//! written into the agent's home as root, and no link there is followed with any privilege but the agent's.
//!
//! - `~/.claude.json`, `mcpServers.torqos`: `torq-mcp` on standard input and output. The entry named `torqos`
//!   is TorqOS's, put back as it should be at each start; everything else in the file is left as it was. A
//!   file that is not a plain file of JSON (a link, a directory, text that is not an object) is left alone,
//!   and said so.
//! - `~/.claude/skills/torqos`: a link to the system's skill (`/etc/torq/skills/torqos`). Made where nothing
//!   is, or where TorqOS's own link was (into /etc/torq or the store); anything else there is the person's,
//!   left alone, and said so.
//!
//! Advice for the tool, which it may rewrite: the limits are the broker's, never in these files.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// torq-mcp, where every program of the system finds it.
pub const MCP: &str = "/run/current-system/sw/bin/torq-mcp";
/// The skill, in the system (modules/agent.nix: environment.etc).
pub const SKILL: &str = "/etc/torq/skills/torqos";
/// What both are called in Claude Code's settings.
pub const NAME: &str = "torqos";
/// The largest `~/.claude.json` read (Claude Code keeps its projects' history there); larger is left alone.
const MAX_SETTINGS: u64 = 16 << 20;

/// Link TorqOS into Claude Code's settings in `home`: `mcp` as its MCP server, `skill` as its skill. What
/// was left alone, and why, each a line for the person.
pub fn claude_code(home: &Path, mcp: &str, skill: &Path) -> Vec<String> {
    let mut notes = Vec::new();
    if let Err(e) = server(home, mcp) {
        notes.push(format!("{}: left as it is ({e:#}): TorqOS's tools are not linked into Claude Code", home.join(".claude.json").display()));
    }
    match skill_link(home, skill) {
        Ok(Some(note)) => notes.push(note),
        Ok(None) => {}
        Err(e) => notes.push(format!("TorqOS's skill is not linked into Claude Code: {e:#}")),
    }
    notes
}

/// `~/.claude.json`'s `mcpServers.torqos`, written only if it is not already as it should be.
fn server(home: &Path, mcp: &str) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = home.join(".claude.json");
    let mut settings = match std::fs::symlink_metadata(&file) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(e) => return Err(e).context("reading it"),
        Ok(m) if !m.file_type().is_file() => anyhow::bail!("not a plain file"),
        Ok(_) => {
            let mut text = String::new();
            std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
                .open(&file)
                .context("reading it")?
                .take(MAX_SETTINGS + 1)
                .read_to_string(&mut text)
                .context("reading it")?;
            if text.len() as u64 > MAX_SETTINGS {
                anyhow::bail!("larger than {MAX_SETTINGS} bytes");
            }
            serde_json::from_str(&text).context("not JSON")?
        }
    };
    let entry = json!({ "type": "stdio", "command": mcp, "args": [], "env": {} });
    let Value::Object(top) = &mut settings else { anyhow::bail!("not a JSON object") };
    let servers = top.entry("mcpServers").or_insert_with(|| json!({}));
    let Value::Object(servers) = servers else { anyhow::bail!("its mcpServers is not an object") };
    if servers.get(NAME) == Some(&entry) {
        return Ok(());
    }
    servers.insert(NAME.into(), entry);
    // A new file beside it, then renamed over it: whole or as it was.
    let part = home.join(format!(".claude.json.torq-{}", std::process::id()));
    let _ = std::fs::remove_file(&part);
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&part).context("writing it")?;
    f.write_all(serde_json::to_string_pretty(&settings)?.as_bytes()).and_then(|()| f.write_all(b"\n")).and_then(|()| f.sync_all()).context("writing it")?;
    std::fs::rename(&part, &file).context("writing it")
}

/// Whether a link's target is TorqOS's own skill, where a system put it (now, or in an older system).
fn ours(target: &Path) -> bool {
    target.starts_with("/etc/torq/") || target.starts_with("/nix/store/")
}

/// A directory of the agent's own, made (0700) if missing; anything else there is not followed.
fn own_dir(p: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => Ok(()),
        Ok(_) => anyhow::bail!("{}: not a directory (a link is not followed)", p.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => std::fs::DirBuilder::new().mode(0o700).create(p).with_context(|| format!("making {}", p.display())),
        Err(e) => Err(e).with_context(|| format!("reading {}", p.display())),
    }
}

/// `~/.claude/skills/torqos` → `skill`; a note if something of the person's is there instead.
fn skill_link(home: &Path, skill: &Path) -> Result<Option<String>> {
    let dir: PathBuf = home.join(".claude");
    own_dir(&dir)?;
    own_dir(&dir.join("skills"))?;
    let at = dir.join("skills").join(NAME);
    match std::fs::symlink_metadata(&at) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("reading {}", at.display())),
        Ok(m) if m.file_type().is_symlink() => {
            let target = std::fs::read_link(&at).with_context(|| format!("reading {}", at.display()))?;
            if target == skill {
                return Ok(None);
            }
            if !ours(&target) {
                return Ok(Some(format!("{}: yours (a link to {}), left as it is: TorqOS's skill is not linked", at.display(), target.display())));
            }
            std::fs::remove_file(&at).with_context(|| format!("replacing {}", at.display()))?;
        }
        Ok(_) => return Ok(Some(format!("{}: yours, left as it is: TorqOS's skill is not linked", at.display()))),
    }
    std::os::unix::fs::symlink(skill, &at).with_context(|| format!("linking {}", at.display()))?;
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SKILL_AT: &str = "/etc/torq/skills/torqos";

    fn read(p: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }

    /// From nothing: the server entry and the skill's link, both TorqOS's.
    #[test]
    fn linked_into_a_new_home() {
        let d = tempfile::tempdir().unwrap();
        assert!(claude_code(d.path(), MCP, Path::new(SKILL_AT)).is_empty());
        assert_eq!(read(&d.path().join(".claude.json")), json!({"mcpServers": {"torqos": {"type": "stdio", "command": MCP, "args": [], "env": {}}}}));
        assert_eq!(std::fs::read_link(d.path().join(".claude/skills/torqos")).unwrap(), Path::new(SKILL_AT));
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(d.path().join(".claude.json")).unwrap().permissions().mode() & 0o777, 0o600);
        // Again: nothing changes (no write when it is as it should be).
        let before = std::fs::metadata(d.path().join(".claude.json")).unwrap().modified().unwrap();
        assert!(claude_code(d.path(), MCP, Path::new(SKILL_AT)).is_empty());
        assert_eq!(std::fs::metadata(d.path().join(".claude.json")).unwrap().modified().unwrap(), before);
    }

    /// What the person (or the tool) set stays: other keys, other servers; only `torqos` is TorqOS's, put back.
    #[test]
    fn what_was_there_stays() {
        let d = tempfile::tempdir().unwrap();
        let mine = json!({"numStartups": 3, "projects": {"/home/alice/p": {"allowedTools": ["Bash"]}}, "mcpServers": {"github": {"command": "gh-mcp"}, "torqos": {"command": "/tmp/evil"}}});
        std::fs::write(d.path().join(".claude.json"), mine.to_string()).unwrap();
        assert!(claude_code(d.path(), MCP, Path::new(SKILL_AT)).is_empty());
        let v = read(&d.path().join(".claude.json"));
        assert_eq!(v["numStartups"], 3);
        assert_eq!(v["projects"], mine["projects"]);
        assert_eq!(v["mcpServers"]["github"], json!({"command": "gh-mcp"}));
        assert_eq!(v["mcpServers"]["torqos"]["command"], MCP);
    }

    /// Never through a link, never over what is not plain JSON: left alone, and said so.
    #[test]
    fn what_it_cannot_read_as_its_own_is_left_alone() {
        let d = tempfile::tempdir().unwrap();
        let elsewhere = d.path().join("elsewhere.json");
        std::fs::write(&elsewhere, "{}").unwrap();
        std::os::unix::fs::symlink(&elsewhere, d.path().join(".claude.json")).unwrap();
        let notes = claude_code(d.path(), MCP, Path::new(SKILL_AT));
        assert!(notes.len() == 1 && notes[0].contains("not a plain file"), "{notes:?}");
        assert_eq!(std::fs::read_to_string(&elsewhere).unwrap(), "{}", "the link not followed");
        for text in ["not json {", "[1, 2]", r#"{"mcpServers": 7}"#] {
            let d = tempfile::tempdir().unwrap();
            std::fs::write(d.path().join(".claude.json"), text).unwrap();
            let notes = claude_code(d.path(), MCP, Path::new(SKILL_AT));
            assert_eq!(notes.len(), 1, "{text}: {notes:?}");
            assert_eq!(std::fs::read_to_string(d.path().join(".claude.json")).unwrap(), text, "{text}");
        }
        // ~/.claude a link: not followed, nothing made there.
        let d = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(other.path(), d.path().join(".claude")).unwrap();
        let notes = claude_code(d.path(), MCP, Path::new(SKILL_AT));
        assert!(notes.len() == 1 && notes[0].contains("not a directory"), "{notes:?}");
        assert_eq!(std::fs::read_dir(other.path()).unwrap().count(), 0);
    }

    /// The skill's place: TorqOS's own link replaced (a system's older one), the person's left alone.
    #[test]
    fn the_skill_link_is_torqs_or_left_alone() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join(".claude/skills")).unwrap();
        let at = d.path().join(".claude/skills/torqos");
        std::os::unix::fs::symlink("/nix/store/aaaa-torq-skill/torqos", &at).unwrap();
        assert!(claude_code(d.path(), MCP, Path::new(SKILL_AT)).is_empty());
        assert_eq!(std::fs::read_link(&at).unwrap(), Path::new(SKILL_AT));
        std::fs::remove_file(&at).unwrap();
        std::os::unix::fs::symlink("/home/alice/my-skill", &at).unwrap();
        let notes = claude_code(d.path(), MCP, Path::new(SKILL_AT));
        assert!(notes.len() == 1 && notes[0].contains("yours"), "{notes:?}");
        assert_eq!(std::fs::read_link(&at).unwrap(), Path::new("/home/alice/my-skill"));
        std::fs::remove_file(&at).unwrap();
        std::fs::create_dir(&at).unwrap();
        let notes = claude_code(d.path(), MCP, Path::new(SKILL_AT));
        assert!(notes.len() == 1 && notes[0].contains("yours"), "{notes:?}");
        assert!(std::fs::symlink_metadata(&at).unwrap().is_dir());
    }
}
