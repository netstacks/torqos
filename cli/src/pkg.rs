//! `kestrel pkg`: installed software is JSON in the config repo (ADR-011). The pure parts live
//! here; `main.rs` does the I/O (reading the repo, evaluating, talking to the broker).

use anyhow::{Context, Result};

/// Which package list a command changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    System,
    User(String),
}

impl Target {
    /// Path in the config repo. `users/<name>.packages.json` is the exact shape the broker treats as
    /// user tier; `packages.json` is system tier.
    pub fn path(&self) -> String {
        match self {
            Target::System => "packages.json".into(),
            Target::User(u) => format!("users/{u}.packages.json"),
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Target::System => "system-wide".into(),
            Target::User(u) => format!("for {u}"),
        }
    }
}

/// The user names the broker accepts as user tier (broker/src/policy.rs, `is_user_data`).
pub fn valid_user(name: &str) -> bool {
    let mut c = name.chars();
    matches!(c.next(), Some('a'..='z' | '_')) && c.all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_' | '-'))
}

/// What the CLI accepts as a package name: nixpkgs attribute characters, at most one dot. Which
/// package sets are allowed, and whether the package exists and is available here, is decided by
/// the system's own module when the change is evaluated — one rule, not two copies of it.
pub fn valid_package(name: &str) -> bool {
    let parts: Vec<&str> = name.split('.').collect();
    parts.len() <= 2
        && parts.iter().all(|p| {
            let mut c = p.chars();
            matches!(c.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
                && c.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '\'' | '+' | '-'))
        })
}

pub fn parse_list(text: &str) -> Result<Vec<String>> {
    serde_json::from_str(text).context("not a JSON list of package names")
}

