//! `torqcli` (ADR-016): the shell itself — the real `Machine` behind a session (Nix evaluation and
//! the broker) and the entry point. The commands are in
//! session.rs, the tree in tree.rs, the candidate in candidate.rs.

use crate::candidate::{Candidate, Repo};
use crate::session::{Configuration, History, Machine, Mode, Session, Ssh, Waiting};
use crate::syntax::Style;
use crate::tree::Schema;
use crate::{broker, check_repo, config_name, evaluate, history, me, run, scratch_dir, Cli, BROKER, REPO};
use anyhow::{bail, Context, Result};
use std::fs;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

/// The schema of the running system (modules/schema.nix).
const SCHEMA: &str = "/etc/torq/schema.json";
/// Every NixOS setting of the running system (modules/nixos-schema.nix): the tree's paths.
const NIXOS_SCHEMA: &str = "/etc/torq/nixos-schema.json";

struct Real<'a> {
    cli: &'a Cli,
}

impl Real<'_> {
    /// The committed configuration at `base`, as the broker will build it (not the repo's working
    /// copy), with `files` changed (None: deleted), in a scratch folder (kept while the first is).
    fn changed_copy(&self, base: &str, files: &[(String, Option<String>)]) -> Result<(tempfile::TempDir, PathBuf)> {
        let scratch = scratch_dir()?;
        let copy = scratch.path().join("config");
        fs::create_dir(&copy)?;
        Repo::new(&self.cli.repo).archive(base, &copy)?;
        for (path, content) in files {
            no_link_on_the_way(&copy, path)?;
            let f = copy.join(path);
            match content {
                Some(c) => {
                    fs::create_dir_all(f.parent().context("a tree file has a folder")?)?;
                    fs::write(&f, c)?;
                }
                None => {
                    let _ = fs::remove_file(&f);
                }
            }
        }
        Ok((scratch, copy))
    }

    /// If reverting `target` turns SSH back on (it turned it off), the keys that would then get in —
    /// as at a commit: none refuses. After the revert, services.json is as it was before the change
    /// (later changes to it conflict, and are refused by the broker).
    fn revert_ssh_keys(&self, target: &history::Entry) -> Result<String> {
        let changed = target.data["files"].as_array().is_some_and(|f| f.iter().any(|v| v.as_str() == Some("services.json")));
        let Some(after) = target.data["commit"].as_str().filter(|_| changed) else { return Ok(String::new()) };
        let repo = Repo::new(&self.cli.repo);
        let head = repo.head()?;
        let Some(before) = repo.parent_of(after)? else { return Ok(String::new()) };
        let has_ssh = |rev: &str| -> Result<(bool, Option<String>)> {
            let text = repo.file_at(rev, "services.json")?.map(|b| String::from_utf8_lossy(&b).into_owned());
            let on = text.as_deref().and_then(|t| serde_json::from_str::<Vec<String>>(t).ok()).is_some_and(|l| l.iter().any(|s| s == "ssh"));
            Ok((on, text))
        };
        let ((was_on, was), (now_on, _)) = (has_ssh(&before)?, has_ssh(&head)?);
        if !was_on || now_on {
            return Ok(String::new());
        }
        let (owner, keys) = self.ssh_keys(&head, &[("services.json".to_string(), was)])?;
        if keys.is_empty() {
            bail!(
                "not reverted: this turns SSH back on, and SSH here lets in {owner}'s keys only — they have none, so nobody \
                 could log in; add a public key to ~/.ssh/authorized_keys first"
            );
        }
        Ok(format!("Keys that will be able to log in as {owner}:\n{}\n", keys.join("\n")))
    }


    /// The broker's status as `show system alarms` reports it — without colour (the shell cleans its
    /// output, which would show colour codes as text), naming the shell's own commands.
    fn status_report(&self) -> Result<(serde_json::Value, crate::status::Report)> {
        let log = run::start_log()?;
        let v = match run::broker_status(broker(self.cli, &["status"]), &log)? {
            Ok(v) => v,
            Err(why) => bail!("couldn't check this machine: {why}"),
        };
        let last = crate::read_ledger(self.cli).ok().and_then(|l| history::last_change(&l));
        let report = crate::status::render(&v, last.as_deref(), &self.cli.repo.display().to_string(), &crate::ui::Ui::plain());

        Ok((v, report))
    }
}

/// A broker outcome as a result, its message cleaned for the terminal.
fn done(o: run::Outcome, what: &str) -> Result<serde_json::Value> {
    match o {
        run::Outcome::Done(v) => Ok(v),
        run::Outcome::Busy => bail!("{what}: another change is being made right now; try again in a moment"),
        run::Outcome::Refused(m) => bail!("{what} refused: {}", history::one_line(&m, 300)),
        run::Outcome::Broken(m) => bail!("{what}: {m}"),
    }
}

