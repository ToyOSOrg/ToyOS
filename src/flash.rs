//! `toyos-flash` — an image written whole to a USB stick on this host,
//! choosing the stick rather than being told which one it is: a disk's name
//! moves between plug-ins, so a command that carries one eventually names the
//! wrong disk. The image is the one a build made, or the one `--image` names.
//!
//! **A candidate hangs off a USB device whose own port the kernel calls
//! `removable`**: the driver core's `removable` attribute, which usbcore takes
//! from the platform firmware's account of the port, or else from a compound
//! hub's descriptor. A disk on another bus, a USB device wired into the
//! machine, and one on a port whose removability the kernel does not know are
//! never candidates; a stick behind an ordinary hub is the last of these, and
//! is refused.
//!
//! After the prompt, the disk under the chosen name is read again before
//! anything on it is unmounted, and must be the one shown, by its per-boot
//! `diskseq`, and still a candidate; the descriptor the write goes through must
//! reach that same `diskseq`.
//!
//! Only a Linux host is read, from `/sys/class/block`, the way
//! [`crate::metal`] reads the T14's stick. Any other host is refused by name;
//! writing from macOS is `issues/writing-a-stick-from-a-macos-host.md`.
//!
//! **Root writes; nothing here asks for it.** Run unprivileged, it shows the
//! disk it chose and refuses with the `sudo` line that writes it; run as root,
//! it shows the disk again and writes only on a typed `y`. No password passes
//! through it.

use std::fmt;
use std::io::{BufRead, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::arch::Arch;
use crate::build::Boot;
use crate::flags::{Flag, Value, Vocabulary, CONSOLE_BOOT, DIAG_BOOT};

/// `--disk <name>`: which candidate, where more than one is attached.
const DISK: Flag = Flag { name: "--disk", value: Value::Next };

/// `--image <path>`: an image built elsewhere, as `toyos-metal` takes one.
const IMAGE: Flag = Flag { name: "--image", value: Value::Next };

/// The image is `--image`'s, or the build's own: `cargo run -- --build-only`
/// and its two framebuffer modes write the three this chooses between.
const FLASH: Vocabulary = Vocabulary(&[&DIAG_BOOT, &CONSOLE_BOOT, &IMAGE, &DISK]);

/// What the kernel says of the port a USB device is plugged into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Port {
    Removable,
    Fixed,
    Unknown,
}

/// One whole disk, as the host's kernel describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disk {
    /// `sdb`, as `/dev` and `/sys/class/block` spell it.
    pub name: String,
    /// The USB device the disk is on, `2-1.4`, and its port; `None` where it
    /// is on none.
    pub usb: Option<(String, Port)>,
    pub bytes: u64,
    /// The device's vendor and model where it has them: shown, never asked.
    pub label: String,
    /// What names this disk and no other for as long as the host is up:
    /// Linux's `diskseq`.
    pub id: u64,
}

impl fmt::Display for Disk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "/dev/{} \"{}\", {} bytes, ", self.name, self.label, self.bytes)?;
        match &self.usb {
            Some((device, port)) => write!(f, "on USB device {device}, port {port:?}"),
            None => write!(f, "on no USB device"),
        }
    }
}

