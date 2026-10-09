//! An empty FAT32 volume, the one the build lays every FAT partition down as
//! before `toyos-fat32` writes its files: a boot sector and its backup, an
//! FSInfo sector, two FATs and a root directory holding the volume label.
//!
//! Here and not in `toyos-fat32`, which is the kernel's driver and has no
//! format path by design. The geometry is fatgen103's with the choices of the
//! `fatfs` crate this replaced, so every volume is laid out as before:
//! 512-byte sectors, eight reserved sectors, the cluster size by volume size,
//! and the FAT sized for the clusters left once the FATs are paid for.
//! `differs_from_the_fatfs_format_only_in_the_bios_stub` holds that, against
//! `fatfs` itself.
//!
//! The boot sector carries no BIOS boot code: the jump fatgen103 requires is
//! there, and the 420 bytes after the BPB are zero.

const SECTOR: usize = 512;
const RESERVED_SECTORS: u32 = 8;
const FATS: u32 = 2;
const FSINFO_SECTOR: usize = 1;
const BACKUP_BOOT_SECTOR: usize = 6;
const ROOT_CLUSTER: u32 = 2;
/// A fixed disk, which is what every FAT32 volume is.
const MEDIA: u8 = 0xF8;
/// The largest cluster number FAT32 can name, less the two reserved entries.
const MAX_FAT32_CLUSTERS: u32 = 0x0FFF_FFF4;
const END_OF_CHAIN: u32 = 0x0FFF_FFFF;
const ATTR_VOLUME_ID: u8 = 0x08;

/// An empty FAT32 volume of `bytes` under `label`.
pub(crate) fn format(bytes: usize, label: [u8; 11]) -> Vec<u8> {
    assert_eq!(bytes % SECTOR, 0, "a {bytes}-byte volume is not whole {SECTOR}-byte sectors");
    let total = u32::try_from(bytes / SECTOR)
        .unwrap_or_else(|_| panic!("a {bytes}-byte volume has more sectors than FAT32 counts"));
    let per_cluster = sectors_per_cluster(bytes as u64);
    let per_fat = sectors_per_fat(total, per_cluster);
    let first_data = RESERVED_SECTORS + FATS * per_fat;
    let clusters = (total - first_data) / per_cluster;
    assert!(
        (toyos_fat32::MIN_FAT32_CLUSTERS..=MAX_FAT32_CLUSTERS).contains(&clusters),
        "a {bytes}-byte volume has {clusters} clusters, which is no FAT32"
    );

    let mut volume = vec![0u8; bytes];
    let boot = boot_sector(total, per_cluster, per_fat, label);
    volume[..SECTOR].copy_from_slice(&boot);
    let backup = BACKUP_BOOT_SECTOR * SECTOR;
    volume[backup..backup + SECTOR].copy_from_slice(&boot);

    let fsinfo = &mut volume[FSINFO_SECTOR * SECTOR..][..SECTOR];
    put(fsinfo, 0, &0x4161_5252u32.to_le_bytes());
    put(fsinfo, 484, &0x6141_7272u32.to_le_bytes());
    // Free count and next free: "unknown", which `toyos-fat32` counts and records.
    put(fsinfo, 488, &u32::MAX.to_le_bytes());
    put(fsinfo, 492, &u32::MAX.to_le_bytes());
    put(fsinfo, 508, &0xAA55_0000u32.to_le_bytes());

    let entries = per_fat as usize * SECTOR / 4;
    for copy in 0..FATS {
        let at = (RESERVED_SECTORS + copy * per_fat) as usize * SECTOR;
        let fat = &mut volume[at..at + per_fat as usize * SECTOR];
        let mut set = |cluster: usize, value: u32| put(fat, cluster * 4, &value.to_le_bytes());
        set(0, 0x0FFF_FF00 | u32::from(MEDIA));
        set(1, u32::MAX);
        set(ROOT_CLUSTER as usize, END_OF_CHAIN);
        // The entries past the last cluster that the FAT's last sector still holds.
        for cluster in clusters as usize + 2..entries {
            set(cluster, END_OF_CHAIN);
        }
    }

    let root = first_data as usize * SECTOR;
    put(&mut volume[root..], 0, &label);
    volume[root + 11] = ATTR_VOLUME_ID;
    volume
}

/// 512 bytes up to 260 MiB, 4 KiB up to 8 GiB, and past that 1 KiB a 2 GiB
/// of the size rounded up to a power of two, at most 32 KiB.
fn sectors_per_cluster(bytes: u64) -> u32 {
    const MIB: u64 = 1024 * 1024;
    let cluster = if bytes <= 260 * MIB {
        512
    } else if bytes <= 8 * 1024 * MIB {
        4096
    } else {
        (bytes.next_power_of_two() / (2 * 1024 * MIB) * 1024).clamp(512, 32 * 1024)
    };
    (cluster / SECTOR as u64) as u32
}

/// The smallest FAT whose entries cover every cluster the volume has left
/// once the reserved sectors and both FATs are taken out, the two reserved
/// entries included.
fn sectors_per_fat(total: u32, per_cluster: u32) -> u32 {
    let rest = u64::from(total - RESERVED_SECTORS) + 2 * u64::from(per_cluster);
    let per = u64::from(per_cluster * SECTOR as u32 * 8 / 32 + FATS);
    rest.div_ceil(per) as u32
}

