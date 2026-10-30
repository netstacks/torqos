//! How `kestrel` looks (docs/CLI-DESIGN.md): plain language, one visual language, colour with
//! meaning only — and plain text when colour is off (NO_COLOR, not a terminal), so nothing depends
//! on colour to be understood. Everything here is a pure function of its input, for snapshot tests.

use std::io::IsTerminal;
use std::time::Duration;

pub const WIDTH: usize = 80;

#[derive(Clone, Copy)]
pub struct Ui {
    pub color: bool,
}

impl Ui {
    /// Colour on a terminal unless NO_COLOR is set (https://no-color.org) or TERM=dumb.
    pub fn detect() -> Self {
        let color = std::io::stdout().is_terminal()
            && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty())
            && std::env::var("TERM").map_or(true, |t| t != "dumb");
        Ui { color }
    }

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

    /// `✓ what` … `right` (right-aligned to 80 columns when it fits).
    pub fn ok(&self, what: &str, right: &str) -> String {
        format!("  {} {}", self.green("✓"), self.with_right(what, right, 4))
    }
    pub fn fail(&self, what: &str) -> String {
        format!("  {} {what}", self.red("✗"))
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

    /// The heading of a proposed change: what it is, and (for system changes) that it affects the
    /// whole machine.
    pub fn heading(&self, title: &str, system: bool) -> String {
        if !system {
            return format!("  {}", self.bold(title));
        }
        let tag = "system change";
        format!("  {}{}{}", self.bold(title), pad_to(title, 2, tag), self.yellow(tag))
    }
}

/// Spaces that put a right-hand tag at the end of the line after `left` (indented by `indent`).
fn pad_to(left: &str, indent: usize, right: &str) -> String {
    let used = indent + width(left) + width(right);
    " ".repeat(if used + 2 <= WIDTH { WIDTH - used } else { 2 })
}

/// Display width, counting each character as one column (the text here is our own wording and
/// already-cleaned names; wide characters only make a line a little longer, never break it).
pub fn width(s: &str) -> usize {
    s.chars().count()
}

/// A row in a change summary: `+ name version` and a dimmed description, in aligned columns.
pub fn change_row(ui: &Ui, sign: char, name: &str, version: &str, description: &str) -> String {
    let left = format!("{sign} {name}{}", if version.is_empty() { String::new() } else { format!(" {version}") });
    let desc = crate::history::one_line(description, WIDTH.saturating_sub(4 + 22 + 1));
    format!("    {left:<22} {}", ui.dim(&desc)).trim_end().to_string()
}

/// "change 23 · undo: kestrel undo 23"
pub fn change_ref(seq: u64) -> String {
    format!("change {seq} · undo: kestrel undo {seq}")
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
        assert_eq!(p.ok("Installed htop", &change_ref(23)), format!("  ✓ Installed htop{}change 23 · undo: kestrel undo 23", " ".repeat(80 - 4 - 14 - 33)));
        assert!(!p.fail("x").contains('\x1b'));
        assert!(Ui { color: true }.ok("x", "").contains("\x1b[32m"));
    }

    #[test]
    fn lines_fit_80_columns() {
        let p = Ui::plain();
        let line = p.ok("Installed htop", &change_ref(23));
        assert_eq!(width(&line), 80, "{line:?}");
        let long = p.ok(&"x".repeat(70), &change_ref(23));
        assert!(long.contains("  change 23"), "falls back to two spaces when it cannot fit");
        let h = p.heading("Install for everyone on this machine", true);
        assert_eq!(width(&h), 80, "{h:?}");
        assert!(h.ends_with("system change"));
    }

    #[test]
    fn change_rows_align() {
        let p = Ui::plain();
        assert_eq!(change_row(&p, '+', "htop", "3.4.1", "Interactive process viewer"), "    + htop 3.4.1           Interactive process viewer");
        assert_eq!(change_row(&p, '-', "ripgrep", "", ""), "    - ripgrep");
    }

    #[test]
    fn elapsed_reads_like_people_say_it() {
        assert_eq!(elapsed(Duration::from_secs(3)), "3s");
        assert_eq!(elapsed(Duration::from_secs(72)), "1m 12s");
    }
}
