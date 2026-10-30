use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use torq_broker::config::{self, Requested, Settings};
use torq_broker::{ledger::Entry, policy::Actor, Broker, Busy};
use serde_json::json;
use std::path::{Path, PathBuf};

/// TorqOS change broker — the only writer of the system config repo.
///
/// Run as root on a TorqOS machine, it takes its settings only from /etc/torq/broker.json and
/// refuses flags or TORQ_* variables that differ from it. It always runs with a cleared
/// environment.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Path to the config git repository [default: /var/lib/torq/config].
    #[arg(long, env = "TORQ_CONFIG_REPO")]
    repo: Option<PathBuf>,
    /// Directory for the ledger and broker state [default: /var/lib/torq/state].
    #[arg(long, env = "TORQ_STATE_DIR")]
    state_dir: Option<PathBuf>,
    /// Command run after each commit; `{repo}` is replaced with the repo path
    /// [default: nixos-rebuild switch --flake {repo}].
    #[arg(long, env = "TORQ_REBUILD_CMD")]
    rebuild_cmd: Option<String>,
    /// Link to the running system, recorded after every rebuild and compared by `status`
    /// [default: /run/current-system].
    #[arg(long, env = "TORQ_CURRENT_SYSTEM")]
    current_system: Option<PathBuf>,
    /// The system profile: what the next boot starts, compared by `status`
    /// [default: /nix/var/nix/profiles/system].
    #[arg(long, env = "TORQ_SYSTEM_PROFILE")]
    system_profile: Option<PathBuf>,
    /// Machine-readable output: one JSON value per command (a ledger entry, or the status), and
    /// `{"error": "..."}` on failure. For the `torq` CLI (ADR-014); people get plain text.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Initialise the config repo and ledger (idempotent).
    Init,
    /// Submit a change (unified diff against the repo) for review.
    Propose {
        #[arg(long)]
        diff: PathBuf,
        #[arg(long)]
        reason: String,
        #[arg(long, value_enum, default_value = "user")]
        actor: Actor,
        /// The proposing session ingested untrusted content (web, email, downloads).
        #[arg(long)]
        tainted: bool,
        /// The commit this change was prepared against: refused if the configuration has moved on.
        #[arg(long)]
        base: Option<String>,
    },
    /// Show a proposal or applied change.
    Show { seq: u64 },
    /// Approve and apply a proposal: commit, rebuild, ledger.
    Apply {
        seq: u64,
        #[arg(long)]
        approved_by: String,
        #[arg(long)]
        confirm_tainted: bool,
    },
    /// Undo one applied change: a new commit that reverts it, rebuild, ledger.
    Revert {
        seq: u64,
        #[arg(long)]
        approved_by: String,
    },
    /// Finish an apply or revert whose broker died: discard its unledgered commit, reset to the
    /// last ledgered config, rebuild, and ledger it. apply and revert do this first on their own.
    Reconcile {
        #[arg(long)]
        approved_by: String,
    },
    /// Return the machine to its last ledgered configuration after drift (switched outside the
    /// broker, a changed boot default, a failed restore): rebuild it and ledger that. Never
    /// automatic (ADR-012). Refuses when the config repo itself was changed outside the broker.
    Repair {
        #[arg(long)]
        approved_by: String,
    },
    /// Print the ledger.
    Log {
        /// Leave out proposals' full diffs (for listing: the diffs make the ledger large).
        #[arg(long)]
        brief: bool,
    },
    /// Verify the ledger hash chain.
    Verify,
    /// Compare the machine with its ledger: running system, config repo, interrupted operations,
    /// ledger chain. Exits 1 if anything is not as ledgered, 75 if an operation is running.
    Status,
}

fn main() {
    let cli = Cli::parse();
    let json = cli.json;
    if let Err(e) = run(cli) {
        // Another operation is running: "try again later", exit 75 (EX_TEMPFAIL) from any command.
        let busy = e.downcast_ref::<Busy>().is_some();
        if json {
            println!("{}", json!({ "error": format!("{e:#}"), "busy": busy }));
        } else {
            eprintln!("Error: {e:?}");
        }
        std::process::exit(if busy { 75 } else { 1 });
    }
}

fn print_entry(e: &Entry) -> Result<()> {
    println!("{}", serde_json::to_string(e)?);
    Ok(())
}

