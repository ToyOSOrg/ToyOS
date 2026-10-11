//! `toyos-flash` — the image a build made, written whole to a USB stick on this
//! host, choosing the stick rather than being told which one it is: a disk's
//! name moves between plug-ins, so a command that carries one eventually names
//! the wrong disk.
//!
//! Two gates stand between it and a disk of this host. The first is the
//! candidate set, every whole disk of a physical device that says it is
//! external; the second, [`usb_stick`], asks the chosen disk again, on its own
//! account and after it is opened, whether it is external and on USB. A disk
//! that answers differently once opened — another disk under the name, or the
//! same disk saying otherwise — is refused before a byte is written.
//!
//! Each host answers from its kernel's own account of the disk, and runs no
//! host program: macOS from the IOKit registry, Linux from `/sys/class/block`
//! as [`crate::metal`] reads the T14's stick. Any other host is refused by
//! name.
//!
//! **Root writes; nothing here asks for it.** Run unprivileged, it shows the
//! disk it chose and refuses with the `sudo` line that writes it; run as root,
//! it shows the disk again and writes only on a typed `y`. No password passes
//! through it.

use std::fmt;
use std::io::{BufRead, Write};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use crate::arch::Arch;
use crate::build::Boot;
use crate::flags::{Flag, Value, Vocabulary, CONSOLE_BOOT, DIAG_BOOT};

/// `--disk <name>`: which candidate, where more than one is attached.
const DISK: Flag = Flag { name: "--disk", value: Value::Next };

/// The image is the build's own: `cargo run -- --build-only` and its two
/// framebuffer modes write the three this chooses between.
const FLASH: Vocabulary = Vocabulary(&[&DIAG_BOOT, &CONSOLE_BOOT, &DISK]);

/// One whole disk, as its host's kernel describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disk {
    /// `disk4` on macOS, `sdb` on Linux.
    pub name: String,
    /// A whole disk of a device, and not one synthesised over another disk's
    /// partition or a file.
    pub physical: bool,
    /// The device says it is not built into this machine.
    pub external: bool,
    /// The interconnect the device says it is on.
    pub bus: String,
    pub bytes: u64,
    pub vendor: String,
    pub model: String,
    /// What names this disk and no other for as long as the host is up: the
    /// IOKit registry entry's ID, Linux's `diskseq`.
    pub id: u64,
}

