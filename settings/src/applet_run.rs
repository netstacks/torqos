//! `torq-applet-run <applet>`: how COSMIC's panel starts TorqOS's applets. It runs the applet as its child and
//! exits with what the applet did, as a code even when a signal ended the applet, because the panel starts an
//! applet again only after an exit with a code (`torq::app::run_applet`; KNOWN-GAPS, "An applet a signal ends").

// No unsafe code and no dead code, ever: `forbid` cannot be undone by any `#[allow]` below it,
// so the compiler is the guard (docs/DECISIONS.md ADR-020).
#![forbid(unsafe_code)]
#![forbid(dead_code)]

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::ExitCode::from(torq::app::run_applet(&args))
}
