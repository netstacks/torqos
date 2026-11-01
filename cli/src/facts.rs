//! What the machine is and how it is doing — `show version`, `show system uptime`, `show
//! interfaces`, `show system storage` — as a network device answers them. The facts are read from
//! the running system (shell.rs: /proc, `ip -j`, `df`); laying them out is pure, and tested here.

use crate::ui::Ui;

/// The running machine, read at the moment of asking.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Facts {
    /// NixOS's release (`26.05.20261029.6f06d20`).
    pub nixos: String,
    /// The Linux kernel's release.
    pub kernel: String,
    /// Seconds since the machine started.
    pub uptime: u64,
    /// The load averages over 1, 5 and 15 minutes.
    pub load: [f64; 3],
    pub interfaces: Vec<Interface>,
    pub filesystems: Vec<Filesystem>,
    /// The system versions kept, each one bootable.
    pub generations: usize,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Interface {
    pub name: String,
    /// As the kernel says it: up, down, unknown (a loopback).
    pub state: String,
    pub mac: String,
    /// Addresses with their prefix length (`192.168.64.4/24`).
    pub addresses: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Filesystem {
    pub device: String,
    pub mount: String,
    /// In bytes.
    pub size: u64,
    pub used: u64,
    pub free: u64,
}

/// `show version`: what this machine runs. `software`: where TorqOS comes from and when it is from
/// (the configuration's lock, as `show configuration` names it).
pub fn version(f: &Facts, software: &str) -> String {
    rows(&[
        ("TorqOS", software.to_string()),
        ("NixOS", f.nixos.clone()),
        ("Linux kernel", f.kernel.clone()),
        ("torqcli", env!("CARGO_PKG_VERSION").to_string()),
    ])
}

/// `show system uptime`: how long the machine has run, its load, and its last change.
pub fn uptime(f: &Facts, last: Option<&str>, ui: &Ui) -> String {
    let mut out = rows(&[("Up", duration(f.uptime)), ("Load", format!("{:.2} {:.2} {:.2} (over 1, 5 and 15 minutes)", f.load[0], f.load[1], f.load[2]))]);
    if let Some(l) = last {
        out.push_str(&format!("{}\n", ui.line("Last change   ", l)));
    }
    out
}

/// `show interfaces`: each network interface, its state, addresses and hardware address. Data:
/// never wrapped (a filter reads whole lines).
pub fn interfaces(f: &Facts) -> String {
    let w = |get: &dyn Fn(&Interface) -> usize, head: &str| f.interfaces.iter().map(get).max().unwrap_or(0).max(head.len());
    let (wn, ws) = (w(&|i| i.name.len(), "Interface"), w(&|i| i.state.len(), "State"));
    let wa = w(&|i| i.addresses.iter().map(String::len).max().unwrap_or(1), "Address");
    let mut out = format!("{:<wn$}  {:<ws$}  {:<wa$}  {}\n", "Interface", "State", "Address", "Hardware address");
    for i in &f.interfaces {
        let first = i.addresses.first().map_or("-", String::as_str);
        out.push_str(&format!("{:<wn$}  {:<ws$}  {:<wa$}  {}\n", i.name, i.state, first, i.mac).trim_end().to_string());
        out.push('\n');
        for a in i.addresses.iter().skip(1) {
            out.push_str(&format!("{:<wn$}  {:<ws$}  {a}\n", "", ""));
        }
    }
    out
}

/// `show system storage`: each filesystem's size and use, and the system versions kept on it.
pub fn storage(f: &Facts, ui: &Ui) -> String {
    let rows_: Vec<[String; 6]> = f
        .filesystems
        .iter()
        .map(|s| [s.device.clone(), size(s.size), size(s.used), size(s.free), format!("{}%", (s.used * 100).checked_div(s.size).unwrap_or(0)), s.mount.clone()])
        .collect();
    let head = ["Filesystem", "Size", "Used", "Free", "Use", "Mounted on"];
    let width = |c: usize| rows_.iter().map(|r| r[c].len()).max().unwrap_or(0).max(head[c].len());
    let line = |r: &[&str; 6]| format!("{:<a$}  {:>b$}  {:>c$}  {:>d$}  {:>e$}  {}\n", r[0], r[1], r[2], r[3], r[4], r[5], a = width(0), b = width(1), c = width(2), d = width(3), e = width(4));
    let mut out = line(&head);
    for r in &rows_ {
        out.push_str(&line(&[&r[0], &r[1], &r[2], &r[3], &r[4], &r[5]]));
    }
    let kept = format!(
        "System versions kept: {} (each one a configuration the machine can start; the regular clean-up, nix.gc in the configuration, removes old ones).",
        f.generations
    );
    out.push_str(&format!("{}\n", ui.line("", &kept)));
    out
}

/// Two columns: a name, then its value.
fn rows(r: &[(&str, String)]) -> String {
    let w = r.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
    r.iter().map(|(k, v)| format!("{k:<w$}  {v}\n")).collect()
}

/// `1 hour 32 minutes`, `3 days 2 hours`, `45 seconds`: the two largest parts.
pub fn duration(secs: u64) -> String {
    let parts = [(secs / 86_400, "day"), (secs / 3600 % 24, "hour"), (secs / 60 % 60, "minute"), (secs % 60, "second")];
    let shown: Vec<String> = parts.iter().skip_while(|(n, _)| *n == 0).take(2).filter(|(n, _)| *n > 0).map(|(n, u)| format!("{n} {u}{}", if *n == 1 { "" } else { "s" })).collect();
    if shown.is_empty() { "0 seconds".into() } else { shown.join(" ") }
}

/// Bytes as people read a disk's size: 62G, 87M, 1.5T.
pub fn size(bytes: u64) -> String {
    let units = ["B", "K", "M", "G", "T", "P"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < units.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if v < 10.0 && u > 0 { format!("{v:.1}{}", units[u]) } else { format!("{v:.0}{}", units[u]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> Facts {
        Facts {
            nixos: "26.05.20261029.6f06d20".into(),
            kernel: "6.18.59".into(),
            uptime: 5519,
            load: [0.1, 0.25, 1.0],
            interfaces: vec![
                Interface { name: "enp0s1".into(), state: "up".into(), mac: "ae:21:5a:0b:a0:55".into(), addresses: vec!["192.168.64.4/24".into(), "fd5c::1/64".into()] },
                Interface { name: "lo".into(), state: "unknown".into(), mac: "00:00:00:00:00:00".into(), addresses: vec![] },
            ],
            filesystems: vec![Filesystem { device: "/dev/vda2".into(), mount: "/".into(), size: 62 << 30, used: 33 << 30, free: 27 << 30 }],
            generations: 23,
        }
    }

    #[test]
    fn what_the_machine_runs_and_how_it_is() {
        let f = facts();
        let v = version(&f, "torq 2026-11-01");
        assert!(v.starts_with("TorqOS        torq 2026-11-01\nNixOS         26.05.20261029.6f06d20\nLinux kernel  6.18.59\ntorqcli       "), "{v}");
        let u = uptime(&f, Some("#85, 2026-11-01 20:10: M2"), &Ui::plain());
        assert_eq!(u, "Up    1 hour 31 minutes\nLoad  0.10 0.25 1.00 (over 1, 5 and 15 minutes)\nLast change   #85, 2026-11-01 20:10: M2\n");
        let i = interfaces(&f);
        assert_eq!(i, "Interface  State    Address          Hardware address\nenp0s1     up       192.168.64.4/24  ae:21:5a:0b:a0:55\n                    fd5c::1/64\nlo         unknown  -                00:00:00:00:00:00\n");
        let s = storage(&f, &Ui::plain());
        assert!(s.starts_with("Filesystem  Size  Used  Free  Use  Mounted on\n/dev/vda2    62G   33G   27G  53%  /\nSystem versions kept: 23"), "{s}");
        assert_eq!((duration(45), duration(3600), duration(90061), duration(0)), ("45 seconds".into(), "1 hour".into(), "1 day 1 hour".into(), "0 seconds".into()));
        assert_eq!((size(512), size(87 << 20), size(1536 << 30)), ("512B".into(), "87M".into(), "1.5T".into()));
    }
}