/// Every way `toyos-flash` refuses, by name.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    Usage(String),
    /// This host has no reader of its disks here.
    Host(&'static str),
    /// The host's account of its disks could not be read.
    Read(String),
    Image(String),
    NoCandidate,
    /// More than one candidate, and no `--disk` to choose among them.
    Ambiguous(Vec<Disk>),
    /// `--disk` named something that is no candidate.
    NotACandidate { asked: String, candidates: Vec<Disk> },
    /// The disk under the chosen name after the prompt is not the candidate
    /// shown.
    Moved { shown: Box<Disk>, now: Box<Disk> },
    /// The descriptor opened reaches another disk than the one shown.
    Reached { shown: Box<Disk>, diskseq: u64 },
    TooSmall { image: u64, disk: Box<Disk> },
    NotRoot { command: String },
    Declined,
    Io { what: String, why: String },
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let list = |disks: &[Disk]| disks.iter().map(|d| format!("\n  {d}")).collect::<String>();
        match self {
            Self::Usage(why) => write!(f, "{why}"),
            Self::Host(os) => write!(f, "toyos-flash writes a stick only from a Linux host, and this host is {os}"),
            Self::Read(why) => write!(f, "the host's disks do not read: {why}"),
            Self::Image(why) => write!(f, "the image is not flashable: {why}"),
            Self::NoCandidate => write!(f, "no disk on a removable USB port is attached"),
            Self::Ambiguous(disks) => {
                write!(f, "more than one disk is on a removable USB port; name one with --disk:{}", list(disks))
            }
            Self::NotACandidate { asked, candidates } => write!(
                f,
                "--disk {asked} is on no removable USB port; the candidates are:{}",
                if candidates.is_empty() { " none".to_string() } else { list(candidates) }
            ),
            Self::Moved { shown, now } => write!(
                f,
                "the disk under that name is not the candidate shown, so nothing was written:\n  shown {shown}\n  \
                 now   {now}"
            ),
            Self::Reached { shown, diskseq } => write!(
                f,
                "the descriptor opened reaches diskseq {diskseq}, not the {} of {shown}, so nothing was written",
                shown.id
            ),
            Self::TooSmall { image, disk } => write!(f, "the image's {image} bytes do not fit {disk}"),
            Self::NotRoot { command } => {
                write!(f, "writing a disk takes root, and nothing was written; write it with\n  {command}")
            }
            Self::Declined => write!(f, "nothing was written"),
            Self::Io { what, why } => write!(f, "{what}: {why}"),
        }
    }
}

fn io(what: impl Into<String>) -> impl FnOnce(std::io::Error) -> Refusal {
    let what = what.into();
    move |e| Refusal::Io { what, why: e.to_string() }
}

/// The disk hangs off a USB device whose port the kernel calls removable.
fn candidate(disk: &Disk) -> bool {
    matches!(disk.usb, Some((_, Port::Removable)))
}

/// The disk to write: the one candidate, or the candidate `asked` names.
pub fn choose(disks: Vec<Disk>, asked: Option<&str>) -> Result<Disk, Refusal> {
    let mut candidates: Vec<Disk> = disks.into_iter().filter(candidate).collect();
    match asked {
        Some(asked) => match candidates.iter().position(|d| d.name == asked) {
            Some(at) => Ok(candidates.swap_remove(at)),
            None => Err(Refusal::NotACandidate { asked: asked.to_string(), candidates }),
        },
        None => match candidates.len() {
            0 => Err(Refusal::NoCandidate),
            1 => Ok(candidates.remove(0)),
            _ => Err(Refusal::Ambiguous(candidates)),
        },
    }
}

/// The disk under the shown one's name, read afresh, is the one shown and
/// still a candidate.
fn still(shown: &Disk, now: Disk) -> Result<(), Refusal> {
    if now.id == shown.id && candidate(&now) {
        Ok(())
    } else {
        Err(Refusal::Moved { shown: Box::new(shown.clone()), now: Box::new(now) })
    }
}

/// The open descriptor, whose disk answered `diskseq`, reaches the disk shown.
fn reaches(shown: &Disk, diskseq: u64) -> Result<(), Refusal> {
    if diskseq == shown.id {
        Ok(())
    } else {
        Err(Refusal::Reached { shown: Box::new(shown.clone()), diskseq })
    }
}