/// Resolve the settings (config.rs), then replace this process's environment, working directory and
/// umask with the broker's own, before anything runs: every child (git, sh, the rebuild) inherits
/// them, and nothing of the caller's — GIT_*, NIX_*, LD_*, their HOME and its .gitconfig, their
/// current directory — may reach a root broker's work. Path arguments are made absolute first, so a
/// relative `--diff` still means what the caller meant.
fn settings(cli: &mut Cli) -> Result<Settings> {
    let abs = |p: &mut PathBuf| -> Result<()> {
        *p = std::path::absolute(&*p).with_context(|| format!("resolving {}", p.display()))?;
        Ok(())
    };
    for p in [&mut cli.repo, &mut cli.state_dir, &mut cli.current_system, &mut cli.system_profile].into_iter().flatten() {
        abs(p)?;
    }
    if let Cmd::Propose { diff, .. } = &mut cli.cmd {
        abs(diff)?;
    }
    let root = config::running_as_root();
    let path = std::env::var("PATH").unwrap_or_default();
    let home = if root { PathBuf::from("/root") } else { std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| "/".into()) };
    let asked = Requested {
        repo: cli.repo.clone(),
        state_dir: cli.state_dir.clone(),
        rebuild: cli.rebuild_cmd.clone(),
        current_system: cli.current_system.clone(),
        system_profile: cli.system_profile.clone(),
    };
    // Only a root broker uses the file, so only it reads it: a malformed or newer one must not
    // break a test broker.
    let fixed = if root { Settings::read(Path::new(config::FIXED))? } else { None };
    let s = config::resolve(root, fixed, asked, Settings::defaults(path, home))?;
    let inherited = std::env::vars_os().filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)));
    config::take_over_process(&config::environment(&s, inherited))?;
    Ok(s)
}

