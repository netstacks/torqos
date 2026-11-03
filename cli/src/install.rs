//! The ISO installer (ADR-031). On the TorqOS installer's console, and nowhere else: lists the
//! disks TorqOS could go on, asks for one and for its name again, writes the TorqOS image to it —
//! the same disk CI builds and tests — reads it back, and leaves this machine's hardware, as NixOS
//! detects it, on the new disk for first-boot setup (setup.rs `hardware_file`). Nothing else runs
//! on the installer: nobody can log in, nothing listens (modules/installer.nix).

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// What the installer carries (modules/installer.nix): `image.raw.zst`, the image compressed;
/// `size` and `sha256`, the image's once decompressed; `file-systems.nix`, the image's disk.
const INSTALLER: &str = "/etc/torq/installer";
const BIN: &str = "/run/current-system/sw/bin";
/// Where, inside the new disk's root, first-boot setup finds the hardware detected here.
pub const DETECTED: &str = "var/lib/torq/install/hardware-configuration.nix";
const WELCOME: &str = "Welcome to the TorqOS installer. It puts TorqOS on one of this machine's disks, erasing everything on it.";
/// The image's root file system's label (hosts/image/hardware-configuration.nix).
const ROOT_LABEL: &str = "nixos";
/// Its boot partition's (the same file).
const BOOT_LABEL: &str = "ESP";
/// What is written at a time: and so how often the progress line moves.
const CHUNK: usize = 4 << 20;
/// The end of a disk that is zeroed: what an earlier life left there (a partition table's backup,
/// a RAID or ZFS label) would otherwise outlive the install and confuse what reads the disk.
const TAIL: u64 = 1 << 20;

/// A disk TorqOS could be installed on.
#[derive(Debug, PartialEq)]
pub struct Disk {
    /// Its kernel name: `sda`, `nvme0n1`, `vda`.
    pub name: String,
    pub bytes: u64,
    /// Vendor and model, as the disk says; may be empty.
    pub model: String,
    /// How it is attached.
    pub bus: &'static str,
    /// Its partitions now, by name.
    pub parts: Vec<String>,
    /// Which disk it is, as the kernel numbers them: its `diskseq` (never reused while the machine
    /// runs: a disk unplugged and another plugged in under the same name has another) and its
    /// device number (`major:minor`). Checked again once the disk is open for writing.
    pub seq: String,
    pub dev: String,
}

/// Block devices that are never a disk to install to: loop, RAM and compressed-RAM devices,
/// optical drives, device-mapper and RAID devices, floppies, network and test block devices, and
/// an eMMC's boot and replay-protected areas (mmcblk0boot0, mmcblk0rpmb), which are not its disk.
fn not_a_disk(name: &str) -> bool {
    ["loop", "ram", "zram", "sr", "dm-", "md", "fd", "nbd", "mtdblock", "nullb"].iter().any(|p| name.starts_with(p))
        || name.ends_with("rpmb")
        || (name.starts_with("mmcblk") && name.contains("boot"))
}

/// How a disk is attached, from where it is in `/sys/devices`.
fn bus(real: &Path) -> &'static str {
    let s = real.to_string_lossy();
    // A USB disk is a USB disk, whatever bridge it has (a SATA one shows `/ata` too).
    if s.contains("/usb") {
        "USB"
    } else if s.contains("/nvme") {
        "NVMe"
    } else if s.contains("/virtio") {
        "virtio"
    } else if s.contains("/mmc") {
        "SD/eMMC"
    } else if s.contains("/ata") {
        "SATA"
    } else {
        "disk"
    }
}

/// The disks in use, from `mountinfo` (/proc/self/mountinfo) and `sys` (/sys): every disk holding
/// something mounted — the installer's own medium among them — and every disk under a device that
/// does (a loop device's file, a device-mapper or RAID device's disks).
pub fn busy(sys: &Path, mountinfo: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in mountinfo.lines() {
        // The third field is the device, major:minor; 0 is no device (proc, tmpfs, overlay …).
        match line.split(' ').nth(2) {
            Some(dev) if !dev.starts_with("0:") => mark(&sys.join("dev/block").join(dev), &mut out, 0),
            _ => {}
        }
    }
    out
}

/// `dev` (a block device's directory in /sys) and what it is made of, as their disks' names.
fn mark(dev: &Path, out: &mut BTreeSet<String>, depth: usize) {
    let Ok(real) = std::fs::canonicalize(dev) else { return };
    // A partition's disk is the directory it is in.
    let disk = if real.join("partition").exists() { real.parent() } else { Some(real.as_path()) };
    if let Some(name) = disk.and_then(Path::file_name) {
        out.insert(name.to_string_lossy().into_owned());
    }
    if depth < 8 {
        if let Ok(slaves) = std::fs::read_dir(real.join("slaves")) {
            for s in slaves.flatten() {
                mark(&s.path(), out, depth + 1);
            }
        }
    }
}

