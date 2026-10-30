//! `kestrel`: the unprivileged command line. It never changes the system itself: it writes a
//! proposed change as a diff and hands it to `kestrel-broker`, which ledgers, applies and can undo it.

mod history;
mod pkg;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use pkg::{Edit, Target};
use std::fs;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const REPO: &str = "/var/lib/kestrel/config";
const BROKER: &str = "sudo kestrel-broker";

#[derive(Parser)]
#[command(name = "kestrel", version, about = "Kestrel: change this machine through the broker")]
struct Cli {
    /// The machine's config repo. Only for testing, together with --broker: the real broker
    /// manages /var/lib/kestrel/config and nothing else.
    #[arg(long, env = "KESTREL_CONFIG_REPO", default_value = REPO)]
    repo: PathBuf,
    /// How to run the broker (it needs root).
    #[arg(long, env = "KESTREL_BROKER", default_value = BROKER)]
    broker: String,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Installed software.
    #[command(subcommand)]
    Pkg(PkgCmd),
    /// Move this machine to the newest Kestrel, and so its nixpkgs (a system change: always asks).
    Update {
        /// Update every input of the machine's config flake, not just Kestrel.
        #[arg(long)]
        all: bool,
    },
    /// What changed on this machine, and how to undo it.
    Log {
        /// How many entries to show (newest last).
        #[arg(long, short, default_value_t = 20)]
        n: usize,
    },
    /// Undo a change (the number `kestrel log` shows); without one, the latest change.
    Undo {
        seq: Option<u64>,
        /// Undo your own (user-tier) change without asking; system changes always ask.
        #[arg(long, short)]
        yes: bool,
    },
    /// Is this machine as its ledger says? (exits non-zero if not)
    Status,
}

#[derive(Subcommand)]
enum PkgCmd {
    /// Show installed packages (system-wide and yours).
    List,
    /// Search this machine's nixpkgs.
    Search {
        #[arg(required = true)]
        terms: Vec<String>,
    },
    /// Install packages for you (or system-wide with --system).
    Add {
        #[arg(required = true)]
        names: Vec<String>,
        /// Install for every user (a system-tier change: always asks, --yes is refused).
        #[arg(long)]
        system: bool,
        /// Apply your own (user-tier) change without asking.
        #[arg(long, short)]
        yes: bool,
    },
    /// Remove packages.
    Remove {
        #[arg(required = true)]
        names: Vec<String>,
        /// Remove for every user (a system-tier change: always asks, --yes is refused).
        #[arg(long)]
        system: bool,
        /// Apply your own (user-tier) change without asking.
        #[arg(long, short)]
        yes: bool,
    },
}

fn main() {
    let cli = Cli::parse();
    let r = match &cli.cmd {
        Cmd::Pkg(PkgCmd::List) => list(&cli.repo),
        Cmd::Pkg(PkgCmd::Search { terms }) => search(&cli.repo, terms),
        Cmd::Pkg(PkgCmd::Add { names, system, yes }) => change(&cli, names, *system, *yes, Edit::Add),
        Cmd::Pkg(PkgCmd::Remove { names, system, yes }) => change(&cli, names, *system, *yes, Edit::Remove),
        Cmd::Update { all } => update(&cli, *all),
        Cmd::Log { n } => read_ledger(&cli).map(|l| print!("{}", history::render(&l, *n))),
        Cmd::Undo { seq, yes } => undo(&cli, *seq, *yes),
        Cmd::Status => {
            // The broker's own report and exit code (75: busy, not checked).
            let st = broker(&cli, &["status"]).status();
            std::process::exit(st.ok().and_then(|s| s.code()).unwrap_or(1));
        }
    };
    if let Err(e) = r {
        eprintln!("kestrel: {e:#}");
        std::process::exit(1);
    }
}

fn me() -> Result<String> {
    let out = Command::new("id").arg("-un").output().context("running id -un")?;
    let user = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || user.is_empty() {
        bail!("cannot tell who you are (id -un failed)");
    }
    Ok(user)
}

/// A package list as it is on disk: its exact text (a diff must start from that, whatever its
/// formatting) and its parsed names. None if the file does not exist yet.
struct ListFile {
    text: String,
    names: Vec<String>,
}

fn read_list(repo: &Path, target: &Target) -> Result<Option<ListFile>> {
    let p = repo.join(target.path());
    match fs::read_to_string(&p) {
        Ok(text) => {
            let names = pkg::parse_list(&text).with_context(|| p.display().to_string())?;
            Ok(Some(ListFile { text, names }))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| p.display().to_string()),
    }
}

