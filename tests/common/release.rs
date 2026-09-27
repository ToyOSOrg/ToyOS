//! The image release's command lines booted as its notes print them
//! (`toyos_build::imagerelease::Host::command`), and what the release image
//! does to a disk it was not given.
//!
//! Each guest boots a copy of the image `cargo run -- --release-boot
//! --build-only` writes, built once per run by that same sequence, and is
//! judged by its console alone: these boots are not the harness's profiles.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use toyos_build::arch::Arch;
use toyos_build::build::{self, Boot};
use toyos_build::fingerprint::{first_difference, whole_device};
use toyos_build::imagerelease::Host;

/// One guest's ceiling from power-on to a painting desktop, before
/// `super::qemu::budget` scales it: a liveness guard, never a verdict.
const DESKTOP: Duration = Duration::from_secs(120);

/// The compositor's report, one every two seconds of a painting desktop.
const FRAMES: &str = "compositor: frames=";

/// The release image, built once per run.
fn image() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT.get_or_init(|| {
        let root = super::compile::repo_root();
        let boot = Boot::release(&root);
        let plan = build::plan_for(&root, &boot, false, &[]);
        build::build(&root, boot, false, &plan)
    })
}

/// A copy of the release image under this lane, for one guest to write to.
fn stick(name: &str) -> Result<PathBuf, String> {
    let to = super::lane::dir().join(name);
    std::fs::copy(image(), &to).map_err(|e| format!("copy the release image to {}: {e}", to.display()))?;
    Ok(to)
}

/// A QEMU started from a release command line, and its console so far.
struct Guest {
    child: Child,
    lines: Receiver<String>,
    log: String,
    stderr: PathBuf,
}

impl Guest {
    /// `argv` with `extra` after it and no window: the notes' line, headless.
    fn start(argv: &[String], extra: &[String], stderr: PathBuf) -> Result<Guest, String> {
        let err = std::fs::File::create(&stderr).map_err(|e| format!("{}: {e}", stderr.display()))?;
        let arch = Arch::X86_64;
        if argv[0] != arch.qemu() {
            return Err(format!("a release command line starts {:?}, not {}", argv[0], arch.qemu()));
        }
        let mut child = Command::new(arch.qemu())
            .args(&argv[1..])
            .args(extra)
            .args(["-display", "none"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(err)
            .spawn()
            .map_err(|e| format!("{}: {e}", arch.qemu()))?;
        let out = child.stdout.take().expect("piped");
        let (send, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(out).split(b'\n') {
                let Ok(line) = line else { return };
                if send.send(String::from_utf8_lossy(&line).into_owned()).is_err() {
                    return;
                }
            }
        });
        Ok(Guest { child, lines, log: String::new(), stderr })
    }

    /// Read the console until `done` holds of it, refusing a panic the moment
    /// one is printed.
    fn until(&mut self, what: &str, done: impl Fn(&str) -> bool) -> Result<(), String> {
        let budget = super::qemu::budget(DESKTOP);
        let deadline = Instant::now() + budget;
        loop {
            if let Some(line) = self.log.lines().find(|l| l.contains("PANIC") || l.contains("panicked at")) {
                return Err(format!("the guest panicked before {what}: {line}\n{}", self.log));
            }
            if done(&self.log) {
                return Ok(());
            }
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(line) => {
                    self.log.push_str(&line);
                    self.log.push('\n');
                }
                Err(RecvTimeoutError::Timeout) => {
                    return Err(format!("no {what} within {budget:?}:\n{}", self.log));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    let stderr = std::fs::read_to_string(&self.stderr).unwrap_or_default();
                    return Err(format!(
                        "QEMU closed its console before {what}:\n{}\nQEMU's stderr:\n{stderr}",
                        self.log
                    ));
                }
            }
        }
    }