fn run(mut cli: Cli) -> Result<()> {
    let json = cli.json;
    let s = settings(&mut cli)?;
    let b = Broker::new(&s.repo, &s.state_dir, &s.rebuild)
        .with_current_system(&s.current_system)
        .with_system_profile(&s.system_profile);
    match cli.cmd {
        Cmd::Init => {
            b.init()?;
            if json {
                println!("{}", json!({ "initialised": s.repo, "ledger": b.ledger.path() }));
            } else {
                println!("initialised {} (ledger: {})", s.repo.display(), b.ledger.path().display());
            }
        }
        Cmd::Propose { diff, reason, actor, tainted, base } => {
            let e = b.propose_on(base.as_deref(), &diff, &reason, actor, tainted)?;
            if json {
                return print_entry(&e);
            }
            println!("proposal #{} · tier: {} · actor: {}{}", e.body.seq, e.body.tier, e.body.actor,
                if tainted { " · TAINTED" } else { "" });
            println!("files: {}", e.body.data["files"]);
            println!("\n{}", printable_text(e.body.data["diff"].as_str().unwrap_or_default()));
            println!("approve with: torq-broker apply {} --approved-by <name>", e.body.seq);
        }
        Cmd::Show { seq } => {
            let e = b.ledger.get(seq)?;
            if json {
                return print_entry(&e);
            }
            println!("{}", printable_text(&serde_json::to_string_pretty(&e)?));
            if let Some(c) = e.body.data.get("commit").and_then(|c| c.as_str()) {
                println!("\n{}", printable_text(&b.repo.show(c)?));
            }
        }
        Cmd::Apply { seq, approved_by, confirm_tainted } => {
            let e = b.apply(seq, &approved_by, confirm_tainted)?;
            if json {
                return print_entry(&e);
            }
            println!("applied #{} as commit {} (ledger #{})", seq, e.body.data["commit"], e.body.seq);
            println!("undo with: torq-broker revert {} --approved-by <name>", e.body.seq);
        }
        Cmd::Revert { seq, approved_by } => {
            let e = b.revert(seq, &approved_by)?;
            if json {
                return print_entry(&e);
            }
            println!("reverted #{} as commit {} (ledger #{})", seq, e.body.data["commit"], e.body.seq);
        }
        Cmd::Reconcile { approved_by } => match (b.reconcile(&approved_by)?, json) {
            (e, true) => println!("{}", json!({ "reconciled": e })),
            (Some(e), false) => println!("{} (ledger #{})", e.body.reason, e.body.seq),
            (None, false) => println!("nothing to reconcile"),
        },
        Cmd::Repair { approved_by } => {
            let e = b.repair(&approved_by)?;
            if json {
                return print_entry(&e);
            }
            println!("{} (ledger #{})", printable(&e.body.reason), e.body.seq);
        }
        Cmd::Log { brief } => {
            for mut e in b.ledger.read_all()? {
                if brief {
                    if let Some(d) = e.body.data.as_object_mut() {
                        d.remove("diff");
                    }
                }
                if json {
                    print_entry(&e)?;
                } else {
                    // Ledger text comes from whoever proposed: never let it drive the terminal.
                    println!(
                        "#{:<4} {} {:<12} {:<9} {:<6}{} {}",
                        e.body.seq, e.body.ts, e.body.kind, e.body.tier, printable(&e.body.actor),
                        if e.body.tainted { " ⚠" } else { "" }, printable(&e.body.reason)
                    );
                }
            }
        }
        Cmd::Verify => {
            let n = b.ledger.verify()?;
            if json {
                println!("{}", json!({ "entries": n }));
            } else {
                println!("ledger OK: {n} entries, chain intact");
            }
        }
        Cmd::Status => {
            let s = b.status()?;
            let problems = s.problems();
            if json && s.busy.is_some() {
                // Mid-operation, pending.json exists and the tree may be half-applied: reporting
                // `interrupted` or `dirty` now would describe a running change as a broken one.
                println!("{}", json!({ "ok": false, "busy": s.busy, "problems": [] }));
                std::process::exit(75);
            }
            if json {
                println!(
                    "{}",
                    json!({
                        "ok": problems.is_empty() && s.busy.is_none(),
                        "busy": s.busy,
                        "problems": problems,
                        "ledger_entries": s.ledger.as_ref().ok(),
                        "ledger_error": s.ledger.as_ref().err(),
                        "head": s.head,
                        "ledgered_commit": s.ledgered_commit,
                        "running_system": s.running_system,
                        "next_boot": s.next_boot,
                        "ledgered_system": s.ledgered_system,
                        "checked_running_system": s.ledgered_system.is_some(),
                        "dirty": s.dirty,
                        "interrupted": s.interrupted,
                        "unrestored": s.unrestored,
                    })
                );
                std::process::exit(if s.busy.is_some() { 75 } else if problems.is_empty() { 0 } else { 1 });
            }
            if let Some(who) = &s.busy {
                println!("a broker operation is running ({who}); not checked — try again when it is done");
                std::process::exit(75); // EX_TEMPFAIL: "busy" without parsing the text
            }
            let or = |v: &Option<String>, none: &str| v.clone().unwrap_or_else(|| none.to_string());
            println!("ledger          {}", match &s.ledger { Ok(n) => format!("OK, {n} entries"), Err(e) => e.clone() });
            println!("config repo     {}  (ledgered: {})", or(&s.head, "?"), or(&s.ledgered_commit, "none"));
            println!(
                "running system  {}  (ledgered: {})",
                or(&s.running_system, "?"),
                or(&s.ledgered_system, "not recorded yet: from the next change on")
            );
            if problems.is_empty() && s.ledgered_system.is_none() {
                println!("OK as far as recorded: the running system cannot be checked until a change records it");
            } else if problems.is_empty() {
                println!("OK: the machine is as the ledger says");
            } else {
                for p in &problems {
                    println!("PROBLEM: {p}");
                }
                std::process::exit(1);
            }
        }
    }
    Ok(())
}


/// Ledger text and proposed diffs are written by whoever proposed, the agent included, and must not
/// be able to make one thing look like another on a terminal (escape sequences, carriage returns,
/// bidi overrides — "Trojan Source" — invisible characters). Show those as visible escapes.
/// (The CLI's `history::clean` does the same; keep the two in step.)
fn hidden(c: char) -> bool {
    c.is_control()
        || matches!(c,
            '\u{200b}'..='\u{200f}' | '\u{2028}' | '\u{2029}' | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{2069}' | '\u{061c}' | '\u{00ad}' | '\u{feff}'
            | '\u{fe00}'..='\u{fe0f}' | '\u{e0000}'..='\u{e007f}'
            | '\u{3164}' | '\u{ffa0}' | '\u{2800}' | '\u{115f}' | '\u{1160}')
}

/// One line: every hidden character escaped, newlines included.
fn printable(s: &str) -> String {
    s.chars().map(|c| if hidden(c) { c.escape_unicode().to_string() } else { c.to_string() }).collect()
}

/// Multi-line text (diffs, `git show`): keeps newlines and tabs, escapes every other hidden character.
fn printable_text(s: &str) -> String {
    s.chars()
        .map(|c| if hidden(c) && c != '\n' && c != '\t' { c.escape_unicode().to_string() } else { c.to_string() })
        .collect()
}
