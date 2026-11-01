//! A whole disk: its node on a Linux host, and an image written to one whole.
//! The metal loop (`toyos-metal`) and `toyos-flash` each write an image to a
//! disk a node names, and read both the same way.

use std::fmt;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::image::LBA;

/// Why a node or an image is no whole disk's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unfit {
    /// A device node that is not `/dev/sd<letter>` or `/dev/nvme<n>n<m>`.
    Node(String),
    File { path: String, why: String },
    /// The image's length is not a whole number of sectors.
    Sectors { bytes: u64 },
    /// The final sector does not begin `EFI PART`: the backup table is gone.
    BackupHeader { at: u64, saw: String },
}

impl fmt::Display for Unfit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Node(got) => write!(
                f,
                "{got:?} is not /dev/sd<letter> or /dev/nvme<n>n<m>; a write takes a whole disk \
                 and names no partition and no other device"
            ),
            Self::File { path, why } => write!(f, "{path}: {why}"),
            Self::Sectors { bytes } => write!(
                f,
                "the image is {bytes} bytes, which is not a whole number of {LBA}-byte sectors"
            ),
            Self::BackupHeader { at, saw } => write!(
                f,
                "the image's final sector at byte {at} begins {saw:?} and not \"EFI PART\": a \
                 healthy primary table hides a missing backup"
            ),
        }
    }
}

/// A whole disk's node: a SCSI disk, `/dev/sd` and one lower-case letter, or
/// an NVMe namespace, `/dev/nvme<controller>n<namespace>` with one digit each.
/// A partition is spelled from one and never passed as one, so no job can be
/// aimed at a partition of a disk it was not given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Node {
    Scsi(u8),
    Nvme { controller: u8, namespace: u8 },
}

impl Node {
    pub fn parse(path: &str) -> Result<Self, Unfit> {
        let scsi = path.strip_prefix("/dev/sd").and_then(|rest| {
            let [byte] = rest.as_bytes() else { return None };
            byte.is_ascii_lowercase().then_some(Node::Scsi(*byte))
        });
        let nvme = path.strip_prefix("/dev/nvme").and_then(|rest| {
            let [controller, b'n', namespace] = rest.as_bytes() else { return None };
            (controller.is_ascii_digit() && (b'1'..=b'9').contains(namespace))
                .then_some(Node::Nvme { controller: *controller, namespace: *namespace })
        });
        scsi.or(nvme).ok_or_else(|| Unfit::Node(path.to_string()))
    }

    /// The disk's own name, as `/dev` and `/sys/class/block` spell it.
    pub fn name(&self) -> String {
        match *self {
            Node::Scsi(letter) => format!("sd{}", letter as char),
            Node::Nvme { controller, namespace } => format!("nvme{}n{}", controller as char, namespace as char),
        }
    }

    /// A partition's name: `sda3`, and `nvme0n1p3` where the disk's own name
    /// ends in a digit.
    pub fn part_name(&self, index: u32) -> String {
        match self {
            Node::Scsi(_) => format!("{}{index}", self.name()),
            Node::Nvme { .. } => format!("{}p{index}", self.name()),
        }
    }

    pub fn whole(&self) -> String {
        format!("/dev/{}", self.name())
    }

    pub fn partition(&self, index: u32) -> String {
        format!("/dev/{}", self.part_name(index))
    }

    /// Where `/sys` answers for the disk.
    pub fn sysfs(&self) -> String {
        format!("/sys/class/block/{}", self.name())
    }
}

/// A whole disk's image, opened: whole sectors, `EFI PART` in the *final*
/// one, and its length in bytes.
pub fn whole_disk(path: &Path) -> Result<(std::fs::File, u64), Unfit> {
    let sector = u64::from(LBA);
    let unreadable = |why: String| Unfit::File { path: path.display().to_string(), why };
    let mut file = std::fs::File::open(path).map_err(|e| unreadable(e.to_string()))?;
    let bytes = file.metadata().map_err(|e| unreadable(e.to_string()))?.len();
    if bytes == 0 || bytes % sector != 0 {
        return Err(Unfit::Sectors { bytes });
    }

    let at = bytes - sector;
    let mut last = vec![0u8; sector as usize];
    file.seek(SeekFrom::Start(at)).map_err(|e| unreadable(e.to_string()))?;
    file.read_exact(&mut last).map_err(|e| unreadable(e.to_string()))?;
    if &last[..8] != b"EFI PART" {
        let saw = String::from_utf8_lossy(&last[..8]).to_string();
        return Err(Unfit::BackupHeader { at, saw });
    }
    Ok((file, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_node_is_a_whole_scsi_or_nvme_disk_and_nothing_else() {
        for name in [
            "/dev/nvme0n1p3",
            "/dev/nvme0n0",
            "/dev/nvme10n1",
            "/dev/nvme0",
            "/dev/sda1",
            "/dev/sdaa",
            "/dev/SDA",
            "sda",
        ] {
            assert_eq!(Node::parse(name), Err(Unfit::Node(name.to_string())), "{name}");
        }
        assert_eq!(Node::parse("/dev/sda").unwrap().whole(), "/dev/sda");
        assert_eq!(Node::parse("/dev/sdb").unwrap().partition(3), "/dev/sdb3");
        let nvme = Node::parse("/dev/nvme0n1").unwrap();
        assert_eq!(nvme.whole(), "/dev/nvme0n1");
        assert_eq!(nvme.partition(3), "/dev/nvme0n1p3");
    }

    /// **A whole disk's image is whole sectors with `EFI PART` in its final
    /// one**: a ragged or empty file, and one whose signature is only in the
    /// primary header, the false pass a healthy front hides, are refused.
    #[test]
    fn a_whole_disk_is_whole_sectors_ending_in_the_backup_header() {
        let dir = toyos_tmpdir::TempDir::new("whole-disk");
        let write = |name: &str, bytes: &[u8]| {
            let at = dir.join(name);
            std::fs::write(&at, bytes).expect("write");
            at
        };
        let refused = |name: &str, bytes: &[u8]| whole_disk(&write(name, bytes)).map(|(_, n)| n).unwrap_err();
        assert_eq!(refused("ragged.img", &[0u8; 1000]), Unfit::Sectors { bytes: 1000 });
        assert_eq!(refused("empty.img", &[]), Unfit::Sectors { bytes: 0 });
        assert_eq!(
            refused("headless.img", &[0u8; 1024]),
            Unfit::BackupHeader { at: 512, saw: "\0\0\0\0\0\0\0\0".to_string() }
        );
        let mut front = vec![0u8; 1536];
        front[512..520].copy_from_slice(b"EFI PART");
        assert!(matches!(refused("front-only.img", &front), Unfit::BackupHeader { at: 1024, .. }));
        let mut whole = front.clone();
        whole[1024..1032].copy_from_slice(b"EFI PART");
        assert_eq!(whole_disk(&write("whole.img", &whole)).map(|(_, n)| n), Ok(1536));
    }
}
