//! The partition table the build writes onto every disk it makes: a
//! protective MBR, the primary GPT header and its entry array at the front,
//! and the backup array and header at the back, in 512-byte logical blocks
//! with the 128 entries of 128 bytes UEFI §5.3 requires room for.
//!
//! Here and not in `toyos-gpt`, which is the parser the kernel and the loader
//! read the table with; the build is the one program that writes one.
//! `toyos-gpt`'s CRC-32 and GUIDs are shared, because they are the format's
//! own and the parser is what proves them. `the_gpt_crate_writes_the_same_table`
//! holds every byte against the `gpt` crate this replaced.

use toyos_gpt::Guid;

const LBA: u64 = crate::image::LBA as u64;
const ENTRIES: u64 = 128;
const ENTRY_BYTES: u64 = 128;
const ARRAY_LBAS: u64 = ENTRIES * ENTRY_BYTES / LBA;
/// The MBR, the header and the array before it.
const FIRST_USABLE: u64 = 2 + ARRAY_LBAS;
const HEADER_BYTES: usize = 92;
/// A name is at most 36 UTF-16 code units.
const NAME_UNITS: usize = 36;

/// One partition: its type, its own GUID, its first and last block, and its name.
pub(crate) struct Entry<'a> {
    pub kind: Guid,
    pub unique: Guid,
    pub first_lba: u64,
    pub last_lba: u64,
    pub name: &'a str,
}

/// A table's two halves: `primary` goes at byte 0 of the disk and `backup`
/// ends at its last byte.
pub(crate) struct Table {
    pub primary: Vec<u8>,
    pub backup: Vec<u8>,
}

/// The last block a partition on a disk of `lbas` blocks may use.
fn last_usable(lbas: u64) -> u64 {
    lbas - 2 - ARRAY_LBAS
}