/// One name per line, sorted and de-duplicated, so every change is a minimal, readable diff.
pub fn render(list: &[String]) -> String {
    let mut v = list.to_vec();
    v.sort();
    v.dedup();
    if v.is_empty() {
        return "[]\n".into();
    }
    format!("{}\n", serde_json::to_string_pretty(&v).expect("a list of strings serialises"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edit {
    Add,
    Remove,
}

/// The new list, and the names that actually change it (adding what is there, or removing what
/// is not, is skipped).
pub fn edit(current: &[String], names: &[String], how: Edit) -> (Vec<String>, Vec<String>) {
    let mut list = current.to_vec();
    let mut changed = Vec::new();
    for n in names {
        let present = list.contains(n);
        match how {
            Edit::Add if !present => list.push(n.clone()),
            Edit::Remove if present => list.retain(|x| x != n),
            _ => continue,
        }
        if !changed.contains(n) {
            changed.push(n.clone());
        }
    }
    (list, changed)
}

/// A failed pre-check (the Nix evaluation of the changed configuration), as a person needs it:
/// what is wrong and what to do. Nothing has been proposed at this point.
pub fn explain_check_error(msg: &str) -> String {
    let name = msg.split('\'').nth(1).unwrap_or_default();
    if msg.contains("is not a package in nixpkgs") {
        format!("\"{name}\" isn't a package. Find the right name: kestrel pkg search {name}")
    } else if msg.contains("is not available here") {
        format!("{name} can't be installed here: it is unfree, broken, insecure, or not built for this machine.")
    } else if msg.contains("is not a package name") {
        format!("\"{name}\" isn't a package name. Find one: kestrel pkg search <words>")
    } else {
        format!("This change wouldn't work, so nothing was changed: {msg}")
    }
}

/// What a broker refusal while proposing or applying means. `what` is the change in words
/// ("install htop"); whether the machine is as it was is decided by the caller from `status`.
pub fn explain_apply_error(what: &str, msg: &str) -> String {
    // Specific phrases first: the interrupted-change message also mentions "rebuilds".
    if msg.contains("interrupted and must be reconciled") {
        format!("Couldn't {what}: an earlier change was interrupted. Finish it first: kestrel status")
    } else if msg.contains("does not apply cleanly") {
        format!("Couldn't {what}: the configuration changed in the meantime. Try again.")
    } else if msg.contains("rebuild") {
        format!("Couldn't {what}: building the new system failed.")
    } else {
        format!("Couldn't {what}: {msg}")
    }
}

/// An AuthorizedKeysFile pattern as sshd expands it for one user: `%%`, `%h` (home), `%u` (name)
/// and `%U` (uid), in one left-to-right pass; a relative result is relative to the home directory.
/// Any other token is refused rather than guessed at.
pub fn expand_keys_pattern(pattern: &str, home: &str, user: &str, uid: &str) -> Result<String> {
    let mut out = String::new();
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('%') => out.push('%'),
            Some('h') => out.push_str(home),
            Some('u') => out.push_str(user),
            Some('U') => out.push_str(uid),
            other => anyhow::bail!("AuthorizedKeysFile pattern '{pattern}' uses %{} — not understood here", other.map(String::from).unwrap_or_default()),
        }
    }
    Ok(if out.starts_with('/') { out } else { format!("{}/{out}", home.trim_end_matches('/')) })
}

/// What an authorized_keys line's options mean for who gets in, as a prefix for its description:
/// a `cert-authority` line is not one key but everyone holding a certificate that CA signed.
pub fn key_options_note(line: &str) -> String {
    let line = line.trim();
    let is_key_type = |t: &str| t.starts_with("ssh-") || t.starts_with("ecdsa-") || t.starts_with("sk-");
    if line.split_whitespace().next().is_some_and(is_key_type) {
        return String::new();
    }
    // Read the options as sshd does: they end at the first space or tab outside double quotes, and
    // are separated by commas outside quotes; names are case-insensitive.
    let mut names = Vec::new();
    let (mut current, mut quoted, mut end) = (String::new(), false, line.len());
    for (i, c) in line.char_indices() {
        match c {
            '"' => quoted = !quoted,
            ' ' | '\t' if !quoted => {
                end = i;
                break;
            }
            ',' if !quoted => {
                names.push(std::mem::take(&mut current));
                continue;
            }
            _ => {}
        }
        if !quoted && c != '"' {
            current.push(c);
        }
    }
    names.push(current);
    let names: Vec<String> = names.iter().map(|o| o.split('=').next().unwrap_or("").trim().to_ascii_lowercase()).collect();
    let opts: String = line[..end].chars().take(60).collect();
    if names.iter().any(|n| n == "cert-authority") {
        "CERTIFICATE AUTHORITY — admits anyone with a certificate it signed: ".into()
    } else if names.iter().any(|n| n == "no-touch-required") {
        format!("no touch needed on the hardware key ({opts}): ")
    } else {
        format!("with options ({opts}): ")
    }
}

/// What an update moves, from the old and new flake.lock: each changed input with its date before
/// and after ("kestrel", "2026-10-29", "2026-11-04"). Unknown dates are "?".
pub fn lock_changes(old: &str, new: &str) -> Vec<(String, String, String)> {
    let parse = |s: &str| serde_json::from_str::<serde_json::Value>(s).unwrap_or_default();
    let (old, new) = (parse(old), parse(new));
    let secs = |v: &serde_json::Value| v["locked"]["lastModified"].as_u64();
    // Same day before and after (a path input during development): the time tells them apart.
    let when = |v: Option<u64>, same_day: bool| match v {
        Some(s) if same_day => format!("{} {:02}:{:02}", ymd(s), s % 86_400 / 3_600, s % 3_600 / 60),
        Some(s) => ymd(s),
        None => "?".into(),
    };
    let mut out = Vec::new();
    if let Some(nodes) = new["nodes"].as_object() {
        for (name, node) in nodes {
            let before = &old["nodes"][name];
            if name != "root" && node["locked"] != before["locked"] {
                let (a, b) = (secs(before), secs(node));
                let same_day = matches!((a, b), (Some(a), Some(b)) if ymd(a) == ymd(b));
                out.push((name.clone(), when(a, same_day), when(b, same_day)));
            }
        }
    }
    out
}

/// A Unix time as a UTC date, "2026-10-30" (civil-from-days, no time-zone database needed).
pub fn ymd(secs: u64) -> String {
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}")
}

