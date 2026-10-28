//! Another system's disk, as a bench's internal disk is before ToyOS is
//! installed on it, and the judge of what ToyOS did to it.
//!
//! The layout is the benches': the host's ESP, its `/boot` and its root, and
//! the owner's ToyOS DATA, unformatted. Every block of every partition carries
//! a pattern of its own, so DATA's first block is neither a volume of ours nor
//! a designation.

use super::serial::Serial;
use toyos_gpt::Guid;

const MIB: u64 = 1024 * 1024;
const LBA: u64 = toyos_build::image::LBA as u64;
/// Room at each end for the table and its backup, partitions on whole MiB.
const EDGE: u64 = MIB;

/// `0FC63DAF-8483-4772-8E79-3D69D8477DE4`, Linux filesystem data.
const LINUX: Guid = Guid::from_fields(0x0FC6_3DAF, 0x8483, 0x4772, [0x8E, 0x79, 0x3D, 0x69, 0xD8, 0x47, 0x7D, 0xE4]);

/// Each partition's name, type and size.
const PARTS: [(&str, Guid, u64); 4] = [
    ("EFI System Partition", Guid::EFI_SYSTEM, 8 * MIB),
    ("boot", LINUX, 8 * MIB),
    ("root", LINUX, 16 * MIB),
    ("ToyOS data", Guid::TOYOS_DATA, 16 * MIB),
];

/// The disk's every byte.
pub fn bytes() -> Vec<u8> {
    let total = EDGE + PARTS.iter().map(|(_, _, len)| len).sum::<u64>() + EDGE;
    let mut disk = vec![0u8; total as usize];
    let mut at = EDGE;
    let mut entries = Vec::new();
    for (i, (name, kind, len)) in PARTS.iter().enumerate() {
        let unique = Guid::from_fields(0x7057_0000 + i as u32, 0, 0x4000, [0x80, 0, 0, 0, 0, 0, 0, i as u8]);
        entries.push(toyos_gpt::edit::Slot::new(*kind, unique, at / LBA, (at + len) / LBA - 1, name).expect("a short name"));
        for (sector, block) in disk[at as usize..(at + len) as usize].chunks_mut(LBA as usize).enumerate() {
            block.fill(((i as u8 + 1).wrapping_mul(0x25) ^ sector as u8) | 1);
        }
        at += len;
    }
    let disk_guid = Guid::from_fields(0x7057_00ff, 0, 0x4000, [0x80; 8]);
    toyos_build::image::write_gpt(&mut std::io::Cursor::new(&mut disk), total, disk_guid, &entries);
    disk
}

/// What the nvme driver of `bb:dd.f` said, past its name: `nvme 04:00.0: `.
fn said_by_nvme(line: &str) -> Option<&str> {
    let (_, rest) = line.split_once("nvme ")?;
    let (at, said) = rest.split_once(": ")?;
    let at = at.as_bytes();
    (at.len() == 7 && at[2] == b':' && at[5] == b'.').then_some(said)
}

/// A boot off a stick drove an NVMe disk that is another system's and left
/// it alone: a driver brought a controller up and offered its partitions, it
/// opened at least one session and each on a ToyOS DATA partition, and the file server
/// found that DATA neither ours nor designated, and so neither mounted nor
/// formatted it, and served DATA absent rather than from memory. Answers how
/// many partitions the disks' tables named.
pub fn left_alone(log: &Serial) -> Result<usize, String> {
    log.must_say("diskserver: every disk driver has settled")?;
    let said: Vec<&str> = log.text().lines().filter_map(said_by_nvme).collect();
    if !said.iter().any(|s| s.starts_with("up: ")) {
        return Err(format!("no nvme driver brought its controller up\n{}", log.text()));
    }
    let kinds: Vec<(&str, &str)> = said
        .iter()
        .filter_map(|s| s.strip_prefix("partition "))
        .filter_map(|s| s.split_once(" of type "))
        .map(|(guid, rest)| (guid, rest.split(' ').next().unwrap_or_default()))
        .collect();
    if kinds.is_empty() {
        return Err(format!("no nvme driver named a partition of its disk\n{}", log.text()));
    }
    let data = Guid::TOYOS_DATA.to_string();
    let opened: Vec<&str> = said
        .iter()
        .filter_map(|s| s.strip_prefix("session "))
        .filter_map(|s| s.split_once(" opened "))
        .map(|(_, rest)| rest.split(' ').next().unwrap_or_default())
        .collect();
    if opened.is_empty() {
        return Err(format!("no nvme driver opened a session, so none was seen on DATA\n{}", log.text()));
    }
    for guid in opened {
        match kinds.iter().find(|(g, _)| *g == guid) {
            Some((_, kind)) if *kind == data => {}
            other => return Err(format!("an nvme driver opened a session on {guid}, of type {other:?}, which is no ToyOS DATA")),
        }
    }
    log.must_say("fileserver: no volume of ours and no designation stamp; nothing will be written to this partition")?;
    log.must_say("fileserver: the DATA partition holds no volume of ours and no designation stamp; DATA is absent this boot")?;
    log.must_not_say("are in memory and will not survive a reboot")?;
    log.must_not_say("fileserver: mounted the DATA volume")?;
    log.must_not_say("designates this partition for ToyOS; formatting it")?;
    Ok(kinds.len())
}
