use anyhow::Result;
use clap::{Parser, Subcommand};
use kestrel_broker::{ledger::Entry, policy::Actor, Broker};
use serde_json::json;
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
    /// The system profile: what the next boot starts. Compared by `status`.
    #[arg(long, env = "KESTREL_SYSTEM_PROFILE", default_value = "/nix/var/nix/profiles/system")]
    system_profile: PathBuf,
    /// Machine-readable output: one JSON value per command (a ledger entry, or the status), and
    /// `{"error": "..."}` on failure. For the `kestrel` CLI (ADR-014); people get plain text.
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
        if json {
            println!("{}", json!({ "error": format!("{e:#}") }));
        } else {
            eprintln!("Error: {e:?}");
        }
        std::process::exit(1);
    }
}

fn print_entry(e: &Entry) -> Result<()> {
    println!("{}", serde_json::to_string(e)?);
    Ok(())
}

fn run(cli: Cli) -> Result<()> {
    let json = cli.json;
    let b = Broker::new(&cli.repo, &cli.state_dir, &cli.rebuild_cmd)
        .with_current_system(&cli.current_system)
        .with_system_profile(&cli.system_profile);
    match cli.cmd {
        Cmd::Init => {
            b.init()?;
            if json {
                println!("{}", json!({ "initialised": cli.repo, "ledger": b.ledger.path() }));
            } else {
                println!("initialised {} (ledger: {})", cli.repo.display(), b.ledger.path().display());
            }
        }
        Cmd::Propose { diff, reason, actor, tainted } => {
            let e = b.propose(&diff, &reason, actor, tainted)?;
            if json {
                return print_entry(&e);
            }
            println!("proposal #{} · tier: {} · actor: {}{}", e.body.seq, e.body.tier, e.body.actor,
                if tainted { " · TAINTED" } else { "" });
            println!("files: {}", e.body.data["files"]);
            println!("\n{}", printable_text(e.body.data["diff"].as_str().unwrap_or_default()));
            println!("approve with: kestrel-broker apply {} --approved-by <name>", e.body.seq);
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
            println!("undo with: kestrel-broker rollback {} --approved-by <name>", e.body.seq);
        }
        Cmd::Rollback { seq, approved_by } => {
            let e = b.rollback(seq, &approved_by)?;
            if json {
                return print_entry(&e);
            }
            println!("rolled back #{} as commit {} (ledger #{})", seq, e.body.data["commit"], e.body.seq);
        }
        Cmd::Reconcile { approved_by } => match (b.reconcile(&approved_by)?, json) {
            (Some(e), true) => print_entry(&e)?,
            (None, true) => println!("null"),
            (Some(e), false) => println!("{} (ledger #{})", e.body.reason, e.body.seq),
            (None, false) => println!("nothing to reconcile"),
        },
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