impl Machine for Real<'_> {
    fn check(&self, base: &str, files: &[(String, Option<String>)]) -> Result<()> {
        let (_scratch, copy) = self.changed_copy(base, files)?;
        let log = run::start_log()?;
        let _p = run::Progress::start("Checking");
        let host = config_name(&copy)?;
        evaluate(&copy, &host, &log)?;
        // A tree file the configuration does not read would commit and change nothing: a flake.nix
        // that does not pass `configDir` to torq.lib.mkSystem (as `torq pkg` checked).
        if files.iter().any(|(p, _)| p != "flake.lock") && !crate::reads_tree(&copy, &host)? {
            bail!(
                "this machine's config does not read its settings files yet: its flake.nix must call \
                 torq.lib.mkSystem with `configDir = ./.;` (a system change, made as Nix)"
            );
        }
        Ok(())
    }

    fn commit(&self, diff: &str, base: &str, tier: &str, reason: &str, confirm: Option<u32>) -> Result<u64> {
        let scratch = scratch_dir()?;
        let file = scratch.path().join("change.diff");
        fs::write(&file, diff)?;
        let log = run::start_log()?;
        let file = file.to_string_lossy();
        let args = propose_args(&file, base, reason);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let proposed = done(run::broker_json(broker(self.cli, &args), &log)?, "commit")?;
        let seq = proposed["seq"].as_u64().context("the broker did not say which proposal it made")?;
        // What was shown (and, for the system, asked) must be what the broker applies.
        if proposed["tier"].as_str() != Some(tier) {
            bail!(
                "not committed: the broker treats this as a {} change, not a {tier} one (recorded as proposal {seq}, not applied)",
                history::one_line(proposed["tier"].as_str().unwrap_or("?"), 20)
            );
        }
        let (seq_s, minutes) = (seq.to_string(), confirm.map(|m| m.to_string()));
        let mut args = vec!["apply", &seq_s, "--approved-by"];
        let who = me()?;
        args.push(&who);
        if let Some(m) = &minutes {
            args.extend(["--confirm", m]);
        }
        let outcome = {
            let _p = if crate::asks_for_password(self.cli) { None } else { Some(run::Progress::start("Committing")) };
            run::broker_json(broker(self.cli, &args), &log)
        };
        match outcome.map_err(anyhow::Error::from).and_then(|o| done(o, "commit")) {
            Ok(applied) => applied["seq"].as_u64().context("the broker did not say which change it applied"),
            // The answer was lost, or unclear: the ledger says whether the change was made. (A
            // refusal is certain, and keeps the candidate.)
            Err(e) => match crate::read_ledger(self.cli).ok().and_then(|l| {
                l.iter().find(|x| x.kind == "applied" && x.data["proposal"].as_u64() == Some(seq)).map(|x| x.seq)
            }) {
                Some(applied) => Ok(applied),
                None => Err(e),
            },
        }
    }

    fn pending_confirm(&self) -> Result<Option<Waiting>> {
        let log = run::start_log()?;
        let v = match run::broker_status(broker(self.cli, &["status"]), &log)? {
            Ok(v) => v,
            Err(why) => bail!("cannot check for a commit confirmed waiting: {why}"),
        };
        let p = &v["pending_confirm"];
        Ok(p["applied"].as_u64().map(|seq| Waiting {
            seq,
            deadline: history::one_line(p["deadline"].as_str().unwrap_or("?"), 40),
            by: p["approved_by"].as_str().map(|s| history::one_line(s, 40)),
        }))
    }

    fn confirm(&self) -> Result<u64> {
        let log = run::start_log()?;
        let v = done(run::broker_json(broker(self.cli, &["confirm", "--approved-by", &me()?]), &log)?, "confirm")?;
        v["seq"].as_u64().context("the broker did not say what it recorded")
    }

    fn alarms(&self, detail: bool) -> Result<String> {
        let (v, report) = self.status_report()?;
        let mut out = report.text;
        if let Some(seq) = v["pending_confirm"]["applied"].as_u64() {
            out.push_str(&format!(
                "  change #{seq} (commit confirmed) rolls back at {} unless confirmed\n",
                history::one_line(v["pending_confirm"]["deadline"].as_str().unwrap_or("?"), 40)
            ));
        }
        if detail {
            // The facts behind the report, as the broker gives them (`torq-broker status`).
            let fact = |k: &str| match &v[k] {
                serde_json::Value::Null => "-".to_string(),
                serde_json::Value::String(s) => history::one_line(s, 120),
                other => history::one_line(&other.to_string(), 120),
            };
            for (label, key) in [
                ("config repo commit", "head"),
                ("ledgered commit", "ledgered_commit"),
                ("ledger entries", "ledger_entries"),
                ("ledgered system", "ledgered_system"),
                ("running system", "running_system"),
                ("next boot", "next_boot"),
            ] {
                out.push_str(&format!("  {label:<20} {}\n", fact(key)));
            }
            out.push_str(&format!("  {:<20} {} status\n", "from", history::one_line(&self.cli.broker, 80)));
        }
        Ok(out)
    }

    fn search(&self, terms: &[String]) -> Result<String> {
        if let Some(t) = terms.iter().find(|t| t.starts_with('-')) {
            bail!("show packages available: '{t}' is not a search term");
        }
        crate::search(&self.cli.repo, terms)?; // prints as it goes (nix search)
        Ok(String::new())
    }

    /// Only when the report says a repair can fix what is wrong, and only
    /// with a person's yes — it rebuilds and switches the whole system.
    fn repair(&self) -> Result<String> {
        let (_, report) = self.status_report()?;
        match report.fix {
            crate::status::Fix::Repair => {}
            crate::status::Fix::Nothing if report.exit == 0 => return Ok(format!("{}  Nothing to repair.\n", report.text)),
            _ => bail!("not repaired: a repair would not fix this (see `show system alarms`); a person has to look"),
        }
        let shown: String = report.text.lines().map(|l| format!("{}\n", history::clean(l))).collect();
        if !self.ask(&format!("{shown}This rebuilds and switches the system (all users). Repair? ")) {
            bail!("not repaired: a repair needs a person to approve it at a terminal");
        }
        let log = run::start_log()?;
        let v = {
            let _p = if crate::asks_for_password(self.cli) { None } else { Some(run::Progress::start("Repairing")) };
            done(run::broker_json(broker(self.cli, &["repair", "--approved-by", &me()?]), &log)?, "repair")?
        };
        Ok(format!("repaired: the machine is back at its last ledgered configuration (#{})\n", v["seq"].as_u64().unwrap_or(0)))
    }

    /// As `torq service enable ssh` did: the catalog's SSH lets in the owner's keys only, so these —
    /// as the changed configuration will have them — are exactly what approving it approves.
    fn ssh_keys(&self, base: &str, files: &[(String, Option<String>)]) -> Result<(String, Vec<String>)> {
        let (_scratch, copy) = self.changed_copy(base, files)?;
        let _p = run::Progress::start("Reading the SSH keys");
        let host = config_name(&copy)?;
        let owner = crate::owner(&copy, &host)?;
        let mut rows = Vec::new();
        for (note, id, place) in crate::owner_keys(&copy, &host, &owner)? {
            if !note.is_empty() {
                rows.push(format!("  ! {}", history::one_line(note.trim_end_matches(": "), 120)));
            }
            // Never truncated: a key is approved by what it is.
            rows.push(format!("    {}  (from {})", history::clean(&id), history::one_line(&place, 80)));
        }
        Ok((owner, rows))
    }

    /// As `torq service list` did, of the active configuration as committed.
    fn ssh_outside(&self) -> Result<bool> {
        let head = Repo::new(&self.cli.repo).head()?;
        let (_scratch, copy) = self.changed_copy(&head, &[])?;
        let host = config_name(&copy)?;
        let out = std::process::Command::new("nix")
            .args(["eval", "--json"])
            .arg(format!("{}#nixosConfigurations.{host}.config", crate::flake_ref(&copy)))
            .args([
                "--apply",
                "c: c.services.openssh.enable && !(builtins.elem \"ssh\" c.torq.services.enabled) \
                 && c.services.openssh.settings.PasswordAuthentication != false",
            ])
            .output()
            .context("running nix eval")?;
        if !out.status.success() {
            bail!("cannot read this machine's services: {}", history::one_line(&crate::last_error(&out.stderr), 200));
        }
        Ok(serde_json::from_slice::<bool>(&out.stdout)?)
    }

    fn ssh(&self, base: &str, files: &[(String, Option<String>)]) -> Result<(Ssh, Ssh)> {
        let state = |files: &[(String, Option<String>)]| -> Result<Ssh> {
            let (_scratch, copy) = self.changed_copy(base, files)?;
            let host = config_name(&copy)?;
            let out = std::process::Command::new("nix")
                .args(["eval", "--json"])
                .arg(format!("{}#nixosConfigurations.{host}.config", crate::flake_ref(&copy)))
                .args([
                    "--apply",
                    "c: { on = c.services.openssh.enable; catalog = builtins.elem \"ssh\" c.torq.services.enabled; \
                     passwords = c.services.openssh.settings.PasswordAuthentication != false; }",
                ])
                .output()
                .context("running nix eval")?;
            if !out.status.success() {
                bail!("cannot read the SSH settings: {}", history::one_line(&crate::last_error(&out.stderr), 200));
            }
            let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
            Ok(Ssh { on: v["on"] == true, catalog: v["catalog"] == true, passwords: v["passwords"] == true })
        };
        let _p = run::Progress::start("Reading the SSH settings");
        Ok((state(&[])?, state(files)?))
    }

    fn ledger(&self, count: usize) -> Result<String> {
        let entries = crate::read_ledger(self.cli)?;
        Ok(history::render(&entries, count, &crate::ui::Ui::plain(), &history::Clock::system()))
    }

    /// As `torq undo` did, with a commit's rule for asking: a change only to your own folder goes
    /// ahead; anything else (the system, someone else's folder) needs a person at a terminal.
    fn revert(&self, seq: u64, me: &str) -> Result<String> {
        let entries = crate::read_ledger(self.cli)?;
        let target = history::undo_target(&entries, Some(seq))?;
        let files: Vec<String> =
            target.data["files"].as_array().map(|f| f.iter().filter_map(|v| v.as_str()).map(|s| history::one_line(s, 80)).collect()).unwrap_or_default();
        let mine = only_own_folder(&target.tier, &files, me);
        let what = format!(
            "Revert change #{} \"{}\" ({}, by {}{})\n  files: {}\n",
            target.seq,
            history::one_line(&target.reason, 60),
            history::one_line(&target.ts, 25),
            history::one_line(target.approved_by.as_deref().unwrap_or(&target.actor), 20),
            if target.tainted { ", from a session that read untrusted content" } else { "" },
            if files.is_empty() { "-".to_string() } else { files.join(", ") }
        );
        let what = format!("{what}{}", self.revert_ssh_keys(target)?);
        if !mine {
            let why = if target.tier == "user" { "This changes someone else's settings." } else { "This changes the system (all users)." };
            if !self.ask(&format!("{what}{why} Revert? ")) {
                bail!("not reverted: this needs a person to approve it at a terminal");
            }
        }
        let log = run::start_log()?;
        let outcome = {
            let _p = if crate::asks_for_password(self.cli) { None } else { Some(run::Progress::start("Reverting")) };
            run::broker_json(broker(self.cli, &["revert", &target.seq.to_string(), &format!("--approved-by={me}")]), &log)
        };
        match outcome {
            Ok(run::Outcome::Refused(m)) if m.contains("later changes conflict") => {
                // Name what stands in the way, from the ledger, rather than git's view of it.
                let conflicts = history::later_conflicts(&entries, target.seq);
                let later: Vec<String> = conflicts.iter().map(|s| format!("#{s}")).collect();
                // A revert cannot itself be reverted: then only the opposite change helps.
                let reverted = conflicts.iter().any(|s| entries.iter().any(|e| e.seq == *s && e.kind == "reverted"));
                let advice = if reverted {
                    "make the opposite change in configuration mode"
                } else {
                    "revert those first (newest first), or make the opposite change in configuration mode"
                };
                bail!(
                    "change #{} cannot be reverted on its own: later changes ({}) changed the same files; {advice}",
                    target.seq,
                    if later.is_empty() { "?".to_string() } else { later.join(", ") }
                )
            }
            o => match o.and_then(|o| done(o, "revert")) {
                Ok(v) => Ok(format!("{what}reverted: change #{} undone (change #{})\n", target.seq, v["seq"].as_u64().unwrap_or(0))),
                // The answer was lost, or unclear: the ledger says whether it was reverted.
                Err(e) => match crate::read_ledger(self.cli).ok().and_then(|l| {
                    l.iter().find(|x| x.kind == "reverted" && x.data["applied"].as_u64() == Some(target.seq)).map(|x| x.seq)
                }) {
                    Some(n) => Ok(format!("{what}reverted: change #{} undone (change #{n}; the broker's answer was lost)\n", target.seq)),
                    None => Err(e),
                },
            },
        }
    }

    /// The ledger's entries that produced a configuration (as the broker's `ledgered_commit` counts
    /// them), newest first. The ledger is the broker's: read through it, as a commit is.
    fn configurations(&self, _repo: &Repo, count: usize) -> Result<History> {
        let entries = crate::read_ledger(self.cli)?;
        Ok(History { configurations: configurations_of(&entries, count), at: ledgered_commit(&entries) })
    }

    /// As `torq update` does it, but on the committed configuration the candidate is based on (as
    /// the broker builds it), not the repo's working copy; nothing is proposed here.
    fn update_lock(&self, base: &str, all: bool) -> Result<String> {
        let scratch = scratch_dir()?;
        let copy = scratch.path().join("config");
        fs::create_dir(&copy)?;
        Repo::new(&self.cli.repo).archive(base, &copy)?;
        let out = {
            let _p = run::Progress::start("Looking for updates");
            let mut cmd = std::process::Command::new("nix");
            cmd.args(["flake", "update", "--flake"]).arg(crate::flake_ref(&copy));
            if !all {
                cmd.arg("torq");
            }
            cmd.output().context("running nix flake update")?
        };
        if !out.status.success() {
            bail!("couldn't look for updates: {}", history::one_line(&crate::last_error(&out.stderr), 200));
        }
        fs::read_to_string(copy.join("flake.lock")).context("reading the updated flake.lock")
    }

    fn ask(&self, question: &str) -> bool {
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            return false;
        }
        // Only an answer typed after the question counts: anything already waiting (typed or pasted
        // ahead, e.g. a "y" after "commit") is thrown away.
        // SAFETY: tcflush on fd 0, a terminal (checked above); no memory is involved.
        unsafe {
            libc::tcflush(0, libc::TCIFLUSH);
        }
        print!("{question}[y/N] ");
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        std::io::stdin().lock().read_line(&mut answer).is_ok() && matches!(answer.trim(), "y" | "yes")
    }
    fn edit(&self, path: &str, text: &str) -> Result<Option<String>> {
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            bail!("file edit opens your editor: at a terminal (`file show {path}` prints it)");
        }
        // A private folder of this user's, named as the file, so the editor knows it is Nix.
        let scratch = scratch_dir()?;
        let name = Path::new(path).file_name().context("a config file has a name")?;
        let f = scratch.path().join(name);
        fs::write(&f, text)?;
        let editor = ["VISUAL", "EDITOR"].iter().find_map(|v| std::env::var(v).ok().filter(|e| !e.trim().is_empty()));
        let editor = editor.unwrap_or_else(|| if which("nano") { "nano".into() } else { "vi".into() });
        // As the shell would run $EDITOR (it may carry options: `code --wait`).
        let status = std::process::Command::new("sh").arg("-c").arg(format!("{editor} \"$1\"")).arg("sh").arg(&f).status()?;
        if !status.success() {
            bail!("{path}: the editor ({editor}) failed; nothing changed");
        }
        let now = fs::read_to_string(&f).with_context(|| format!("{path}: not text; nothing changed"))?;
        Ok((now != text).then_some(now))
    }

}

