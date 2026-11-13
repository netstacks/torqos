//! What crosses between TorqOS's programs (ADR-044): the broker's socket, a person's agent service's (ADR-051 3), and later the ledger's words.
//! The broker, torqcli and the agent each take them from here, so a word added on one side is added on
//! all of them, and the compiler says where it is not handled. Nothing of the broker's logic is here.

// No unsafe code and no dead code, ever: `forbid` cannot be undone by any `#[allow]` below it,
// so the compiler is the guard (docs/DECISIONS.md ADR-020).
#![forbid(unsafe_code)]
#![forbid(dead_code)]

pub mod agent;
pub mod clean;
pub mod desktop;
pub mod releases;
pub mod secrets;
pub mod socket;
