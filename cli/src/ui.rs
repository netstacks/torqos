//! How `torq` looks (docs/CLI-DESIGN.md): plain language, one visual language, colour with
//! meaning only — and plain text when colour is off (NO_COLOR, not a terminal), so nothing depends
//! on colour to be understood. Everything here is a pure function of its input, for snapshot tests.


use std::time::Duration;

pub const WIDTH: usize = 80;

#[derive(Clone, Copy)]
pub struct Ui {
    pub color: bool,
}

impl Ui {
    /// No colour (tests, and text the shell cleans before printing).
    pub fn plain() -> Self {
        Ui { color: false }
    }

    fn paint(&self, code: &str, s: &str) -> String {
        if self.color { format!("\x1b[{code}m{s}\x1b[0m") } else { s.to_string() }
    }
    pub fn green(&self, s: &str) -> String {
        self.paint("32", s)
    }
    pub fn red(&self, s: &str) -> String {
        self.paint("31", s)
    }
    pub fn yellow(&self, s: &str) -> String {
        self.paint("33", s)
    }
    pub fn dim(&self, s: &str) -> String {
        self.paint("2", s)
    }
    pub fn bold(&self, s: &str) -> String {
        self.paint("1", s)
    }
    pub fn cyan(&self, s: &str) -> String {
        self.paint("36", s)
    }
    pub fn cyan_bold(&self, s: &str) -> String {
        self.paint("1;36", s)
    }
    pub fn blue(&self, s: &str) -> String {
        self.paint("34", s)
    }

    /// Colour on a terminal only, and never with NO_COLOR set (any value) or TERM=dumb: for what goes
    /// to standard output (`err`: to standard error).
    pub fn for_terminal(err: bool) -> Self {
        use std::io::IsTerminal;
        let tty = if err { std::io::stderr().is_terminal() } else { std::io::stdout().is_terminal() };
        let off = std::env::var_os("NO_COLOR").is_some() || std::env::var("TERM").is_ok_and(|t| t == "dumb");
        Ui { color: tty && !off }
    }

    /// `✓ what` … `right` (right-aligned to 80 columns when it fits).
    pub fn ok(&self, what: &str, right: &str) -> String {
        format!("  {} {}", self.green("✓"), self.with_right(what, right, 4))
    }
    pub fn warn(&self, what: &str) -> String {
        format!("  {} {what}", self.yellow("⚠"))
    }

    /// `left` with `right` (dimmed) at the end of the 80-column line, or two spaces after `left`
    /// when it does not fit. `indent` is how many columns precede `left` on its line.
    pub fn with_right(&self, left: &str, right: &str, indent: usize) -> String {
        if right.is_empty() {
            return left.to_string();
        }
        let used = indent + width(left) + width(right);
        let gap = if used + 2 <= WIDTH { WIDTH - used } else { 2 };
        format!("{left}{}{}", " ".repeat(gap), self.dim(right))
    }

}

/// Display width, counting each character as one column (the text here is our own wording and
/// already-cleaned names; wide characters only make a line a little longer, never break it).
pub fn width(s: &str) -> usize {
    s.chars().count()
}

/// Elapsed time as people say it: "3s", "1m 12s".
pub fn elapsed(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 { format!("{s}s") } else { format!("{}m {:02}s", s / 60, s % 60) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_has_no_escapes_colour_has() {
        let p = Ui::plain();
        assert_eq!(p.ok("Installed htop", "change 23"), format!("  ✓ Installed htop{}change 23", " ".repeat(80 - 4 - 14 - 9)));
        assert!(!p.warn("x").contains('\x1b'));
        assert!(Ui { color: true }.ok("x", "").contains("\x1b[32m"));
    }

    #[test]
    fn lines_fit_80_columns() {
        let p = Ui::plain();
        let line = p.ok("Installed htop", "change 23 · rolled back");
        assert_eq!(width(&line), 80, "{line:?}");
        let long = p.ok(&"x".repeat(70), "change 23 · rolled back");
        assert!(long.contains("  change 23"), "falls back to two spaces when it cannot fit");
    }

    #[test]
    fn elapsed_reads_like_people_say_it() {
        assert_eq!(elapsed(Duration::from_secs(3)), "3s");
        assert_eq!(elapsed(Duration::from_secs(72)), "1m 12s");
    }
}
