//! `torq`: the unprivileged command line. It never changes the system itself: it writes a
//! proposed change as a diff and hands it to `torq-broker`, which ledgers, applies and can undo it.

mod history;
mod pkg;
mod run;
mod status;
mod ui;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use pkg::{Edit, Target};
use std::fs;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const REPO: &str = "/var/lib/torq/config";
const BROKER: &str = "sudo torq-broker";

#[derive(Parser)]
#[command(name = "torq", version, about = "TorqOS: change this machine through the broker")]
struct Cli {
    /// The machine's config repo. Only for testing, together with --broker: the real broker
    /// manages /var/lib/torq/config and nothing else.
    #[arg(long, env = "TORQ_CONFIG_REPO", default_value = REPO)]
    repo: PathBuf,
    /// How to run the broker (it needs root).
    #[arg(long, env = "TORQ_BROKER", default_value = BROKER)]
    broker: String,
    /// Also show the raw change: the diff the broker records in the ledger.
    #[arg(long, global = true)]
    diff: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Installed software.
    #[command(subcommand)]
    Pkg(PkgCmd),
    /// Services (SSH, containers, VPN, ...) from TorqOS's catalog; always system changes.
    #[command(subcommand)]
    Service(ServiceCmd),
    /// Move this machine to the newest TorqOS, and so its nixpkgs (a system change: always asks).
    Update {
        /// Update every input of the machine's config flake, not just TorqOS.
        #[arg(long)]
        all: bool,
    },
    /// What changed on this machine, and how to undo it; with a number, that change in detail.
    Log {
        /// One change in detail.
        change: Option<u64>,
        /// How many changes to show (newest last).
        #[arg(long, short, default_value_t = 20)]
        n: usize,
    },
    /// Undo a change (the number `torq log` shows); without one, the latest change not yet
    /// undone. Undo is not redo: undoing again goes further back; to redo, make the change again.
    Undo {
        seq: Option<u64>,
        /// Undo a change to your own packages without asking; anything else always asks.
        #[arg(long, short)]
        yes: bool,
    },
    /// Is this machine as its history says? (exits 1 if not, 75 while a change is being applied)
    Status {
        /// Return the machine to its history if it drifted (asks first; never automatic).
        #[arg(long)]
        repair: bool,
    },
}

