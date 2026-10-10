//! The survey: what this machine is, written to the stick through the
//! firmware's own FAT driver before the kernel runs, so a kernel that hangs or
//! dies on a machine it has never seen has already left the machine's
//! description behind.
//!
//! **It reads the machine and writes nothing to it.** Memory is read only
//! where firmware's own map describes the whole range (`watchdog::described`),
//! configuration space only on the buses the MCFG decodes and a bridge forwards, and nothing
//! here calls a variable service, sets a GOP mode or writes a port. The one
//! thing read and not copied is the MSDM table's body: it is the machine's
//! Windows product key.
//!
//! Each stage writes its own files, flushed and closed, into one directory per
//! boot on the log partition, and says a line in `loader.log` before it starts
//! and after it ends: a hang names the stage it was in and leaves every
//! earlier one on the stick. The firmware's watchdog is restarted at each
//! stage, so a stage that hangs resets the machine rather than holding it.

use alloc::collections::BTreeSet;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use uefi::proto::console::gop::{GraphicsOutput, PixelFormat};
use uefi::proto::media::file::{Directory, File, FileAttribute, FileMode};
use uefi::proto::unsafe_protocol;
use uefi::table::cfg::{SMBIOS3_GUID, SMBIOS_GUID};
use uefi::prelude::*;
use uefi::CString16;

use crate::{loaderlog, protocol, watchdog};

const HEAD: &str = "Survey:";

/// The largest table copied: past every DSDT a laptop carries.
const MAX_TABLE: usize = 16 << 20;
/// The largest EDID block list a display hands over.
const MAX_EDID: usize = 32 << 10;

/// Files a stage hands back: a name in the boot's directory, and its bytes.
type Files = Vec<(String, Vec<u8>)>;

/// A stage: its name, and what it writes and says.
type Stage<'a> = (&'a str, &'a dyn Fn() -> (Files, String));