impl fmt::Display for Disk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} \"{} {}\", {} bytes, {}, {}",
            host::node(&self.name),
            self.vendor,
            self.model,
            self.bytes,
            self.bus,
            if self.external { "external" } else { "internal" },
        )
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
    /// `--disk` named something the first gate did not admit.
    NotACandidate { asked: String, candidates: Vec<Disk> },
    /// The second gate: the disk did not say external and USB.
    NotAUsbStick(Box<Disk>),
    /// The opened disk is not the one that was shown.
    Moved { shown: Box<Disk>, opened: Box<Disk> },
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
            Self::Host(os) => write!(
                f,
                "toyos-flash reads disks on Linux and macOS; this host is {os}, and nothing here \
                 reads its disks"
            ),
            Self::Read(why) => write!(f, "the host's disks do not read: {why}"),
            Self::Image(why) => write!(f, "the image is not flashable: {why}"),
            Self::NoCandidate => write!(f, "no external disk of a physical device is attached"),
            Self::Ambiguous(disks) => {
                write!(f, "more than one external disk is attached; name one with --disk:{}", list(disks))
            }
            Self::NotACandidate { asked, candidates } => write!(
                f,
                "--disk {asked} is no external disk of a physical device; the candidates are:{}",
                if candidates.is_empty() { " none".to_string() } else { list(candidates) }
            ),
            Self::NotAUsbStick(disk) => {
                write!(f, "{disk} does not say it is external and on USB, so it is not written")
            }
            Self::Moved { shown, opened } => write!(
                f,
                "the disk opened is not the one shown, so nothing was written:\n  shown  {shown}\n  \
                 opened {opened}"
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

/// The second gate: the disk says, on its own account, that it is external and
/// on USB.
fn usb_stick(disk: &Disk) -> Result<(), Refusal> {
    if disk.external && disk.bus == "USB" {
        Ok(())
    } else {
        Err(Refusal::NotAUsbStick(Box::new(disk.clone())))
    }
}

/// The disk to write: the one candidate, or the candidate `asked` names, and
/// through the second gate. A candidate is a whole disk of a physical device
/// that says it is external.
pub fn choose(disks: Vec<Disk>, asked: Option<&str>) -> Result<Disk, Refusal> {
    let mut candidates: Vec<Disk> = disks.into_iter().filter(|d| d.physical && d.external).collect();
    let chosen = match asked {
        Some(asked) => match candidates.iter().position(|d| d.name == asked) {
            Some(at) => candidates.swap_remove(at),
            None => return Err(Refusal::NotACandidate { asked: asked.to_string(), candidates }),
        },
        None => match candidates.len() {
            0 => return Err(Refusal::NoCandidate),
            1 => candidates.remove(0),
            _ => return Err(Refusal::Ambiguous(candidates)),
        },
    };
    usb_stick(&chosen)?;
    Ok(chosen)
}

/// The disk an open descriptor reaches, read afresh, is the one shown and still
/// passes the second gate.
fn still(shown: &Disk, opened: Disk) -> Result<(), Refusal> {
    if opened.id != shown.id || opened.name != shown.name {
        return Err(Refusal::Moved { shown: Box::new(shown.clone()), opened: Box::new(opened) });
    }
    usb_stick(&opened)
}

/// The image the build wrote for the mode the command line names.
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
    let boot = match (FLASH.present(args, &DIAG_BOOT), FLASH.present(args, &CONSOLE_BOOT)) {
        (false, false) => Boot::shipped(root),
        (true, false) => Boot::diag(root),
        (false, true) => Boot::console(root),
        (true, true) => {
            return Err(Refusal::Usage("--diag-boot and --console-boot are two images; flash one".into()))
        }
    };
    let disk = FLASH.value(args, &DISK).map(str::to_string);
    Ok((root.join(boot.image_for(Arch::X86_64)), disk))
}

/// One read and one write of the copy, a whole number of sectors.
const CHUNK: usize = 4 << 20;

/// Show the chosen disk, and as root, write `args`' image to it.
pub fn run(args: &[String]) -> Result<(), Refusal> {
    let (path, asked) = image(args)?;
    let (file, bytes) =
        crate::metal::whole_disk(&path).map_err(|r| Refusal::Image(format!("{}: {r}", path.display())))?;
    let disk = choose(host::disks()?, asked.as_deref())?;
    if bytes > disk.bytes {
        return Err(Refusal::TooSmall { image: bytes, disk: Box::new(disk) });
    }
    println!("image  {}, {bytes} bytes", path.display());
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
    print!("erase {} and write the image to it? [y/N] ", host::node(&disk.name));
    std::io::stdout().flush().map_err(io("asking"))?;
    let mut reply = String::new();
    std::io::stdin().lock().read_line(&mut reply).map_err(io("reading the answer"))?;
    if reply.trim() != "y" {
        return Err(Refusal::Declined);
    }

    host::unmount(&disk)?;
    let (mut device, opened) = host::open(&disk)?;
    still(&disk, opened)?;

    let began = std::time::Instant::now();
    copy(&file, bytes, &mut device)?;
    host::finish(device)?;
    println!("wrote {bytes} bytes to {} in {:.1} s", host::node(&disk.name), began.elapsed().as_secs_f64());
    Ok(())
}

/// The image's `bytes` onto `device`, from the image's first byte whatever
/// its cursor.
fn copy(image: &std::fs::File, bytes: u64, device: &mut impl Write) -> Result<(), Refusal> {
    let mut buf = vec![0u8; CHUNK];
    let mut at = 0u64;
    while at < bytes {
        let want = CHUNK.min((bytes - at) as usize);
        image.read_exact_at(&mut buf[..want], at).map_err(io("reading the image"))?;
        device.write_all(&buf[..want]).map_err(io("writing the disk"))?;
        at += want as u64;
    }
    Ok(())
}