/// Whether anything is built on `dir` (a disk's or a partition's directory in /sys): a
/// device-mapper or RAID device it is part of.
fn held(dir: &Path) -> bool {
    std::fs::read_dir(dir.join("holders")).is_ok_and(|mut r| r.next().is_some())
}

/// The disks TorqOS could be installed on, from `sys` (/sys): whole, writable disks with a size,
/// none in use (`busy`) or part of anything (an LVM volume group, a RAID array), none of the
/// paths an NVMe disk with several controllers hides behind its own (`hidden`).
pub fn disks(sys: &Path, busy: &BTreeSet<String>) -> Result<Vec<Disk>> {
    let block = sys.join("block");
    let mut names: Vec<String> = std::fs::read_dir(&block).with_context(|| format!("reading {}", block.display()))?.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    let mut out = Vec::new();
    for name in names {
        let d = block.join(&name);
        let read = |f: &str| std::fs::read_to_string(d.join(f)).map(|s| s.trim().to_string()).unwrap_or_default();
        if not_a_disk(&name) || busy.contains(&name) || read("ro") == "1" || read("hidden") == "1" {
            continue;
        }
        // The image's partition table is in 512-byte sectors: on a disk with larger ones (a 4Kn
        // drive, some USB bridges) the kernel would find no partitions in it.
        if !matches!(read("queue/logical_block_size").as_str(), "" | "512") {
            continue;
        }
        let parts: Vec<PathBuf> = std::fs::read_dir(&d).map(|r| r.flatten().map(|e| e.path()).filter(|p| p.file_name().is_some_and(|f| f.to_string_lossy().starts_with(&name)) && p.join("partition").exists()).collect()).unwrap_or_default();
        if held(&d) || parts.iter().any(|p| held(p)) {
            continue;
        }
        // In 512-byte sectors, whatever the disk's own.
        let bytes = read("size").parse::<u64>().unwrap_or(0) * 512;
        if bytes == 0 {
            continue;
        }
        // "ATA" is a SATA disk's vendor as Linux reads it: not the maker.
        let vendor = read("device/vendor");
        let model = [if vendor == "ATA" { String::new() } else { vendor }, read("device/model")].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" ");
        let bus = bus(&std::fs::canonicalize(&d).unwrap_or(d.clone()));
        let (seq, dev) = (read("diskseq"), read("dev"));
        let mut parts: Vec<String> = parts.iter().filter_map(|p| p.file_name().map(|f| f.to_string_lossy().into_owned())).collect();
        parts.sort();
        out.push(Disk { name, bytes, model, bus, parts, seq, dev });
    }
    Ok(out)
}