fn list(repo: &Path) -> Result<()> {
    let user = me()?;
    let mut failed = false;
    for target in [Target::System, Target::User(user)] {
        println!("{} ({}):", target.describe(), target.path());
        match read_list(repo, &target) {
            Ok(f) => {
                let names = f.map(|f| f.names).unwrap_or_default();
                if names.is_empty() {
                    println!("  (none)");
                }
                for n in names {
                    println!("  {n}");
                }
            }
            Err(e) => {
                println!("  (unreadable: {e:#})");
                failed = true;
            }
        }
    }
    if failed {
        bail!("a package list could not be read");
    }
    Ok(())
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

/// Decide how approval happens before proposing anything, so a refusal leaves no proposal behind.
/// The CLI cannot enforce who approves (anything running as you can run it; KNOWN-GAPS #4) — but it
/// will not make a system-tier change without a person answering a prompt.
fn approval(system: bool, yes: bool) -> Result<()> {
    if system && yes {
        bail!("system changes are always confirmed at the prompt; run again without --yes");
    }
    if !yes && !std::io::stdin().is_terminal() {
        if system {
            bail!("nothing proposed: a system change is confirmed at a prompt, and there is no terminal");
        }
        bail!("nothing proposed: there is no terminal to ask on (--yes applies a change to your own packages)");
    }
    Ok(())
}

/// A private (0700), unpredictable scratch directory: the broker, as root, reads the diff from it.
/// In XDG_RUNTIME_DIR (the user's own) if there is one, else /tmp; TMPDIR is not consulted.
fn scratch_dir() -> Result<tempfile::TempDir> {
    let base = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"));
    tempfile::Builder::new().prefix("kestrel-").tempdir_in(base).context("creating a scratch directory")
}

/// One change to one file of the config repo, already made in a scratch copy of it (`copy`).
struct Change<'a> {
    copy: &'a Path,
    /// Repo-relative path of the changed file.
    path: &'a str,
    /// Its exact current text (None: the file is new) — a diff must start from that.
    old: Option<&'a str>,
    new: &'a str,
    reason: String,
}

/// Evaluate the changed copy, then propose the diff, ask (unless `yes`) and apply it.
fn submit(cli: &Cli, scratch: &Path, host: &str, c: Change, yes: bool) -> Result<()> {
    eprintln!("checking the change against this machine's configuration ({host})...");
    evaluate(c.copy, host)?;
    let diff = make_diff(scratch, c.path, c.old, c.new)?;
    let diff_file = scratch.join("change.diff");
    fs::write(&diff_file, &diff)?;

    let out = broker(cli, &["propose", "--diff", &diff_file.to_string_lossy(), "--reason", &c.reason, "--actor", "user"])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit()) // the broker's (or sudo's) own reason, if it refuses
        .output()
        .context("running the broker")?;
    let shown = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        bail!("the broker refused the proposal (see above)");
    }
    let seq = pkg::proposal_seq(&shown).context("could not read the proposal number from the broker")?;
    // The broker's own "approve with: ..." hint does not apply: we ask, then apply.
    for line in shown.lines().filter(|l| !l.starts_with("approve with:")) {
        println!("{line}");
    }
    if !yes && !confirm(&format!("{}?", c.reason))? {
        println!("not applied; proposal #{seq} stays in the ledger unapplied.");
        return Ok(());
    }
    let st = broker(cli, &["apply", &seq.to_string(), "--approved-by", &me()?])
        .status()
        .context("running the broker")?;
    if !st.success() {
        bail!("apply of proposal #{seq} failed (see above; nothing was left half-applied)");
    }
    Ok(())
}