/// An SMBIOS structure type, its name, and the string-index bytes said of it.
type Structure = (u8, &'static str, &'static [(usize, &'static str)]);

/// Run every stage into a new directory on the log partition, named by the
/// firmware's clock, or by the counter at entry where it would not say.
pub fn run(st: &SystemTable<Boot>, rsdp: u64, entry_counter: u64) {
    let name = match st.runtime_services().get_time() {
        Ok(t) => alloc::format!(
            "survey-{:04}{:02}{:02}-{:02}{:02}{:02}",
            t.year(),
            t.month(),
            t.day(),
            t.hour(),
            t.minute(),
            t.second()
        ),
        Err(_) => alloc::format!("survey-tsc{entry_counter}"),
    };
    let mut dir = match loaderlog::with_open_volume(|root| directory(root, &name)) {
        Ok(Ok(dir)) => dir,
        Ok(Err(why)) | Err(why) => return println!("{HEAD} no directory {name} ({why}), so nothing is surveyed"),
    };
    println!("{HEAD} this boot's description goes to {name}");
    let stages: [Stage<'_>; 6] = [
        ("firmware", &|| firmware(st)),
        ("cpuid", &cpuid),
        ("memory map", &|| memory_map(st)),
        ("smbios", &|| smbios(st)),
        ("acpi", &|| acpi(st, rsdp)),
        ("gop", &|| gop(st)),
    ];
    for (stage, survey) in stages {
        restart(st);
        println!("{HEAD} {stage} begins");
        let (files, said) = survey();
        let mut bytes = 0usize;
        for (file, content) in &files {
            match put(&mut dir, file, content) {
                Ok(()) => bytes += content.len(),
                Err(why) => println!("{HEAD} {file}: {why}"),
            }
        }
        println!("{HEAD} {stage}: {said}; {} file(s), {bytes} bytes", files.len());
    }
    // Last, after every other stage is on the stick.
    restart(st);
    if skip_pci_set() {
        println!("{HEAD} pci skipped: {SKIP_PCI} is on the log partition, left by a boot that began the PCI stage and never ended it, or put there by hand");
    } else {
        skip_pci_mark(true);
        println!("{HEAD} pci begins");
        let said = pci(st, rsdp, &mut dir);
        skip_pci_mark(false);
        println!("{HEAD} pci: {said}");
    }
    restart(st);
}

/// The firmware's watchdog, started over: each stage, and the loader after
/// the last, is held to the whole bound on its own.
fn restart(st: &SystemTable<Boot>) {
    if let Err(e) = st.boot_services().set_watchdog_timer(crate::FIRMWARE_WATCHDOG_SECS, crate::WATCHDOG_CODE, None) {
        println!("{HEAD} firmware refused to restart its watchdog ({e})");
    }
}

fn directory(root: &mut Directory, name: &str) -> Result<Directory, String> {
    let name16 = CString16::try_from(name).map_err(|_| String::from("the name is not UCS-2"))?;
    root.open(&name16, FileMode::CreateReadWrite, FileAttribute::DIRECTORY)
        .map_err(|e| alloc::format!("it would not be created ({e})"))?
        .into_directory()
        .ok_or_else(|| String::from("a file of that name is there"))
}

fn put(dir: &mut Directory, name: &str, bytes: &[u8]) -> Result<(), String> {
    let name16 = CString16::try_from(name).map_err(|_| String::from("the name is not UCS-2"))?;
    let file = dir
        .open(&name16, FileMode::CreateReadWrite, FileAttribute::empty())
        .map_err(|e| alloc::format!("would not be created ({e})"))?;
    let mut file = file.into_regular_file().ok_or_else(|| String::from("is a directory"))?;
    file.write(bytes).map_err(|e| alloc::format!("took {} of {} bytes ({})", e.data(), bytes.len(), e.status()))?;
    file.flush().map_err(|e| alloc::format!("would not flush ({e})"))
}

/// `len` bytes of physical memory at `at`, where firmware's map describes the
/// whole range as one region, and a refusal by name where it does not.
fn phys(st: &SystemTable<Boot>, at: u64, len: usize) -> Result<Vec<u8>, String> {
    if at == 0 || len == 0 || !watchdog::described(st, at, len as u64) {
        return Err(alloc::format!("{at:#x}+{len:#x} is not one region of firmware's memory map, so it is not read"));
    }
    let mut bytes = Vec::with_capacity(len);
    // SAFETY: boot services identity-map physical memory, and `described`
    // found the whole range inside one region of firmware's own map.
    bytes.extend_from_slice(unsafe { core::slice::from_raw_parts(at as *const u8, len) });
    Ok(bytes)
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    b.get(at..at + 2).map_or(0, |s| u16::from_le_bytes([s[0], s[1]]))
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    b.get(at..at + 4).map_or(0, |s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    b.get(at..at + 8).map_or(0, |s| u64::from_le_bytes(s.try_into().expect("eight bytes")))
}

/// Printable ASCII of `b`, every other byte as `.`.
fn text(b: &[u8]) -> String {
    b.iter().map(|&c| if (0x20..0x7f).contains(&c) { c as char } else { '.' }).collect()
}

fn firmware(st: &SystemTable<Boot>) -> (Files, String) {
    let mut out = String::new();
    let rev = st.uefi_revision().0;
    let _ = writeln!(out, "vendor {}", st.firmware_vendor());
    let _ = writeln!(out, "firmware revision {:#010x}", st.firmware_revision());
    let _ = writeln!(out, "UEFI revision {}.{}", rev >> 16, rev & 0xffff);
    for entry in st.config_table() {
        let _ = writeln!(out, "config table {} at {:#x}", entry.guid, entry.address as u64);
    }
    let said = alloc::format!("{} UEFI {}.{}", st.firmware_vendor(), rev >> 16, rev & 0xffff);
    (alloc::vec![(String::from("firmware.txt"), out.into_bytes())], said)
}

/// Leaves that take a subleaf, every one of whose rows is kept.
const SUBLEAVED: &[u32] = &[
    0x4, 0x7, 0xb, 0xd, 0xf, 0x10, 0x12, 0x14, 0x17, 0x18, 0x1b, 0x1d, 0x1e, 0x1f, 0x20, 0x23, 0x24,
    0x8000_001d, 0x8000_0020, 0x8000_0026,
];

fn cpuid() -> (Files, String) {
    let Some(zero) = crate::arch::cpuid(0, 0) else {
        return (Vec::new(), String::from("this architecture has no CPUID"));
    };
    let leaf = |l: u32, s: u32| crate::arch::cpuid(l, s).expect("CPUID answered leaf 0");
    let hypervisor = leaf(1, 0)[2] & (1 << 31) != 0;
    let mut out = String::new();
    let mut bases = alloc::vec![0u32, 0x8000_0000];
    if hypervisor {
        bases.push(0x4000_0000);
    }
    for base in bases {
        let max = if base == 0 { zero[0] } else { leaf(base, 0)[0] };
        // A range whose base leaf names no leaf of its own range is absent.
        if max < base || max - base > 0xff {
            let _ = writeln!(out, "range {base:#x}: max {max:#x}, not a leaf of this range");
            continue;
        }
        for l in base..=max {
            let subs = if SUBLEAVED.contains(&l) { 64 } else { 1 };
            for s in 0..subs {
                let r = leaf(l, s);
                if s == 0 || r != [0; 4] {
                    let _ = writeln!(out, "{l:#010x}.{s:02x}: {:08x} {:08x} {:08x} {:08x}", r[0], r[1], r[2], r[3]);
                }
            }
        }
    }
    let mut vendor = Vec::new();
    for word in [zero[1], zero[3], zero[2]] {
        vendor.extend_from_slice(&word.to_le_bytes());
    }
    let mut brand = Vec::new();
    if leaf(0x8000_0000, 0)[0] >= 0x8000_0004 {
        for l in 0x8000_0002..=0x8000_0004u32 {
            for word in leaf(l, 0) {
                brand.extend_from_slice(&word.to_le_bytes());
            }
        }
    }
    let sig = leaf(1, 0)[0];
    let base_family = (sig >> 8) & 0xf;
    let family = if base_family == 0xf { base_family + ((sig >> 20) & 0xff) } else { base_family };
    let model = if matches!(base_family, 6 | 0xf) { (((sig >> 16) & 0xf) << 4) | ((sig >> 4) & 0xf) } else { (sig >> 4) & 0xf };
    let said = alloc::format!(
        "{} \"{}\" family {family:#x} model {model:#x} stepping {} (signature {sig:#x}){}, read on the boot CPU only",
        text(&vendor),
        text(&brand).trim_matches(|c| c == ' ' || c == '.'),
        sig & 0xf,
        if hypervisor { ", under a hypervisor" } else { "" },
    );
    let _ = writeln!(out, "{said}");
    (alloc::vec![(String::from("cpuid.txt"), out.into_bytes())], said)
}

fn memory_map(st: &SystemTable<Boot>) -> (Files, String) {
    let bs = st.boot_services();
    let sizes = bs.memory_map_size();
    let mut buffer = alloc::vec![0u8; sizes.map_size + 8 * sizes.entry_size];
    let map = match bs.memory_map(&mut buffer) {
        Ok(map) => map,
        Err(e) => return (Vec::new(), alloc::format!("firmware would not give its map ({e})")),
    };
    let mut out = String::new();
    let mut count = 0usize;
    for d in map.entries() {
        count += 1;
        let end = d.phys_start.saturating_add(d.page_count.saturating_mul(4096));
        let _ = writeln!(
            out,
            "{:#014x}..{end:#014x} {:>10} pages {:?} att={:#018x} virt={:#x}",
            d.phys_start, d.page_count, d.ty, d.att.bits(), d.virt_start
        );
    }
    (alloc::vec![(String::from("memmap.txt"), out.into_bytes())], alloc::format!("{count} descriptors"))
}

/// The strings of one SMBIOS structure at `at` in `t`, and where the next one
/// begins; `None` past the table's end.
fn smbios_strings(t: &[u8], at: usize) -> Option<(Vec<&[u8]>, usize)> {
    let len = usize::from(*t.get(at + 1)?);
    let mut i = at.checked_add(len)?;
    let mut strings = Vec::new();
    loop {
        let end = i + t.get(i..)?.iter().position(|&c| c == 0)?;
        if end == i {
            // The double NUL: the formatted area's end when no string came first.
            let next = if strings.is_empty() { end + 2 } else { end + 1 };
            return Some((strings, next));
        }
        strings.push(&t[i..end]);
        i = end + 1;
    }
}

fn smbios(st: &SystemTable<Boot>) -> (Files, String) {
    let (entry, v3) = match st.config_table().iter().find(|e| e.guid == SMBIOS3_GUID) {
        Some(e) => (e.address as u64, true),
        None => match st.config_table().iter().find(|e| e.guid == SMBIOS_GUID) {
            Some(e) => (e.address as u64, false),
            None => return (Vec::new(), String::from("firmware publishes no SMBIOS table")),
        },
    };
    let head = match phys(st, entry, if v3 { 0x18 } else { 0x1f }) {
        Ok(head) => head,
        Err(why) => return (Vec::new(), alloc::format!("entry point: {why}")),
    };
    let (version, table_at, table_len) = if v3 {
        ((head[7], head[8]), u64_at(&head, 0x10), u32_at(&head, 0x0c) as usize)
    } else {
        ((head[6], head[7]), u64::from(u32_at(&head, 0x18)), usize::from(u16_at(&head, 0x16)))
    };
    let t = match phys(st, table_at, table_len.min(MAX_TABLE)) {
        Ok(t) => t,
        Err(why) => return (Vec::new(), alloc::format!("table: {why}")),
    };
    // Which string-index bytes of which structure type are said, by name. No
    // serial number, UUID or asset tag is among them.
    const FIELDS: &[Structure] = &[
        (0, "bios", &[(4, "vendor"), (5, "version"), (8, "date")]),
        (1, "system", &[(4, "manufacturer"), (5, "product"), (6, "version"), (0x19, "sku"), (0x1a, "family")]),
        (2, "board", &[(4, "manufacturer"), (5, "product"), (6, "version")]),
        (3, "chassis", &[(4, "manufacturer")]),
        (4, "processor", &[(4, "socket"), (7, "manufacturer"), (0x10, "version")]),
    ];
    let mut out = String::new();
    let _ = writeln!(out, "SMBIOS {}.{} ({}), {table_len} bytes", version.0, version.1, if v3 { "64-bit entry" } else { "32-bit entry" });
    let mut at = 0usize;
    let mut structures = 0usize;
    let mut product = String::new();
    while at + 4 <= t.len() {
        let ty = t[at];
        let len = usize::from(t[at + 1]);
        let Some((strings, next)) = smbios_strings(&t, at) else { break };
        structures += 1;
        let _ = write!(out, "type {ty} len {len}");
        if let Some((_, name, fields)) = FIELDS.iter().find(|(t, _, _)| *t == ty) {
            let _ = write!(out, " ({name})");
            for (offset, field) in *fields {
                let index = if *offset < len { t.get(at + offset).copied().unwrap_or(0) } else { 0 };
                let value = usize::from(index).checked_sub(1).and_then(|i| strings.get(i)).map_or(String::new(), |s| text(s));
                if ty == 1 && matches!(*field, "manufacturer" | "product" | "version") {
                    product.push_str(&value);
                    product.push(' ');
                }
                let _ = write!(out, " {field}=\"{value}\"");
            }
        }
        let _ = writeln!(out);
        if ty == 127 || next <= at {
            break;
        }
        at = next;
    }
    let said = alloc::format!("{structures} structures, system \"{}\"", product.trim_end());
    (alloc::vec![(String::from("smbios.txt"), out.into_bytes())], said)
}

/// A table at `at`: its 8-byte signature-and-length head read first, then the
/// whole of it, bounded by [`MAX_TABLE`].
fn table(st: &SystemTable<Boot>, at: u64) -> Result<Vec<u8>, String> {
    let head = phys(st, at, 8)?;
    let len = u32_at(&head, 4) as usize;
    if !(8..=MAX_TABLE).contains(&len) {
        return Err(alloc::format!("{} at {at:#x} says it is {len} bytes", text(&head[..4])));
    }
    phys(st, at, len)
}

/// A signature as a file name may carry it.
fn file_sig(sig: &[u8]) -> String {
    sig.iter().map(|&c| if c.is_ascii_alphanumeric() { c as char } else { '_' }).collect()
}

fn acpi(st: &SystemTable<Boot>, rsdp: u64) -> (Files, String) {
    let mut files = Files::new();
    let mut list = String::new();
    let head = match phys(st, rsdp, 36) {
        Ok(head) => head,
        Err(why) => return (files, alloc::format!("RSDP: {why}")),
    };
    let _ = writeln!(list, "RSDP at {rsdp:#x} revision {} oem \"{}\"", head[15], text(&head[9..15]));
    files.push((String::from("acpi-RSDP.dat"), head.clone()));
    let (root_at, width) = match (head[15] >= 2, u64_at(&head, 24)) {
        (true, x) if x != 0 => (x, 8usize),
        _ => (u64::from(u32_at(&head, 16)), 4usize),
    };
    // Every table to visit, in the order found: the root's entries, then what
    // the FADT and FPDT point at, which the root does not list.
    let mut queue: Vec<u64> = alloc::vec![root_at];
    let mut seen: BTreeSet<u64> = BTreeSet::new();
    let mut sigs: Vec<[u8; 4]> = Vec::new();
    let mut index = 0usize;
    while let Some(at) = (index < queue.len()).then(|| queue[index]) {
        index += 1;
        if at == 0 || !seen.insert(at) {
            continue;
        }
        let bytes = match table(st, at) {
            Ok(bytes) => bytes,
            Err(why) => {
                let _ = writeln!(list, "{at:#x}: {why}");
                continue;
            }
        };
        let sig: [u8; 4] = bytes[..4].try_into().expect("a head of eight bytes");
        sigs.push(sig);
        // FACS, FBPT and S3PT carry no standard header and no checksum.
        let standard = bytes.len() >= 36 && !matches!(&sig, b"FACS" | b"FBPT" | b"S3PT");
        let mut line = alloc::format!("{} at {at:#x} len {}", text(&sig), bytes.len());
        if standard {
            let sum = bytes.iter().fold(0u8, |a, b| a.wrapping_add(*b));
            let _ = write!(
                line,
                " rev {} oem \"{}\" \"{}\" oemrev {:#x} creator \"{}\" {:#x} checksum {}",
                bytes[8],
                text(&bytes[10..16]),
                text(&bytes[16..24]),
                u32_at(&bytes, 24),
                text(&bytes[28..32]),
                u32_at(&bytes, 32),
                if sum == 0 { "ok" } else { "BAD" }
            );
        }
        if at == root_at && standard {
            for i in 0..(bytes.len() - 36) / width {
                let p = 36 + i * width;
                queue.push(if width == 8 { u64_at(&bytes, p) } else { u64::from(u32_at(&bytes, p)) });
            }
        }
        match &sig {
            b"FACP" => {
                let x = |off: usize, legacy: usize| match u64_at(&bytes, off) {
                    0 => u64::from(u32_at(&bytes, legacy)),
                    v => v,
                };
                queue.push(x(140, 40));
                queue.push(x(132, 36));
            }
            b"FPDT" => {
                let mut p = 36;
                while p + 16 <= bytes.len() && bytes[p + 2] >= 16 {
                    if matches!(u16_at(&bytes, p), 0 | 1) {
                        queue.push(u64_at(&bytes, p + 8));
                    }
                    p += usize::from(bytes[p + 2]);
                }
            }
            _ => {}
        }
        if &sig == b"MSDM" {
            let _ = writeln!(list, "{line} not copied: its body is the Windows product key");
            continue;
        }
        let name = alloc::format!("acpi-{}-{:02}.dat", file_sig(&sig), sigs.len() - 1);
        let _ = writeln!(list, "{line} -> {name}");
        files.push((name, bytes));
    }
    let iommu = match (sigs.contains(b"DMAR"), sigs.contains(b"IVRS")) {
        (true, true) => "both DMAR and IVRS",
        (true, false) => "Intel VT-d (DMAR)",
        (false, true) => "AMD-Vi (IVRS)",
        (false, false) => "no IOMMU table",
    };
    let _ = writeln!(list, "IOMMU: {iommu}");
    files.insert(0, (String::from("acpi.txt"), list.into_bytes()));
    (files, alloc::format!("{} tables, IOMMU: {iommu}", sigs.len()))
}

/// `EFI_EDID_ACTIVE_PROTOCOL` and `EFI_EDID_DISCOVERED_PROTOCOL` (UEFI 2.10
/// §12.9.3): one shape, two GUIDs.
#[repr(C)]
#[unsafe_protocol("bd8c1056-9f36-44ec-92a8-a6337f817986")]
struct EdidActive {
    size: u32,
    edid: *const u8,
}

#[repr(C)]
#[unsafe_protocol("1c0c34f6-d380-41fa-a049-8ad06c1a66aa")]
struct EdidDiscovered {
    size: u32,
    edid: *const u8,
}

fn gop(st: &SystemTable<Boot>) -> (Files, String) {
    let bs = st.boot_services();
    let mut files = Files::new();
    let mut out = String::new();
    let handles = bs.find_handles::<GraphicsOutput>().unwrap_or_default();
    let mut current = String::new();
    for (n, handle) in handles.iter().enumerate() {
        let Ok(gop) = protocol::get::<GraphicsOutput>(bs, *handle) else {
            let _ = writeln!(out, "gop {n}: would not open");
            continue;
        };
        let info = gop.current_mode_info();
        let (w, h) = info.resolution();
        let _ = writeln!(out, "gop {n}: current {w}x{h} {:?} stride {}", info.pixel_format(), info.stride());
        if current.is_empty() {
            current = alloc::format!("{w}x{h}");
        }
        for mode in gop.modes(bs) {
            let m = mode.info();
            let (mw, mh) = m.resolution();
            let bitmask = if m.pixel_format() == PixelFormat::Bitmask { alloc::format!(" {:?}", m.pixel_bitmask()) } else { String::new() };
            let _ = writeln!(out, "gop {n}: mode {mw}x{mh} {:?} stride {}{bitmask}", m.pixel_format(), m.stride());
        }
    }
    let mut edids = 0usize;
    let mut edid = |kind: &str, size: u32, at: *const u8, files: &mut Files, out: &mut String| {
        let len = (size as usize).min(MAX_EDID);
        match phys(st, at as u64, len) {
            Ok(bytes) => {
                let name = alloc::format!("edid-{kind}-{edids}.bin");
                let _ = writeln!(out, "edid {kind}: {size} bytes -> {name}");
                files.push((name, bytes));
                edids += 1;
            }
            Err(why) => {
                let _ = writeln!(out, "edid {kind}: {why}");
            }
        }
    };
    for handle in bs.find_handles::<EdidActive>().unwrap_or_default() {
        if let Ok(p) = protocol::get::<EdidActive>(bs, handle) {
            edid("active", p.size, p.edid, &mut files, &mut out);
        }
    }
    for handle in bs.find_handles::<EdidDiscovered>().unwrap_or_default() {
        if let Ok(p) = protocol::get::<EdidDiscovered>(bs, handle) {
            edid("discovered", p.size, p.edid, &mut files, &mut out);
        }
    }
    files.insert(0, (String::from("gop.txt"), out.into_bytes()));
    (files, alloc::format!("{} GOP(s), current {current}", handles.len()))
}

/// PCI Express link speeds by their encoding (PCIe 6.0 §7.5.3.6).
fn speed(code: u32) -> &'static str {
    match code {
        1 => "2.5GT/s",
        2 => "5GT/s",
        3 => "8GT/s",
        4 => "16GT/s",
        5 => "32GT/s",
        6 => "64GT/s",
        _ => "?",
    }
}

/// One function's line: its identity, class, BARs or bus numbers, and every
/// capability the two lists name, walked within the function's own space.
/// Whether it carries a PCI Express capability is the second half.
fn describe(cfg: &[u8], place: &str) -> (String, bool) {
    let mut line = alloc::format!(
        "{place} {:04x}:{:04x} class {:02x}{:02x}{:02x} rev {:02x} hdr {:02x}",
        u16_at(cfg, 0),
        u16_at(cfg, 2),
        cfg[0xb],
        cfg[0xa],
        cfg[0x9],
        cfg[0x8],
        cfg[0xe]
    );
    match cfg[0xe] & 0x7f {
        0 => {
            let _ = write!(line, " sub {:04x}:{:04x} bars", u16_at(cfg, 0x2c), u16_at(cfg, 0x2e));
            for b in 0..6 {
                let _ = write!(line, " {:08x}", u32_at(cfg, 0x10 + 4 * b));
            }
            let _ = write!(line, " pin {}", cfg[0x3d]);
        }
        1 => {
            let _ = write!(line, " buses {:02x}/{:02x}/{:02x} bars {:08x} {:08x}", cfg[0x18], cfg[0x19], cfg[0x1a], u32_at(cfg, 0x10), u32_at(cfg, 0x14));
        }
        _ => {}
    }
    let mut pcie = false;
    if u16_at(cfg, 6) & 0x10 != 0 {
        let _ = write!(line, " caps");
        let mut p = usize::from(cfg[0x34] & 0xfc);
        for _ in 0..48 {
            if p < 0x40 || p + 2 > 0x100 {
                break;
            }
            let id = cfg[p];
            let _ = write!(line, " {id:02x}@{p:02x}");
            if id == 0x10 && p + 0x14 <= 0x100 {
                pcie = true;
                let kind = (u16_at(cfg, p + 2) >> 4) & 0xf;
                let cap = u32_at(cfg, p + 0x0c);
                let sta = u32::from(u16_at(cfg, p + 0x12));
                let _ = write!(
                    line,
                    "(type {kind} link {}x{} of {}x{})",
                    speed(sta & 0xf),
                    (sta >> 4) & 0x3f,
                    speed(cap & 0xf),
                    (cap >> 4) & 0x3f
                );
            }
            p = usize::from(cfg[p + 1] & 0xfc);
        }
    }
    if pcie && cfg.len() >= 0x1000 {
        let _ = write!(line, " ext");
        let mut p = 0x100usize;
        for _ in 0..(0x1000 - 0x100) / 4 {
            let h = u32_at(cfg, p);
            if h == 0 || h == u32::MAX {
                break;
            }
            let _ = write!(line, " {:04x}v{}@{p:03x}", h & 0xffff, (h >> 16) & 0xf);
            let next = (h >> 20) as usize & 0xffc;
            if next < 0x100 || next == p {
                break;
            }
            p = next;
        }
    }
    (line, pcie)
}

/// An MCFG allocation (PCI Firmware 3.3 Table 4-3): base for bus 0, segment, and the buses it decodes.
struct Ecam {
    base: u64,
    segment: u16,
    first: u8,
    last: u8,
}

/// Every ECAM window the MCFG names, found from the RSDP's root table.
fn mcfg(st: &SystemTable<Boot>, rsdp: u64) -> Result<Vec<Ecam>, String> {
    let head = phys(st, rsdp, 36)?;
    let (root_at, width) = match (head[15] >= 2, u64_at(&head, 24)) {
        (true, x) if x != 0 => (x, 8usize),
        _ => (u64::from(u32_at(&head, 16)), 4usize),
    };
    let root = table(st, root_at)?;
    for i in 0..root.len().saturating_sub(36) / width {
        let p = 36 + i * width;
        let at = if width == 8 { u64_at(&root, p) } else { u64::from(u32_at(&root, p)) };
        let Ok(t) = table(st, at) else { continue };
        if &t[..4] != b"MCFG" {
            continue;
        }
        let mut out = Vec::new();
        let mut e = 44;
        while e + 16 <= t.len() {
            out.push(Ecam { base: u64_at(&t, e), segment: u16_at(&t, e + 8), first: t[e + 10], last: t[e + 11] });
            e += 16;
        }
        return Ok(out);
    }
    Err(String::from("the root table names no MCFG"))
}

impl Ecam {
    /// One configuration dword, read straight from the window. Only a bus the
    /// MCFG decodes is ever addressed: past `last` lies whatever else the
    /// platform maps there, which is not configuration space.
    fn read(&self, bus: u8, dev: u8, f: u8, offset: usize) -> u32 {
        assert!((self.first..=self.last).contains(&bus) && dev < 32 && f < 8 && offset < 0x1000 && offset.is_multiple_of(4));
        let at = self.base + (u64::from(bus) << 20) + (u64::from(dev) << 15) + (u64::from(f) << 12) + offset as u64;
        // SAFETY: boot services identity-map the address space, and the
        // assert holds the address inside the window the MCFG declares.
        unsafe { core::ptr::read_volatile(at as *const u32) }
    }
}

/// A file opened once and appended to, each piece flushed before the next
/// read, so a stage that stops leaves everything before the stop.
struct Trail(Option<uefi::proto::media::file::RegularFile>);

impl Trail {
    fn open(dir: &mut Directory, name: &str) -> Self {
        let file = CString16::try_from(name)
            .ok()
            .and_then(|n| dir.open(&n, FileMode::CreateReadWrite, FileAttribute::empty()).ok())
            .and_then(|f| f.into_regular_file());
        if file.is_none() {
            println!("{HEAD} {name} would not open, so its lines are lost");
        }
        Self(file)
    }

    fn put(&mut self, bytes: &[u8]) {
        let Some(file) = self.0.as_mut() else { return };
        if file.write(bytes).is_err() || file.flush().is_err() {
            println!("{HEAD} a write would not flush, so the rest of this file is lost");
            self.0 = None;
        }
    }
}

/// The file whose presence skips the PCI stage. It is made before the stage
/// and deleted after it, so a boot that stops inside the stage leaves it, and
/// the next boot goes on to the kernel without it.
const SKIP_PCI: &str = "survey-skip-pci";

fn skip_pci_set() -> bool {
    let name = CString16::try_from(SKIP_PCI).expect("ASCII");
    matches!(loaderlog::with_open_volume(|root| root.open(&name, FileMode::Read, FileAttribute::empty()).is_ok()), Ok(true))
}

fn skip_pci_mark(on: bool) {
    let name = CString16::try_from(SKIP_PCI).expect("ASCII");
    let done = loaderlog::with_open_volume(|root| {
        let file = root.open(&name, FileMode::CreateReadWrite, FileAttribute::empty()).map_err(|e| e.status())?;
        let mut file = file.into_regular_file().ok_or(Status::INVALID_PARAMETER)?;
        if on {
            file.flush().map_err(|e| e.status())
        } else {
            file.delete().map_err(|e| e.status())
        }
    });
    if !matches!(done, Ok(Ok(()))) {
        println!("{HEAD} {SKIP_PCI} would not be {}", if on { "made" } else { "deleted" });
    }
}

/// Every function behind each MCFG window's first bus, walked down through
/// the buses its bridges forward and no other, each line and dump flushed to
/// the stick before the next function is read.
fn pci(st: &SystemTable<Boot>, rsdp: u64, dir: &mut Directory) -> String {
    let windows = match mcfg(st, rsdp) {
        Ok(w) => w,
        Err(why) => return alloc::format!("no ECAM window ({why}), so nothing is read"),
    };
    let mut lines = Trail::open(dir, "pci.txt");
    let mut dumps = Trail::open(dir, "pci-config.txt");
    let mut functions = 0usize;
    for ecam in &windows {
        let seg = ecam.segment;
        lines.put(alloc::format!("MCFG segment {seg:04x} base {:#x} buses {:02x}-{:02x}\n", ecam.base, ecam.first, ecam.last).as_bytes());
        if ecam.first > ecam.last {
            continue;
        }
        let mut visited: BTreeSet<u8> = BTreeSet::new();
        let mut buses: Vec<u8> = alloc::vec![ecam.first];
        while let Some(bus) = buses.pop() {
            if !visited.insert(bus) {
                continue;
            }
            for dev in 0..32u8 {
                let present = |f: u8| !matches!(ecam.read(bus, dev, f, 0) & 0xffff, 0 | 0xffff);
                if !present(0) {
                    continue;
                }
                let multi = (ecam.read(bus, dev, 0, 0xc) >> 16) & 0x80 != 0;
                for f in 0..if multi { 8 } else { 1 } {
                    if !present(f) {
                        continue;
                    }
                    let place = alloc::format!("{seg:04x}:{bus:02x}:{dev:02x}.{f}");
                    let id = ecam.read(bus, dev, f, 0);
                    println!("{HEAD} pci {place} {:04x}:{:04x} is read", id & 0xffff, id >> 16);
                    let mut cfg = Vec::with_capacity(0x1000);
                    for offset in (0..0x100).step_by(4) {
                        cfg.extend_from_slice(&ecam.read(bus, dev, f, offset).to_le_bytes());
                    }
                    // Extended space only where a PCI Express capability says
                    // the function has one.
                    if describe(&cfg, &place).1 {
                        for offset in (0x100..0x1000).step_by(4) {
                            cfg.extend_from_slice(&ecam.read(bus, dev, f, offset).to_le_bytes());
                        }
                    }
                    let (line, _) = describe(&cfg, &place);
                    lines.put(alloc::format!("{line}\n").as_bytes());
                    // `lspci -F` reads this shape.
                    let mut dump = alloc::format!("{bus:02x}:{dev:02x}.{f} segment {seg:04x}\n");
                    for (row, chunk) in cfg.chunks(16).enumerate() {
                        let _ = write!(dump, "{:02x}:", row * 16);
                        for b in chunk {
                            let _ = write!(dump, " {b:02x}");
                        }
                        dump.push('\n');
                    }
                    dump.push('\n');
                    dumps.put(dump.as_bytes());
                    functions += 1;
                    // A bridge's forwarded buses, those the window decodes.
                    if cfg[0xe] & 0x7f == 1 {
                        let (secondary, subordinate) = (cfg[0x19], cfg[0x1a]);
                        if secondary > bus && secondary <= subordinate {
                            for b in (secondary..=subordinate.min(ecam.last)).rev() {
                                buses.push(b);
                            }
                        }
                    }
                }
            }
        }
        lines.put(alloc::format!("segment {seg:04x}: {} bus(es) walked\n", visited.len()).as_bytes());
    }
    alloc::format!("{functions} functions under {} ECAM window(s)", windows.len())
}