/// Linux's account of a disk, out of `/sys/class/block/<name>`.
#[cfg(any(target_os = "linux", test))]
mod sysfs {
    use super::Disk;
    use crate::metal::Identity;

    /// `devpath` is where the disk's `/sys/class/block` link resolves, which
    /// passes through a USB root hub, `usb<bus>`, exactly where the disk is on
    /// USB, and lies under `/devices/virtual/` exactly where no device is
    /// behind it.
    pub fn disk(name: &str, identity: &Identity, devpath: &str, diskseq: u64) -> Disk {
        let usb = devpath.split('/').any(|part| {
            part.strip_prefix("usb").is_some_and(|bus| !bus.is_empty() && bus.bytes().all(|b| b.is_ascii_digit()))
        });
        Disk {
            name: name.to_string(),
            physical: !devpath.contains("/devices/virtual/"),
            external: identity.removable,
            bus: if usb { "USB" } else { "not USB" }.to_string(),
            bytes: identity.sectors * 512,
            vendor: identity.vendor.clone(),
            model: identity.model.clone(),
            id: diskseq,
        }
    }
}

/// macOS's account of a disk, out of its `IOMedia` registry entry.
#[cfg(any(target_os = "macos", test))]
mod registry {
    use super::Disk;

    /// What the registry answered for one `IOMedia`: the entry's own
    /// properties, the class of its provider, and the two dictionaries the
    /// nearest ancestor carrying each holds.
    #[derive(Debug, Clone, Default)]
    pub struct Media {
        pub bsd_name: Option<String>,
        pub whole: bool,
        pub size: u64,
        /// The provider is an `IOBlockStorageDriver`: the media of a device,
        /// not one an APFS container or a disk image synthesises.
        pub on_a_driver: bool,
        /// `Protocol Characteristics`' `Physical Interconnect`.
        pub interconnect: Option<String>,
        /// `Protocol Characteristics`' `Physical Interconnect Location`.
        pub location: Option<String>,
        /// `Device Characteristics`' `Vendor Name` and `Product Name`.
        pub vendor: Option<String>,
        pub product: Option<String>,
        pub entry_id: u64,
    }

    /// A whole disk's [`Disk`]; `None` for a partition or a nameless entry.
    pub fn disk(media: &Media) -> Option<Disk> {
        let name = media.bsd_name.clone().filter(|_| media.whole)?;
        Some(Disk {
            name,
            physical: media.on_a_driver,
            external: media.location.as_deref() == Some("External"),
            bus: media.interconnect.clone().unwrap_or_else(|| "no interconnect".to_string()),
            bytes: media.size,
            vendor: media.vendor.clone().unwrap_or_default(),
            model: media.product.clone().unwrap_or_default(),
            id: media.entry_id,
        })
    }
}

#[cfg(target_os = "linux")]
mod host {
    use std::fs::{self, File, OpenOptions};
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;

    use super::{io, sysfs, Disk, Refusal};
    use crate::metal::{Identity, Node};

    /// `BLKGETDISKSEQ`, `_IOR(0x12, 128, __u64)` in `linux/fs.h`.
    const BLKGETDISKSEQ: libc::c_ulong = 0x8008_1280;

    pub fn node(name: &str) -> String {
        format!("/dev/{name}")
    }

    fn read(path: &str) -> Result<String, Refusal> {
        fs::read_to_string(path).map_err(|e| Refusal::Read(format!("{path}: {e}")))
    }

    fn disk(node: &Node) -> Result<Disk, Refusal> {
        let at = node.sysfs();
        let identity = [
            format!("{at}/size"),
            format!("{at}/device/vendor"),
            format!("{at}/device/model"),
            format!("{at}/removable"),
        ]
        .iter()
        .map(|p| read(p))
        .collect::<Result<Vec<_>, _>>()?
        .concat();
        let identity = Identity::parse(&identity).map_err(|r| Refusal::Read(format!("{at}: {r}")))?;
        let devpath = fs::read_link(&at).map_err(|e| Refusal::Read(format!("{at}: {e}")))?;
        let seq = read(&format!("{at}/diskseq"))?;
        let seq = seq.trim().parse().map_err(|_| Refusal::Read(format!("{at}/diskseq: {seq:?}")))?;
        Ok(sysfs::disk(&node.name(), &identity, &devpath.to_string_lossy(), seq))
    }