/// The inputs `nix flake update` reports ("• Updated|Added|Removed input 'x': ..." plus detail
/// lines), for the change's reason. Only a summary: the diff the person approves is what matters.
pub fn moved_inputs(nix_stderr: &str) -> Vec<String> {
    nix_stderr
        .lines()
        .filter_map(|l| {
            let l = l.trim().strip_prefix("• ")?;
            let (verb, rest) = l.split_once(" input '")?;
            let name = rest.split('\'').next()?;
            match verb {
                "Updated" => Some(name.to_string()),
                "Added" | "Removed" => Some(format!("{name} ({})", verb.to_lowercase())),
                _ => None,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn paths_match_the_broker_tiers() {
        assert_eq!(Target::System.path(), "packages.json");
        assert_eq!(Target::User("viper".into()).path(), "users/viper.packages.json");
    }

    #[test]
    fn user_names() {
        for ok in ["viper", "_svc", "a-b_9"] {
            assert!(valid_user(ok), "{ok}");
        }
        for bad in ["", "Viper", "-x", "a.b", "a/b", "..", "ca sey"] {
            assert!(!valid_user(bad), "{bad}");
        }
    }

    #[test]
    fn package_names() {
        for ok in ["htop", "python3Packages.requests", "gcc13", "g++", "vim_configurable", "font-awesome", "xorg'"] {
            assert!(valid_package(ok), "{ok}");
        }
        // Injection into the nix expression, path tricks, too deep: refused before anything runs.
        for bad in ["", "a.b.c", "\"htop\"", "htop;", "${x}", "a b", "../x", ".htop", "htop.", "1password"] {
            assert!(!valid_package(bad), "{bad}");
        }
    }

    #[test]
    fn render_is_sorted_deduplicated_one_per_line() {
        assert_eq!(render(&v(&["ripgrep", "htop", "htop"])), "[\n  \"htop\",\n  \"ripgrep\"\n]\n");
        assert_eq!(render(&[]), "[]\n");
        assert_eq!(parse_list(&render(&v(&["b", "a"]))).unwrap(), v(&["a", "b"]));
    }

    #[test]
    fn parse_rejects_non_lists() {
        assert!(parse_list("{\"htop\": true}").is_err());
        assert!(parse_list("[1, 2]").is_err());
        assert_eq!(parse_list("[]").unwrap(), Vec::<String>::new());
    }

    #[test]
    fn edit_reports_only_real_changes() {
        let (l, c) = edit(&v(&["htop"]), &v(&["htop", "ripgrep", "ripgrep"]), Edit::Add);
        assert_eq!((l, c), (v(&["htop", "ripgrep"]), v(&["ripgrep"])));
        let (l, c) = edit(&v(&["htop", "ripgrep"]), &v(&["htop", "nope"]), Edit::Remove);
        assert_eq!((l, c), (v(&["ripgrep"]), v(&["htop"])));
        let (_, c) = edit(&v(&["htop"]), &v(&["htop"]), Edit::Add);
        assert!(c.is_empty());
    }

    #[test]
    fn dates_and_lock_changes() {
        assert_eq!(ymd(0), "1970-01-01");
        assert_eq!(ymd(1_793_386_952), "2026-10-30");
        assert_eq!(ymd(951_782_400), "2000-02-29");
        let lock = |kt: u64, nt: u64| {
            format!(r#"{{"nodes":{{"kestrel":{{"locked":{{"lastModified":{kt},"narHash":"a{kt}"}}}},"nixpkgs":{{"locked":{{"lastModified":{nt}}}}},"root":{{}}}}}}"#)
        };
        let moved = lock_changes(&lock(1_793_000_000, 1_790_000_000), &lock(1_793_386_952, 1_790_000_000));
        assert_eq!(moved, vec![("kestrel".to_string(), "2026-10-26".to_string(), "2026-10-30".to_string())]);
        let same = lock_changes(&lock(1_793_380_000, 1), &lock(1_793_386_952, 1));
        assert_eq!(same[0].1, "2026-10-30 17:06"); // date -u -d @1793380000
        assert_eq!(same[0].2, "2026-10-30 19:02");
    }

    #[test]
    fn errors_say_what_to_do() {
        assert_eq!(
            explain_check_error("kestrel.packages: 'htpo' (user viper) is not a package in nixpkgs"),
            "\"htpo\" isn't a package. Find the right name: kestrel pkg search htpo"
        );
        assert!(explain_check_error("kestrel.packages: 'vscode' (user viper) is not available here: unfree, ...")
            .starts_with("vscode can't be installed here"));
        assert!(explain_check_error("something else").starts_with("This change wouldn't work"));
        assert_eq!(
            explain_apply_error("install htop", "apply rolled back because the rebuild failed: rebuild command failed"),
            "Couldn't install htop: building the new system failed."
        );
        assert!(explain_apply_error("x", "diff does not apply cleanly to the current config").contains("Try again"));
        let interrupted = "the apply of #2 was interrupted and must be reconciled first: run `kestrel-broker reconcile --approved-by <name>` (it resets the config to the last ledgered commit and rebuilds)";
        assert!(explain_apply_error("x", interrupted).contains("an earlier change was interrupted"));
    }

    #[test]
    fn keys_patterns_expand_like_sshd() {
        let e = |p| expand_keys_pattern(p, "/home/viper", "viper", "1000");
        assert_eq!(e("%h/.ssh/authorized_keys").unwrap(), "/home/viper/.ssh/authorized_keys");
        assert_eq!(e("/etc/ssh/authorized_keys.d/%u").unwrap(), "/etc/ssh/authorized_keys.d/viper");
        assert_eq!(e("/keys/%U").unwrap(), "/keys/1000");
        assert_eq!(e("/odd/%%h").unwrap(), "/odd/%h", "one pass: %% is a literal %");
        assert_eq!(e(".ssh/authorized_keys2").unwrap(), "/home/viper/.ssh/authorized_keys2", "relative to home");
        assert!(e("/keys/%i").is_err() && e("/keys/%").is_err());
    }

    #[test]
    fn key_option_notes() {
        assert_eq!(key_options_note("ssh-ed25519 AAAA me@mac"), "");
        assert!(key_options_note("cert-authority ssh-ed25519 AAAA ca").starts_with("CERTIFICATE AUTHORITY"));
        assert!(key_options_note("cert-authority,principals=\"a b\" ecdsa-sha2-nistp256 AAAA").starts_with("CERTIFICATE"));
        let r = key_options_note("command=\"echo hi there\",from=\"10.0.0.1\" ssh-rsa AAAA x");
        assert!(r.starts_with("with options (command=\"echo hi there\",from=\"10.0.0.1\")"), "{r}");
        // The reviewer's ways to make a CA look like a restricted key — sshd accepts all three.
        for ca in [
            "cert-authority\tssh-ed25519 AAAA ca",
            "command=\"x ssh-\",cert-authority ssh-ed25519 AAAA ca",
            "CERT-AUTHORITY ssh-ed25519 AAAA ca",
        ] {
            assert!(key_options_note(ca).starts_with("CERTIFICATE AUTHORITY"), "{ca:?}");
        }
        // ...and a CA mentioned only inside a quoted command is not one.
        assert!(key_options_note("command=\"a,cert-authority,b\" ssh-ed25519 AAAA").starts_with("with options"));
        assert!(key_options_note("no-touch-required sk-ssh-ed25519@openssh.com AAAA").starts_with("no touch"));
    }

    #[test]
    fn summarises_moved_inputs() {
        let out = "fetching...\n\
                   • Updated input 'kestrel':\n    'path:/a?narHash=x' (2026-10-30)\n  → 'path:/a?narHash=y'\n\
                   • Added input 'kestrel/hw':\n    'github:x/y'\n\
                   • Removed input 'old'\n\
                   warning: something\n";
        assert_eq!(moved_inputs(out), v(&["kestrel", "kestrel/hw (added)", "old (removed)"]));
        assert!(moved_inputs("nothing to see").is_empty());
    }
}
