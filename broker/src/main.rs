use anyhow::Result;
use clap::{Parser, Subcommand};
use kestrel_broker::{policy::Actor, Broker};
use std::path::PathBuf;

/// Kestrel change broker — the only writer of the system config repo.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Path to the config git repository.
    #[arg(long, env = "KESTREL_CONFIG_REPO", default_value = "/var/lib/kestrel/config")]
    repo: PathBuf,
    /// Directory for the ledger and broker state.
    #[arg(long, env = "KESTREL_STATE_DIR", default_value = "/var/lib/kestrel/state")]
    state_dir: PathBuf,
    /// Command run after each commit; `{repo}` is replaced with the repo path.
    #[arg(long, env = "KESTREL_REBUILD_CMD", default_value = "nixos-rebuild switch --flake {repo}")]
    rebuild_cmd: String,
    /// Link to the running system, recorded after every rebuild and compared by `status`.
    #[arg(long, env = "KESTREL_CURRENT_SYSTEM", default_value = "/run/current-system")]
    current_system: PathBuf,
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
    /// Undo an applied change: revert commit, rebuild, ledger.
    Rollback {
        seq: u64,
        #[arg(long)]
        approved_by: String,
    },
    /// Finish an apply or rollback whose broker died: discard its unledgered commit, reset to the
    /// last ledgered config, rebuild, and ledger it. apply and rollback do this first on their own.
    Reconcile {
        #[arg(long)]
        approved_by: String,
    },
    /// Print the ledger.
    Log {
        #[arg(long)]
        json: bool,
    },
    /// Verify the ledger hash chain.
    Verify,
    /// Compare the machine with its ledger: running system, config repo, interrupted operations,
    /// ledger chain. Exits non-zero if anything is not as ledgered.
    Status,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let b = Broker::new(&cli.repo, &cli.state_dir, &cli.rebuild_cmd).with_current_system(&cli.current_system);
    match cli.cmd {
        Cmd::Init => {
            b.init()?;
            println!("initialised {} (ledger: {})", cli.repo.display(), b.ledger.path().display());
        }
        Cmd::Propose { diff, reason, actor, tainted } => {
            let e = b.propose(&diff, &reason, actor, tainted)?;
            println!("proposal #{} · tier: {} · actor: {}{}", e.body.seq, e.body.tier, e.body.actor,
                if tainted { " · TAINTED" } else { "" });
            println!("files: {}", e.body.data["files"]);
            println!("\n{}", e.body.data["diff"].as_str().unwrap_or_default());
            println!("approve with: kestrel-broker apply {} --approved-by <name>", e.body.seq);
        }
        Cmd::Show { seq } => {
            let e = b.ledger.get(seq)?;
            println!("{}", serde_json::to_string_pretty(&e)?);
            if let Some(c) = e.body.data.get("commit").and_then(|c| c.as_str()) {
                println!("\n{}", b.repo.show(c)?);
            }
        }
        Cmd::Apply { seq, approved_by, confirm_tainted } => {
            let e = b.apply(seq, &approved_by, confirm_tainted)?;
            println!("applied #{} as commit {} (ledger #{})", seq, e.body.data["commit"], e.body.seq);
            println!("undo with: kestrel-broker rollback {} --approved-by <name>", e.body.seq);
        }
        Cmd::Rollback { seq, approved_by } => {
            let e = b.rollback(seq, &approved_by)?;
            println!("rolled back #{} as commit {} (ledger #{})", seq, e.body.data["commit"], e.body.seq);
        }
        Cmd::Reconcile { approved_by } => match b.reconcile(&approved_by)? {
            Some(e) => println!("{} (ledger #{})", e.body.reason, e.body.seq),
            None => println!("nothing to reconcile"),
        },
        Cmd::Log { json } => {
            for e in b.ledger.read_all()? {
                if json {
                    println!("{}", serde_json::to_string(&e)?);
                } else {
                    println!(
                        "#{:<4} {} {:<12} {:<9} {:<6}{} {}",
                        e.body.seq, e.body.ts, e.body.kind, e.body.tier, e.body.actor,
                        if e.body.tainted { " ⚠" } else { "" }, e.body.reason
                    );
                }
            }
        }
        Cmd::Verify => {
            let n = b.ledger.verify()?;
            println!("ledger OK: {n} entries, chain intact");
        }
        Cmd::Status => {
            let s = b.status()?;
            let or = |v: &Option<String>, none: &str| v.clone().unwrap_or_else(|| none.to_string());
            println!("ledger          {}", match &s.ledger { Ok(n) => format!("OK, {n} entries"), Err(e) => e.clone() });
            println!("config repo     {}  (ledgered: {})", or(&s.head, "?"), or(&s.ledgered_commit, "none"));
            println!(
                "running system  {}  (ledgered: {})",
                or(&s.running_system, "?"),
                or(&s.ledgered_system, "not recorded yet: from the next change on")
            );
            let problems = s.problems();
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