/// The image the command line names, and the disk it asks for.
fn image(args: &[String]) -> Result<(PathBuf, Option<String>), Refusal> {
    let line = FLASH.walk(args);
    if let Some(word) = line.unknown.or_else(|| line.positionals.first().copied()) {
        return Err(Refusal::Usage(format!(
            "unknown argument {word:?}. toyos-flash accepts:\n{}",
            FLASH.usage()
        )));
    }
    if let Some(why) = line.malformed() {
        return Err(Refusal::Usage(why));
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let built = |boot: Boot| root.join(boot.image_for(Arch::X86_64));
    let path = match (FLASH.value(args, &IMAGE), FLASH.present(args, &DIAG_BOOT), FLASH.present(args, &CONSOLE_BOOT)) {
        (Some(path), false, false) => PathBuf::from(path),
        (None, false, false) => built(Boot::shipped(root)),
        (None, true, false) => built(Boot::diag(root)),
        (None, false, true) => built(Boot::console(root)),
        _ => return Err(Refusal::Usage("--image, --diag-boot and --console-boot each name an image; flash one".into())),
    };
    Ok((path, FLASH.value(args, &DISK).map(str::to_string)))
}

/// One read and one write of the copy, a whole number of sectors.
const CHUNK: usize = 4 << 20;

/// Show the chosen disk, and as root, write `args`' image to it.
pub fn run(args: &[String]) -> Result<(), Refusal> {
    let (path, asked) = image(args)?;
    let disks = host::disks()?;
    let (mut file, bytes) =
        crate::metal::whole_disk(&path).map_err(|r| Refusal::Image(format!("{}: {r}", path.display())))?;
    println!("image  {}, {bytes} bytes", path.display());
    for disk in &disks {
        println!("read   {disk}");
    }
    let disk = choose(disks, asked.as_deref())?;
    if bytes > disk.bytes {
        return Err(Refusal::TooSmall { image: bytes, disk: Box::new(disk) });
    }
    println!("disk   {disk}");

    // SAFETY: `geteuid` reads the process's credentials and cannot fail.
    if unsafe { libc::geteuid() } != 0 {
        let exe = std::env::current_exe().map_err(io("finding this program"))?;
        let mut command = vec!["sudo".to_string(), exe.display().to_string()];
        command.extend(args.iter().cloned());
        if asked.is_none() {
            command.extend(["--disk".to_string(), disk.name]);
        }
        return Err(Refusal::NotRoot { command: command.join(" ") });
    }
    print!("erase /dev/{} and write the image to it? [y/N] ", disk.name);
    std::io::stdout().flush().map_err(io("asking"))?;
    let mut reply = String::new();
    std::io::stdin().lock().read_line(&mut reply).map_err(io("reading the answer"))?;
    if reply.trim() != "y" {
        return Err(Refusal::Declined);
    }

    still(&disk, host::disk(&disk.name)?)?;
    host::unmount(&disk)?;
    let (mut device, diskseq) = host::open(&disk)?;
    reaches(&disk, diskseq)?;

    let began = std::time::Instant::now();
    copy(&mut file, bytes, &mut device)?;
    host::finish(device)?;
    println!("wrote {bytes} bytes to /dev/{} in {:.1} s", disk.name, began.elapsed().as_secs_f64());
    Ok(())
}

/// The image's `bytes` onto `device`, from the image's first byte whatever
/// its cursor.
fn copy(image: &mut std::fs::File, bytes: u64, device: &mut impl Write) -> Result<(), Refusal> {
    image.seek(SeekFrom::Start(0)).map_err(io("reading the image"))?;
    let mut buf = vec![0u8; CHUNK];
    let mut at = 0u64;
    while at < bytes {
        let want = CHUNK.min((bytes - at) as usize);
        image.read_exact(&mut buf[..want]).map_err(io("reading the image"))?;
        device.write_all(&buf[..want]).map_err(io("writing the disk"))?;
        at += want as u64;
    }
    Ok(())
}

/// What a disk's place in `/sys/devices` says.
#[cfg(any(target_os = "linux", test))]
mod sysfs {
    use super::{Port, Refusal};

    /// The driver core's `removable` attribute, whose three words
    /// `Documentation/ABI/testing/sysfs-devices-removable` defines.
    pub fn port(word: &str) -> Result<Port, Refusal> {
        match word.trim_end_matches('\n') {
            "removable" => Ok(Port::Removable),
            "fixed" => Ok(Port::Fixed),
            "unknown" => Ok(Port::Unknown),
            other => Err(Refusal::Read(format!("a USB port that is {other:?}"))),
        }
    }

    /// usbcore names a device `<bus>-<port>[.<port>…]`; a root hub is
    /// `usb<bus>` and an interface `<device>:<config>.<interface>`.
    fn usb_device_name(part: &str) -> bool {
        let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        part.split_once('-').is_some_and(|(bus, ports)| digits(bus) && ports.split('.').all(digits))
    }

    /// The directory and name of the deepest USB device on `devpath`, a
    /// disk's canonical `/sys/devices` path: the device the disk is part of,
    /// whose port is the one it was plugged into.
    pub fn usb_device(devpath: &str) -> Option<(&str, &str)> {
        let mut found = None;
        let mut start = 0;
        for part in devpath.split('/') {
            let end = start + part.len();
            if usb_device_name(part) {
                found = Some((&devpath[..end], part));
            }
            start = end + 1;
        }
        found
    }
}

#[cfg(target_os = "linux")]
mod host {
    use std::fs::{self, File, OpenOptions};
    use std::io::ErrorKind;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;

    use super::{io, sysfs, Disk, Refusal};
    use crate::metal::Node;

    /// `BLKGETDISKSEQ`, `_IOR(0x12, 128, __u64)` in `linux/fs.h`.
    const BLKGETDISKSEQ: libc::c_ulong = 0x8008_1280;

    fn read(path: &str) -> Result<String, Refusal> {
        fs::read_to_string(path).map_err(|e| Refusal::Read(format!("{path}: {e}")))
    }

    fn number(path: &str) -> Result<u64, Refusal> {
        let text = read(path)?;
        text.trim().parse().map_err(|_| Refusal::Read(format!("{path}: {text:?}")))
    }

    /// A file only some disks have: an NVMe namespace's device has a model
    /// and no vendor.
    fn shown(path: &str) -> Result<Option<String>, Refusal> {
        match fs::read_to_string(path) {
            Ok(text) => Ok(Some(text.trim().to_string())),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Refusal::Read(format!("{path}: {e}"))),
        }
    }

    /// The disk `/sys/class/block/<name>` describes, and the port of the USB
    /// device it is on.
    pub fn disk(name: &str) -> Result<Disk, Refusal> {
        let at = format!("/sys/class/block/{name}");
        let devpath = fs::canonicalize(&at).map_err(|e| Refusal::Read(format!("{at}: {e}")))?;
        let devpath = devpath.to_string_lossy();
        let usb = match sysfs::usb_device(&devpath) {
            Some((dir, device)) => Some((device.to_string(), sysfs::port(&read(&format!("{dir}/removable"))?)?)),
            None => None,
        };
        let label = [shown(&format!("{at}/device/vendor"))?, shown(&format!("{at}/device/model"))?];
        Ok(Disk {
            name: name.to_string(),
            usb,
            bytes: number(&format!("{at}/size"))? * 512,
            label: label.into_iter().flatten().collect::<Vec<_>>().join(" "),
            id: number(&format!("{at}/diskseq"))?,
        })
    }

    /// Every SCSI and NVMe whole disk, the only nodes [`Node`] can name.
    pub fn disks() -> Result<Vec<Disk>, Refusal> {
        let dir = "/sys/class/block";
        let entries = fs::read_dir(dir).map_err(|e| Refusal::Read(format!("{dir}: {e}")))?;
        let mut disks = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| Refusal::Read(format!("{dir}: {e}")))?;
            if let Ok(node) = Node::parse(&format!("/dev/{}", entry.file_name().to_string_lossy())) {
                disks.push(disk(&node.name())?);
            }
        }
        disks.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(disks)
    }

    /// `\040` and its kin, as `/proc/self/mountinfo` escapes a path.
    fn unescape(field: &str) -> String {
        let bytes = field.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut at = 0;
        while at < bytes.len() {
            let octal = bytes.get(at + 1..at + 4).filter(|d| bytes[at] == b'\\' && d.iter().all(|b| (b'0'..=b'7').contains(b)));
            match octal {
                Some(d) => {
                    out.push((d[0] - b'0') << 6 | (d[1] - b'0') << 3 | (d[2] - b'0'));
                    at += 4;
                }
                None => {
                    out.push(bytes[at]);
                    at += 1;
                }
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    /// Unmount every filesystem on the disk or one of its partitions, matched
    /// by device number.
    pub fn unmount(disk: &Disk) -> Result<(), Refusal> {
        let at = format!("/sys/class/block/{}", disk.name);
        let mut devs = vec![read(&format!("{at}/dev"))?.trim().to_string()];
        for entry in fs::read_dir(&at).map_err(|e| Refusal::Read(format!("{at}: {e}")))? {
            let path = entry.map_err(|e| Refusal::Read(format!("{at}: {e}")))?.path();
            if path.join("partition").exists() {
                devs.push(read(&format!("{}/dev", path.display()))?.trim().to_string());
            }
        }
        for line in read("/proc/self/mountinfo")?.lines() {
            let fields: Vec<&str> = line.split(' ').collect();
            let (Some(dev), Some(point)) = (fields.get(2), fields.get(4)) else { continue };
            if devs.iter().any(|d| d == dev) {
                let point = unescape(point);
                println!("unmounting {point}");
                let c = std::ffi::CString::new(point.clone()).expect("mountinfo holds no NUL");
                // SAFETY: a NUL-terminated path.
                if unsafe { libc::umount2(c.as_ptr(), 0) } != 0 {
                    return Err(io(format!("unmounting {point}"))(std::io::Error::last_os_error()));
                }
            }
        }
        Ok(())
    }

    /// The disk, opened exclusively — which the kernel refuses while anything
    /// on it is mounted — and the `diskseq` of what the descriptor reaches.
    pub fn open(disk: &Disk) -> Result<(File, u64), Refusal> {
        let path = format!("/dev/{}", disk.name);
        let file = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_EXCL)
            .open(&path)
            .map_err(io(format!("opening {path}")))?;
        let mut diskseq = 0u64;
        // SAFETY: `BLKGETDISKSEQ` writes one `u64`.
        if unsafe { libc::ioctl(file.as_raw_fd(), BLKGETDISKSEQ, &mut diskseq) } != 0 {
            return Err(io(format!("asking {path} its diskseq"))(std::io::Error::last_os_error()));
        }
        Ok((file, diskseq))
    }

    pub fn finish(device: File) -> Result<(), Refusal> {
        device.sync_all().map_err(io("flushing the disk"))
    }
}

#[cfg(not(target_os = "linux"))]
mod host {
    use super::{Disk, Refusal};

    pub fn disks() -> Result<Vec<Disk>, Refusal> {
        Err(Refusal::Host(std::env::consts::OS))
    }
    pub fn disk(_: &str) -> Result<Disk, Refusal> {
        unreachable!("no disk is chosen on a host with no reader")
    }
    pub fn unmount(_: &Disk) -> Result<(), Refusal> {
        unreachable!("no disk is chosen on a host with no reader")
    }
    pub fn open(_: &Disk) -> Result<(std::fs::File, u64), Refusal> {
        unreachable!("no disk is chosen on a host with no reader")
    }
    pub fn finish(_: std::fs::File) -> Result<(), Refusal> {
        unreachable!("no disk is chosen on a host with no reader")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A disk on the USB device `device` whose port the kernel calls `port`,
    /// or on none.
    fn disk(name: &str, on: Option<(&str, &str)>, id: u64) -> Disk {
        Disk {
            name: name.to_string(),
            usb: on.map(|(device, port)| (device.to_string(), sysfs::port(port).unwrap())),
            bytes: 30_752_000_000,
            label: "SanDisk Ultra".to_string(),
            id,
        }
    }

    fn stick(name: &str, id: u64) -> Disk {
        disk(name, Some(("2-1", "removable\n")), id)
    }

    fn nvme() -> Disk {
        disk("nvme0n1", None, 1)
    }

    #[test]
    fn the_stick_on_a_removable_usb_port_is_chosen_beside_the_internal_disk() {
        let chosen = choose(vec![nvme(), stick("sda", 9)], None).unwrap();
        assert_eq!((chosen.name.as_str(), chosen.id), ("sda", 9));
    }

    #[test]
    fn a_disk_on_no_usb_device_is_no_candidate_even_when_named() {
        assert_eq!(choose(vec![nvme()], None), Err(Refusal::NoCandidate));
        let named = choose(vec![nvme(), stick("sda", 9)], Some("nvme0n1"));
        assert!(matches!(named, Err(Refusal::NotACandidate { candidates, .. }) if candidates == [stick("sda", 9)]));
    }

    #[test]
    fn a_usb_disk_on_a_fixed_or_unknown_port_is_no_candidate() {
        for port in ["fixed\n", "unknown\n"] {
            let reader = disk("sda", Some(("1-4", port)), 9);
            assert_eq!(choose(vec![reader.clone()], None), Err(Refusal::NoCandidate), "{port}");
            assert!(matches!(choose(vec![reader], Some("sda")), Err(Refusal::NotACandidate { .. })), "{port}");
        }
    }

    #[test]
    fn a_port_the_kernel_describes_in_another_word_is_refused() {
        assert!(matches!(sysfs::port("hotplug\n"), Err(Refusal::Read(_))));
    }

    #[test]
    fn two_sticks_are_refused_until_one_is_named() {
        let two = vec![stick("sda", 9), stick("sdb", 7)];
        assert!(matches!(choose(two.clone(), None), Err(Refusal::Ambiguous(d)) if d.len() == 2));
        assert_eq!(choose(two, Some("sdb")).unwrap().id, 7);
    }

    /// Canonical `/sys/devices` paths as the kernel lays them out: a USB
    /// mass-storage disk on a root port and behind a hub's fourth port, an
    /// NVMe namespace and an ATA disk.
    #[test]
    fn the_usb_device_is_the_deepest_one_on_the_disks_path() {
        let root_port =
            "/sys/devices/pci0000:00/0000:00:14.0/usb2/2-1/2-1:1.0/host0/target0:0:0/0:0:0:0/block/sda";
        let hub = "/sys/devices/pci0000:00/0000:00:14.0/usb1/1-2/1-2.4/1-2.4:1.0/host3/target3:0:0/3:0:0:0/block/sdb";
        let nvme = "/sys/devices/pci0000:00/0000:00:06.0/0000:02:00.0/nvme/nvme0/nvme0n1";
        let ata = "/sys/devices/pci0000:00/0000:00:17.0/ata1/host1/target1:0:0/1:0:0:0/block/sdc";
        assert_eq!(
            sysfs::usb_device(root_port),
            Some(("/sys/devices/pci0000:00/0000:00:14.0/usb2/2-1", "2-1"))
        );
        assert_eq!(
            sysfs::usb_device(hub),
            Some(("/sys/devices/pci0000:00/0000:00:14.0/usb1/1-2/1-2.4", "1-2.4"))
        );
        assert_eq!(sysfs::usb_device(nvme), None);
        assert_eq!(sysfs::usb_device(ata), None);
    }

    #[test]
    fn the_disk_under_the_name_after_the_prompt_must_be_the_candidate_shown() {
        let shown = stick("sda", 9);
        assert_eq!(still(&shown, shown.clone()), Ok(()));
        assert!(matches!(still(&shown, stick("sda", 10)), Err(Refusal::Moved { .. })), "replugged");
        let fixed = disk("sda", Some(("2-1", "fixed\n")), 9);
        assert!(matches!(still(&shown, fixed), Err(Refusal::Moved { .. })), "no longer a candidate");
    }

    #[test]
    fn a_descriptor_reaching_another_disk_writes_nothing() {
        let shown = stick("sda", 9);
        assert_eq!(reaches(&shown, 9), Ok(()));
        assert!(matches!(reaches(&shown, 10), Err(Refusal::Reached { diskseq: 10, .. })));
    }

    #[test]
    fn the_image_is_the_one_named_and_only_one() {
        let words = |w: &[&str]| w.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(matches!(image(&words(&["--image", "a.img", "--diag-boot"])), Err(Refusal::Usage(_))));
        assert_eq!(image(&words(&["--image", "a.img"])), Ok((PathBuf::from("a.img"), None)));
    }

    #[test]
    fn the_image_lands_whole_from_its_first_byte_after_its_check() {
        let dir = toyos_tmpdir::TempDir::new("flash");
        let mut image: Vec<u8> = (0..CHUNK + 3 * 512).map(|i| (i % 251) as u8).collect();
        let last = image.len() - 512;
        image[last..last + 8].copy_from_slice(b"EFI PART");
        let at = dir.join("built.img");
        std::fs::write(&at, &image).unwrap();

        let (mut file, bytes) = crate::metal::whole_disk(&at).unwrap();
        let mut device = Vec::new();
        copy(&mut file, bytes, &mut device).unwrap();
        assert!(device == image, "the disk holds other bytes than the image");
    }
}
