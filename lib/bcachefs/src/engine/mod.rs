//! bcachefs as bcachefs-tools v1.39.4 defines metadata version 1.39 (1063),
//! on one device: a reader of clean volumes, [`mkfs`] of a minimal complete
//! volume, and ToyOS's check of what it writes ([`fsck`]).
//!
//! **Everything read came off a disk this crate does not own.** Bytes become
//! typed values in one place each, the superblock in [`superblock`], a node
//! and its keys in `node`, a value in `value`; what those return is not
//! checked again. A structure outside what this reader decodes is refused by
//! name ([`Error::Unsupported`]), never guessed at.

mod bkey;
mod crypt;
mod fsck;
mod mkfs;
mod node;
mod read;
mod superblock;
mod value;

pub use bkey::{Bpos, Key, Version};
pub use crypt::MIN_KDF;
pub use fsck::{fsck, Finding};
pub use mkfs::{mkfs, Encryption, Options};
pub use read::{DirEntry, Inode, Volume};
pub use superblock::{Kdf, Superblock};
pub use value::DirentType;

/// Why an operation on a volume did not happen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Device(toyos_blockdev::Error),
    /// A checksum or MAC disagreed with the bytes it covers.
    Checksum(&'static str),
    /// Bytes that are not the format, and what was wrong with them.
    Corrupt(&'static str),
    /// Valid bcachefs this crate does not handle, by name.
    Unsupported(&'static str),
    /// An encrypted volume opened without its passphrase.
    Locked,
    /// The passphrase does not unwrap the volume's key.
    Passphrase,
    NotFound,
    NotDirectory,
    /// An argument the caller should not have made, by name.
    Invalid(&'static str),
    /// The random source failed.
    Entropy,
}

impl From<toyos_blockdev::Error> for Error {
    fn from(e: toyos_blockdev::Error) -> Self {
        Error::Device(e)
    }
}

pub type Result<T> = core::result::Result<T, Error>;

/// Bytes per sector, whatever the device's block.
pub const SECTOR: usize = 512;

/// The metadata version this crate reads and writes: 1.39.
pub const VERSION: u16 = 1 << 10 | 39;

pub(crate) fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

pub(crate) fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

pub(crate) fn le64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

pub(crate) fn put16(b: &mut [u8], at: usize, v: u16) {
    b[at..at + 2].copy_from_slice(&v.to_le_bytes());
}

pub(crate) fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

pub(crate) fn put64(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

/// Bits `[lo, lo + len)` of `word`.
pub(crate) fn field(word: u64, lo: u32, len: u32) -> u64 {
    (word >> lo) & if len == 64 { !0 } else { (1 << len) - 1 }
}

/// The btrees this crate names, numbered as the format numbers them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Btree {
    Extents = 0,
    Inodes = 1,
    Dirents = 2,
    Xattrs = 3,
    Alloc = 4,
    Quotas = 5,
    Stripes = 6,
    Reflink = 7,
    Subvolumes = 8,
    Snapshots = 9,
    Lru = 10,
    Freespace = 11,
    NeedDiscard = 12,
    Backpointers = 13,
    BucketGens = 14,
    SnapshotTrees = 15,
    DeletedInodes = 16,
    LoggedOps = 17,
    ReconcileWork = 18,
    SubvolumeChildren = 19,
    Accounting = 20,
}

impl Btree {
    pub(crate) const ALL: [Btree; 21] = [
        Btree::Extents,
        Btree::Inodes,
        Btree::Dirents,
        Btree::Xattrs,
        Btree::Alloc,
        Btree::Quotas,
        Btree::Stripes,
        Btree::Reflink,
        Btree::Subvolumes,
        Btree::Snapshots,
        Btree::Lru,
        Btree::Freespace,
        Btree::NeedDiscard,
        Btree::Backpointers,
        Btree::BucketGens,
        Btree::SnapshotTrees,
        Btree::DeletedInodes,
        Btree::LoggedOps,
        Btree::ReconcileWork,
        Btree::SubvolumeChildren,
        Btree::Accounting,
    ];

    pub(crate) fn from_id(id: u32) -> Option<Btree> {
        Btree::ALL.get(id as usize).copied()
    }

    /// Keys are extents, positioned at their end.
    pub(crate) fn extents(self) -> bool {
        matches!(self, Btree::Extents | Btree::Reflink | Btree::Freespace)
    }
}

/// Key types, numbered as the format numbers them: the order of
/// *bcachefs: Principles of Operation* §11.5's list.
pub(crate) mod kind {
    pub const DELETED: u8 = 0;
    pub const WHITEOUT: u8 = 1;
    pub const ERROR: u8 = 2;
    pub const HASH_WHITEOUT: u8 = 4;
    pub const EXTENT: u8 = 6;
    pub const RESERVATION: u8 = 7;
    pub const INODE: u8 = 8;
    pub const DIRENT: u8 = 10;
    pub const REFLINK_P: u8 = 15;
    pub const INLINE_DATA: u8 = 17;
    pub const BTREE_PTR_V2: u8 = 18;
    pub const SUBVOLUME: u8 = 21;
    pub const SNAPSHOT: u8 = 22;
    pub const INODE_V2: u8 = 23;
    pub const SET: u8 = 25;
    pub const ALLOC_V4: u8 = 27;
    pub const BACKPOINTER: u8 = 28;
    pub const INODE_V3: u8 = 29;
    pub const SNAPSHOT_TREE: u8 = 31;
    pub const ACCOUNTING: u8 = 34;
    pub const INODE_ALLOC_CURSOR: u8 = 35;
    pub const EXTENT_WHITEOUT: u8 = 36;
}

#[cfg(test)]
mod tests;