fn change(cli: &Cli, names: &[String], system: bool, yes: bool, how: Edit) -> Result<()> {
    check_repo(cli)?;
    let user = me()?;
    approval(system, yes)?;
    let target = if system {
        Target::System
    } else {
        if !pkg::valid_user(&user) {
            bail!("'{user}' is not a user name Kestrel can keep a package list for");
        }
        Target::User(user.clone())
    };
    for n in names {
        if !pkg::valid_package(n) {
            bail!("'{n}' is not a package name (a nixpkgs attribute, e.g. htop or python3Packages.requests)");
        }
    }
    let old = read_list(&cli.repo, &target)?;
    let (new, changed) = pkg::edit(old.as_ref().map(|f| f.names.as_slice()).unwrap_or_default(), names, how);
    let verb = if how == Edit::Add { "Install" } else { "Remove" };
    if changed.is_empty() {
        println!("nothing to do: {} {}", if how == Edit::Add { "already installed" } else { "not installed" }, target.describe());
        return Ok(());
    }

    let scratch = scratch_dir()?;
    let scratch = scratch.path();
    let host = config_name(&cli.repo)?;
    let copy = scratch.join("config");
    copy_tree(&cli.repo, &copy)?;
    if !reads_package_lists(&copy, &host)? {
        bail!(
            "this machine's config does not read package lists yet: its flake.nix must call \
             kestrel.lib.mkSystem with `configDir = ./.;` (a system change)"
        );
    }
    let path = target.path();
    let new_text = pkg::render(&new);
    let file = copy.join(&path);
    fs::create_dir_all(file.parent().unwrap())?;
    fs::write(&file, &new_text)?;
    let change = Change {
        copy: &copy,
        path: &path,
        old: old.as_ref().map(|f| f.text.as_str()),
        new: &new_text,
        reason: format!("{verb} {} {}", changed.join(", "), target.describe()),
    };
    submit(cli, scratch, &host, change, yes)
}

/// The ledger, read through the broker (it is root-only).
fn read_ledger(cli: &Cli) -> Result<Vec<history::Entry>> {
    let out = broker(cli, &["log", "--json"])
        .stderr(Stdio::inherit())
        .output()
        .context("running the broker")?;
    if !out.status.success() {
        bail!("could not read the ledger (see above)");
    }
    history::parse(&String::from_utf8_lossy(&out.stdout))
}

/// `kestrel undo [n]`: roll back an applied change through the broker, after saying what it was.
fn undo(cli: &Cli, seq: Option<u64>, yes: bool) -> Result<()> {
    let entries = read_ledger(cli)?;
    let target = history::undo_target(&entries, seq)?;
    let files = target.data["files"].as_array().map(|f| {
        f.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(", ")
    });
    // Say what it is before anything else, refusals included.
    println!(
        "#{}  {}  ({} tier{})",
        target.seq,
        target.reason,
        target.tier,
        files.map(|f| format!("; {f}")).unwrap_or_default()
    );
    approval(target.tier != "user", yes)?;
    if !yes && !confirm(&format!("Undo \"{}\"?", target.reason))? {
        println!("not undone");
        return Ok(());
    }
    let st = broker(cli, &["rollback", &target.seq.to_string(), "--approved-by", &me()?])
        .status()
        .context("running the broker")?;
    if !st.success() {
        bail!("undo of #{} failed (see above)", target.seq);
    }
    Ok(())
}

/// `kestrel update`: move this machine to the newest Kestrel (and so nixpkgs, which follows it), as
/// one ledgered, undoable change to flake.lock (ADR-004). `all`: every input of the config flake.
fn update(cli: &Cli, all: bool) -> Result<()> {
    check_repo(cli)?;
    approval(true, false)?;
    let scratch = scratch_dir()?;
    let scratch = scratch.path();
    let host = config_name(&cli.repo)?;
    let copy = scratch.join("config");
    copy_tree(&cli.repo, &copy)?;
    let old = fs::read_to_string(copy.join("flake.lock")).context("reading flake.lock")?;
    eprintln!("fetching the newest inputs...");
    let mut cmd = Command::new("nix");
    cmd.args(["flake", "update", "--flake"]).arg(flake_ref(&copy));
    if !all {
        cmd.arg("kestrel");
    }
    let out = cmd.output().context("running nix flake update")?;
    if !out.status.success() {
        bail!("nix flake update failed: {}", last_error(&out.stderr));
    }
    let new = fs::read_to_string(copy.join("flake.lock")).context("reading the updated flake.lock")?;
    if new == old {
        println!("already up to date");
        return Ok(());
    }
    let moved = pkg::moved_inputs(&String::from_utf8_lossy(&out.stderr));
    eprint!("{}", String::from_utf8_lossy(&out.stderr));
    let change = Change {
        copy: &copy,
        path: "flake.lock",
        old: Some(&old),
        new: &new,
        reason: if moved.is_empty() { "Update".to_string() } else { format!("Update {}", moved.join(", ")) },
    };
    submit(cli, scratch, &host, change, false)
}

/// The broker. Its repo is never passed: the real broker (as root) manages the machine's config
/// repo and nothing else, and `change` has refused to read a different one with it.
fn broker(cli: &Cli, args: &[&str]) -> Command {
    let mut words = cli.broker.split_whitespace();
    let mut c = Command::new(words.next().unwrap_or("kestrel-broker"));
    c.args(words).args(args);
    c
}