/// Refuse to write `path` under `root` if any folder on the way is a link (or the file itself is):
/// a committed `users/dev -> /home/dev/x` must not make the check write outside its scratch copy.
fn no_link_on_the_way(root: &Path, path: &str) -> Result<()> {
    let mut at = root.to_path_buf();
    for part in Path::new(path).components() {
        at.push(part);
        if fs::symlink_metadata(&at).is_ok_and(|m| m.file_type().is_symlink()) {
            bail!("{} is a link in the configuration; torqcli does not write through it", at.strip_prefix(root).unwrap_or(&at).display());
        }
    }
    Ok(())
}

#[cfg(test)]
#[test]
fn writes_never_go_through_a_link() {
    let d = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::create_dir_all(d.path().join("users")).unwrap();
    std::os::unix::fs::symlink(outside.path(), d.path().join("users/dev")).unwrap();
    let e = format!("{:#}", no_link_on_the_way(d.path(), "users/dev/cli.json").unwrap_err());
    assert!(e.contains("users/dev is a link"), "{e}");
    std::os::unix::fs::symlink("/etc/passwd", d.path().join("services.json")).unwrap();
    assert!(no_link_on_the_way(d.path(), "services.json").is_err(), "the file itself too");
    assert!(no_link_on_the_way(d.path(), "system/host.json").is_ok());
}

