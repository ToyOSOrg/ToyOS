//! What the front of a partition says it holds, judged before a designation
//! writes there.
//!
//! **A bug fence, not a licence**: a partition is designated because its
//! authorization says so, and a signature recognised here refuses it, since
//! a volume somebody can name is the likeliest mistake. Finding none proves
//! nothing about what the partition holds. Each offset is the format's own;
//! where a published document defines it, it is named beside it.

/// How much of a partition [`judge`] reads: every signature below lies in it.
pub const READ_BYTES: usize = 1 << 20;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Content {
    /// A volume of ToyOS's own format.
    Volume,
    /// A designation stamp naming this many blocks.
    Designated(u64),
    /// Another format's signature, by name.
    Foreign(&'static str),
    /// Nothing recognised.
    Unrecognised,
}

/// `bytes` at `at` in `front`.
fn has(front: &[u8], at: usize, bytes: &[u8]) -> bool {
    front.get(at..at + bytes.len()) == Some(bytes)
}

/// Every format recognised, by name: where its signature is, and what it is.
const SIGNATURES: &[(&str, usize, &[u8])] = &[
    // The ext4 Disk Layout: the superblock at 1024, `s_magic` 0xEF53 at its byte 0x38.
    ("an ext2/3/4 file system", 1024 + 0x38, &[0x53, 0xEF]),
    // XFS Algorithms & Data Structures, the superblock: `sb_magicnum` "XFSB" at 0.
    ("an XFS file system", 0, b"XFSB"),
    // Btrfs on-disk format: the superblock at 64 KiB, its magic at 0x40.
    ("a btrfs file system", 0x1_0000 + 0x40, b"_BHRfS_M"),
    // NTFS boot sector: the OEM ID at 3.
    ("an NTFS file system", 3, b"NTFS    "),
    // exFAT File System Specification §3.1.2: FileSystemName at 3.
    ("an exFAT file system", 3, b"EXFAT   "),
    // Microsoft FAT Specification: BS_FilSysType at 54 (FAT12/16) and 82 (FAT32).
    ("a FAT file system", 54, b"FAT1"),
    ("a FAT32 file system", 82, b"FAT32   "),
    // LUKS1 On-Disk Format Specification and LUKS2's: the magic at 0.
    ("a LUKS encrypted volume", 0, b"LUKS\xBA\xBE"),
    // Linux swap: "SWAPSPACE2" ending a 4 KiB page.
    ("a Linux swap area", 4096 - 10, b"SWAPSPACE2"),
    // LVM2's physical volume label: "LABELONE" opening the second sector.
    ("an LVM2 physical volume", 512, b"LABELONE"),
    // md RAID's version 1.1 and 1.2 superblocks: magic 0xa92b4efc at 0 and 4 KiB.
    ("an md RAID member", 0, &[0xFC, 0x4E, 0x2B, 0xA9]),
    ("an md RAID member", 4096, &[0xFC, 0x4E, 0x2B, 0xA9]),
    // ECMA-119: the first volume descriptor at 32 KiB, its identifier "CD001" at 1.
    ("an ISO 9660 image", 0x8000 + 1, b"CD001"),
    // Apple File System Reference: the container superblock's `nx_magic` "NXSB" at 32.
    ("an APFS container", 32, b"NXSB"),
    // Technical Note TN1150: the HFS Plus volume header at 1024, signature "H+" or "HX".
    ("an HFS Plus volume", 1024, b"H+"),
    ("an HFSX volume", 1024, b"HX"),
    // F2FS: the superblock at 1024, magic 0xF2F52010.
    ("an F2FS file system", 1024, &[0x10, 0x20, 0xF5, 0xF2]),
    // UEFI 2.10 §5.3.2: a GPT header, "EFI PART", at LBA 1 of 512-byte blocks.
    ("a GPT partition table", 512, b"EFI PART"),
    // An MBR or boot sector's signature, 0x55AA at 510.
    ("a boot sector", 510, &[0x55, 0xAA]),
];

/// What the front of a partition of `blocks` blocks holds: `front` is its
/// first [`READ_BYTES`], or all of it on a smaller one.
pub fn judge(front: &[u8], blocks: u64) -> Content {
    if bcachefs::carries_volume(front) {
        return Content::Volume;
    }
    if let Some(stamped) = bcachefs::designated(front) {
        return if stamped == blocks { Content::Designated(stamped) } else { Content::Foreign("a stamp for another size") };
    }
    match SIGNATURES.iter().find(|(_, at, bytes)| has(front, *at, bytes)) {
        Some((name, _, _)) => Content::Foreign(name),
        None => Content::Unrecognised,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each signature is recognised where its format puts it, a ToyOS volume
    /// and a stamp are told apart from them, and a stamp for another size is
    /// not taken for this partition's.
    #[test]
    fn each_format_is_recognised_where_it_puts_its_signature() {
        let blank = vec![0u8; READ_BYTES];
        assert_eq!(judge(&blank, 100), Content::Unrecognised);
        for (name, at, bytes) in SIGNATURES {
            let mut front = blank.clone();
            front[*at..*at + bytes.len()].copy_from_slice(bytes);
            assert_eq!(judge(&front, 100), Content::Foreign(name), "{name}");
            let mut moved = blank.clone();
            moved[*at + 1..*at + 1 + bytes.len()].copy_from_slice(bytes);
            assert_ne!(judge(&moved, 100), Content::Foreign(name), "{name} one byte off");
        }
        let mut stamped = blank.clone();
        stamped[..4096].copy_from_slice(&bcachefs::designation(100));
        assert_eq!(judge(&stamped, 100), Content::Designated(100));
        assert_eq!(judge(&stamped, 99), Content::Foreign("a stamp for another size"));
        let mut ours = blank.clone();
        ours[..4].copy_from_slice(b"BCFS");
        assert_eq!(judge(&ours, 100), Content::Volume);
        assert_eq!(judge(&blank[..4096], 1), Content::Unrecognised, "a partition shorter than the read");
    }
}