    /// Every SCSI and NVMe whole disk, the only nodes [`Node`] can name.
    pub fn disks() -> Result<Vec<Disk>, Refusal> {
        let dir = "/sys/class/block";
        let entries = fs::read_dir(dir).map_err(|e| Refusal::Read(format!("{dir}: {e}")))?;
        let mut disks = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| Refusal::Read(format!("{dir}: {e}")))?;
            if let Ok(node) = Node::parse(&format!("/dev/{}", entry.file_name().to_string_lossy())) {
                disks.push(disk(&node)?);
            }
        }
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
    /// on it is mounted — and read afresh, with the `diskseq` of what the
    /// descriptor reaches.
    pub fn open(disk: &Disk) -> Result<(File, Disk), Refusal> {
        let path = node(&disk.name);
        let file = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_EXCL)
            .open(&path)
            .map_err(io(format!("opening {path}")))?;
        let mut seq = 0u64;
        // SAFETY: `BLKGETDISKSEQ` writes one `u64`.
        if unsafe { libc::ioctl(file.as_raw_fd(), BLKGETDISKSEQ, &mut seq) } != 0 {
            return Err(io(format!("asking {path} its diskseq"))(std::io::Error::last_os_error()));
        }
        let opened = self::disk(&Node::parse(&path).expect("a candidate is a Node"))?;
        if opened.id != seq {
            return Err(Refusal::Moved { shown: Box::new(disk.clone()), opened: Box::new(opened) });
        }
        Ok((file, opened))
    }

    pub fn finish(device: File) -> Result<(), Refusal> {
        device.sync_all().map_err(io("flushing the disk"))
    }
}

#[cfg(target_os = "macos")]
mod host {
    use std::ffi::{c_char, CStr, CString};
    use std::fs::{File, OpenOptions};
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::MetadataExt;