/// `propose`'s arguments. Every value given as `--name=value`: a reason or comment that starts with
/// `-` (any change of an existing value does: "-   host-name two; …") must not read as an option.
fn propose_args(diff: &str, base: &str, reason: &str) -> Vec<String> {
    vec!["propose".into(), format!("--diff={diff}"), format!("--base={base}"), format!("--reason={reason}"), "--actor=user".into()]
}

/// A `-c` script's commands: split at `;` and newlines, but not inside "double" or 'single' quotes
/// (as `words` reads them).
fn script_lines(script: &str) -> Vec<String> {
    let (mut out, mut cur, mut open, mut escaped) = (Vec::new(), String::new(), None, false);
    for c in script.chars() {
        match c {
            _ if escaped => {
                cur.push(c);
                escaped = false;
                continue;
            }
            '\\' if open == Some('"') => escaped = true,
            '"' | '\'' if open.is_none() => open = Some(c),
            _ if open == Some(c) => open = None,
            ';' | '\n' if open.is_none() => {
                out.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    out.push(cur);
    out.into_iter().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect()
}

#[cfg(test)]
#[test]
fn propose_values_are_never_options() {
    let a = propose_args("/tmp/d", "abc", "-   host-name two; +   host-name three;");
    assert_eq!(a[3], "--reason=-   host-name two; +   host-name three;");
    assert!(a[1..].iter().all(|x| x.starts_with("--") && x.contains('=')), "{a:?}");
}

#[cfg(test)]
#[test]
fn scripts_split_outside_quotes() {
    assert_eq!(script_lines("configure; set a b\ncommit comment \"a; b\" ;"), ["configure", "set a b", "commit comment \"a; b\""]);
    assert_eq!(script_lines(r#"commit comment "say \"x; y\"""#), [r#"commit comment "say \"x; y\"""#]);
    assert_eq!(script_lines("show configuration | grep 'a;b'; show | cut -d';' -f1"), ["show configuration | grep 'a;b'", "show | cut -d';' -f1"]);
    assert_eq!(script_lines(r#"commit comment "it's; fine""#), [r#"commit comment "it's; fine""#], "a ' inside \" is text");
}

/// The shell: `torqcli` (interactive), `torqcli -c "<command>; <command>…"` (a private candidate,
/// stops at the first error) or `torqcli <command>` (one operational command). Returns the exit code.
pub fn main(args: &[String]) -> i32 {
    if matches!(args, [v] if v == "--version" || v == "-V") {
        say(&format!("torqcli {}\n", env!("CARGO_PKG_VERSION")));
        return 0;
    }
    match run_shell(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {}", history::one_line(&format!("{e:#}"), 300));
            1
        }
    }
}

/// How torqcli was asked to run.
#[derive(Debug, PartialEq)]
enum Invocation {
    /// The shell.
    Interactive,
    /// `-c "<command>; <command>…"`.
    Script(String),
    /// `torqcli <command>`: one operational command, its words as the calling shell split them (one
    /// argument: a whole line, split here).
    Command(Vec<String>),
}

const USAGE: &str = "usage: torqcli [--style juniper|cisco] [-c \"<command>; <command>…\" | <command>]";

fn parse_args(args: &[String]) -> Result<(Invocation, Option<Style>)> {
    let usage = || anyhow::anyhow!(USAGE);
    let (mut run, mut style) = (Invocation::Interactive, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-c" if run == Invocation::Interactive => run = Invocation::Script(it.next().ok_or_else(usage)?.clone()),
            "--style" if style.is_none() => style = Some(Style::parse(it.next().ok_or_else(usage)?).ok_or_else(usage)?),
            "-h" | "--help" => return Err(usage()),
            o if o.starts_with('-') => return Err(usage()),
            _ if run == Invocation::Interactive => {
                let rest: Vec<String> = std::iter::once(a).chain(it).cloned().collect();
                let w = if rest.len() == 1 { crate::session::words(&rest[0])? } else { rest };
                if matches!(w.first().map(String::as_str), Some("configure" | "config")) {
                    bail!("torqcli <command> runs one operational command; to change the configuration: torqcli -c \"configure; set …; commit\"");
                }
                if w.is_empty() {
                    return Err(usage());
                }
                return Ok((Invocation::Command(w), style));
            }
            _ => return Err(usage()),
        }
    }
    Ok((run, style))
}

#[cfg(test)]
#[test]
fn a_command_on_the_command_line() {
    let a = |s: &[&str]| parse_args(&s.iter().map(|x| x.to_string()).collect::<Vec<_>>());
    let w = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    assert_eq!(a(&[]).unwrap(), (Invocation::Interactive, None));
    assert_eq!(a(&["show", "system", "alarms"]).unwrap().0, Invocation::Command(w(&["show", "system", "alarms"])));
    assert_eq!(a(&["show configuration | grep 'a b'"]).unwrap().0, Invocation::Command(w(&["show", "configuration", "|", "grep", "a b"])), "one argument: a line");
    assert_eq!(a(&["show", "configuration", "|", "grep", "a b"]).unwrap().0, Invocation::Command(w(&["show", "configuration", "|", "grep", "a b"])), "words as given");
    assert_eq!(a(&["--style", "cisco", "show", "running-config"]).unwrap(), (Invocation::Command(w(&["show", "running-config"])), Some(Style::Cisco)));
    assert_eq!(a(&["show", "--style", "cisco"]).unwrap().0, Invocation::Command(w(&["show", "--style", "cisco"])), "options come first");
    assert_eq!(a(&["-c", "show; show"]).unwrap().0, Invocation::Script("show; show".into()));
    for bad in [&["-c", "x", "show"][..], &["-c", "x", "-c", "y"], &["-x"], &["-c"], &["--style", "vi", "show"], &[""]] {
        assert!(a(bad).is_err(), "{bad:?}");
    }
    let e = format!("{:#}", a(&["configure"]).unwrap_err());
    assert!(e.contains("torqcli -c"), "{e}");
}
fn run_shell(args: &[String]) -> Result<i32> {
    let (run, style) = parse_args(args)?;
    let env = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_string());
    let cli = Cli {
        repo: PathBuf::from(env("TORQ_CONFIG_REPO", REPO)),
        broker: env("TORQ_BROKER", BROKER),
    };
    check_repo(&cli)?;
    let schema = Schema::parse(&fs::read_to_string(SCHEMA).with_context(|| format!("reading {SCHEMA} (is this TorqOS?)"))?)?;
    let tree = crate::nixos::Tree::parse(&fs::read_to_string(NIXOS_SCHEMA).with_context(|| format!("reading {NIXOS_SCHEMA} (is this TorqOS, from S1 on?)"))?)?;
    let machine = Real { cli: &cli };
    let who = me()?;
    // Scripts and commands speak Juniper unless told otherwise (they must not change meaning with a
    // setting); the interactive shell speaks your style (users <you> cli style, else system cli
    // style). Piped input is a script too: the same rule (and it stops at its first error, below).
    let piped = !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal());
    let style = match (style, &run) {
        (Some(s), _) => s,
        (None, Invocation::Script(_) | Invocation::Command(_)) => Style::Juniper,
        (None, Invocation::Interactive) if piped => Style::Juniper,
        (None, Invocation::Interactive) => configured_style(&who),
    };
    // Only the interactive shell keeps a candidate between runs; a script's is its own.
    let saved = if run == Invocation::Interactive { Some(Candidate::default_path()?) } else { None };
    let mut s = Session::new(schema, tree, Repo::new(&cli.repo), saved, &machine, &who, style);
    let fail = |line: &str, e: anyhow::Error| {
        eprintln!("error: {}: {}", history::one_line(line, 80), history::one_line(&format!("{e:#}"), 300));
        Ok(1)
    };
    match run {
        Invocation::Script(script) => {
            for line in script_lines(&script) {
                match s.exec(&line) {
                    Ok(out) => print_clean(&out),
                    Err(e) => return fail(&line, e),
                }
            }
            Ok(0)
        }
        Invocation::Command(w) => {
            // A `|` word is the pipe: shown as one, not quoted.
            let shown = w.iter().map(|x| if x == "|" { x.clone() } else { crate::syntax::quote(x) }).collect::<Vec<_>>().join(" ");
            match s.exec_words(&w) {
                Ok(out) => {
                    print_clean(&out);
                    Ok(0)
                }
                Err(e) => fail(&shown, e),
            }
        }
        Invocation::Interactive => interactive(&mut s),
    }
}

/// Output line by line, each cleaned of anything that could fool a terminal (the configuration is
/// checked on load, but the ledger's reasons and messages are not ours).
fn print_clean(out: &str) {
    for line in out.lines() {
        say(&format!("{}\n", history::clean(line)));
    }
}

/// Write to standard output. Whoever reads it stopped (`torqcli show configuration | head`): stop
/// too, quietly, as a tool killed by SIGPIPE would (141), not with a panic. Any other failure to
/// write (a full disk) is an error, never a quiet success with the output missing.
fn say(text: &str) {
    let mut o = std::io::stdout().lock();
    if let Err(e) = o.write_all(text.as_bytes()).and_then(|_| o.flush()) {
        if e.kind() == std::io::ErrorKind::BrokenPipe {
            std::process::exit(141);
        }
        eprintln!("error: writing the output: {e}");
        std::process::exit(1);
    }
}

/// The prompt: `user@host> ` in operational mode, `[edit]` then `user@host# ` in configuration mode.
/// The style this machine's configuration gives `user` (/etc/torq/cli.json, modules/cli.nix).
fn configured_style(user: &str) -> Style {
    let v: serde_json::Value = fs::read(CLI_STYLES).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    v["users"][user].as_str().or(v["style"].as_str()).and_then(Style::parse).unwrap_or(Style::Juniper)
}

const CLI_STYLES: &str = "/etc/torq/cli.json";

/// The prompt, and a line above it: Juniper `[edit system]` then `user@host# `; Cisco
/// `user@host(config-system)# `. Operational mode: `user@host> ` in both.
fn prompt(s: &Session, user: &str, host: &str) -> (Option<String>, String) {
    let path: Vec<String> = s.edit_path().iter().map(|w| history::clean(w)).collect();
    match (s.mode, s.style) {
        (Mode::Operational, _) => (None, format!("{user}@{host}> ")),
        (Mode::Configuration, Style::Juniper) => {
            let at = if path.is_empty() { "[edit]".to_string() } else { format!("[edit {}]", path.join(" ")) };
            (Some(format!("\n{at}")), format!("{user}@{host}# "))
        }
        (Mode::Configuration, Style::Cisco) => {
            let at = if path.is_empty() { "config".to_string() } else { format!("config-{}", path.join("-")) };
            (None, format!("{user}@{host}({at})# "))
        }
    }
}

/// The interactive shell: with a line editor (history, Tab, `?`) on a terminal, one plain line at a
/// time otherwise (piped input).
fn interactive(s: &mut Session) -> Result<i32> {
    let host = fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_default().trim().to_string();
    let user = me()?;
    if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        return editor::run(s, &user, &host);
    }
    let stdin = std::io::stdin();
    loop {
        let (above, p) = prompt(s, &user, &host);
        if let Some(a) = above {
            say(&format!("{a}\n"));
        }
        say(&p);
        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            say("\n");
            return Ok(0);
        }
        // Piped lines are a script: stop at the first error, so a later `commit` cannot commit
        // what an earlier failed line did not change.
        if !run_line(s, line.trim()) {
            return Ok(1);
        }
        if s.done {
            return Ok(0);
        }
    }
}

/// Run one line; false if it failed (the error is printed).
fn run_line(s: &mut Session, line: &str) -> bool {
    match s.exec(line) {
        Ok(out) => {
            print_clean(&out);
            true
        }
        Err(e) => {
            println!("error: {}", history::one_line(&format!("{e:#}"), 300));
            false
        }
    }
}

/// The line editor: rustyline, with Tab and `?` answered by complete.rs.
mod editor {
    use super::{prompt, run_line};
    use crate::complete;
    use crate::session::{words, Session};
    use anyhow::Result;
    use rustyline::completion::{Completer, Pair};
    use rustyline::error::ReadlineError;
    use rustyline::highlight::Highlighter;
    use rustyline::hint::Hinter;
    use rustyline::validate::Validator;
    use rustyline::{Cmd, ConditionalEventHandler, Context, Event, EventContext, EventHandler, KeyEvent, RepeatCount};
    use std::sync::{Arc, Mutex};

    /// What Tab and `?` need: the schema, and the mode the session is in now.
    #[derive(Clone)]
    struct Help {
        know: Arc<complete::Know>,
        /// Where the session is: mode, style, edit path (updated before each line is read).
        at: Arc<Mutex<complete::At>>,
    }

    /// The complete words before the cursor, and the one being typed. None inside a quoted string.
    fn at_cursor(before: &str) -> Option<(Vec<String>, String)> {
        // Inside an open quote (or a line that does not split): `?` is a character.
        if words(before).is_err() {
            return None;
        }
        let partial = if before.ends_with(char::is_whitespace) { "" } else { before.rsplit(char::is_whitespace).next().unwrap_or("") };
        let done = &before[..before.len() - partial.len()];
        Some((words(done).ok()?, partial.to_string()))
    }

    impl Help {
        fn choices(&self, before: &str) -> Option<(Vec<complete::Choice>, String)> {
            let (w, partial) = at_cursor(before)?;
            let at = self.at.lock().unwrap().clone();
            Some((complete::choices_at(&self.know, &at, &w, &partial), partial))
        }
    }

    impl Completer for Help {
        type Candidate = Pair;
        fn complete(&self, line: &str, pos: usize, _: &Context<'_>) -> rustyline::Result<(usize, Vec<Pair>)> {
            let Some((choices, partial)) = self.choices(&line[..pos]) else { return Ok((pos, vec![])) };
            let pairs = complete::tab(&choices).into_iter().map(|(display, replacement)| Pair { display, replacement }).collect();
            Ok((pos - partial.len(), pairs))
        }
    }
    impl Hinter for Help {
        type Hint = String;
    }
    impl Highlighter for Help {}
    impl Validator for Help {}
    impl rustyline::Helper for Help {}

    /// `?`: list what may come next, Junos style, and redraw the line as it was.
    struct Question(Help);
    impl ConditionalEventHandler for Question {
        fn handle(&self, _: &Event, _: RepeatCount, _: bool, ctx: &EventContext) -> Option<Cmd> {
            let (choices, _) = self.0.choices(&ctx.line()[..ctx.pos()])?; // in quotes: a plain '?'
            let text: String = complete::render(&choices).lines().map(|l| format!("{}\n", crate::history::clean(l))).collect();
            print!("?\r\n{}", text.replace('\n', "\r\n"));
            Some(Cmd::Repaint)
        }
    }

    #[cfg(test)]
    #[test]
    fn the_word_at_the_cursor() {
        let v = |s: &str| s.split_whitespace().map(String::from).collect::<Vec<_>>();
        assert_eq!(at_cursor("set sys"), Some((v("set"), "sys".into())));
        assert_eq!(at_cursor("set system "), Some((v("set system"), "".into())));
        assert_eq!(at_cursor(""), Some((vec![], "".into())));
        assert_eq!(at_cursor("commit comment \"why"), None, "inside quotes: `?` is a character");
        assert_eq!(at_cursor("commit comment \"why\" "), Some((vec!["commit".into(), "comment".into(), "why".into()], "".into())));
        assert_eq!(at_cursor("show | grep 'a b"), None, "inside single quotes too");
    }

    pub fn run(s: &mut Session, user: &str, host: &str) -> Result<i32> {
        let at = |s: &Session| complete::At { mode: s.mode, style: s.style, edit: s.edit_path().to_vec() };
        let know = complete::Know { schema: s.schema.clone(), tree: s.tree.clone() };
        let help = Help { know: Arc::new(know), at: Arc::new(Mutex::new(at(s))) };
        let config = rustyline::Config::builder().completion_type(rustyline::CompletionType::List).auto_add_history(true).build();
        let mut ed: rustyline::Editor<Help, rustyline::history::DefaultHistory> = rustyline::Editor::with_config(config)?;
        ed.set_helper(Some(help.clone()));
        ed.bind_sequence(KeyEvent::from('?'), EventHandler::Conditional(Box::new(Question(help.clone()))));
        loop {
            *help.at.lock().unwrap() = at(s);
            let (above, p) = prompt(s, user, host);
            if let Some(a) = above {
                println!("{a}");
            }
            match ed.readline(&p) {
                // A paste of several lines (bracketed paste) arrives as one: run each, in order, as
                // if typed — pasting a block of `set` commands is how configurations travel.
                Ok(text) => {
                    // Stop at the first error, as `-c` does: a later `commit` must not commit part
                    // of what was pasted.
                    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
                    for (i, line) in lines.iter().enumerate() {
                        if !run_line(s, line) {
                            if i + 1 < lines.len() {
                                println!("(the {} pasted lines after it were not run)", lines.len() - i - 1);
                            }
                            break;
                        }
                        if s.done {
                            break;
                        }
                    }
                }
                // Ctrl-C: drop the line, as a shell does.
                Err(ReadlineError::Interrupted) => continue,
                Err(ReadlineError::Eof) => {
                    println!();
                    return Ok(0);
                }
                Err(e) => return Err(e.into()),
            }
            if s.done {
                return Ok(0);
            }
        }
    }
}

/// Whether a change touches only `me`'s own folder (user tier, every file under `users/<me>/`): the
/// one kind a revert or a commit makes without asking.
fn only_own_folder(tier: &str, files: &[String], me: &str) -> bool {
    let own = format!("users/{me}/");
    tier == "user" && !files.is_empty() && files.iter().all(|f| f.starts_with(&own) && !f[own.len()..].contains(".."))
}

#[cfg(test)]
#[test]
fn only_ones_own_folder_goes_without_asking() {
    let f = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert!(only_own_folder("user", &f(&["users/viper/packages.json", "users/viper/cli.json"]), "viper"));
    assert!(!only_own_folder("user", &f(&["users/viper/packages.json", "users/eve/cli.json"]), "viper"), "someone else's");
    assert!(!only_own_folder("user", &f(&["users/viperx/packages.json"]), "viper"), "a longer name is not yours");
    assert!(!only_own_folder("user", &f(&["users/viper/../eve/cli.json"]), "viper"));
    assert!(!only_own_folder("system", &f(&["users/viper/packages.json"]), "viper"), "the system tier always asks");
    assert!(!only_own_folder("user", &[], "viper"), "no files: nothing known, ask");
}

/// Where the ledger says the config repo is: the commit of its last entry that moved it — the
/// broker's own count (`ledgered_commit`, broker/src/lib.rs), repairs and interrupted changes too.
fn ledgered_commit(entries: &[history::Entry]) -> Option<String> {
    entries
        .iter()
        .rev()
        .filter(|e| matches!(e.kind.as_str(), "init" | "applied" | "reverted" | "interrupted" | "repaired" | "rolled_back"))
        .find_map(|e| e.data["commit"].as_str().map(String::from))
}

/// The configurations in a ledger, newest first: its `init`, `applied`, `reverted` and `rolled_back`
/// entries (each made a commit), at most `count`. Text from the ledger is one line each, cleaned.
fn configurations_of(entries: &[history::Entry], count: usize) -> Vec<Configuration> {
    entries
        .iter()
        .rev()
        .filter(|e| matches!(e.kind.as_str(), "init" | "applied" | "reverted" | "rolled_back"))
        .filter_map(|e| {
            let id = e.data["commit"].as_str()?;
            let who = e.approved_by.clone().filter(|w| !w.is_empty()).unwrap_or_else(|| e.actor.clone());
            Some(Configuration {
                id: id.to_string(),
                date: history::one_line(&e.ts, 25),
                who: history::one_line(&who, 20),
                what: history::one_line(&e.reason, 80),
                seq: Some(e.seq),
            })
        })
        .take(count)
        .collect()
}

#[cfg(test)]
#[test]
fn configurations_are_the_ledger_entries_that_made_one() {
    let ledger = [
        r#"{"seq":1,"ts":"2026-10-31T01:00:00Z","actor":"user","approved_by":null,"kind":"init","tier":"system","reason":"Config repository initialised","data":{"commit":"aaaa"}}"#,
        r#"{"seq":2,"ts":"2026-10-31T02:00:00Z","actor":"user","approved_by":null,"kind":"proposal","tier":"user","reason":"x","data":{}}"#,
        r#"{"seq":3,"ts":"2026-10-31T02:01:00Z","actor":"user","approved_by":"viper","kind":"applied","tier":"user","reason":"+   style cisco;","data":{"commit":"bbbb","proposal":2}}"#,
        r#"{"seq":4,"ts":"2026-10-31T02:02:00Z","actor":"user","approved_by":"viper","kind":"confirmed","tier":"user","reason":"Confirmed","data":{"applied":3}}"#,
        r#"{"seq":5,"ts":"2026-10-31T03:00:00Z","actor":"broker","approved_by":null,"kind":"rolled_back","tier":"user","reason":"Change #3 rolled back\nfake: line","data":{"commit":"aaaa","applied":3}}"#,
        r#"{"seq":6,"ts":"2026-10-31T04:00:00Z","actor":"user","approved_by":"viper","kind":"apply_failed","tier":"user","reason":"broke","data":{}}"#,
        // A repair returns to a configuration the ledger has: it makes none of its own.
        r#"{"seq":7,"ts":"2026-10-31T05:00:00Z","actor":"user","approved_by":"viper","kind":"repaired","tier":"system","reason":"Repaired","data":{"commit":"aaaa"}}"#,
    ]
    .join("\n");
    let entries = history::parse(&ledger).unwrap();
    let c = configurations_of(&entries, 10);
    let ids: Vec<&str> = c.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, ["aaaa", "bbbb", "aaaa"], "newest first; a rollback is a configuration of its own");
    assert_eq!((c[0].who.as_str(), c[0].seq), ("broker", Some(5)));
    assert_eq!(c[1].who, "viper", "the person who approved it");
    assert_eq!(c[0].what, "Change #3 rolled back fake: line", "one line");
    assert_eq!(configurations_of(&entries, 1).len(), 1);
    assert_eq!(ledgered_commit(&entries).as_deref(), Some("aaaa"), "the repair is the newest entry that moved it");
    let more = format!("{ledger}\n{}", r#"{"seq":8,"ts":"t","actor":"broker","approved_by":null,"kind":"interrupted","tier":"user","reason":"i","data":{"commit":"cccc"}}"#);
    assert_eq!(ledgered_commit(&history::parse(&more).unwrap()).as_deref(), Some("cccc"), "as the broker counts: interrupted too");
}

/// Whether `prog` is a program on PATH.
fn which(prog: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(prog).is_file()))
}
