//! `kestrel status`: is this machine as its history says — in words (docs/CLI-DESIGN.md), from the
//! broker's `status --json` facts, never its problem text. Pure, for snapshot tests.

use crate::history::one_line;
use crate::ui::Ui;
use serde_json::Value;

/// What `kestrel status --repair` can do about what was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fix {
    /// Nothing to do.
    Nothing,
    /// `--repair` returns the machine to its ledgered system (the broker's `repair`).
    Repair,
    /// A person has to look: the configuration was edited outside Kestrel, or the history is damaged.
    Person,
}

pub struct Report {
    pub text: String,
    pub exit: i32,
    pub fix: Fix,
}

/// `v`: the broker's `status --json`. `last`: the latest change, for the all-well line.
pub fn render(v: &Value, last: Option<&str>, ui: &Ui) -> Report {
    if v["busy"].is_string() {
        return Report {
            text: format!("  {} A change is being applied right now. Check again when it is done.\n", ui.dim("⋯")),
            exit: 75,
            fix: Fix::Nothing,
        };
    }
    let s = |k: &str| v[k].as_str();
    let mut found: Vec<(String, Fix)> = Vec::new();
    if s("ledger_error").is_some() {
        found.push(("The history (ledger) is damaged, so nothing can be checked or changed.".into(), Fix::Person));
    }
    if let Some(i) = v["interrupted"].as_object() {
        let op = one_line(i.get("op").and_then(|o| o.as_str()).unwrap_or("change"), 20);
        let target = i.get("target").and_then(|t| t.as_u64()).map(|t| t.to_string()).unwrap_or_else(|| "?".into());
        found.push((format!("A change was interrupted (the {op} of change {target})."), Fix::Repair));
    }
    match (s("head"), s("ledgered_commit")) {
        (None, Some(_)) => found.push(("The configuration can't be read.".into(), Fix::Person)),
        (Some(h), Some(l)) if h != l => found.push(("The configuration was changed outside Kestrel.".into(), Fix::Person)),
        _ => {}
    }
    if v["dirty"] == true {
        found.push(("The configuration has edits made outside Kestrel.".into(), Fix::Person));
    }
    if v["unrestored"].is_string() {
        found.push(("A change failed, and the machine couldn't be put back exactly as it was.".into(), Fix::Repair));
    }
    if let Some(want) = s("ledgered_system") {
        if s("running_system") != Some(want) {
            found.push(("It is running a system Kestrel didn't build (switched outside Kestrel).".into(), Fix::Repair));
        }
        if s("next_boot").is_some_and(|n| n != want) {
            found.push(("The next boot would start a system Kestrel didn't build.".into(), Fix::Repair));
        }
    }

    if found.is_empty() {
        let text = if v["checked_running_system"] == true {
            let last = last.map(|l| format!("    {}\n", ui.dim(&format!("Last change: {}", one_line(l, 60))))).unwrap_or_default();
            format!("{}\n{last}", ui.ok("This machine is exactly as its history says.", ""))
        } else {
            format!(
                "{}\n    {}\n",
                ui.ok("This machine is as its history says, as far as recorded.", ""),
                ui.dim("(the running system is checked from the next change on)")
            )
        };
        return Report { text, exit: 0, fix: Fix::Nothing };
    }
    let fix = if found.iter().any(|(_, f)| *f == Fix::Person) { Fix::Person } else { Fix::Repair };
    let mut text = format!("{}\n", ui.warn("This machine is not as its history says."));
    for (what, _) in &found {
        text.push_str(&format!("    · {what}\n"));
    }
    text.push_str(&match fix {
        Fix::Repair => format!("    To return it to its history:  kestrel status --repair   {}\n", ui.dim("(asks first)")),
        _ => "    This needs a person: look at /var/lib/kestrel/config (--repair won't touch it).\n".to_string(),
    });
    Report { text, exit: 1, fix }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ok() -> Value {
        json!({ "ok": true, "busy": null, "head": "c1", "ledgered_commit": "c1", "dirty": false,
                "interrupted": null, "unrestored": null, "running_system": "/nix/store/a",
                "next_boot": "/nix/store/a", "ledgered_system": "/nix/store/a", "checked_running_system": true })
    }

    #[test]
    fn all_well() {
        let r = render(&ok(), Some("Install htop for viper"), &Ui::plain());
        assert_eq!((r.exit, r.fix), (0, Fix::Nothing));
        assert_eq!(r.text, "  ✓ This machine is exactly as its history says.\n    Last change: Install htop for viper\n");
    }

    #[test]
    fn switched_outside_kestrel_is_repairable() {
        let mut v = ok();
        v["running_system"] = json!("/nix/store/stock-nixos");
        v["next_boot"] = json!("/nix/store/stock-nixos");
        let r = render(&v, None, &Ui::plain());
        assert_eq!((r.exit, r.fix), (1, Fix::Repair));
        let expected = "  ⚠ This machine is not as its history says.\n    · It is running a system Kestrel didn't build (switched outside Kestrel).\n    · The next boot would start a system Kestrel didn't build.\n    To return it to its history:  kestrel status --repair   (asks first)\n";
        assert_eq!(r.text, expected);
    }

    #[test]
    fn an_edited_configuration_needs_a_person() {
        let mut v = ok();
        v["dirty"] = json!(true);
        v["running_system"] = json!("/nix/store/other");
        let r = render(&v, None, &Ui::plain());
        assert_eq!((r.exit, r.fix), (1, Fix::Person), "repair would build the edit: a person decides");
        assert!(r.text.contains("needs a person"), "{}", r.text);
    }

    #[test]
    fn busy_and_unrecorded() {
        let r = render(&json!({ "ok": false, "busy": "123 (nixos-rebuild)", "problems": [] }), None, &Ui::plain());
        assert_eq!((r.exit, r.fix), (75, Fix::Nothing));
        let mut v = ok();
        v["checked_running_system"] = json!(false);
        v["ledgered_system"] = json!(null);
        assert!(render(&v, None, &Ui::plain()).text.contains("as far as recorded"));
    }

    #[test]
    fn interrupted_is_repairable() {
        let mut v = ok();
        v["interrupted"] = json!({ "op": "apply", "target": 12 });
        let r = render(&v, None, &Ui::plain());
        assert_eq!(r.fix, Fix::Repair);
        assert!(r.text.contains("A change was interrupted (the apply of change 12)."), "{}", r.text);
    }
}