    use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType};
    use objc2_io_kit::{
        kIOMainPortDefault, kIORegistryIterateParents, kIORegistryIterateRecursively, kIOServicePlane,
        io_object_t, IOBSDNameMatching, IOIteratorNext, IOObjectConformsTo, IOObjectRelease,
        IORegistryEntryCreateCFProperty, IORegistryEntryGetParentEntry, IORegistryEntryGetRegistryEntryID,
        IORegistryEntrySearchCFProperty, IOServiceGetMatchingService, IOServiceGetMatchingServices,
        IOServiceMatching,
    };

    use super::registry::{self, Media};
    use super::{io, Disk, Refusal};

    /// `DKIOCSYNCHRONIZE`, `_IOW('d', 22, dk_synchronize_t)` in `sys/disk.h`,
    /// and its 24-byte argument; zero asks for the whole medium.
    const DKIOCSYNCHRONIZE: libc::c_ulong = 0x8018_6416;
    /// `DKIOCEJECT`, `_IO('d', 21)` in `sys/disk.h`.
    const DKIOCEJECT: libc::c_ulong = 0x2000_6415;

    /// The raw node: no buffer cache between the copy and the device.
    pub fn node(name: &str) -> String {
        format!("/dev/r{name}")
    }

    /// An IOKit object this module holds a reference to.
    struct Object(io_object_t);

    impl Drop for Object {
        fn drop(&mut self) {
            IOObjectRelease(self.0);
        }
    }

    /// An `io_name_t`, which IOKit reads as a C string.
    fn name(text: &CStr) -> *mut [c_char; 128] {
        text.as_ptr().cast_mut().cast()
    }

    fn property(entry: &Object, key: &str) -> Option<CFRetained<CFType>> {
        // SAFETY: a live entry and a CFString key.
        unsafe { IORegistryEntryCreateCFProperty(entry.0, Some(&CFString::from_str(key)), None, 0) }
    }

    /// The nearest ancestor's dictionary `key`, its string `field`.
    fn inherited(entry: &Object, key: &str, field: &str) -> Option<String> {
        let options = kIORegistryIterateParents | kIORegistryIterateRecursively;
        // SAFETY: a live entry, the service plane's name and a CFString key.
        let found = unsafe {
            IORegistryEntrySearchCFProperty(entry.0, name(kIOServicePlane), Some(&CFString::from_str(key)), None, options)
        }?;
        let dict = found.downcast::<CFDictionary>().ok()?;
        // SAFETY: a registry dictionary is keyed by CFString and holds CFTypes.
        let dict = unsafe { CFRetained::cast_unchecked::<CFDictionary<CFString, CFType>>(dict) };
        let value = dict.get(&CFString::from_str(field))?;
        Some(value.downcast::<CFString>().ok()?.to_string())
    }

    fn string(entry: &Object, key: &str) -> Option<String> {
        Some(property(entry, key)?.downcast::<CFString>().ok()?.to_string())
    }

    fn number(entry: &Object, key: &str) -> Option<i64> {
        property(entry, key)?.downcast::<CFNumber>().ok()?.as_i64()
    }

    fn media(entry: &Object) -> Media {
        let mut parent = 0;
        // SAFETY: a live entry; the parent is released when `Object` drops.
        let on_a_driver = unsafe { IORegistryEntryGetParentEntry(entry.0, name(kIOServicePlane), &mut parent) } == 0
            && {
                let parent = Object(parent);
                // SAFETY: a live entry and a class name.
                unsafe { IOObjectConformsTo(parent.0, name(c"IOBlockStorageDriver")) }
            };
        let mut entry_id = 0;
        // SAFETY: a live entry.
        unsafe { IORegistryEntryGetRegistryEntryID(entry.0, &mut entry_id) };
        Media {
            bsd_name: string(entry, "BSD Name"),
            whole: property(entry, "Whole").and_then(|v| Some(v.downcast::<CFBoolean>().ok()?.as_bool())) == Some(true),
            size: number(entry, "Size").and_then(|n| u64::try_from(n).ok()).unwrap_or(0),
            on_a_driver,
            interconnect: inherited(entry, "Protocol Characteristics", "Physical Interconnect"),
            location: inherited(entry, "Protocol Characteristics", "Physical Interconnect Location"),
            vendor: inherited(entry, "Device Characteristics", "Vendor Name"),
            product: inherited(entry, "Device Characteristics", "Product Name"),
            entry_id,
        }
    }

    /// Every whole `IOMedia` in the registry.
    pub fn disks() -> Result<Vec<Disk>, Refusal> {
        let mut iterator = 0;
        // SAFETY: the matching dictionary is consumed; the iterator is released below.
        let status = unsafe {
            let matching = IOServiceMatching(c"IOMedia".as_ptr()).map(|m| CFRetained::cast_unchecked(m));
            IOServiceGetMatchingServices(kIOMainPortDefault, matching, &mut iterator)
        };
        if status != 0 {
            return Err(Refusal::Read(format!("IOServiceGetMatchingServices answered {status:#x}")));
        }
        let iterator = Object(iterator);
        let mut disks = Vec::new();
        loop {
            let entry = IOIteratorNext(iterator.0);
            if entry == 0 {
                break;
            }
            disks.extend(registry::disk(&media(&Object(entry))));
        }
        Ok(disks)
    }

    /// The `IOMedia` named `bsd_name`, with its BSD device number.
    fn by_name(bsd_name: &str) -> Result<(Disk, i64, i64), Refusal> {
        let c = CString::new(bsd_name).expect("a BSD name holds no NUL");
        // SAFETY: a NUL-terminated name; the matching dictionary is consumed.
        let entry = unsafe {
            let matching = IOBSDNameMatching(kIOMainPortDefault, 0, c.as_ptr()).map(|m| CFRetained::cast_unchecked(m));
            IOServiceGetMatchingService(kIOMainPortDefault, matching)
        };
        if entry == 0 {
            return Err(Refusal::Read(format!("no IOMedia is named {bsd_name}")));
        }
        let entry = Object(entry);
        let disk = registry::disk(&media(&entry))
            .ok_or_else(|| Refusal::Read(format!("{bsd_name} is no whole disk")))?;
        let dev = |key| number(&entry, key).ok_or_else(|| Refusal::Read(format!("{bsd_name} has no {key}")));
        Ok((disk, dev("BSD Major")?, dev("BSD Minor")?))
    }

    /// Unmount every filesystem whose device is the disk or one of its slices.
    pub fn unmount(disk: &Disk) -> Result<(), Refusal> {
        let mut mounts: *mut libc::statfs = std::ptr::null_mut();
        // SAFETY: `getmntinfo` points `mounts` at its own buffer of `count` entries.
        let count = unsafe { libc::getmntinfo(&mut mounts, libc::MNT_NOWAIT) };
        if count <= 0 {
            return Err(io("listing the mounts")(std::io::Error::last_os_error()));
        }
        // SAFETY: as above; the buffer lives until the next `getmntinfo`.
        let mounts = unsafe { std::slice::from_raw_parts(mounts, count as usize) };
        let whole = format!("/dev/{}", disk.name);
        let points: Vec<CString> = mounts
            .iter()
            .filter(|m| {
                // SAFETY: `f_mntfromname` is NUL-terminated.
                let from = unsafe { CStr::from_ptr(m.f_mntfromname.as_ptr()) }.to_string_lossy();
                from.strip_prefix(&whole).is_some_and(|rest| {
                    rest.is_empty()
                        || rest.strip_prefix('s').is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                })
            })
            // SAFETY: `f_mntonname` is NUL-terminated.
            .map(|m| unsafe { CStr::from_ptr(m.f_mntonname.as_ptr()) }.to_owned())
            .collect();
        for point in points {
            println!("unmounting {}", point.to_string_lossy());
            // SAFETY: a NUL-terminated path.
            if unsafe { libc::unmount(point.as_ptr(), 0) } != 0 {
                return Err(io(format!("unmounting {}", point.to_string_lossy()))(std::io::Error::last_os_error()));
            }
        }
        Ok(())
    }

    /// The raw node, opened for writing — which the kernel refuses while a
    /// slice of it is mounted — and the registry's entry for the device number
    /// the descriptor reaches.
    pub fn open(disk: &Disk) -> Result<(File, Disk), Refusal> {
        let path = node(&disk.name);
        let file = OpenOptions::new().write(true).open(&path).map_err(io(format!("opening {path}")))?;
        let rdev = file.metadata().map_err(io(format!("reading {path}")))?.rdev();
        let (opened, major, minor) = by_name(&disk.name)?;
        let rdev = rdev as libc::dev_t;
        if (i64::from(libc::major(rdev)), i64::from(libc::minor(rdev))) != (major, minor) {
            return Err(Refusal::Moved { shown: Box::new(disk.clone()), opened: Box::new(opened) });
        }
        Ok((file, opened))
    }

    /// Flush the device's cache, then eject the medium while this descriptor
    /// is its only opener, so no volume of the new table is mounted after it.
    pub fn finish(device: File) -> Result<(), Refusal> {
        let whole = [0u8; 24];
        for (what, request, arg) in [
            ("flushing the disk", DKIOCSYNCHRONIZE, whole.as_ptr()),
            ("ejecting the disk", DKIOCEJECT, std::ptr::null()),
        ] {
            // SAFETY: each request reads at most its 24-byte argument.
            if unsafe { libc::ioctl(device.as_raw_fd(), request, arg) } != 0 {
                return Err(io(what)(std::io::Error::last_os_error()));
            }
        }
        Ok(())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod host {
    use super::{Disk, Refusal};

    pub fn node(name: &str) -> String {
        name.to_string()
    }
    pub fn disks() -> Result<Vec<Disk>, Refusal> {
        Err(Refusal::Host(std::env::consts::OS))
    }
    pub fn unmount(_: &Disk) -> Result<(), Refusal> {
        unreachable!("no disk is chosen on a host with no reader")
    }
    pub fn open(_: &Disk) -> Result<(std::fs::File, Disk), Refusal> {
        unreachable!("no disk is chosen on a host with no reader")
    }
    pub fn finish(_: std::fs::File) -> Result<(), Refusal> {
        unreachable!("no disk is chosen on a host with no reader")
    }
}

#[cfg(test)]
mod tests {
    use super::registry::{self, Media};
    use super::*;
    use crate::metal::Identity;

    /// What this development Mac's registry answered for its four whole
    /// `IOMedia` entries, read by [`host::disks`]: the internal SSD, two of the
    /// APFS containers' synthesised disks over its partitions, and a mounted
    /// disk image.
    fn this_mac() -> Vec<Media> {
        let ssd = |name: &str, size, on_a_driver, entry_id| Media {
            bsd_name: Some(name.to_string()),
            whole: true,
            size,
            on_a_driver,
            interconnect: Some("Apple Fabric".to_string()),
            location: Some("Internal".to_string()),
            vendor: Some(String::new()),
            product: Some("APPLE SSD AP1024Z".to_string()),
            entry_id,
        };
        vec![
            ssd("disk0", 1_000_555_581_440, true, 4_294_970_307),
            ssd("disk1", 524_288_000, false, 4_294_970_324),
            ssd("disk3", 994_662_584_320, false, 4_294_970_325),
            Media {
                bsd_name: Some("disk5".to_string()),
                whole: true,
                size: 118_489_088,
                on_a_driver: true,
                interconnect: Some("Virtual Interface".to_string()),
                location: Some("File".to_string()),
                vendor: Some("Apple".to_string()),
                product: Some("Disk Image".to_string()),
                entry_id: 4_296_178_321,
            },
        ]
    }

    /// A device's whole disk on `interconnect` at `location`, in the words
    /// `IOStorageProtocolCharacteristics.h` defines for them.
    fn device(name: &str, interconnect: &str, location: &str) -> Media {
        Media {
            bsd_name: Some(name.to_string()),
            whole: true,
            size: 30_752_000_000,
            on_a_driver: true,
            interconnect: Some(interconnect.to_string()),
            location: Some(location.to_string()),
            vendor: Some("SanDisk".to_string()),
            product: Some("Ultra".to_string()),
            entry_id: 4_296_200_000,
        }
    }

    fn mac(extra: &[Media]) -> Vec<Disk> {
        this_mac().iter().chain(extra).filter_map(registry::disk).collect()
    }

    #[test]
    fn no_disk_of_this_mac_is_a_candidate() {
        assert_eq!(choose(mac(&[]), None), Err(Refusal::NoCandidate));
        for name in ["disk0", "disk1", "disk3", "disk5"] {
            assert!(
                matches!(choose(mac(&[]), Some(name)), Err(Refusal::NotACandidate { .. })),
                "{name} was admitted"
            );
        }
    }

    #[test]
    fn a_usb_stick_beside_this_macs_disks_is_the_one_chosen() {
        let stick = choose(mac(&[device("disk4", "USB", "External")]), None).unwrap();
        assert_eq!((stick.name.as_str(), stick.id), ("disk4", 4_296_200_000));
    }

    #[test]
    fn an_external_disk_not_on_usb_passes_the_first_gate_and_not_the_second() {
        let thunderbolt = mac(&[device("disk4", "PCI-Express", "External")]);
        assert!(matches!(choose(thunderbolt, None), Err(Refusal::NotAUsbStick(d)) if d.name == "disk4"));
    }

    #[test]
    fn a_usb_disk_built_into_the_machine_is_no_candidate() {
        for location in ["Internal", "Internal/External"] {
            let reader = mac(&[device("disk4", "USB", location)]);
            assert_eq!(choose(reader, None), Err(Refusal::NoCandidate), "{location}");
        }
    }

    #[test]
    fn a_disk_synthesised_over_a_sticks_partition_is_not_the_stick() {
        let stick = device("disk4", "USB", "External");
        let container = Media { bsd_name: Some("disk6".to_string()), on_a_driver: false, entry_id: 1, ..stick.clone() };
        let chosen = choose(mac(&[stick, container]), None).unwrap();
        assert_eq!(chosen.name, "disk4");
    }

    #[test]
    fn two_sticks_are_refused_until_one_is_named() {
        let second = Media { entry_id: 7, ..device("disk6", "USB", "External") };
        let two = mac(&[device("disk4", "USB", "External"), second]);
        assert!(matches!(choose(two.clone(), None), Err(Refusal::Ambiguous(d)) if d.len() == 2));
        assert_eq!(choose(two, Some("disk6")).unwrap().id, 7);
    }

    /// A disk `/sys/class/block` describes: `metal`'s parse of its four
    /// files, and where its link resolves, laid out as the kernel lays out a
    /// USB mass-storage disk, an NVMe namespace and a loop device.
    fn linux(name: &str, removable: bool, devpath: &str, diskseq: u64) -> Disk {
        let text = format!("60062500\nSanDisk \nUltra           \n{}\n", u8::from(removable));
        sysfs::disk(name, &Identity::parse(&text).unwrap(), devpath, diskseq)
    }

    const USB: &str = "../../devices/pci0000:00/0000:00:14.0/usb2/2-1/2-1:1.0/host0/target0:0:0/0:0:0:0/block/sda";
    const NVME: &str = "../../devices/pci0000:00/0000:00:06.0/0000:02:00.0/nvme/nvme0/nvme0n1";
    const ATA: &str = "../../devices/pci0000:00/0000:00:17.0/ata1/host1/target1:0:0/1:0:0:0/block/sdb";

    #[test]
    fn a_removable_usb_disk_is_the_one_chosen_on_linux() {
        let disks = vec![linux("nvme0n1", false, NVME, 1), linux("sda", true, USB, 9)];
        let stick = choose(disks, None).unwrap();
        assert_eq!((stick.name.as_str(), stick.bytes, stick.id), ("sda", 60_062_500 * 512, 9));
    }

    #[test]
    fn a_fixed_disk_is_no_candidate_on_linux() {
        let disks = vec![linux("nvme0n1", false, NVME, 1), linux("sda", false, USB, 9)];
        assert_eq!(choose(disks.clone(), None), Err(Refusal::NoCandidate));
        assert!(matches!(choose(disks, Some("nvme0n1")), Err(Refusal::NotACandidate { .. })));
    }

    #[test]
    fn a_removable_disk_not_on_usb_is_refused_on_linux() {
        assert!(matches!(choose(vec![linux("sdb", true, ATA, 3)], None), Err(Refusal::NotAUsbStick(_))));
    }

    #[test]
    fn a_virtual_disk_is_no_candidate_on_linux() {
        let virt = linux("sdc", true, "../../devices/virtual/block/sdc", 4);
        assert_eq!(choose(vec![virt], None), Err(Refusal::NoCandidate));
    }

    #[test]
    fn the_image_lands_whole_from_its_first_byte_after_its_check() {
        let dir = toyos_tmpdir::TempDir::new("flash");
        let mut image: Vec<u8> = (0..CHUNK + 3 * 512).map(|i| (i % 251) as u8).collect();
        let last = image.len() - 512;
        image[last..last + 8].copy_from_slice(b"EFI PART");
        let at = dir.join("built.img");
        std::fs::write(&at, &image).unwrap();

        let (file, bytes) = crate::metal::whole_disk(&at).unwrap();
        let mut device = Vec::new();
        copy(&file, bytes, &mut device).unwrap();
        assert!(device == image, "the disk holds other bytes than the image");
    }

    #[test]
    fn the_opened_disk_is_the_shown_one_and_still_a_usb_stick() {
        let shown = linux("sda", true, USB, 9);
        assert_eq!(still(&shown, shown.clone()), Ok(()));
        let replugged = linux("sda", true, USB, 10);
        assert!(matches!(still(&shown, replugged), Err(Refusal::Moved { .. })));
        let now_fixed = linux("sda", false, USB, 9);
        assert!(matches!(still(&shown, now_fixed), Err(Refusal::NotAUsbStick(_))));
    }
}