/// The table of a disk of `lbas` blocks named `disk`, carrying `entries` in
/// their order.
pub(crate) fn table(lbas: u64, disk: Guid, entries: &[Entry<'_>]) -> Table {
    assert!(entries.len() as u64 <= ENTRIES, "{} partitions in a table of {ENTRIES}", entries.len());
    let last_usable = last_usable(lbas);
    let mut array = vec![0u8; (ENTRIES * ENTRY_BYTES) as usize];
    for (entry, slot) in entries.iter().zip(array.chunks_mut(ENTRY_BYTES as usize)) {
        assert!(
            FIRST_USABLE <= entry.first_lba && entry.first_lba <= entry.last_lba && entry.last_lba <= last_usable,
            "partition {:?} at blocks {}..={} is not inside {FIRST_USABLE}..={last_usable}",
            entry.name,
            entry.first_lba,
            entry.last_lba
        );
        let name: Vec<u16> = entry.name.encode_utf16().collect();
        assert!(name.len() <= NAME_UNITS, "the partition name {:?} is over {NAME_UNITS} UTF-16 units", entry.name);
        slot[..16].copy_from_slice(&entry.kind.0);
        slot[16..32].copy_from_slice(&entry.unique.0);
        slot[32..40].copy_from_slice(&entry.first_lba.to_le_bytes());
        slot[40..48].copy_from_slice(&entry.last_lba.to_le_bytes());
        for (unit, at) in name.iter().zip((56..).step_by(2)) {
            slot[at..at + 2].copy_from_slice(&unit.to_le_bytes());
        }
    }
    let array_crc = toyos_gpt::crc32(&array);
    let header = |at: u64, other: u64, array_at: u64| {
        let mut block = vec![0u8; LBA as usize];
        block[..8].copy_from_slice(b"EFI PART");
        block[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        block[12..16].copy_from_slice(&(HEADER_BYTES as u32).to_le_bytes());
        block[24..32].copy_from_slice(&at.to_le_bytes());
        block[32..40].copy_from_slice(&other.to_le_bytes());
        block[40..48].copy_from_slice(&FIRST_USABLE.to_le_bytes());
        block[48..56].copy_from_slice(&last_usable.to_le_bytes());
        block[56..72].copy_from_slice(&disk.0);
        block[72..80].copy_from_slice(&array_at.to_le_bytes());
        block[80..84].copy_from_slice(&(ENTRIES as u32).to_le_bytes());
        block[84..88].copy_from_slice(&(ENTRY_BYTES as u32).to_le_bytes());
        block[88..92].copy_from_slice(&array_crc.to_le_bytes());
        let crc = toyos_gpt::crc32(&block[..HEADER_BYTES]);
        block[16..20].copy_from_slice(&crc.to_le_bytes());
        block
    };

    let mut primary = protective_mbr(lbas);
    primary.extend(header(1, lbas - 1, 2));
    primary.extend(&array);
    let mut backup = array;
    backup.extend(header(lbas - 1, 1, last_usable + 1));
    Table { primary, backup }
}

/// LBA 0: one partition of type 0xEE over the whole disk but its first
/// block, or as much of it as 32 bits count, at the CHS UEFI §5.2.3 gives it.
fn protective_mbr(lbas: u64) -> Vec<u8> {
    let mut block = vec![0u8; LBA as usize];
    block[446..454].copy_from_slice(&[0x00, 0x00, 0x02, 0x00, 0xEE, 0xFF, 0xFF, 0xFF]);
    block[454..458].copy_from_slice(&1u32.to_le_bytes());
    block[458..462].copy_from_slice(&u32::try_from(lbas - 1).unwrap_or(u32::MAX).to_le_bytes());
    block[510..512].copy_from_slice(&[0x55, 0xAA]);
    block
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::ImageSectors;
    use std::collections::BTreeMap;
    use std::io::{Cursor, Read, Seek, SeekFrom};

    const ALIGN: u64 = 2048;

    /// The partitions of a boot image with the DATA an install puts first,
    /// one of them a length the 1 MiB alignment does not round.
    const ROWS: [(&str, Guid, u64); 6] = [
        ("ToyOS data", Guid::TOYOS_DATA, 3 << 20),
        ("ESP", Guid::EFI_SYSTEM, 34 << 20),
        ("ToyOS slots", Guid::TOYOS_SLOTS, 1 << 20),
        ("ToyOS log", Guid::MICROSOFT_BASIC, 34 << 20),
        ("ToyOS slot A", Guid::TOYOS_BOOT, (40 << 20) + 4096),
        ("ToyOS root A", Guid::TOYOS_ROOT, 7 << 20),
    ];

    /// `ROWS` under `uniques`, each on the first 1 MiB boundary after the last.
    fn entries(uniques: &[Guid]) -> Vec<Entry<'static>> {
        let mut at = ALIGN;
        ROWS.iter()
            .zip(uniques)
            .map(|((name, kind, len), unique)| {
                let first = at;
                at = (first + len / LBA).next_multiple_of(ALIGN);
                Entry { kind: *kind, unique: *unique, first_lba: first, last_lba: first + len / LBA - 1, name }
            })
            .collect()
    }

    fn disk_of(total: u64, table: &Table) -> Vec<u8> {
        let mut disk = vec![0u8; total as usize];
        disk[..table.primary.len()].copy_from_slice(&table.primary);
        let back = disk.len() - table.backup.len();
        disk[back..].copy_from_slice(&table.backup);
        disk
    }

    /// The `gpt` crate's table for `ROWS`, laid as `src/image.rs` laid it with
    /// that crate, on a disk of `total` bytes: its bytes, its disk GUID and
    /// the unique GUIDs it drew.
    fn theirs(total: u64) -> (Vec<u8>, Guid, Vec<Guid>) {
        let mut bytes = vec![0u8; total as usize];
        let mut cursor = Cursor::new(&mut bytes);
        gpt::mbr::ProtectiveMBR::with_lb_size(u32::try_from(total / LBA - 1).unwrap_or(u32::MAX))
            .overwrite_lba0(&mut cursor)
            .expect("the protective MBR");
        let mut disk = gpt::GptConfig::default()
            .initialized(false)
            .writable(true)
            .logical_block_size(gpt::disk::LogicalBlockSize::Lb512)
            .create_from_device(Box::new(cursor), None)
            .expect("a table");
        disk.update_partitions(BTreeMap::new()).expect("an empty table");
        for (name, kind, len) in ROWS {
            let kind = gpt::partition_types::Type {
                guid: kind.to_string().leak(),
                os: gpt::partition_types::OperatingSystem::None,
            };
            disk.add_partition(name, len, kind, 0, Some(ALIGN)).expect("a partition");
        }
        let mut device = disk.write().expect("the table");
        let mut out = vec![0u8; total as usize];
        device.seek(SeekFrom::Start(0)).and_then(|_| device.read_exact(&mut out)).expect("read back");
        let guid = |at: usize| Guid(out[at..at + 16].try_into().unwrap());
        let disk_guid = guid(LBA as usize + 56);
        let uniques = (0..ROWS.len()).map(|i| guid(2 * LBA as usize + i * ENTRY_BYTES as usize + 16)).collect();
        (out, disk_guid, uniques)
    }

    /// **The differential oracle**: the `gpt` crate, a third party's GPT
    /// writer sharing no code with this one, lays the same partitions on the
    /// same disk under the same GUIDs into the same bytes, every one of them,
    /// on a disk the alignment rounds and on one it does not.
    #[test]
    fn the_gpt_crate_writes_the_same_table() {
        for total in [128u64 << 20, (160 << 20) + 3 * 4096] {
            let (theirs, disk, uniques) = theirs(total);
            let ours = disk_of(total, &table(total / LBA, disk, &entries(&uniques)));
            let differing = (0..total as usize).filter(|&i| ours[i] != theirs[i]).count();
            assert_eq!(differing, 0, "{total}: {differing} bytes differ from the gpt crate's");
        }
    }

    /// `toyos-gpt`, the parser the loader and the kernel read with, finds
    /// every partition at its blocks under its type, from the primary and,
    /// with the primary torn, from the backup.
    #[test]
    fn toyos_gpt_reads_it_from_either_copy() {
        let total: u64 = 128 << 20;
        let uniques: Vec<Guid> = ROWS.iter().map(|_| Guid(uuid::Uuid::new_v4().to_bytes_le())).collect();
        let entries = entries(&uniques);
        let disk = disk_of(total, &table(total / LBA, Guid(uuid::Uuid::new_v4().to_bytes_le()), &entries));
        let mut torn = disk.clone();
        torn[LBA as usize + 30] ^= 1;
        for image in [&disk, &torn] {
            for entry in &entries {
                let found = toyos_gpt::locate(&mut ImageSectors(image), entry.unique).expect("the partition").partition();
                assert_eq!(
                    (found.type_guid(), found.first_lba(), found.lba_count().get()),
                    (entry.kind, entry.first_lba, entry.last_lba - entry.first_lba + 1),
                    "{}",
                    entry.name
                );
            }
        }
    }
}
