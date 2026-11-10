//! `torq-agent-launcher`: the agent in COSMIC's launcher, a pop-launcher plugin (ADR-051 4, step 4). Its work is
//! the library's (`torq::agent::launcher`).

// No unsafe code and no dead code, ever (ADR-020).
#![forbid(unsafe_code)]
#![cfg_attr(not(test), forbid(dead_code))]
#![cfg_attr(test, deny(dead_code))]

fn main() {
    std::process::exit(torq::agent::launcher::run());
}
