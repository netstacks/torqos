//! TorqOS Settings (ADR-045 2): torqcli in a COSMIC window. A client of the broker's socket and nothing
//! more: the person's own account, no privilege, the same requests torqcli sends and the same words
//! (`torq::app`). Every action it offers is a torqcli command the person could type, and it shows which.

// No unsafe code and no dead code, ever: `forbid` cannot be undone by any `#[allow]` below it,
// so the compiler is the guard (docs/DECISIONS.md ADR-020).
#![forbid(unsafe_code)]
#![forbid(dead_code)]

mod window;

fn main() -> cosmic::iced::Result {
    let settings = cosmic::app::Settings::default().size_limits(cosmic::iced::Limits::NONE.min_width(400.0).min_height(300.0));
    cosmic::app::run::<window::Settings>(settings, ())
}