    /// The desktop is up: every program the shipped config starts said it
    /// started, the three that announce themselves did, and the compositor
    /// has painted.
    fn desktop(&mut self) -> Result<(), String> {
        let mut said: Vec<String> = build::boot_start(&super::compile::repo_root().join("system.toml"))
            .iter()
            .map(|program| format!("init: started {program}"))
            .collect();
        said.extend(
            ["compositor: ready", "netd: ready", "logd: this boot's kernel log is", FRAMES].map(String::from),
        );
        self.until("the desktop", |log| said.iter().all(|line| log.contains(line.as_str())))
    }
}

impl Drop for Guest {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The notes' command line for this host boots the release image to a
/// painting desktop.
pub fn release_command_boots() -> Result<(), String> {
    let host = Host::this()?;
    let stick = stick("release-command.img")?;
    let argv = host.command(&stick.display().to_string());
    let mut guest = Guest::start(&argv, &[], super::lane::dir().join("release-command.stderr"))?;
    guest.desktop()?;
    eprintln!("  [release] {host:?}'s command line reached a painting desktop");
    Ok(())
}

/// Beside the release image, a disk laid out as another operating system's
/// and a stick with no partition table come back byte for byte after a boot
/// to the desktop, while the kernel says it read both and opened a volume on
/// neither.
pub fn release_writes_no_other_disk() -> Result<(), String> {
    const OTHER_BYTES: u64 = 192 << 20;
    const SPARE_BYTES: u64 = 64 << 20;
    let host = Host::this()?;
    let dir = super::lane::dir();
    let stick = stick("release-disks.img")?;
    let other = dir.join("another-os.img");
    let parts = another_os_disk(&other, OTHER_BYTES)?;
    let spare = dir.join("spare-stick.img");
    pattern_file(&spare, SPARE_BYTES, 0x5A)?;
    let before = [whole_device(&stick), whole_device(&other), whole_device(&spare)];

    let extra: Vec<String> = [
        "-drive".to_string(),
        format!("if=none,id=other,format=raw,file={}", other.display()),
        "-device".to_string(),
        "nvme,serial=other,drive=other".to_string(),
        "-drive".to_string(),
        format!("if=none,id=spare,format=raw,file={}", spare.display()),
        "-device".to_string(),
        "usb-storage,bus=xhci.0,drive=spare".to_string(),
    ]
    .to_vec();
    let argv = host.command(&stick.display().to_string());
    let mut guest = Guest::start(&argv, &extra, dir.join("release-disks.stderr"))?;
    guest.desktop()?;

    // What the kernel opened, before anything about the bytes: a page cache is
    // what every write to a DATA volume goes through.
    if let Some(line) = guest.log.lines().find(|l| l.contains("page cache: device")) {
        return Err(format!("the release opened a DATA volume on a disk it was not given: {line}\n{}", guest.log));
    }
    // The case was reached: the kernel read both tables and took neither.
    let read_other = format!("gpt: device 1 has {parts} partitions and none of them is ours");
    for said in [
        read_other.as_str(),
        "has no partition table we can use",
        "storage: this machine carries 0 TOYOS-DATA partitions",
    ] {
        if !guest.log.contains(said) {
            return Err(format!("the kernel never said {said:?}\n{}", guest.log));
        }
    }
    // The machine runs on past the desktop, so what a boot writes late is in.
    let settled = guest.log.matches(FRAMES).count() + 3;
    guest.until("three more frame reports", |log| log.matches(FRAMES).count() >= settled)?;
    drop(guest);

    let after = [whole_device(&stick), whole_device(&other), whole_device(&spare)];
    if first_difference(&before[0], &after[0]).is_none() {
        return Err(
            "the stick came back unchanged, so no write of this boot reached a backing file and \
             the comparison below proves nothing"
                .into(),
        );
    }
    for (name, (b, a)) in ["the other operating system's disk", "the spare stick"]
        .iter()
        .zip(before[1..].iter().zip(&after[1..]))
    {
        if let Some(diff) = first_difference(b, a) {
            return Err(format!("the release wrote to {name}: {diff}"));
        }
    }
    eprintln!(
        "  [release] {host:?}: beside the stick, an NVMe disk of {parts} foreign partitions and a \
         stick with no table came back byte for byte"
    );
    Ok(())
}

/// `len` bytes of `fill`, so a write of anything, zeros included, moves the
/// fingerprint.
fn pattern_file(path: &Path, len: u64, fill: u8) -> Result<(), String> {
    let mut file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let chunk = vec![fill; 1 << 20];
    for _ in 0..len / chunk.len() as u64 {
        file.write_all(&chunk).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(())
}

/// A disk laid out as a PC's with another operating system on it: an EFI
/// system partition, Microsoft's reserved and basic-data partitions and a
/// Linux filesystem, each carrying its format's signature, over a filled
/// disk. Answers how many partitions it carries.
fn another_os_disk(path: &Path, len: u64) -> Result<usize, String> {
    use gpt::partition_types::{BASIC, EFI, LINUX_FS, MICROSOFT_RESERVED};
    const MIB: u64 = 1 << 20;
    pattern_file(path, len, 0xA5)?;

    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let mbr = gpt::mbr::ProtectiveMBR::with_lb_size(u32::try_from(len / 512 - 1).unwrap_or(u32::MAX));
    mbr.overwrite_lba0(&mut file).map_err(|e| format!("protective MBR: {e}"))?;
    let mut disk = gpt::GptConfig::default()
        .initialized(false)
        .writable(true)
        .logical_block_size(gpt::disk::LogicalBlockSize::Lb512)
        .create_from_device(Box::new(file), None)
        .map_err(|e| format!("a table on {}: {e}", path.display()))?;
    disk.update_partitions(BTreeMap::<u32, gpt::partition::Partition>::new())
        .map_err(|e| format!("an empty table: {e}"))?;
    let layout = [
        ("EFI system partition", 32 * MIB, EFI),
        ("Microsoft reserved partition", 16 * MIB, MICROSOFT_RESERVED),
        ("Basic data partition", 64 * MIB, BASIC),
        ("Linux filesystem", 64 * MIB, LINUX_FS),
    ];
    let mut starts = Vec::new();
    for (name, bytes, kind) in layout {
        let id = disk
            .add_partition(name, bytes, kind, 0, Some(2048))
            .map_err(|e| format!("add {name}: {e}"))?;
        let placed = &disk.partitions()[&id];
        starts.push(placed.bytes_start(gpt::disk::LogicalBlockSize::Lb512).map_err(|e| e.to_string())?);
    }
    let mut device = disk.write().map_err(|e| format!("write the table: {e}"))?;

    // FAT32's and NTFS's boot sectors, and ext4's superblock magic.
    let mut fat = [0u8; 512];
    fat[..3].copy_from_slice(&[0xEB, 0x58, 0x90]);
    fat[3..11].copy_from_slice(b"MSDOS5.0");
    fat[82..90].copy_from_slice(b"FAT32   ");
    fat[510..].copy_from_slice(&[0x55, 0xAA]);
    let mut ntfs = [0u8; 512];
    ntfs[..3].copy_from_slice(&[0xEB, 0x52, 0x90]);
    ntfs[3..11].copy_from_slice(b"NTFS    ");
    ntfs[510..].copy_from_slice(&[0x55, 0xAA]);
    for (at, bytes) in [(starts[0], &fat[..]), (starts[2], &ntfs[..]), (starts[3] + 1024 + 56, &[0x53, 0xEF][..])] {
        device.seek(SeekFrom::Start(at)).map_err(|e| e.to_string())?;
        device.write_all(bytes).map_err(|e| e.to_string())?;
    }
    device.flush().map_err(|e| e.to_string())?;

    // The premise, by the parser the kernel selects DATA with.
    if toyos_build::image::data_partition_of(path).is_ok() {
        return Err(format!("{} carries a TOYOS-DATA partition, so it is no other system's disk", path.display()));
    }
    Ok(starts.len())
}