/// A repo override only makes sense with a test broker: with the real one (anything run through
/// sudo, however it is spelled), a stray KESTREL_CONFIG_REPO would mean evaluating one repo while
/// root changes another.
fn check_repo(cli: &Cli) -> Result<()> {
    let real_broker = cli.broker == BROKER || cli.broker.split_whitespace().any(|w| w == "sudo" || w.ends_with("/sudo"));
    if real_broker && cli.repo != Path::new(REPO) {
        bail!(
            "the config repo is set to {} (KESTREL_CONFIG_REPO?), but the broker only manages {REPO}; \
             unset it",
            cli.repo.display()
        );
    }
    Ok(())
}

/// The single nixosConfiguration in the config repo. (Not the hostname: a machine whose config was
/// switched away from Kestrel may have another one.)
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
    match names.as_slice() {
        [one] => Ok(one.clone()),
        _ => bail!("expected one nixosConfiguration in {}, found {names:?}", repo.display()),
    }
}

/// Evaluate the whole system from the changed scratch copy: the same modules that will build it
/// report unknown, unfree or unsupported packages, or a broken update, before anything is proposed.
fn evaluate(copy: &Path, host: &str) -> Result<()> {
    let out = Command::new("nix")
        .args(["eval", "--raw"])
        .arg(format!("{}#nixosConfigurations.{host}.config.system.build.toplevel.drvPath", flake_ref(copy)))
        .output()
        .context("running nix eval")?;
    if !out.status.success() {
        bail!("this change would not build: {}", last_error(&out.stderr));
    }
    Ok(())
}

/// Whether this machine's config reads package lists at all (`configDir`); if not, changing them
/// would do nothing.
fn reads_package_lists(copy: &Path, host: &str) -> Result<bool> {
    let out = Command::new("nix")
        .args(["eval", "--json"])
        .arg(format!("{}#nixosConfigurations.{host}.config.kestrel", flake_ref(copy)))
        .args(["--apply", "k: (k.packages.configDir or null) != null"])
        .output()
        .context("running nix eval")?;
    if !out.status.success() {
        bail!("cannot read the config repo: {}", last_error(&out.stderr));
    }
    Ok(serde_json::from_slice(&out.stdout)?)
}

/// The most useful line of a nix error: ours if there is one, else nix's last `error:`.
fn last_error(stderr: &[u8]) -> String {
    let s = String::from_utf8_lossy(stderr);
    let lines: Vec<&str> = s.lines().map(str::trim).collect();
    lines
        .iter()
        .find(|l| l.contains("kestrel.packages:"))
        .or_else(|| lines.iter().rev().find(|l| l.starts_with("error:")))
        .map(|l| l.trim_start_matches("error:").trim().to_string())
        .unwrap_or_else(|| s.trim().to_string())
}

/// `old` is the file's exact current text — not a re-rendering of it — or the diff would not apply
/// to a hand-formatted list. `new` is the canonical rendering that was evaluated.
fn make_diff(scratch: &Path, path: &str, old: Option<&str>, new: &str) -> Result<String> {
    let (a, a_label) = match old {
        Some(text) => {
            let f = scratch.join("old");
            fs::write(&f, text)?;
            (f, format!("a/{path}"))
        }
        None => (PathBuf::from("/dev/null"), "/dev/null".to_string()),
    };
    let b = scratch.join("new");
    fs::write(&b, new)?;
    let out = Command::new("diff")
        .arg("-u")
        .args(["--label", &a_label, "--label", &format!("b/{path}")])
        .arg(&a)
        .arg(&b)
        .output()
        .context("running diff")?;
    // diff exits 1 when the files differ, which is the point.
    if out.status.code() != Some(1) {
        bail!("diff failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Copy the config repo (minus .git) for the pre-check. Symlinks are copied as symlinks, never
/// followed, so the copy evaluates exactly like the original.
fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to)?;
    for e in fs::read_dir(from).with_context(|| from.display().to_string())? {
        let e = e?;
        if e.file_name() == ".git" {
            continue;
        }
        let (src, dst) = (e.path(), to.join(e.file_name()));
        let t = e.file_type()?;
        if t.is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(&src)?, &dst)?;
        } else if t.is_dir() {
            copy_tree(&src, &dst)?;
        } else if t.is_file() {
            fs::copy(&src, &dst).with_context(|| src.display().to_string())?;
        }
    }
    Ok(())
}

/// Ask on the terminal (`change` has already refused when there is none).
fn confirm(question: &str) -> Result<bool> {
    print!("{question} [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
}