fn boot_sector(total: u32, per_cluster: u32, per_fat: u32, label: [u8; 11]) -> [u8; SECTOR] {
    let mut s = [0u8; SECTOR];
    put(&mut s, 0, &[0xEB, 0x58, 0x90]);
    put(&mut s, 3, b"MSWIN4.1");
    put(&mut s, 11, &(SECTOR as u16).to_le_bytes());
    s[13] = per_cluster as u8;
    put(&mut s, 14, &(RESERVED_SECTORS as u16).to_le_bytes());
    s[16] = FATS as u8;
    // The FAT12/16 root entry count and 16-bit sizes stay zero on FAT32.
    if total < 0x1_0000 {
        put(&mut s, 19, &(total as u16).to_le_bytes());
    }
    s[21] = MEDIA;
    // Sectors per track and heads: no reader of a FAT32 volume uses either.
    put(&mut s, 24, &0x20u16.to_le_bytes());
    put(&mut s, 26, &0x40u16.to_le_bytes());
    if total >= 0x1_0000 {
        put(&mut s, 32, &total.to_le_bytes());
    }
    put(&mut s, 36, &per_fat.to_le_bytes());
    put(&mut s, 44, &ROOT_CLUSTER.to_le_bytes());
    put(&mut s, 48, &(FSINFO_SECTOR as u16).to_le_bytes());
    put(&mut s, 50, &(BACKUP_BOOT_SECTOR as u16).to_le_bytes());
    s[64] = 0x80;
    s[66] = 0x29;
    put(&mut s, 67, &0x1234_5678u32.to_le_bytes());
    put(&mut s, 71, &label);
    put(&mut s, 82, b"FAT32   ");
    put(&mut s, 510, &[0x55, 0xAA]);
    s
}

fn put(into: &mut [u8], at: usize, bytes: &[u8]) {
    into[at..at + bytes.len()].copy_from_slice(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Where a FAT32 boot sector's boot code lies, after the BPB.
    const BOOT_CODE: std::ops::Range<usize> = 90..510;

    /// Every size the build formats at, and the cluster sizes' edges.
    const SIZES: [usize; 6] = [
        34 << 20,
        (34 << 20) + 4096,
        100 << 20,
        260 << 20,
        (260 << 20) + 4096,
        300 << 20,
    ];

    fn fatfs_format(bytes: usize, label: [u8; 11]) -> Vec<u8> {
        let mut volume = vec![0u8; bytes];
        fatfs::format_volume(
            Cursor::new(&mut volume),
            fatfs::FormatVolumeOptions::new().fat_type(fatfs::FatType::Fat32).volume_label(label),
        )
        .expect("fatfs formats the volume");
        volume
    }

    /// **The differential oracle**: `fatfs`, a third party's FAT
    /// implementation sharing no code with this one, formats the same size
    /// under the same label into the same bytes, but for the BIOS boot code
    /// it writes into both copies of the boot sector and this module does not.
    #[test]
    fn differs_from_the_fatfs_format_only_in_the_bios_stub() {
        for bytes in SIZES {
            let ours = format(bytes, *b"TOYOS-SLOT ");
            let theirs = fatfs_format(bytes, *b"TOYOS-SLOT ");
            let mut masked = theirs.clone();
            for sector in [0, BACKUP_BOOT_SECTOR] {
                let code = sector * SECTOR + BOOT_CODE.start..sector * SECTOR + BOOT_CODE.end;
                assert!(ours[code.clone()].iter().all(|b| *b == 0), "{bytes}: boot code in sector {sector}");
                assert!(theirs[code.clone()].iter().any(|b| *b != 0), "{bytes}: fatfs wrote no boot code in sector {sector}");
                masked[code].fill(0);
            }
            let differing = (0..bytes).filter(|&at| ours[at] != masked[at]).count();
            assert!(differing == 0, "{bytes}: {differing} bytes differ from fatfs's");
        }
    }

    /// `toyos-fat32` mounts the volume as empty, with every cluster but the
    /// root's free, and once it has recorded that count, as the build has it
    /// do, `toyos-fat32-check` finds nothing wrong with it.
    #[test]
    fn an_empty_volume_mounts_free_and_checks_clean() {
        for bytes in SIZES {
            let mut volume = format(bytes, *b"TOYOS-LOG  ");
            let mut fs = toyos_fat32::Fat32::mount(crate::image::VolumeIo(&mut volume)).expect("it mounts");
            assert!(fs.walk("", 8).expect("it walks").is_empty(), "{bytes}: an empty volume lists files");
            let geometry = *fs.geometry();
            let cluster = u64::from(geometry.bytes_per_cluster());
            assert_eq!(fs.free_bytes().expect("it counts"), (u64::from(geometry.cluster_count) - 1) * cluster, "{bytes}");
            fs.sync().expect("it records the count");
            drop(fs);
            let complaints = toyos_fat32_check::check(&volume);
            assert!(complaints.is_empty(), "{bytes}:\n{}", toyos_fat32_check::describe(&complaints));
        }
    }
}
