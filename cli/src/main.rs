//! `torq`: the unprivileged command line. It never changes the system itself: it writes a
//! proposed change as a diff and hands it to `torq-broker`, which ledgers, applies and can undo it.

mod history;
mod tree;
mod candidate;
mod session;
mod shell;
mod complete;
mod syntax;
mod filter;
// The NixOS settings tree: read and checked here (S1), editable in configuration mode next (S3).
#[allow(dead_code)]
mod nixos;
mod pkg;
mod run;
mod status;
mod ui;

use anyhow::{bail, Context, Result};

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const REPO: &str = "/var/lib/torq/config";
/// By absolute path: nothing earlier on PATH can stand in for sudo or the broker.
const BROKER: &str = "/run/wrappers/bin/sudo /run/current-system/sw/bin/torq-broker";
/// The configuration the broker builds, whatever the host is called (torq.broker.configName).
const CONFIG: &str = "torq";

/// How the shell reaches the machine: its config repo, and how to run the broker (it needs root).
/// `TORQ_CONFIG_REPO` and `TORQ_BROKER` override them, for testing only, together (check_repo).
pub struct Cli {
    pub repo: PathBuf,
    pub broker: String,
}

/// The commands `torq` had before torqcli (ADR-016), and what each is now. None: not one of them.
fn retired(args: &[String]) -> Option<String> {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let now = match a.as_slice() {
        ["pkg", "add", ..] => "configure; set users <you> packages <name>; commit   (--system: set system packages <name>)",
        ["pkg", "remove", ..] => "configure; delete users <you> packages <name>; commit",
        ["pkg", "list", ..] => "show configuration users <you> packages   (and: show configuration system packages)",
        ["pkg", "search", ..] => "show packages available <term>",
        ["pkg", ..] => "configure; set users <you> packages <name>; commit",
        ["service", "list", ..] => "show services",
        ["service", "enable", ..] => "configure; set services <name>; commit",
        ["service", "disable", ..] => "configure; delete services <name>; commit",
        ["service", ..] => "show services",
        ["update", ..] => "request system update [all]; configure; commit",
        ["log", ..] => "show system ledger   (configurations: show system commit)",
        ["undo", ..] => "request system revert <change>   (a whole earlier configuration: rollback <n>; commit)",
        ["status", "--repair", ..] => "request system repair",
        ["status", ..] => "show system alarms",
        _ => return None,
    };
    Some(format!("`torq {}` is gone: TorqOS is driven by torqcli now (`torq` is torqcli). Instead: {now}", a[0]))
}

#[cfg(test)]
#[test]
fn the_old_commands_say_what_replaced_them() {
    let r = |s: &str| retired(&s.split_whitespace().map(String::from).collect::<Vec<_>>());
    assert!(r("pkg add htop").unwrap().contains("set users <you> packages <name>"));
    assert!(r("undo 23").unwrap().contains("request system revert <change>"));
    assert!(r("status --repair").unwrap().contains("request system repair"));
    assert!(r("log").unwrap().contains("show system ledger"));
    assert_eq!(r("show system alarms"), None, "a torqcli command is not a retired one");
    assert_eq!(r("-c show"), None);
}

fn main() {
    // `torq` and `torqcli` are one program: the shell (ADR-016).
    let args: Vec<String> = std::env::args().collect();
    if let Some(msg) = retired(&args[1..]) {
        eprintln!("{msg}");
        std::process::exit(2);
    }
    std::process::exit(shell::main(&args[1..]));
}

fn me() -> Result<String> {
    let out = Command::new("id").arg("-un").output().context("running id -un")?;
    let user = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || user.is_empty() {
        bail!("cannot tell who you are (id -un failed)");
    }
    Ok(user)
}

fn search(repo: &Path, terms: &[String]) -> Result<()> {
    let st = Command::new("nix")
        .args(["search", "--inputs-from"])
        .arg(flake_ref(repo))
        .arg("nixpkgs")
        .args(terms)
        .status()
        .context("running nix search")?;
    if !st.success() {
        bail!("nix search failed");
    }
    Ok(())
}

/// The config repo as a flake. `path:`, not a plain path: the repo belongs to root, and git refuses
/// to read a repository owned by someone else.
fn flake_ref(dir: &Path) -> String {
    format!("path:{}", dir.display())
}

/// A private (0700), unpredictable scratch directory: the broker, as root, reads the diff from it.
/// In XDG_RUNTIME_DIR (the user's own) if there is one, else /tmp; TMPDIR is not consulted.
fn scratch_dir() -> Result<tempfile::TempDir> {
    let base = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"));
    tempfile::Builder::new().prefix("torq-").tempdir_in(base).context("creating a scratch directory")
}

