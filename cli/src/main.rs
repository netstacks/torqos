//! `kestrel`: the unprivileged command line. It never changes the system itself: it writes a
//! proposed change as a diff and hands it to `kestrel-broker`, which ledgers, applies and can undo it.

mod pkg;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use pkg::{Edit, Target};
use std::fs;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Parser)]
#[command(name = "kestrel", version, about = "Kestrel: change this machine through the broker")]
struct Cli {
    /// The machine's config repo.
    #[arg(long, env = "KESTREL_CONFIG_REPO", default_value = "/var/lib/kestrel/config")]
    repo: PathBuf,
    /// How to run the broker (it needs root).
    #[arg(long, env = "KESTREL_BROKER", default_value = "sudo kestrel-broker")]
    broker: String,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Installed software.
    #[command(subcommand)]
    Pkg(PkgCmd),
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
        /// Install for every user (system tier: always needs explicit approval).
        #[arg(long)]
        system: bool,
        /// Apply without asking.
        #[arg(long, short)]
        yes: bool,
    },
    /// Remove packages.
    Remove {
        #[arg(required = true)]
        names: Vec<String>,
        #[arg(long)]
        system: bool,
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

fn read_list(repo: &Path, target: &Target) -> Result<Option<Vec<String>>> {
    let p = repo.join(target.path());
    match fs::read_to_string(&p) {
        Ok(t) => Ok(Some(pkg::parse_list(&t).with_context(|| p.display().to_string())?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| p.display().to_string()),
    }
}

fn list(repo: &Path) -> Result<()> {
    let user = me()?;
    for target in [Target::System, Target::User(user)] {
        let l = read_list(repo, &target)?.unwrap_or_default();
        println!("{} ({}):", target.describe(), target.path());
        if l.is_empty() {
            println!("  (none)");
        }
        for n in l {
            println!("  {n}");
        }
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

fn change(cli: &Cli, names: &[String], system: bool, yes: bool, how: Edit) -> Result<()> {
    let user = me()?;
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
    let (new, changed) = pkg::edit(old.as_deref().unwrap_or_default(), names, how);
    let verb = if how == Edit::Add { "Install" } else { "Remove" };
    if changed.is_empty() {
        println!("nothing to do: {} {}", if how == Edit::Add { "already installed" } else { "not installed" }, target.describe());
        return Ok(());
    }

    let scratch = Scratch::new()?;
    let new_text = pkg::render(&new);
    let host = config_name(&cli.repo)?;
    eprintln!("checking the change against this machine's configuration ({host})...");
    check(&cli.repo, &scratch.0, &target, &new_text, &host)?;

    let diff = make_diff(&scratch.0, &target, old.as_ref().map(|l| pkg::render(l)), &new_text)?;
    let diff_file = scratch.0.join("change.diff");
    fs::write(&diff_file, &diff)?;

    let reason = format!("{verb} {} {}", changed.join(", "), target.describe());
    let out = broker(cli, &["propose", "--diff", &diff_file.to_string_lossy(), "--reason", &reason, "--actor", "user"])
        .stdout(Stdio::piped())
        .output()
        .context("running the broker")?;
    let shown = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        bail!("the broker refused the proposal");
    }
    let seq = pkg::proposal_seq(&shown).context("could not read the proposal number from the broker")?;
    // The broker's own "approve with: ..." hint does not apply: we ask, then apply.
    for line in shown.lines().filter(|l| !l.starts_with("approve with:")) {
        println!("{line}");
    }

    if !yes && !confirm(&format!("{reason}?"))? {
        println!("not applied; proposal #{seq} stays in the ledger unapplied.");
        return Ok(());
    }
    let st = broker(cli, &["apply", &seq.to_string(), "--approved-by", &user])
        .status()
        .context("running the broker")?;
    if !st.success() {
        bail!("apply of proposal #{seq} failed (see above; nothing was left half-applied)");
    }
    Ok(())
}

fn broker(cli: &Cli, args: &[&str]) -> Command {
    let mut words = cli.broker.split_whitespace();
    let mut c = Command::new(words.next().unwrap_or("kestrel-broker"));
    c.args(words).args(args);
    c
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

/// Evaluate the whole system with the new list, in a scratch copy of the repo: the same module
/// that will build it reports unknown, unfree or unsupported packages now, before anything is
/// proposed. Also refuses if this machine's config does not read package lists at all.
fn check(repo: &Path, scratch: &Path, target: &Target, new_text: &str, host: &str) -> Result<()> {
    let copy = scratch.join("config");
    copy_tree(repo, &copy)?;
    let file = copy.join(target.path());
    fs::create_dir_all(file.parent().unwrap())?;
    fs::write(&file, new_text)?;
    let out = Command::new("nix")
        .args(["eval", "--json"])
        .arg(format!("{}#nixosConfigurations.{host}.config", flake_ref(&copy)))
        .args([
            "--apply",
            "c: { drv = c.system.build.toplevel.drvPath; reads = c.kestrel.packages.configDir or null != null; }",
        ])
        .output()
        .context("running nix eval")?;
    if !out.status.success() {
        bail!("this change would not build: {}", last_error(&out.stderr));
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    if v["reads"] != true {
        bail!(
            "this machine's config does not read package lists yet: its flake.nix must call \
             kestrel.lib.mkSystem with `configDir = ./.;` (a system change)"
        );
    }
    Ok(())
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

fn make_diff(scratch: &Path, target: &Target, old: Option<String>, new: &str) -> Result<String> {
    let (a, a_label) = match &old {
        Some(text) => {
            let f = scratch.join("old");
            fs::write(&f, text)?;
            (f, format!("a/{}", target.path()))
        }
        None => (PathBuf::from("/dev/null"), "/dev/null".to_string()),
    };
    let b = scratch.join("new");
    fs::write(&b, new)?;
    let out = Command::new("diff")
        .arg("-u")
        .args(["--label", &a_label, "--label", &format!("b/{}", target.path())])
        .arg(&a)
        .arg(&b)
        .output()
        .context("running diff")?;
    // diff exits 1 when the files differ, which is the point.
    if out.status.code() != Some(1) {
        bail!("diff failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    // An unchanged old file that was not rewritten in our format still diffs as a reformat; fine.
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to)?;
    for e in fs::read_dir(from).with_context(|| from.display().to_string())? {
        let e = e?;
        if e.file_name() == ".git" {
            continue;
        }
        let (src, dst) = (e.path(), to.join(e.file_name()));
        let t = e.file_type()?;
        if t.is_dir() {
            copy_tree(&src, &dst)?;
        } else if t.is_file() {
            fs::copy(&src, &dst).with_context(|| src.display().to_string())?;
        }
    }
    Ok(())
}

fn confirm(question: &str) -> Result<bool> {
    if !std::io::stdin().is_terminal() {
        bail!("not applied: no terminal to ask on (use --yes to apply without asking)");
    }
    print!("{question} [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
}

/// A private scratch directory, removed when done.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Result<Self> {
        let base = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let dir = base.join(format!("kestrel-pkg-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir)?;
        Ok(Scratch(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