/// A size as disks are sold: decimal, one decimal place below ten.
pub fn size(bytes: u64) -> String {
    let units = ["B", "kB", "MB", "GB", "TB", "PB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1000.0 && u < units.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 || v >= 10.0 { format!("{v:.0} {}", units[u]) } else { format!("{v:.1} {}", units[u]) }
}

/// One disk's line in the list.
fn describe(d: &Disk, needed: u64) -> String {
    let model = if d.model.is_empty() { "(no model name)" } else { &d.model };
    let now = match d.parts.len() {
        0 => "no partitions".to_string(),
        1 => "1 partition".to_string(),
        n => format!("{n} partitions"),
    };
    let small = if d.bytes < needed { format!(" — too small: TorqOS needs {}", size(needed)) } else { String::new() };
    format!("{:<10} {:>8}  {:<7} {model}, {now}{small}", d.name, size(d.bytes), d.bus)
}

/// This machine's hardware file: what NixOS detected here (`detected`: nixos-generate-config
/// `--show-hardware-config --no-filesystems`) and the image's disk (`file_systems`), each a NixOS
/// module as it was written, one file.
pub fn hardware_file(detected: &str, file_systems: &str) -> Result<String> {
    if detected.trim().is_empty() {
        bail!("NixOS detected no hardware here (nixos-generate-config said nothing)");
    }
    // The file systems are the image's: anything else would describe disks that are not there.
    if detected.contains("fileSystems") || detected.contains("swapDevices") {
        bail!("the detected hardware names file systems; it should not (--no-filesystems)");
    }
    Ok(format!(
        "# This machine's hardware (ADR-031): what the TorqOS installer detected on it, and the disk of the\n\
         # TorqOS image it was installed from. Attached at the top of the tree (config-files.json).\n\
         {{ ... }}:\n\
         {{\n\
         \x20 imports = [\n\
         \x20   # Detected by the installer (nixos-generate-config --show-hardware-config --no-filesystems):\n\
         \x20   (\n\
         {}\n\
         \x20   )\n\
         \x20   # The TorqOS image's disk:\n\
         \x20   (\n\
         {}\n\
         \x20   )\n\
         \x20 ];\n\
         }}\n",
        detected.trim_end(),
        file_systems.trim_end()
    ))
}

/// A disk's partition `n`'s name: `sda` 2 is `sda2`, `nvme0n1` 2 is `nvme0n1p2`.
pub fn partition(disk: &str, n: u32) -> String {
    if disk.ends_with(|c: char| c.is_ascii_digit()) { format!("{disk}p{n}") } else { format!("{disk}{n}") }
}

fn hex(h: Sha256) -> String {
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// One of the installer's tools, which must succeed; its standard output.
fn tool(name: &str, args: &[&str]) -> Result<String> {
    let o = Command::new(format!("{BIN}/{name}")).args(args).stdin(Stdio::null()).output().with_context(|| format!("running {name}"))?;
    if !o.status.success() {
        bail!("{name} {} failed: {}", args.join(" "), String::from_utf8_lossy(&o.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&o.stdout).into_owned())
}

fn read_meta(name: &str) -> Result<String> {
    let p = Path::new(INSTALLER).join(name);
    Ok(std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?.trim().to_string())
}

fn welcome() -> String {
    match std::fs::read_to_string(crate::setup::BANNER) {
        Ok(banner) => format!("{}\n\n{WELCOME}", banner.trim_end()),
        Err(_) => WELCOME.to_string(),
    }
}

/// A file system's label on `dev`, read from the device itself (`-p`: not blkid's cache).
fn label(dev: &str) -> Option<String> {
    tool("blkid", &["-p", "-o", "value", "-s", "LABEL", "--", dev]).ok().map(|l| l.trim().to_string())
}

/// Whether `d` carries the installer itself: the disk or one of its partitions has the ISO's volume
/// label (`volume`). Mounted, the installer's medium is `busy` already; copied to RAM (its boot
/// menu's "copy to RAM" entry), it is not mounted, and only this says whose it is.
fn carries_installer(d: &Disk, volume: &str) -> bool {
    !volume.is_empty() && std::iter::once(&d.name).chain(&d.parts).any(|p| label(&format!("/dev/{p}")).as_deref() == Some(volume))
}

const DAMAGED: &str = "The installer's copy of TorqOS is damaged (its checksum differs): make the installer again from a checked download.";

/// The image, decompressed, a piece at a time to `each` (with how much has come so far): it must be
/// `bytes` long with the SHA-256 `sha`, or the installer's copy is damaged.
fn stream(image: &Path, bytes: u64, sha: &str, mut each: impl FnMut(&[u8], u64) -> Result<()>) -> Result<()> {
    // The file itself, not /etc's link to it: zstd skips a link given it (and says so only as a
    // warning). Not quiet, so what it says is all in its complaint.
    let image = std::fs::canonicalize(image).with_context(|| format!("finding {}", image.display()))?;
    let mut z = Command::new(format!("{BIN}/zstd")).args(["-dc", "--"]).arg(&image).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().context("running zstd")?;
    let mut src = z.stdout.take().context("zstd's output")?;
    // zstd's complaint, if any: a line or two, read once it has exited (too little to fill the pipe).
    let mut why = z.stderr.take().context("zstd's errors")?;
    let (mut buf, mut h, mut n) = (vec![0u8; CHUNK], Sha256::new(), 0u64);
    let read = (|| -> Result<()> {
        loop {
            let k = src.read(&mut buf).context("reading the image")?;
            if k == 0 {
                return Ok(());
            }
            n += k as u64;
            if n > bytes {
                bail!(DAMAGED);
            }
            h.update(&buf[..k]);
            each(&buf[..k], n)?;
        }
    })();
    if read.is_err() {
        let _ = z.kill();
    }
    let status = z.wait().context("zstd")?;
    read?;
    if !status.success() {
        let mut said = String::new();
        let _ = why.read_to_string(&mut said);
        bail!("{DAMAGED}\n  (zstd: {status}: {})", said.trim());
    }
    if n != bytes || hex(h) != sha {
        bail!(DAMAGED);
    }
    Ok(())
}

/// A progress line, redrawn at most twice a second (a console redraws slowly).
struct Shown(Option<std::time::Instant>);

impl Shown {
    fn show(&mut self, what: &str, n: u64, of: u64) {
        if n == of || self.0.is_none_or(|t| t.elapsed() >= std::time::Duration::from_millis(500)) {
            print!("\r  {what}: {} of {}  ", size(n), size(of));
            let _ = std::io::stdout().flush();
            self.0 = Some(std::time::Instant::now());
        }
    }
}

/// The installer's console: check its own copy of TorqOS, then choose, write, detect; again from
/// the start if anything fails.
pub fn run() -> Result<()> {
    if !rustix::process::geteuid().is_root() {
        bail!("install is the TorqOS installer's, run as root on its console");
    }
    let image = Path::new(INSTALLER).join("image.raw.zst");
    if !image.exists() {
        bail!("install is the TorqOS installer's: this machine is not running it");
    }
    let bytes: u64 = read_meta("size")?.parse().context("the image's size")?;
    let sha = read_meta("sha256")?;
    // No key on the console stops the installer mid-write (Ctrl-C, Ctrl-\, Ctrl-Z would kill or
    // stop it with half an image on the disk): a question is still answered, or left, by its line.
    let stdin = std::io::stdin();
    if let Ok(mut t) = rustix::termios::tcgetattr(&stdin) {
        t.local_modes.remove(rustix::termios::LocalModes::ISIG);
        let _ = rustix::termios::tcsetattr(&stdin, rustix::termios::OptionalActions::Now, &t);
    }
    // Before any disk is offered: a damaged stick or download is found here, with nothing written.
    println!("\n{}\n\n  Checking the installer's own copy of TorqOS ...", welcome());
    let mut shown = Shown(None);
    if let Err(e) = stream(&image, bytes, &sha, |_, n| {
        shown.show("Checked", n, bytes);
        Ok(())
    }) {
        println!("\n\n{e:#}");
        crate::setup::line_under(&welcome, "Press Enter to turn the machine off", "")?;
        let _ = Command::new(format!("{BIN}/systemctl")).arg("poweroff").status();
        return Ok(());
    }
    println!();
    let mut closed = 0;
    loop {
        println!("\n{}\n", welcome());
        let done = install(&image, bytes, &sha);
        closed = if done.as_ref().is_err_and(|e| format!("{e:#}").contains("the console closed")) { closed + 1 } else { 0 };
        match done {
            Ok(name) => {
                println!("\nTorqOS is on {name}. Remove the installer (its USB stick or disc) and restart: the machine starts TorqOS's setup.");
                crate::setup::line_under(&welcome, "Press Enter to restart", "")?;
                let _ = Command::new(format!("{BIN}/systemctl")).arg("reboot").status();
                return Ok(());
            }
            // A Ctrl-D starts over; a console that only ever reads as closed has nobody to start
            // over for: ended, for systemd to start the installer again.
            Err(e) if closed >= 3 => return Err(e),
            Err(e) => println!("\n{e:#}\nStarting over."),
        }
    }
}

/// Choose a disk, write the image (`bytes` long, SHA-256 `sha`) to it, leave the detected hardware
/// on it; the disk's name.
fn install(image: &Path, bytes: u64, sha: &str) -> Result<String> {
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo").context("reading what is mounted")?;
    let sys = Path::new("/sys");
    let volume = read_meta("volume").unwrap_or_default();
    let disks: Vec<Disk> = disks(sys, &busy(sys, &mountinfo))?.into_iter().filter(|d| !carries_installer(d, &volume)).collect();
    if disks.is_empty() {
        crate::setup::line_under(&welcome, "There is no disk to install to (the installer's own is not offered). Attach one, then press Enter", "")?;
        bail!("No disk was chosen.");
    }
    let list = disks.iter().enumerate().map(|(i, d)| format!("  {}  {}", i + 1, describe(d, bytes))).collect::<Vec<_>>().join("\n");
    println!("Disks:\n{list}\n");
    // Shown again with the question, if the console is taken over (setup.rs `asked`).
    let with_list = || format!("{}\n\nDisks:\n{list}", welcome());
    let default = if disks.len() == 1 { "1" } else { "" };
    let d = crate::setup::until(
        || crate::setup::line_under(&with_list, &format!("Install TorqOS on (1-{})", disks.len()), default),
        |c| {
            let d = c.parse::<usize>().ok().filter(|n| (1..=disks.len()).contains(n)).map(|n| &disks[n - 1]);
            match d {
                None => bail!("a number from 1 to {}", disks.len()),
                Some(d) if d.bytes < bytes => bail!("{} is too small: TorqOS needs {}", d.name, size(bytes)),
                Some(d) => Ok(d),
            }
        },
    )?;
    let model = if d.model.is_empty() { String::new() } else { format!(", {}", d.model) };
    let warning = format!("Everything on {} ({}{model}) will be erased.", d.name, size(d.bytes));
    println!("\n{warning}");
    let with_warning = || format!("{}\n\n{warning}", welcome());
    if crate::setup::line_under(&with_warning, &format!("Type its name, {}, to go ahead", d.name), "")? != d.name {
        bail!("Not confirmed: nothing was written.");
    }
    others_labelled(d)?;
    write(image, bytes, sha, d)?;
    if let Err(e) = detect(d) {
        // Without the detected hardware, setup would take the image's (a VM's): no half-installed
        // system left to start from.
        let now = match std::fs::OpenOptions::new().write(true).open(format!("/dev/{}", d.name)).map_err(anyhow::Error::from).and_then(|mut f| clear(&mut f, bytes, d.bytes)) {
            Ok(()) => format!("{} was cleared again: it holds no system.", d.name),
            Err(_) => format!("{} may hold part of TorqOS: install again before starting from it.", d.name),
        };
        bail!("{e:#}\n{now}");
    }
    Ok(d.name.clone())
}

/// The installed system finds its file systems by their labels (`file-systems.nix`): another disk
/// with a file system labelled the same — an earlier NixOS's root, another ESP — could be the one
/// it starts from, or grows, or puts its boot loader on. Refused until that disk is out or cleared.
fn others_labelled(d: &Disk) -> Result<()> {
    for want in [ROOT_LABEL, BOOT_LABEL] {
        // Probed now, not from blkid's cache; none is blkid failing (exit 2).
        let found = tool("blkid", &["-c", "/dev/null", "-o", "device", "-t", &format!("LABEL={want}")]).unwrap_or_default();
        for dev in found.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let name = Path::new(dev).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
            if name != d.name && !d.parts.contains(&name) {
                bail!(
                    "{dev} (not on {}) has a file system labelled {want}, as TorqOS's own is: the installed TorqOS could start from it instead. Disconnect that disk, or clear it, then install again. Nothing was written.",
                    d.name
                );
            }
        }
    }
    Ok(())
}

/// Clear what makes `f`, a disk `disk` bytes long that may hold the image's first `bytes` or part
/// of them, look like a system: its start (the partition table, the boot partition's start), the
/// image's backup partition table, and the disk's end (an earlier backup table some firmware
/// would restore from).
fn clear(f: &mut std::fs::File, bytes: u64, disk: u64) -> Result<()> {
    let zero = vec![0u8; TAIL as usize];
    for at in [0, bytes.saturating_sub(TAIL), disk.saturating_sub(TAIL)] {
        f.seek(SeekFrom::Start(at))?;
        f.write_all(&zero)?;
    }
    f.sync_all()?;
    Ok(())
}

/// `d`, open for writing, and nothing else: exclusive (the kernel refuses a disk something holds —
/// mounted, a RAID's, a swap's), and checked to be the disk that was listed and chosen.
fn open(d: &Disk) -> Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let dev = Path::new("/dev").join(&d.name);
    let f = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(rustix::fs::OFlags::EXCL.bits() as i32)
        .open(&dev)
        .with_context(|| format!("{} is in use: nothing was written", d.name))?;
    // The disk the name means now: unplugged and another plugged in, a name can mean another disk.
    let st = rustix::fs::fstat(&f).with_context(|| format!("which disk {} is", d.name))?;
    let rdev = st.st_rdev as u64;
    let now = format!("{}:{}", rustix::fs::major(rdev), rustix::fs::minor(rdev));
    let sys = Path::new("/sys/dev/block").join(&now);
    let read = |f: &str| std::fs::read_to_string(sys.join(f)).map(|s| s.trim().to_string()).unwrap_or_default();
    let now_bytes = read("size").parse::<u64>().unwrap_or(0) * 512;
    if now != d.dev || read("diskseq") != d.seq || now_bytes != d.bytes {
        bail!("{} is not the disk that was chosen any more (was it replaced?): nothing was written.", d.name);
    }
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo").context("reading what is mounted")?;
    if busy(Path::new("/sys"), &mountinfo).contains(&d.name) {
        bail!("{} is in use now: nothing was written.", d.name);
    }
    Ok(f)
}

/// Write the image (`bytes` long, SHA-256 `sha`) to `d` and read it back. Once anything is written,
/// a failure clears the disk's start again, so it never holds half a system that looks whole.
fn write(image: &Path, bytes: u64, sha: &str, d: &Disk) -> Result<()> {
    let mut out = open(d)?;
    let name = d.name.as_str();
    let wrote = (|| -> Result<()> {
        let mut shown = Shown(None);
        stream(image, bytes, sha, |piece, n| {
            out.write_all(piece).with_context(|| format!("writing to {name}"))?;
            shown.show(&format!("Writing TorqOS to {name}"), n, bytes);
            Ok(())
        })?;
        println!();
        // Past the image, to the disk's end or the last TAIL of it.
        if d.bytes > bytes {
            let at = bytes.max(d.bytes - TAIL);
            out.seek(SeekFrom::Start(at))?;
            out.write_all(&vec![0u8; (d.bytes - at) as usize]).with_context(|| format!("clearing the end of {name}"))?;
        }
        out.sync_all().with_context(|| format!("finishing the writes to {name}"))
    })();
    let checked = wrote.and_then(|()| read_back(bytes, sha, name));
    if let Err(e) = checked {
        let cleared = clear(&mut out, bytes, d.bytes);
        let now = if cleared.is_ok() { format!("{name} was cleared again: it holds no system.") } else { format!("{name} may hold part of TorqOS: install again before starting from it.") };
        bail!("{e:#}\n{now}");
    }
    Ok(())
}

/// Read the first `bytes` of the disk `name` back: they must have the SHA-256 `sha`.
fn read_back(bytes: u64, sha: &str, name: &str) -> Result<()> {
    // What the disk holds, not the kernel's copy of what was written.
    tool("blockdev", &["--flushbufs", &format!("/dev/{name}")])?;
    let mut back = std::fs::OpenOptions::new().read(true).open(format!("/dev/{name}")).with_context(|| format!("reading {name} back"))?.take(bytes);
    let (mut buf, mut h, mut n, mut shown) = (vec![0u8; CHUNK], Sha256::new(), 0u64, Shown(None));
    loop {
        let k = back.read(&mut buf).with_context(|| format!("reading {name} back"))?;
        if k == 0 {
            break;
        }
        n += k as u64;
        h.update(&buf[..k]);
        shown.show(&format!("Checking {name}"), n, bytes);
    }
    println!();
    if n != bytes || hex(h) != sha {
        bail!("{name} did not keep what was written to it (it reads back differently): the disk may be failing. Choose another, or check this one.");
    }
    Ok(())
}

/// Detect this machine's hardware and leave it in the new disk's root, for setup.
fn detect(d: &Disk) -> Result<()> {
    println!("  Detecting this machine's hardware ...");
    let dev = format!("/dev/{}", d.name);
    // The kernel reads the new partition table: once udev, which reads it too when a written disk
    // is closed, is done with it (until then the disk is busy).
    // A device elsewhere that never settles only delays this (as it does the installer's start,
    // modules/installer.nix): what counts is that the partitions are read.
    let settle = || {
        let _ = tool("udevadm", &["settle", "--timeout=60"]);
    };
    settle();
    let mut tries = 0;
    while let Err(e) = tool("blockdev", &["--rereadpt", &dev]) {
        tries += 1;
        if tries == 10 {
            return Err(e.context(format!("reading the partitions TorqOS put on {}", d.name)));
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
        settle();
    }
    settle();
    // The image's root, by its label, among this disk's partitions only.
    let root = (1..=16)
        .map(|n| format!("/dev/{}", partition(&d.name, n)))
        .find(|p| Path::new(p).exists() && label(p).as_deref() == Some(ROOT_LABEL))
        .with_context(|| format!("TorqOS's root file system is not on {} after writing it", d.name))?;
    let detected = tool("nixos-generate-config", &["--show-hardware-config", "--no-filesystems"])?;
    let file = hardware_file(&detected, &read_meta("file-systems.nix")?)?;
    let mnt = tempfile::tempdir().context("a place to mount the new disk")?;
    let at = mnt.path().to_string_lossy().into_owned();
    tool("mount", &["-t", "ext4", "--", &root, &at])?;
    let wrote = (|| -> Result<()> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        let p = mnt.path().join(DETECTED);
        std::fs::DirBuilder::new().recursive(true).mode(0o755).create(p.parent().context("its folder")?)?;
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o644).open(&p)?;
        f.write_all(file.as_bytes())?;
        f.sync_all()?;
        Ok(())
    })();
    // Unmounted whatever happened: lazily if it is still busy, so the disk is not left held.
    if tool("umount", &["--", &at]).is_err() {
        if let Err(e) = tool("umount", &["-l", "--", &at]) {
            // Still mounted: the folder is kept, not emptied (which would empty the new root).
            let _ = mnt.keep();
            return Err(e.context(format!("unmounting {} from {at}", root)));
        }
    }
    wrote.context("writing the detected hardware to the new disk")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    /// A /sys with one disk: `devices/<at>/block/<name>`, `block/<name>` a link to it, its
    /// partitions, and `dev/block/<major>:<minor>` links.
    fn disk(sys: &Path, at: &str, name: &str, sectors: u64, model: &str, parts: &[(&str, &str)], dev: &str) {
        let d = sys.join("devices").join(at).join("block").join(name);
        std::fs::create_dir_all(d.join("device")).unwrap();
        std::fs::write(d.join("size"), format!("{sectors}\n")).unwrap();
        std::fs::write(d.join("ro"), "0\n").unwrap();
        std::fs::write(d.join("dev"), format!("{dev}\n")).unwrap();
        std::fs::write(d.join("diskseq"), format!("{}\n", dev.len())).unwrap();
        std::fs::write(d.join("device/model"), format!("{model}\n")).unwrap();
        std::fs::create_dir_all(sys.join("block")).unwrap();
        symlink(&d, sys.join("block").join(name)).unwrap();
        std::fs::create_dir_all(sys.join("dev/block")).unwrap();
        symlink(&d, sys.join("dev/block").join(dev)).unwrap();
        for (p, pdev) in parts {
            std::fs::create_dir_all(d.join(p)).unwrap();
            std::fs::write(d.join(p).join("partition"), "1\n").unwrap();
            symlink(d.join(p), sys.join("dev/block").join(pdev)).unwrap();
        }
    }

    /// A disk with sectors larger than 512 bytes (where the image's partition table is not one) is
    /// not offered; one that says nothing of its sectors is.
    #[test]
    fn a_disk_with_large_sectors_is_not_offered() {
        let t = tempfile::tempdir().unwrap();
        let sys = t.path();
        disk(sys, "pci0000:00/0000:01:00.0/nvme/nvme0/nvme0n1", "nvme0n1", 244_190_646, "4Kn", &[], "259:0");
        disk(sys, "pci0000:00/0000:02:00.0/nvme/nvme1/nvme1n1", "nvme1n1", 1_000_215_216, "512e", &[], "259:1");
        disk(sys, "pci0000:00/0000:00:1f.2/ata1/host0/target0:0:0/0:0:0:0", "sda", 1_000_215_216, "old", &[], "8:0");
        for (name, sector) in [("nvme0n1", "4096"), ("nvme1n1", "512")] {
            let q = std::fs::canonicalize(sys.join("block").join(name)).unwrap().join("queue");
            std::fs::create_dir_all(&q).unwrap();
            std::fs::write(q.join("logical_block_size"), format!("{sector}\n")).unwrap();
        }
        let d = disks(sys, &BTreeSet::new()).unwrap();
        assert_eq!(d.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["nvme1n1", "sda"]);
    }

    /// Clearing a disk after a failed install: its start, where the image's backup partition table
    /// is, and its own end are zeros; the rest is left.
    #[test]
    fn clearing_leaves_no_partition_table() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("disk");
        let (image, disk) = (5 * TAIL + 7, 9 * TAIL);
        std::fs::write(&p, vec![0xa5u8; disk as usize]).unwrap();
        let mut f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
        clear(&mut f, image, disk).unwrap();
        let got = std::fs::read(&p).unwrap();
        let zero = |a: u64, b: u64| got[a as usize..b as usize].iter().all(|&x| x == 0);
        assert!(zero(0, TAIL) && zero(image - TAIL, image) && zero(disk - TAIL, disk));
        assert!(got[TAIL as usize..(image - TAIL) as usize].iter().all(|&x| x == 0xa5), "the rest is left as it was");
        assert_eq!(got.len() as u64, disk, "nothing past the disk's end");
    }

    /// The installer offers the machine's disks — never its own medium, a disk in use, a device
    /// that is not a disk, a read-only or empty one.
    #[test]
    fn only_disks_nothing_holds() {
        let t = tempfile::tempdir().unwrap();
        let sys = t.path();
        disk(sys, "pci0000:00/0000:00:1f.2/ata1/host0/target0:0:0/0:0:0:0", "sda", 3_907_029_168, "WDC WD20EZAZ", &[("sda1", "8:1"), ("sda2", "8:2")], "8:0");
        disk(sys, "pci0000:00/0000:00:14.0/usb2/2-1/2-1:1.0/host6/target6:0:0/6:0:0:0", "sdb", 60_000_000, "Cruzer", &[("sdb1", "8:17")], "8:16");
        disk(sys, "pci0000:00/0000:01:00.0/nvme/nvme0/nvme0n1", "nvme0n1", 1_000_215_216, "Samsung SSD 980 PRO", &[], "259:0");
        disk(sys, "virtual/block/loop0", "loop0", 2_000_000, "", &[], "7:0");
        disk(sys, "pci0000:00/0000:00:1f.2/ata2/host1/target1:0:0/1:0:0:0", "sdc", 1_000_000, "read-only", &[], "8:32");
        std::fs::write(sys.join("block/sdc/ro"), "1\n").unwrap();
        disk(sys, "pci0000:00/0000:00:1f.2/ata3/host2/target2:0:0/2:0:0:0", "sdd", 0, "empty reader", &[], "8:48");
        // The installer runs from the USB stick: its partition mounted at /iso, the store a loop
        // device on a file there, the root in RAM.
        let mountinfo = "\
22 1 0:21 / / rw - tmpfs tmpfs rw\n\
25 22 8:17 / /iso ro - iso9660 /dev/sdb1 ro\n\
26 22 7:0 / /nix/.ro-store ro - squashfs /dev/loop0 ro\n\
27 22 0:5 / /proc rw - proc proc rw\n";
        let busy = busy(sys, mountinfo);
        assert_eq!(busy, ["loop0", "sdb"].iter().map(|s| s.to_string()).collect(), "the medium's disk, not its partition's name");
        let d = disks(sys, &busy).unwrap();
        assert_eq!(d.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["nvme0n1", "sda"], "sorted; not the stick, loop, read-only or empty");
        assert_eq!((d[0].bus, d[0].parts.len(), d[0].bytes, d[0].dev.as_str()), ("NVMe", 0, 1_000_215_216 * 512, "259:0"));
        assert_eq!((d[1].bus, d[1].parts.clone(), d[1].model.as_str(), d[1].dev.as_str()), ("SATA", vec!["sda1".to_string(), "sda2".to_string()], "WDC WD20EZAZ", "8:0"));
        // Nothing mounted: the stick is offered too, as USB.
        let all = disks(sys, &BTreeSet::new()).unwrap();
        assert_eq!(all.iter().find(|d| d.name == "sdb").map(|d| d.bus), Some("USB"));
    }

    /// A disk that is part of something — an LVM volume group, a RAID array, through the disk or a
    /// partition — is not offered, mounted or not; nor an NVMe disk's hidden second path.
    #[test]
    fn a_disk_that_is_part_of_something_or_hidden_is_not_offered() {
        let t = tempfile::tempdir().unwrap();
        let sys = t.path();
        disk(sys, "pci0000:00/0000:00:1f.2/ata1/host0/target0:0:0/0:0:0:0", "sda", 1_000_000, "lvm", &[("sda1", "8:1")], "8:0");
        disk(sys, "pci0000:00/0000:00:1f.2/ata2/host1/target1:0:0/1:0:0:0", "sdb", 1_000_000, "raid", &[], "8:16");
        disk(sys, "pci0000:00/0000:00:1f.2/ata3/host2/target2:0:0/2:0:0:0", "sdc", 1_000_000, "free", &[], "8:32");
        disk(sys, "pci0000:00/0000:01:00.0/nvme/nvme0/nvme0c0n1", "nvme0c0n1", 1_000_000, "a path", &[], "259:1");
        std::fs::create_dir_all(sys.join("block/sda/sda1/holders/dm-0")).unwrap();
        std::fs::create_dir_all(sys.join("block/sdb/holders/md0")).unwrap();
        std::fs::write(sys.join("block/nvme0c0n1/hidden"), "1\n").unwrap();
        let d = disks(sys, &BTreeSet::new()).unwrap();
        assert_eq!(d.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["sdc"]);
    }

    /// A disk under a device-mapper or RAID device something mounts is in use too.
    #[test]
    fn a_disk_under_a_mounted_device_is_busy() {
        let t = tempfile::tempdir().unwrap();
        let sys = t.path();
        disk(sys, "pci0000:00/0000:00:1f.2/ata1/host0/target0:0:0/0:0:0:0", "sda", 1_000_000, "a", &[("sda1", "8:1")], "8:0");
        disk(sys, "virtual/block/dm-0", "dm-0", 900_000, "", &[], "253:0");
        std::fs::create_dir_all(sys.join("devices/virtual/block/dm-0/block/dm-0/slaves")).unwrap();
        symlink(sys.join("devices/pci0000:00/0000:00:1f.2/ata1/host0/target0:0:0/0:0:0:0/block/sda/sda1"), sys.join("devices/virtual/block/dm-0/block/dm-0/slaves/sda1")).unwrap();
        let busy = busy(sys, "30 22 253:0 / /mnt rw - ext4 /dev/dm-0 rw\n");
        assert!(busy.contains("sda") && busy.contains("dm-0"), "{busy:?}");
        assert!(disks(sys, &busy).unwrap().is_empty());
    }

    #[test]
    fn names_and_sizes() {
        assert_eq!(partition("sda", 2), "sda2");
        assert_eq!(partition("vda", 1), "vda1");
        assert_eq!(partition("nvme0n1", 2), "nvme0n1p2");
        assert_eq!(partition("mmcblk0", 2), "mmcblk0p2");
        assert_eq!(size(512), "512 B");
        assert_eq!(size(3_400_000_000), "3.4 GB");
        assert_eq!(size(512_110_190_592), "512 GB");
        assert_eq!(size(2_000_398_934_016), "2.0 TB");
        for no in ["loop0", "ram1", "zram0", "sr0", "dm-3", "md127", "fd0", "nbd0", "nullb0", "mmcblk0boot0", "mmcblk0boot1", "mmcblk0rpmb"] {
            assert!(not_a_disk(no), "{no}");
        }
        for yes in ["sda", "vda", "nvme0n1", "mmcblk0", "xvda"] {
            assert!(!not_a_disk(yes), "{yes}");
        }
        let d = Disk { name: "sda".into(), bytes: 2_000_000_000, model: String::new(), bus: "SATA", parts: vec!["sda1".into()], seq: "7".into(), dev: "8:0".into() };
        assert_eq!(describe(&d, 3_000_000_000), "sda          2.0 GB  SATA    (no model name), 1 partition — too small: TorqOS needs 3.0 GB");
    }

    /// The hardware file is NixOS's detection and the image's disk, each as written, in one module.
    #[test]
    fn the_hardware_file() {
        let detected = "# Do not modify this file!\n{ config, lib, pkgs, modulesPath, ... }:\n\n{\n  imports = [ (modulesPath + \"/installer/scan/not-detected.nix\") ];\n  boot.initrd.availableKernelModules = [ \"nvme\" \"xhci_pci\" ];\n}\n";
        let fs = "{ ... }:\n{\n  fileSystems.\"/\" = { device = \"/dev/disk/by-label/nixos\"; fsType = \"ext4\"; };\n}\n";
        let f = hardware_file(detected, fs).unwrap();
        assert!(f.contains(detected.trim_end()) && f.contains(fs.trim_end()), "{f}");
        assert!(f.find("not-detected.nix").unwrap() < f.find("by-label/nixos").unwrap());
        assert!(f.trim_end().ends_with("];\n}"), "{f}");
        assert!(hardware_file("  \n", fs).is_err(), "nothing detected");
        assert!(hardware_file("{ ... }: { fileSystems.\"/\" = { }; }", fs).is_err(), "the image's file systems only");
        assert!(hardware_file("{ ... }: { swapDevices = [ ]; }", fs).is_err());
    }
}