#[derive(Subcommand)]
enum ServiceCmd {
    /// The catalog, and what is on.
    List,
    /// Turn services on.
    Enable {
        #[arg(required = true)]
        names: Vec<String>,
    },
    /// Turn services off.
    Disable {
        #[arg(required = true)]
        names: Vec<String>,
    },
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
        Cmd::Service(ServiceCmd::List) => service_list(&cli),
        Cmd::Service(ServiceCmd::Enable { names }) => service_change(&cli, names, Edit::Add),
        Cmd::Service(ServiceCmd::Disable { names }) => service_change(&cli, names, Edit::Remove),
        Cmd::Update { all } => update(&cli, *all),
        Cmd::Log { change, n } => read_ledger(&cli).and_then(|l| {
            let (ui, clock) = (ui::Ui::detect(), history::Clock::system());
            match change {
                Some(seq) => print!("{}", history::render_one(&l, *seq, &ui, &clock)?),
                None => print!("{}", history::render(&l, *n, &ui, &clock)),
            }
            Ok(())
        }),
        Cmd::Undo { seq, yes } => undo(&cli, *seq, *yes),
        Cmd::Status { repair } => status_cmd(&cli, *repair),
    };
    if let Err(e) = r {
        // One clean line, whatever the error carried (paths, quoted config text, io errors).
        eprintln!("{}", ui::Ui::detect().fail(&history::one_line(&format!("{e:#}"), 300)));
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

fn read_list(repo: &Path, path: &str) -> Result<Option<ListFile>> {
    let p = repo.join(path);
    match fs::read_to_string(&p) {
        Ok(text) => {
            let names = pkg::parse_list(&text).with_context(|| p.display().to_string())?;
            Ok(Some(ListFile { text, names }))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| p.display().to_string()),
    }
}

/// `torq pkg list`: your apps and everyone's, with versions and descriptions.
fn list(repo: &Path) -> Result<()> {
    let ui = ui::Ui::detect();
    let user = me()?;
    let host = config_name(repo).ok();
    let mut failed = false;
    for (title, target) in [("Yours", Target::User(user)), ("Everyone", Target::System)] {
        println!("  {}", ui.bold(title));
        match read_list(repo, &target.path()) {
            Ok(f) => {
                let names = f.map(|f| f.names).unwrap_or_default();
                if names.is_empty() {
                    println!("    {}", ui.dim("(nothing extra)"));
                }
                let facts = match &host {
                    Some(h) if !names.is_empty() => {
                        let _p = run::Progress::start("Reading");
                        package_facts(repo, h, &names)
                    }
                    _ => vec![(String::new(), String::new()); names.len()],
                };
                for (n, (version, description)) in names.iter().zip(facts) {
                    println!("{}", ui::package_row(&ui, &history::one_line(n, 40), &version, &description));
                }
            }
            Err(e) => {
                println!("    {}", ui.fail(&format!("unreadable: {}", history::one_line(&format!("{e:#}"), 60))));
                failed = true;
            }
        }
        println!();
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
    tempfile::Builder::new().prefix("torq-").tempdir_in(base).context("creating a scratch directory")
}

/// What a change looks like to the person approving it (docs/CLI-DESIGN.md): in their terms, not
/// the diff the broker records (that is behind --diff).
struct Summary {
    /// "Install for you", "Turn on ssh", "Update this machine".
    title: String,
    /// The change, one row each: packages with version and description, what a service implies.
    rows: Vec<String>,
    /// A system change: marked in the heading, and the question defaults to No.
    system: bool,
    /// "Install?"
    question: String,
    /// The result line: "Installed htop".
    done: String,
    /// The change in words, for errors: "install htop".
    what: String,
}

/// One change to one file of the config repo, already made in a scratch copy of it (`copy`).
struct Change<'a> {
    copy: &'a Path,
    /// Repo-relative path of the changed file.
    path: &'a str,
    /// Its exact current text (None: the file is new) — a diff must start from that.
    old: Option<&'a str>,
    new: &'a str,
    /// The ledger's reason (what `torq log` shows).
    reason: String,
    summary: Summary,
}

/// Check the changed copy, show the change, ask (unless `yes`) — before anything is proposed, so
/// saying no leaves nothing in the ledger — then propose and apply it through the broker, quietly,
/// with its output in the log.
fn submit(cli: &Cli, scratch: &Path, host: &str, c: Change, yes: bool) -> Result<()> {
    let ui = ui::Ui::detect();
    let log = run::start_log()?;
    {
        let _p = run::Progress::start("Checking");
        evaluate(c.copy, host, &log)?;
    }
    let s = &c.summary;
    println!("\n{}", ui.heading(&s.title, s.system));
    for row in &s.rows {
        println!("{row}");
    }
    let diff = make_diff(scratch, c.path, c.old, c.new)?;
    if cli.diff {
        println!();
        for line in diff.lines() {
            println!("    {}", ui.dim(&history::clean(line)));
        }
    }
    println!();
    if !yes && !confirm(&ui, &s.question, !s.system)? {
        println!("  Nothing changed.");
        return Ok(());
    }
    let diff_file = scratch.join("change.diff");
    fs::write(&diff_file, &diff)?;
    let proposed = run::broker_json(
        broker(cli, &["propose", "--diff", &diff_file.to_string_lossy(), "--reason", &c.reason, "--actor", "user"]),
        &log,
    )?;
    let seq = match proposed {
        run::Outcome::Done(v) => {
            let seq = v["seq"].as_u64().context("the broker did not say which proposal it made")?;
            // What was shown and asked must be what the broker will apply.
            let shown = if s.system { "system" } else { "user" };
            if v["tier"].as_str() != Some(shown) {
                let tier = history::one_line(v["tier"].as_str().unwrap_or("?"), 20);
                println!("{}", ui.fail(&format!("Not applied: the broker treats this as a {tier} change, not a {shown} one.")));
                println!("    Nothing changed.");
                println!("    {}", ui.dim(&format!("(Your request is recorded as proposal {seq}, not applied.)")));
                std::process::exit(1);
            }
            seq
        }
        other => return fail(cli, &ui, &s.what, other, &log, Attempt::Nothing),
    };
    let applied = {
        // A spinner redraws its line every 100 ms and would wipe out a sudo password prompt: only
        // show one when the broker will run without asking.
        let _p = if asks_for_password(cli) { None } else { Some(run::Progress::start("Applying")) };
        run::broker_json(broker(cli, &["apply", &seq.to_string(), "--approved-by", &me()?]), &log)?
    };
    match applied {
        run::Outcome::Done(v) => {
            println!("{}", ui.ok(&s.done, &ui::change_ref(v["seq"].as_u64().unwrap_or(seq))));
            Ok(())
        }
        other => fail(cli, &ui, &s.what, other, &log, Attempt::Proposal(seq)),
    }
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

/// What a failed command had got as far as, so `fail` knows which ledger entry would show that it
/// happened anyway.
#[derive(Clone, Copy)]
enum Attempt {
    /// Nothing recorded yet (refused at propose).
    Nothing,
    /// This proposal was made; an `applied` entry for it means it happened.
    Proposal(u64),
    /// Undoing this applied change; a `rolled_back` entry for it means it happened.
    Undo(u64),
}

/// A change that did not go through: what went wrong, and — from the ledger, not assumed — what
/// actually happened: done after all, or nothing changed and the machine is as it was, or not
/// sure. Exits 1.
fn fail(cli: &Cli, ui: &ui::Ui, what: &str, outcome: run::Outcome, log: &Path, attempt: Attempt) -> Result<()> {
    let log_line = || println!("    {}", ui.dim(&format!("log: {}", log.display())));
    if let (run::Outcome::Busy, Attempt::Nothing | Attempt::Undo(_)) = (&outcome, attempt) {
        // Refused before taking the lock: nothing happened.
        println!("{}", ui.fail("Another change is being applied right now. Try again when it is done."));
        println!("    Nothing changed.");
        std::process::exit(1);
    }
    // The broker may have done it before failing (killed after ledgering, a late error): the
    // ledger says, so look before saying anything — and if it cannot be read, claim nothing.
    let landed = |l: &[history::Entry]| match attempt {
        Attempt::Nothing => None,
        Attempt::Proposal(p) => l.iter().find(|e| e.kind == "applied" && e.data["proposal"].as_u64() == Some(p)).map(|e| e.seq),
        Attempt::Undo(a) => l.iter().find(|e| e.kind == "rolled_back" && e.data["applied"].as_u64() == Some(a)).map(|e| e.seq),
    };
    if !matches!(attempt, Attempt::Nothing) {
        match read_ledger(cli) {
            Ok(l) => {
                if let Some(a) = landed(&l) {
                    println!("{}", ui.warn(&format!("Done, but something went wrong afterwards ({what}: change {a}).")));
                    println!("    Check with: torq status");
                    log_line();
                    std::process::exit(1);
                }
            }
            Err(_) => {
                println!("{}", ui.fail(&format!("Couldn't tell whether this worked ({what}).")));
                println!("    Your machine may not be as it was: see torq status");
                log_line();
                std::process::exit(1);
            }
        }
    }
    let msg = match outcome {
        run::Outcome::Busy => format!("Couldn't {what}: another change started first. Try again when it is done."),
        run::Outcome::Refused(m) => pkg::explain_apply_error(what, &history::one_line(&m, 200)),
        run::Outcome::Broken(m) => format!("Couldn't {what}: {m}."),
        run::Outcome::Done(_) => unreachable!("fail() is for outcomes that are not Done"),
    };
    println!("{}", ui.fail(&msg));
    let consistent = matches!(run::broker_status(broker(cli, &["status"]), log), Ok(Ok(v)) if v["ok"] == true);
    if consistent {
        println!("    Nothing changed — your machine is as it was.");
    } else {
        println!("    Your machine may not be as it was: see torq status");
    }
    if let Attempt::Proposal(p) = attempt {
        println!("    {}", ui.dim(&format!("(Your request is recorded as proposal {p}, not applied.)")));
    }
    log_line();
    std::process::exit(1);
}

/// Version and description of packages in this machine's nixpkgs, for showing a change. Unknown
/// facts are empty: they are decoration, the evaluation has already checked the names.
fn package_facts(copy: &Path, host: &str, names: &[String]) -> Vec<(String, String)> {
    // Names passed pkg::valid_package (attribute characters only), so they are safe in a string.
    let list: Vec<String> = names.iter().map(|n| format!("\"{n}\"")).collect();
    let out = Command::new("nix")
        .args(["eval", "--json"])
        .arg(format!("{}#nixosConfigurations.{host}.pkgs", flake_ref(copy)))
        .arg("--apply")
        .arg(format!(
            "p: map (n: let a = p.lib.attrByPath (p.lib.splitString \".\" n) null p; in \
             {{ version = a.version or \"\"; description = a.meta.description or \"\"; }}) [ {} ]",
            list.join(" ")
        ))
        .output();
    let facts: Vec<serde_json::Value> = out.ok().and_then(|o| serde_json::from_slice(&o.stdout).ok()).unwrap_or_default();
    names
        .iter()
        .enumerate()
        .map(|(i, _)| {
            let f = facts.get(i);
            let s = |k: &str| f.and_then(|f| f[k].as_str()).map(|v| history::one_line(v, 60)).unwrap_or_default();
            (s("version"), s("description"))
        })
        .collect()
}

fn change(cli: &Cli, names: &[String], system: bool, yes: bool, how: Edit) -> Result<()> {
    check_repo(cli)?;
    let user = me()?;
    approval(system, yes)?;
    let target = if system {
        Target::System
    } else {
        if !pkg::valid_user(&user) {
            bail!("'{user}' is not a user name TorqOS can keep a package list for");
        }
        Target::User(user.clone())
    };
    for n in names {
        if !pkg::valid_package(n) {
            bail!("'{n}' is not a package name (a nixpkgs attribute, e.g. htop or python3Packages.requests)");
        }
    }
    let old = read_list(&cli.repo, &target.path())?;
    let (new, changed) = pkg::edit(old.as_ref().map(|f| f.names.as_slice()).unwrap_or_default(), names, how);
    let verb = if how == Edit::Add { "Install" } else { "Remove" };
    let ui = ui::Ui::detect();
    let whom = if system { " for everyone" } else { "" };
    if changed.is_empty() {
        let state = if how == Edit::Add { "already installed" } else { "not installed" };
        println!("{}", ui.ok(&format!("{} {} {state}{whom}", names.join(", "), if names.len() == 1 { "is" } else { "are" }), ""));
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
             torq.lib.mkSystem with `configDir = ./.;` (a system change)"
        );
    }
    let path = target.path();
    let new_text = pkg::render(&new);
    let file = copy.join(&path);
    fs::create_dir_all(file.parent().unwrap())?;
    fs::write(&file, &new_text)?;
    let sign = if how == Edit::Add { '+' } else { '-' };
    let facts = {
        let _p = run::Progress::start("Checking");
        package_facts(&copy, &host, &changed)
    };
    let rows = facts
        .into_iter()
        .zip(&changed)
        .map(|((version, description), name)| ui::change_row(&ui, sign, name, &version, &description))
        .collect();
    let list = changed.join(", ");
    let summary = Summary {
        title: match (how, system) {
            (Edit::Add, false) => "Install for you".into(),
            (Edit::Add, true) => "Install for everyone on this machine".into(),
            (Edit::Remove, false) => "Remove from your apps".into(),
            (Edit::Remove, true) => "Remove for everyone on this machine".into(),
        },
        rows,
        system,
        question: format!("{verb}?"),
        done: format!("{} {list}{whom}", if how == Edit::Add { "Installed" } else { "Removed" }),
        what: format!("{} {list}", verb.to_lowercase()),
    };
    let change = Change {
        copy: &copy,
        path: &path,
        old: old.as_ref().map(|f| f.text.as_str()),
        new: &new_text,
        reason: format!("{verb} {list} {}", target.describe()),
        summary,
    };
    let result = submit(cli, scratch, &host, change, yes);
    // A misspelt name: offer the closest real ones (only now, when it is needed: the list of names
    // takes a moment to read).
    if let Err(e) = &result {
        if how == Edit::Add && e.to_string().contains("isn't a package.") {
            if let Some(hint) = did_you_mean(&copy, &host, &changed) {
                bail!("{hint}");
            }
        }
    }
    result
}

/// "\"htpo\" isn't a package. Did you mean htop?" for the first name that does not exist and has
/// close matches in this machine's nixpkgs; None if there is nothing to suggest.
fn did_you_mean(copy: &Path, host: &str, names: &[String]) -> Option<String> {
    let out = Command::new("nix")
        .args(["eval", "--json"])
        .arg(format!("{}#nixosConfigurations.{host}.pkgs", flake_ref(copy)))
        .args(["--apply", "builtins.attrNames"])
        .output()
        .ok()?;
    let all: Vec<String> = serde_json::from_slice(&out.stdout).ok()?;
    names.iter().filter(|n| !n.contains('.') && !all.contains(n)).find_map(|n| {
        let close = pkg::suggest(n, &all);
        (!close.is_empty()).then(|| {
            let close: Vec<String> = close.iter().map(|c| history::one_line(c, 40)).collect();
            format!("\"{n}\" isn't a package. Did you mean {}? (or: torq pkg search {n})", close.join(", "))
        })
    })
}

const SERVICES: &str = "services.json";

/// What the machine's config says about services.
struct Services {
    /// Catalog name → what enabling it implies.
    catalog: Vec<(String, String)>,
    enabled: Vec<String>,
    /// sshd is on outside the catalog (stock settings), e.g. `services.openssh.enable` in a .nix file.
    ssh_outside: bool,
    /// The machine's owner (`torq.user`): the one user the catalog's SSH admits.
    owner: String,
}

fn services(repo: &Path, host: &str) -> Result<Services> {
    let out = Command::new("nix")
        .args(["eval", "--json"])
        .arg(format!("{}#nixosConfigurations.{host}.config", flake_ref(repo)))
        .args([
            "--apply",
            "c: { inherit (c.torq.services) catalog enabled; owner = c.torq.user; \
             ssh_outside = c.services.openssh.enable && !(builtins.elem \"ssh\" c.torq.services.enabled) \
             && c.services.openssh.settings.PasswordAuthentication != false; }",
        ])
        .output()
        .context("running nix eval")?;
    if !out.status.success() {
        bail!("cannot read this machine's services: {}", last_error(&out.stderr));
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    Ok(Services {
        catalog: v["catalog"]
            .as_object()
            .map(|o| o.iter().map(|(k, d)| (k.clone(), d.as_str().unwrap_or_default().to_string())).collect())
            .unwrap_or_default(),
        enabled: serde_json::from_value(v["enabled"].clone())?,
        ssh_outside: v["ssh_outside"] == true,
        owner: v["owner"].as_str().unwrap_or_default().to_string(),
    })
}

fn service_list(cli: &Cli) -> Result<()> {
    let s = services(&cli.repo, &config_name(&cli.repo)?)?;
    for (name, what) in &s.catalog {
        let on = if s.enabled.contains(name) { "on " } else { "   " };
        println!("[{on}] {name:<10} {what}");
    }
    if s.ssh_outside {
        println!(
            "\nSSH is ON outside this catalog, with stock settings (password logins allowed).\n\
             `torq service enable ssh` replaces them with keys only."
        );
    }
    Ok(())
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

/// `torq service enable|disable`: a system change to services.json, always asked.
fn service_change(cli: &Cli, names: &[String], how: Edit) -> Result<()> {
    check_repo(cli)?;
    approval(true, false)?;
    let ui = ui::Ui::detect();
    let host = config_name(&cli.repo)?;
    let svc = services(&cli.repo, &host)?;
    for n in names {
        if !svc.catalog.iter().any(|(c, _)| c == n) {
            let all: Vec<_> = svc.catalog.iter().map(|(c, _)| c.as_str()).collect();
            bail!("\"{n}\" isn't a TorqOS service. There are: {}", all.join(", "));
        }
    }
    let old = read_list(&cli.repo, SERVICES)?;
    let (new, changed) = pkg::edit(old.as_ref().map(|f| f.names.as_slice()).unwrap_or_default(), names, how);
    let on = how == Edit::Add;
    if changed.is_empty() {
        println!("{}", ui.ok(&format!("{} {} already {}", names.join(", "), if names.len() == 1 { "is" } else { "are" }, if on { "on" } else { "off" }), ""));
        return Ok(());
    }
    let scratch = scratch_dir()?;
    let scratch = scratch.path();
    let copy = scratch.join("config");
    copy_tree(&cli.repo, &copy)?;
    if !reads_package_lists(&copy, &host)? {
        bail!(
            "this machine's config does not read services.json yet: its flake.nix must call \
             torq.lib.mkSystem with `configDir = ./.;` (a system change)"
        );
    }
    let new_text = pkg::render(&new);
    fs::write(copy.join(SERVICES), &new_text)?;
    // What a bare `+ "name"` in the diff means, one row per service.
    let mut rows: Vec<String> = svc
        .catalog
        .iter()
        .filter(|(c, _)| changed.contains(c))
        .map(|(name, what)| format!("    · {name}: {}", ui.dim(what)))
        .collect();
    // The catalog's SSH is keys only, for the owner only: without one of their keys it would lock
    // everyone out; with them, approving it means approving exactly these keys — as the changed
    // configuration will have it.
    if on && changed.iter().any(|n| n == "ssh") {
        let keys = owner_keys(&copy, &host, &svc.owner)?;
        if keys.is_empty() {
            bail!(
                "SSH here lets in {}'s keys only, and they have none: nobody could log in. Add a public \
                 key to ~/.ssh/authorized_keys first.",
                svc.owner
            );
        }
        rows.push(format!("    · keys that will be able to log in as {}:", svc.owner));
        // Never truncated: approving SSH means approving exactly these keys. A warning (a
        // certificate authority, options) gets its own line above the key it is about.
        for (note, id, place) in &keys {
            if !note.is_empty() {
                rows.push(format!("        {}", ui.yellow(&history::clean(note.trim_end_matches(": ")))));
            }
            rows.push(format!("        {}", history::clean(id)));
            rows.push(format!("          {}", ui.dim(&format!("from {}", history::clean(place)))));
        }
    }
    let list = changed.join(", ");
    let summary = Summary {
        title: format!("Turn {} {list}", if on { "on" } else { "off" }),
        rows,
        system: true,
        question: format!("Turn {}?", if on { "on" } else { "off" }),
        done: format!("{list} {} {}", if changed.len() == 1 { "is" } else { "are" }, if on { "on" } else { "off" }),
        what: format!("turn {} {list}", if on { "on" } else { "off" }),
    };
    let change = Change {
        copy: &copy,
        path: SERVICES,
        old: old.as_ref().map(|f| f.text.as_str()),
        new: &new_text,
        reason: format!("{} {list}", if on { "Enable" } else { "Disable" }),
        summary,
    };
    submit(cli, scratch, &host, change, false)
}

/// `torq status [--repair]`: the machine against its history, in words; with --repair, and only
/// for what a rebuild can fix, ask and then return it to its history (the broker's `repair`).
fn status_cmd(cli: &Cli, repair: bool) -> Result<()> {
    let ui = ui::Ui::detect();
    let log = run::start_log()?;
    let v = match run::broker_status(broker(cli, &["status"]), &log)? {
        Ok(v) => v,
        Err(why) => bail!("Couldn't check this machine: {why}."),
    };
    let last = read_ledger(cli).ok().and_then(|l| history::last_change(&l));
    let report = status::render(&v, last.as_deref(), &cli.repo.display().to_string(), &ui);
    print!("{}", report.text);
    if !repair {
        std::process::exit(report.exit);
    }
    match report.fix {
        status::Fix::Nothing if report.exit == 0 => {
            println!("  Nothing to repair.");
            return Ok(());
        }
        status::Fix::Repair => {}
        _ => std::process::exit(report.exit), // busy, or a person has to look: said above
    }
    approval(true, false)?;
    println!("\n{}", ui.heading("Return this machine to its history", true));
    println!("    · Rebuild the configuration of its last recorded change, and switch to it.");
    println!();
    if !confirm(&ui, "Repair?", false)? {
        println!("  Nothing changed.");
        return Ok(());
    }
    let done = {
        let _p = if asks_for_password(cli) { None } else { Some(run::Progress::start("Repairing")) };
        run::broker_json(broker(cli, &["repair", "--approved-by", &me()?]), &log)?
    };
    match done {
        run::Outcome::Done(e) => {
            println!("{}", ui.ok("Back to its history", &format!("change {}", e["seq"].as_u64().unwrap_or(0))));
            Ok(())
        }
        run::Outcome::Busy => bail!("Another change is being applied right now. Try again when it is done."),
        run::Outcome::Refused(m) => {
            println!("{}", ui.fail(&format!("Couldn't repair: {}", history::one_line(&m, 200))));
            println!("    See: torq status");
            println!("    {}", ui.dim(&format!("log: {}", log.display())));
            std::process::exit(1);
        }
        run::Outcome::Broken(m) => bail!("Couldn't repair: {m}."),
    }
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

/// `torq undo [n]`: roll back an applied change through the broker, after saying what it was.
fn undo(cli: &Cli, seq: Option<u64>, yes: bool) -> Result<()> {
    let ui = ui::Ui::detect();
    let entries = read_ledger(cli)?;
    let target = history::undo_target(&entries, seq)?;
    let files: Vec<&str> = target.data["files"].as_array().map(|f| f.iter().filter_map(|v| v.as_str()).collect()).unwrap_or_default();
    let reason = history::one_line(&target.reason, 60);
    let system = target.tier != "user";
    // Say what it is before anything else, refusals included.
    println!("\n{}", ui.heading(&format!("Undo \"{reason}\""), system));
    let clock = history::Clock::system();
    let when = history::parse_ts(&target.ts)
        .map(|t| t + clock.offset)
        .map(|t| format!("{} {:02}:{:02}", pkg::ymd(t as u64), t.rem_euclid(86_400) / 3600, t.rem_euclid(3600) / 60))
        .unwrap_or_default();
    let mut by = history::one_line(&target.actor, 20);
    if target.tainted {
        by.push_str(" ⚠ (from a session that read untrusted content)");
    }
    println!("    · change {}, {when}, by {by}", target.seq);
    if !files.is_empty() {
        println!("    · {}", ui.dim(&history::one_line(&files.join(", "), 70)));
    }
    println!();
    // --yes only for a change to exactly your own package list — not someone else's, not the system.
    let me = me()?;
    let mine = files == [format!("users/{me}.packages.json").as_str()];
    if yes && !mine {
        bail!("--yes only undoes changes to your own packages; this one changes something else: run again without --yes");
    }
    approval(false, yes)?; // there must be someone to ask
    // The answer defaults to Yes by the same rule: only for a change to your own packages.
    if !yes && !confirm(&ui, "Undo?", mine)? {
        println!("  Nothing changed.");
        return Ok(());
    }
    let log = run::start_log()?;
    let done = {
        let _p = if asks_for_password(cli) { None } else { Some(run::Progress::start("Undoing")) };
        run::broker_json(broker(cli, &["rollback", &target.seq.to_string(), "--approved-by", &me]), &log)?
    };
    let what = format!("undo change {}", target.seq);
    match done {
        run::Outcome::Done(e) => {
            println!("{}", ui.ok("Undone", &format!("change {}", e["seq"].as_u64().unwrap_or(0))));
            Ok(())
        }
        run::Outcome::Refused(m) if m.contains("later changes conflict") => {
            // Name what stands in the way, from the ledger, rather than repeating git's view of it.
            let later = history::later_conflicts(&entries, target.seq);
            let list = later.iter().map(|s| s.to_string()).collect::<Vec<_>>().join(", ");
            println!(
                "{}",
                ui.fail(&format!(
                    "Can't undo change {} on its own: {} also changed {}.",
                    target.seq,
                    if later.is_empty() { "later changes".to_string() } else { format!("later changes ({list})") },
                    history::one_line(&files.join(", "), 40)
                ))
            );
            // An undo cannot be undone, so then only the opposite change helps.
            if later.iter().any(|s| entries.iter().any(|e| e.seq == *s && e.kind == "rolled_back")) {
                println!("    Make the opposite change yourself (for packages: torq pkg add / remove).");
            } else {
                println!("    Undo those first (newest first), or make the opposite change yourself.");
            }
            println!("    Nothing changed.");
            std::process::exit(1);
        }
        other => fail(cli, &ui, &what, other, &log, Attempt::Undo(target.seq)),
    }
}

/// `torq update`: move this machine to the newest TorqOS (and so nixpkgs, which follows it), as
/// one ledgered, undoable change to flake.lock (ADR-004). `all`: every input of the config flake.
fn update(cli: &Cli, all: bool) -> Result<()> {
    check_repo(cli)?;
    approval(true, false)?;
    let ui = ui::Ui::detect();
    let scratch = scratch_dir()?;
    let scratch = scratch.path();
    let host = config_name(&cli.repo)?;
    let copy = scratch.join("config");
    copy_tree(&cli.repo, &copy)?;
    let old = fs::read_to_string(copy.join("flake.lock")).context("reading flake.lock")?;
    let out = {
        let _p = run::Progress::start("Looking for updates");
        let mut cmd = Command::new("nix");
        cmd.args(["flake", "update", "--flake"]).arg(flake_ref(&copy));
        if !all {
            cmd.arg("torq");
        }
        cmd.output().context("running nix flake update")?
    };
    if !out.status.success() {
        bail!("Couldn't look for updates: {}", history::one_line(&last_error(&out.stderr), 200));
    }
    let new = fs::read_to_string(copy.join("flake.lock")).context("reading the updated flake.lock")?;
    if new == old {
        println!("{}", ui.ok("This machine is up to date", ""));
        return Ok(());
    }
    let changes = pkg::lock_changes(&old, &new);
    let rows = changes
        .iter()
        .map(|(name, before, after)| format!("    {:<12} {before} → {after}", history::one_line(name, 12)))
        .collect();
    let moved = pkg::moved_inputs(&String::from_utf8_lossy(&out.stderr));
    let change = Change {
        copy: &copy,
        path: "flake.lock",
        old: Some(&old),
        new: &new,
        reason: if moved.is_empty() { "Update".to_string() } else { format!("Update {}", moved.join(", ")) },
        summary: Summary {
            title: "Update this machine".into(),
            rows,
            system: true,
            question: "Update?".into(),
            done: "Updated".into(),
            what: "update".into(),
        },
    };
    submit(cli, scratch, &host, change, false)
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

/// The single nixosConfiguration in the config repo. (Not the hostname: a machine whose config was
/// switched away from TorqOS may have another one.)
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

/// Whether this machine's config reads package lists at all (`configDir`); if not, changing them
/// would do nothing.
fn reads_package_lists(copy: &Path, host: &str) -> Result<bool> {
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
fn confirm(ui: &ui::Ui, question: &str, default_yes: bool) -> Result<bool> {
    print!("  {} {} ", ui.bold(question), if default_yes { "[Y/n]" } else { "[y/N]" });
    std::io::stdout().flush()?;
    let mut answer = String::new();
    // End of input (Ctrl-D, a closed pipe) is not an answer: it never approves anything.
    if std::io::stdin().lock().read_line(&mut answer)? == 0 {
        println!();
        return Ok(false);
    }
    Ok(match answer.trim() {
        "" => default_yes,
        a => matches!(a, "y" | "Y" | "yes" | "Yes"),
    })
}