/// Whether running the broker would stop to ask for a password: it runs through sudo, and sudo
/// cannot run it non-interactively (`sudo -n`). Checked against the broker itself, not `sudo -v`,
/// which follows the rules for *all* commands and could ask when the broker does not need it.
fn asks_for_password(cli: &Cli) -> bool {
    let words: Vec<&str> = cli.broker.split_whitespace().collect();
    match words.split_first() {
        Some((first, rest)) if *first == "sudo" || first.ends_with("/sudo") => !Command::new(first)
            .arg("-n")
            .args(rest)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success()),
        _ => false,
    }
}

/// The machine's owner (`torq.user`) in the configuration at `copy`: the one user the catalog's SSH
/// admits.
fn owner(copy: &Path, host: &str) -> Result<String> {
    let out = Command::new("nix")
        .args(["eval", "--raw"])
        .arg(format!("{}#nixosConfigurations.{host}.config.torq.user", flake_ref(copy)))
        .output()
        .context("running nix eval")?;
    if !out.status.success() {
        bail!("cannot read this machine's owner: {}", last_error(&out.stderr));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Who will be able to log in as the owner over SSH under the configuration being approved (`copy`,
/// already changed): the files sshd reads (`services.openssh.authorizedKeysFiles`) and the keys
/// declared in the config (`users.users.<owner>.openssh.authorizedKeys`, which NixOS writes to
/// /etc/ssh/authorized_keys.d/<owner> — taken from the config, not from what is running now).
/// One description per key line, as `ssh-keygen -l` shows it, so the person approving sees exactly
/// whose keys will get in.
fn owner_keys(copy: &Path, host: &str, owner: &str) -> Result<Vec<(String, String, String)>> {
    if !pkg::valid_user(owner) {
        bail!("'{owner}' is not a valid user name");
    }
    let out = Command::new("nix")
        .args(["eval", "--json"])
        .arg(format!("{}#nixosConfigurations.{host}.config", flake_ref(copy)))
        .arg("--apply")
        .arg(format!(
            "c: let k = c.users.users.\"{owner}\".openssh.authorizedKeys; in \
             {{ files = c.services.openssh.authorizedKeysFiles; declared = k.keys ++ map builtins.readFile k.keyFiles; }}"
        ))
        .output()
        .context("running nix eval")?;
    if !out.status.success() {
        bail!("cannot read the SSH settings: {}", last_error(&out.stderr));
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let out = Command::new("getent").args(["passwd", owner]).output().context("running getent")?;
    let entry = String::from_utf8_lossy(&out.stdout);
    let fields: Vec<&str> = entry.trim().split(':').collect();
    let (uid, home) = match fields.as_slice() {
        [_, _, uid, _, _, home, ..] if !home.is_empty() => (*uid, *home),
        _ => bail!("no user '{owner}' on this machine"),
    };

    let mut sources: Vec<(String, String)> = Vec::new(); // (where, text)
    for pattern in v["files"].as_array().into_iter().flatten().filter_map(|f| f.as_str()) {
        // Exactly the file NixOS writes the declared keys to: take those from the config instead.
        if pattern == "/etc/ssh/authorized_keys.d/%u" {
            let declared = v["declared"].as_array().into_iter().flatten().filter_map(|k| k.as_str());
            sources.push(("declared in the config".into(), declared.collect::<Vec<_>>().join("\n")));
            continue;
        }
        let file = pkg::expand_keys_pattern(pattern, home, owner, uid)?;
        match fs::read_to_string(&file) {
            Ok(t) => sources.push((file, t)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => bail!("cannot read {file}: {e} (run this as {owner})"),
        }
    }
    let mut keys = Vec::new();
    for (place, text) in sources {
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            let mut c = Command::new("ssh-keygen");
            c.args(["-l", "-f", "-"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
            let mut child = c.spawn().context("running ssh-keygen")?;
            child.stdin.take().unwrap().write_all(line.as_bytes())?;
            let out = child.wait_with_output()?;
            if out.status.success() {
                let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
                keys.push((pkg::key_options_note(line), id, place.clone()));
            }
        }
    }
    Ok(keys)
}

/// The ledger, read through the broker (it is root-only).
fn read_ledger(cli: &Cli) -> Result<Vec<history::Entry>> {
    let out = broker(cli, &["log", "--json", "--brief"])
        .stderr(Stdio::inherit())
        .output()
        .context("running the broker")?;
    if !out.status.success() {
        bail!("could not read the ledger (see above)");
    }
    history::parse(&String::from_utf8_lossy(&out.stdout))
}

/// The broker. Its repo is never passed: the real broker (as root) manages the machine's config
/// repo and nothing else, and `change` has refused to read a different one with it.
fn broker(cli: &Cli, args: &[&str]) -> Command {
    let mut words = cli.broker.split_whitespace();
    let mut c = Command::new(words.next().unwrap_or("torq-broker"));
    c.args(words).args(args);
    c
}

/// A repo override only makes sense with a test broker: with the real one (anything run through
/// sudo, however it is spelled), a stray TORQ_CONFIG_REPO would mean evaluating one repo while
/// root changes another.
fn check_repo(cli: &Cli) -> Result<()> {
    let real_broker = cli.broker == BROKER || cli.broker.split_whitespace().any(|w| w == "sudo" || w.ends_with("/sudo"));
    if real_broker && cli.repo != Path::new(REPO) {
        bail!(
            "the config repo is set to {} (TORQ_CONFIG_REPO?), but the broker only manages {REPO}; \
             unset it",
            cli.repo.display()
        );
    }
    Ok(())
}

/// The configuration to evaluate: always `torq`, the one the broker builds (never the host name),
/// and the config repo must have it.
fn config_name(repo: &Path) -> Result<String> {
    let out = Command::new("nix")
        .args(["eval", "--json"])
        .arg(format!("{}#nixosConfigurations", flake_ref(repo)))
        .args(["--apply", "builtins.attrNames"])
        .output()
        .context("running nix eval")?;
    if !out.status.success() {
        bail!("cannot read the config repo: {}", last_error(&out.stderr));
    }
    let names: Vec<String> = serde_json::from_slice(&out.stdout)?;
    // The broker always builds `torq` (torq.broker.configName): checking any other configuration
    // here would approve something it never builds.
    if names.iter().any(|n| n == CONFIG) {
        Ok(CONFIG.to_string())
    } else {
        bail!(
            "{} has no configuration named `{CONFIG}` (it has {names:?}); the broker builds only `{CONFIG}`",
            repo.display()
        )
    }
}

/// Evaluate the whole system from the changed scratch copy: the same modules that will build it
/// report unknown, unfree or unsupported packages, or a broken update, before anything is proposed.
fn evaluate(copy: &Path, host: &str, log: &Path) -> Result<()> {
    let out = Command::new("nix")
        .args(["eval", "--raw"])
        .arg(format!("{}#nixosConfigurations.{host}.config.system.build.toplevel.drvPath", flake_ref(copy)))
        .output()
        .context("running nix eval")?;
    if !out.status.success() {
        // The whole of nix's explanation goes to the log; the screen gets one clear sentence.
        let _ = fs::OpenOptions::new().append(true).open(log).and_then(|mut f| f.write_all(&out.stderr));
        bail!("{}", pkg::explain_check_error(&history::one_line(&last_error(&out.stderr), 300)));
    }
    Ok(())
}

/// The most useful line of a nix error — ours if there is one, else nix's last `error:` — as one
/// clean line: it can quote text from the config repo, so it never reaches the terminal raw.
fn last_error(stderr: &[u8]) -> String {
    let s = String::from_utf8_lossy(stderr);
    let lines: Vec<&str> = s.lines().map(str::trim).collect();
    let line = lines
        .iter()
        .find(|l| l.contains("torq.packages:"))
        .or_else(|| lines.iter().rev().find(|l| l.starts_with("error:")))
        .map(|l| l.trim_start_matches("error:").trim().to_string())
        .unwrap_or_else(|| s.trim().to_string());
    history::one_line(&line, 300)
}


/// Whether the configuration at `copy` reads the tree's files (its flake passes `configDir` to
/// torq.lib.mkSystem): without that, a change to them would commit and do nothing.
fn reads_tree(copy: &Path, host: &str) -> Result<bool> {
    let out = Command::new("nix")
        .args(["eval", "--json"])
        .arg(format!("{}#nixosConfigurations.{host}.config.torq", flake_ref(copy)))
        .args(["--apply", "k: (k.packages.configDir or null) != null"])
        .output()
        .context("running nix eval")?;
    if !out.status.success() {
        bail!("cannot read the config repo: {}", last_error(&out.stderr));
    }
    Ok(serde_json::from_slice(&out.stdout)?)
}
