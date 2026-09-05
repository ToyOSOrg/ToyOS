//! What a file is: the inode, dirent and extent values, and the read-only
//! volume API over them.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::block_io::{BlockBuf, BlockIO, BlockNum, BLOCK_SIZE};

use super::bkey::{
    Bpos, Key, TYPE_DIRENT, TYPE_ERROR, TYPE_EXTENT, TYPE_INLINE_DATA, TYPE_INODE_V3,
    TYPE_RESERVATION, TYPE_SUBVOLUME,
};
use super::btree::{Btree, BTREE_DIRENTS, BTREE_EXTENTS, BTREE_INODES, BTREE_SUBVOLUMES};
use super::csum::CsumType;
use super::raw::{bits, Raw};
use super::sb::{Superblock, EXTENT_TYPES_MAX, SECTOR};
use super::UpstreamError;

/// `BCACHEFS_ROOT_SUBVOL` and `BCACHEFS_ROOT_INO`.
const ROOT_SUBVOL: u64 = 1;
const ROOT_INO: u64 = 4096;
/// The snapshot id `bch2_fs_initialize` gives the root subvolume and every key
/// under it, and the only one this reader serves a key from.
const NO_SNAPSHOT: u32 = u32::MAX;
/// `BCH_NAME_MAX`.
pub const NAME_MAX: usize = 512;
/// `INODEv3_FIELDS_START_CUR`: `bch_inode_v3`'s fixed part, in u64s.
const INODE_FIELDS_START_CUR: u64 = 6;
const INODE_FIELDS_AT: usize = 48;

/// The `d_type` values a dirent carries, which are the POSIX ones plus
/// `DT_SUBVOL`.
const DT_DIR: u8 = 4;
const DT_REG: u8 = 8;
const DT_LNK: u8 = 10;
const DT_SUBVOL: u8 = 16;
/// `offsetof(struct bch_dirent, d_name)`.
const DIRENT_NAME_AT: usize = 9;

/// What a directory entry says a name is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Dir,
    Regular,
    Symlink,
    /// A type this reader does not open: a device node, a fifo, a subvolume.
    Other(u8),
}

impl FileKind {
    fn from_dirent(d_type: u8) -> Self {
        match d_type {
            DT_DIR => Self::Dir,
            DT_REG => Self::Regular,
            DT_LNK => Self::Symlink,
            other => Self::Other(other),
        }
    }

    fn from_mode(mode: u16) -> Self {
        match mode & 0o170_000 {
            0o040_000 => Self::Dir,
            0o100_000 => Self::Regular,
            0o120_000 => Self::Symlink,
            _ => Self::Other((mode >> 12) as u8),
        }
    }
}

/// An inode's fixed fields. The varint tail is not decoded: nothing the read
/// path answers needs a field past `bi_size`, and a varint parsed for nobody
/// is a bound nobody checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Attrs {
    pub inum: u64,
    pub size: u64,
    pub sectors: u64,
    pub mode: u16,
    pub kind: FileKind,
}

/// A bcachefs volume, opened read-only.
pub struct Volume<IO: BlockIO> {
    io: IO,
    sb: Superblock,
    root: u64,
    entry_u64s: [u8; EXTENT_TYPES_MAX],
    /// The largest byte offset any file on this volume can have data at.
    device_bytes: u64,
}

impl<IO: BlockIO> Volume<IO> {
    /// Open a volume: validate the superblock, then take the root subvolume's
    /// root inode.
    ///
    /// **Snapshots are refused here rather than filtered later.** Every key a
    /// volume with none carries sits at one snapshot id; serving a file means
    /// deciding which of several versions of a key is visible, and this reader
    /// does not make that decision, so a root subvolume at any other id is
    /// refused by name.
    pub fn open(io: IO) -> Result<Self, UpstreamError> {
        let sb = super::sb::read(&io)?;
        let entry_u64s = sb.extent_entry_u64s()?;
        let root = {
            let subvols = Btree::open(&io, &sb, BTREE_SUBVOLUMES)?;
            let (val, key) = subvols
                .get(Bpos::new(0, ROOT_SUBVOL, 0))?
                .ok_or(UpstreamError::Refused("volume has no root subvolume"))?;
            if key.kind != TYPE_SUBVOLUME {
                return Err(UpstreamError::Refused("root subvolume's key is not a subvolume"));
            }
            let raw = Raw::new(&val, "root subvolume's value ends early");
            if raw.u32(4)? != NO_SNAPSHOT {
                return Err(UpstreamError::Refused("volume has snapshots"));
            }
            raw.u64(8)?
        };
        if root != ROOT_INO {
            return Err(UpstreamError::Refused("root subvolume does not name the root inode"));
        }
        let device_bytes = io
            .block_count()
            .checked_mul(BLOCK_SIZE as u64)
            .ok_or(UpstreamError::Refused("device size in bytes overflows"))?;
        Ok(Self { io, sb, root, entry_u64s, device_bytes })
    }

    pub fn superblock(&self) -> &Superblock {
        &self.sb
    }

    pub fn root(&self) -> u64 {
        self.root
    }

    /// The one snapshot every key on a volume without snapshots sits at.
    fn pos(&self, inode: u64, offset: u64) -> Bpos {
        Bpos::new(inode, offset, NO_SNAPSHOT)
    }

    /// **A scanned range spans every snapshot id, so each key it hands back is
    /// checked rather than assumed.** `Volume::open` refuses a volume whose
    /// root subvolume is not at [`NO_SNAPSHOT`]; this is the same refusal for
    /// a key that claims otherwise, so no version of a key is silently chosen.
    fn one_snapshot(key: &Key) -> Result<(), UpstreamError> {
        if key.pos.snapshot != NO_SNAPSHOT {
            return Err(UpstreamError::Refused("volume holds a key at a snapshot other than its root subvolume's"));
        }
        Ok(())
    }

    /// The inode `inum`, or `None` when the btree holds no such key.
    pub fn stat(&self, inum: u64) -> Result<Option<Attrs>, UpstreamError> {
        let inodes = Btree::open(&self.io, &self.sb, BTREE_INODES)?;
        let Some((val, key)) = inodes.get(self.pos(0, inum))? else {
            return Ok(None);
        };
        if key.kind != TYPE_INODE_V3 {
            return Err(UpstreamError::Refused("inode is a version this reader does not decode"));
        }
        let raw = Raw::new(&val, "inode's value ends early");
        let flags = raw.u64(16)?;
        if bits(flags, 31, 36) != INODE_FIELDS_START_CUR {
            return Err(UpstreamError::Refused("inode's fixed part is not the length this version defines"));
        }
        if val.len() < INODE_FIELDS_AT {
            return Err(UpstreamError::Refused("inode is shorter than its fixed part"));
        }
        let mode = bits(flags, 36, 52) as u16;
        Ok(Some(Attrs {
            inum,
            sectors: raw.u64(24)?,
            size: raw.u64(32)?,
            mode,
            kind: FileKind::from_mode(mode),
        }))
    }

    /// Hand every entry of directory `dir` to `visit`, in the btree's order,
    /// stopping early when it answers `false`.
    ///
    /// A directory's entries occupy one contiguous key range — the hash is the
    /// key's offset — so this reads that range and nothing else.
    pub fn readdir(
        &self,
        dir: u64,
        visit: &mut dyn FnMut(&str, u64, FileKind) -> bool,
    ) -> Result<(), UpstreamError> {
        let dirents = Btree::open(&self.io, &self.sb, BTREE_DIRENTS)?;
        let mut failure = None;
        dirents.range(self.pos(dir, 0), self.pos(dir, u64::MAX), &mut |node, _, key| {
            Self::one_snapshot(key)?;
            if key.kind != TYPE_DIRENT {
                return Err(UpstreamError::Refused("dirents btree holds a key that is not a dirent"));
            }
            let val = node.value(key)?;
            let (name, inum, kind) = match decode_dirent(&val) {
                Ok(parts) => parts,
                Err(err) => {
                    failure = Some(err);
                    return Ok(false);
                }
            };
            Ok(visit(name, inum, kind))
        })?;
        match failure {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    /// The entry `name` of directory `dir`.
    pub fn lookup(&self, dir: u64, name: &str) -> Result<Option<(u64, FileKind)>, UpstreamError> {
        if name.is_empty() || name.len() > NAME_MAX {
            return Err(UpstreamError::Refused("a name of no length, or past BCH_NAME_MAX"));
        }
        let mut found = None;
        self.readdir(dir, &mut |entry, inum, kind| {
            if entry == name {
                found = Some((inum, kind));
                return false;
            }
            true
        })?;
        Ok(found)
    }

    /// Resolve an absolute path to an inode, following no symlink.
    pub fn resolve(&self, path: &str) -> Result<Option<(u64, FileKind)>, UpstreamError> {
        if !path.starts_with('/') {
            return Err(UpstreamError::Refused("a path this reader resolves is absolute"));
        }
        let mut at = (self.root, FileKind::Dir);
        for part in path.split('/').filter(|p| !p.is_empty() && *p != ".") {
            if at.1 != FileKind::Dir {
                return Ok(None);
            }
            match self.lookup(at.0, part)? {
                Some(next) => at = next,
                None => return Ok(None),
            }
        }
        Ok(Some(at))
    }

    /// Read the whole of file `inum`, up to the size its inode declares.
    ///
    /// A range no extent covers is a hole and reads as zeros, which is what
    /// makes the inode's size — not the extents — the length of the answer.
    ///
    /// **`bi_size` is a `u64` off the disk and it sizes this buffer**, which is
    /// the allocator-sizing defect this crate's `alloc_probe` exists to catch.
    /// The device is the bound: a file cannot be longer than the disk it is on,
    /// whatever its inode says, so an inode claiming more is refused before a
    /// byte of it is allocated.
    pub fn read(&self, inum: u64) -> Result<Vec<u8>, UpstreamError> {
        let attrs = self
            .stat(inum)?
            .ok_or(UpstreamError::Refused("a file whose inode the btree does not hold"))?;
        if attrs.sectors > self.device_bytes / SECTOR as u64 {
            return Err(UpstreamError::Refused("inode claims more sectors than the device has"));
        }
        let mut out = output_buffer(attrs.size, self.device_bytes)?;

        let extents = Btree::open(&self.io, &self.sb, BTREE_EXTENTS)?;
        let mut failure = None;
        extents.range(self.pos(inum, 0), self.pos(inum, u64::MAX), &mut |node, _, key| {
            Self::one_snapshot(key)?;
            let val = node.value(key)?;
            if let Err(err) = self.place(key, &val, &mut out) {
                failure = Some(err);
                return Ok(false);
            }
            Ok(true)
        })?;
        match failure {
            Some(err) => Err(err),
            None => Ok(out),
        }
    }

    /// The target of symlink `inum`: its contents, which is where bcachefs
    /// keeps it.
    pub fn read_link(&self, inum: u64) -> Result<String, UpstreamError> {
        let bytes = self.read(inum)?;
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        String::from_utf8(bytes[..end].to_vec())
            .map_err(|_| UpstreamError::Refused("symlink target is not valid UTF-8"))
    }

    /// Copy one extent key's live bytes into `out` at the file offset it covers.
    fn place(&self, key: &Key, val: &Raw<'_>, out: &mut [u8]) -> Result<(), UpstreamError> {
        // A key's position is the *end* of what it covers, in sectors.
        let end_sectors = key.pos.offset;
        let start_sectors = end_sectors
            .checked_sub(key.size as u64)
            .ok_or(UpstreamError::Refused("extent ends before it starts"))?;
        // A key's offset is a number the btree chose, and `1 << 60` sectors is
        // inside the scanned range and outside a `u64` of bytes.
        let at = start_sectors
            .checked_mul(SECTOR as u64)
            .ok_or(UpstreamError::Refused("extent starts past any byte offset"))?;
        let at = usize::try_from(at).map_err(|_| TOO_LARGE)?;
        if at >= out.len() {
            return Ok(());
        }
        let want = out.len() - at;

        match key.kind {
            TYPE_INLINE_DATA => {
                let take = want.min(val.bytes().len());
                out[at..at + take].copy_from_slice(&val.bytes()[..take]);
                Ok(())
            }
            // A reservation is allocated-but-unwritten and reads as zeros,
            // which is what `out` already holds.
            TYPE_RESERVATION => Ok(()),
            TYPE_ERROR => Err(UpstreamError::Refused("file holds an extent marked unrecoverable")),
            TYPE_EXTENT => {
                let bytes = self.read_extent(key, val)?;
                let take = want.min(bytes.len());
                out[at..at + take].copy_from_slice(&bytes[..take]);
                Ok(())
            }
            _ => Err(UpstreamError::Refused("extents btree holds a value type this reader does not read")),
        }
    }

    /// The live bytes of one `KEY_TYPE_extent`, checksum verified.
    ///
    /// The checksum covers the extent as it was *written*, so a trimmed extent
    /// is read whole, verified, and then sliced — a checksum over the live part
    /// alone would verify nothing, because nobody ever computed one.
    fn read_extent(&self, key: &Key, val: &Raw<'_>) -> Result<Vec<u8>, UpstreamError> {
        let (ptr, crc) = find_ptr(val, &self.entry_u64s)?;
        self.read_ptr(&ptr, crc, key.size)
    }

    fn read_ptr(&self, entry: &Raw<'_>, crc: Crc, live_sectors: u32) -> Result<Vec<u8>, UpstreamError> {
        let word = entry.u64(0)?;
        if bits(word, 1, 2) != 0 {
            return Err(UpstreamError::Refused("extent's first pointer is a cache copy"));
        }
        if bits(word, 3, 4) != 0 {
            return Err(UpstreamError::Refused("extent is unwritten"));
        }
        if bits(word, 48, 56) as u8 != self.sb.dev_idx() {
            return Err(UpstreamError::Refused("extent points at a device this filesystem has not got"));
        }
        if crc.compression != 0 {
            return Err(UpstreamError::Refused("extent is compressed"));
        }
        if crc.offset as u64 + live_sectors as u64 > crc.stored_sectors as u64 {
            return Err(UpstreamError::Refused("extent's live range is not inside the extent"));
        }

        let start = bits(word, 4, 48);
        let whole = self.read_sectors(start, crc.stored_sectors)?;
        if !crc.csum_type.verify(&whole, crc.csum) {
            return Err(UpstreamError::Refused("extent's data does not match its checksum"));
        }
        let from = crc.offset as usize * SECTOR;
        let len = live_sectors as usize * SECTOR;
        Ok(whole[from..from + len].to_vec())
    }

    fn read_sectors(&self, start: u64, sectors: u32) -> Result<Vec<u8>, UpstreamError> {
        let per_block = (BLOCK_SIZE / SECTOR) as u64;
        if !start.is_multiple_of(per_block) || !(sectors as u64).is_multiple_of(per_block) {
            return Err(UpstreamError::Refused("extent is not block aligned"));
        }
        let first = start / per_block;
        let blocks = sectors as u64 / per_block;
        if first.checked_add(blocks).is_none_or(|end| end > self.io.block_count()) {
            return Err(UpstreamError::Refused("extent runs past the end of the device"));
        }
        let mut out = vec![0u8; blocks as usize * BLOCK_SIZE];
        let mut buf = BlockBuf::zeroed();
        for i in 0..blocks as usize {
            let block = BlockNum::new(first + i as u64);
            self.io.read_block(block, &mut buf).map_err(|e| UpstreamError::Device(block, e))?;
            out[i * BLOCK_SIZE..(i + 1) * BLOCK_SIZE].copy_from_slice(buf.as_bytes());
        }
        Ok(out)
    }
}

const TOO_LARGE: UpstreamError =
    UpstreamError::Refused("file is larger than this machine can address");

const ENTRY_PTR: u32 = 0;
const ENTRY_CRC32: u32 = 1;
const ENTRY_CRC64: u32 = 2;
const ENTRY_CRC128: u32 = 3;
const ENTRY_STRIPE_PTR: u32 = 4;

/// The buffer a file's contents are read into, sized by its inode.
///
/// **The bound and the allocation are one function** so nothing can size a
/// `Vec` from `bi_size` without passing the device it is on: a file cannot be
/// longer than the disk holding it, whatever its inode claims.
fn output_buffer(size: u64, device_bytes: u64) -> Result<Vec<u8>, UpstreamError> {
    if size > device_bytes {
        return Err(UpstreamError::Refused("inode claims a file longer than the device it is on"));
    }
    Ok(vec![0u8; usize::try_from(size).map_err(|_| TOO_LARGE)?])
}

/// Walk an extent's interleaved entries to its first device pointer, carrying
/// whatever checksum entry preceded it.
///
/// **Each entry's length comes from the volume's own table, and one word of
/// error here is arbitrary device blocks served as file contents.** An entry
/// can precede the checksum and the pointer: `bch2_bkey_extent_flags_set`
/// inserts a `flags` entry at `ptrs.start` through `__extent_entry_insert`, so
/// a value is `[flags, crc32, ptr]` and a walk that oversizes the first entry
/// lands somewhere other than the pointer.
fn find_ptr<'a>(
    val: &Raw<'a>,
    sizes: &[u8; EXTENT_TYPES_MAX],
) -> Result<(Raw<'a>, Crc), UpstreamError> {
    let mut crc: Option<Crc> = None;
    let mut at = 0usize;
    while at < val.bytes().len() {
        let word = val.u64(at)?;
        if word == 0 {
            return Err(UpstreamError::Refused("extent holds an entry of no type"));
        }
        let kind = word.trailing_zeros();
        let u64s = entry_u64s(sizes, kind)?;
        let entry = val.sub(at, u64s * 8, "extent entry runs past its value")?;
        match kind {
            // **A bset with no checksum is refused, so an extent with none is
            // too.** A pointer with no crc entry before it is data nobody
            // computed a checksum over, on a volume whose superblock says it
            // checksums its data — which `check_supported` has already required.
            ENTRY_PTR => {
                return match crc {
                    Some(crc) => Ok((entry, crc)),
                    None => Err(UpstreamError::Refused("extent carries no checksum entry")),
                }
            }
            ENTRY_CRC32 => crc = Some(Crc::crc32(&entry)?),
            ENTRY_CRC64 => crc = Some(Crc::crc64(&entry)?),
            ENTRY_CRC128 => {
                return Err(UpstreamError::Refused("extent carries a 128-bit checksum, which is encryption"))
            }
            ENTRY_STRIPE_PTR => {
                return Err(UpstreamError::Refused("extent points into an erasure-coded stripe"))
            }
            // Rebalance, flags and reconcile entries say nothing about where
            // the bytes are; their length is the whole of what matters.
            _ => {}
        }
        at += u64s * 8;
    }
    Err(UpstreamError::Refused("extent has no device pointer"))
}

/// How far the next entry of an extent's value is, from the table the volume's
/// own superblock states. A type it sizes at zero is one nothing can step over.
fn entry_u64s(sizes: &[u8; EXTENT_TYPES_MAX], kind: u32) -> Result<usize, UpstreamError> {
    match sizes.get(kind as usize).copied() {
        Some(0) | None => {
            Err(UpstreamError::Refused("extent holds an entry type this volume gives no size for"))
        }
        Some(u64s) => Ok(u64s as usize),
    }
}

/// The checksum and geometry one `bch_extent_crc*` entry states about the
/// pointers that follow it.
#[derive(Clone, Copy)]
struct Crc {
    /// Sectors as written, which is what the checksum covers.
    stored_sectors: u32,
    /// Sectors into the written extent where the live data starts.
    offset: u32,
    compression: u64,
    csum_type: CsumType,
    csum: (u64, u64),
}

impl Crc {
    /// A checksum entry that states no checksum verifies everything, so it is
    /// refused where a bset's `none` is.
    fn checked(self) -> Result<Self, UpstreamError> {
        if self.csum_type == CsumType::None {
            return Err(UpstreamError::Refused("extent's checksum entry names no checksum"));
        }
        Ok(self)
    }

    /// `bch_extent_crc32`; its sizes are stored biased by one.
    fn crc32(entry: &Raw<'_>) -> Result<Self, UpstreamError> {
        let w = entry.u32(0)? as u64;
        Self {
            stored_sectors: bits(w, 9, 16) as u32 + 1,
            offset: bits(w, 16, 23) as u32,
            compression: bits(w, 28, 32),
            csum_type: CsumType::from_disk(bits(w, 24, 28))?,
            csum: (entry.u32(4)? as u64, 0),
        }
        .checked()
    }

    /// `bch_extent_crc64`; its 64-bit checksum is split across two fields.
    fn crc64(entry: &Raw<'_>) -> Result<Self, UpstreamError> {
        let w = entry.u64(0)?;
        Self {
            stored_sectors: bits(w, 12, 21) as u32 + 1,
            offset: bits(w, 21, 30) as u32,
            compression: bits(w, 44, 48),
            csum_type: CsumType::from_disk(bits(w, 40, 44))?,
            csum: (entry.u64(8)? | (bits(w, 48, 64) << 48), 0),
        }
        .checked()
    }
}

/// `bch2_dirent_get_name`: the name is what is left after the header once the
/// last word's trailing NULs are taken off.
fn decode_dirent<'a>(val: &Raw<'a>) -> Result<(&'a str, u64, FileKind), UpstreamError> {
    let short = UpstreamError::Refused("dirent is shorter than its header");
    if val.bytes().len() < DIRENT_NAME_AT + 1 || !val.bytes().len().is_multiple_of(8) {
        return Err(short);
    }
    let d_type_byte = val.u8(8)?;
    if d_type_byte & 0x80 != 0 {
        return Err(UpstreamError::Refused("dirent is casefolded"));
    }
    let d_type = d_type_byte & 0x1F;
    if d_type == DT_SUBVOL {
        return Err(UpstreamError::Refused("dirent points at a subvolume"));
    }

    let last = val.u64(val.bytes().len() - 8)?;
    let trailing_nuls = if last == 0 { 8 } else { last.leading_zeros() as usize / 8 };
    let len = val
        .bytes()
        .len()
        .checked_sub(DIRENT_NAME_AT + trailing_nuls)
        .ok_or(UpstreamError::Refused("dirent has no name"))?;
    if len == 0 || len > NAME_MAX {
        return Err(UpstreamError::Refused("dirent's name is empty or past BCH_NAME_MAX"));
    }
    let bytes = val.slice(DIRENT_NAME_AT, len)?;
    if bytes.contains(&b'/') || bytes.contains(&0) {
        return Err(UpstreamError::Refused("dirent's name holds a separator or a NUL"));
    }
    let name = core::str::from_utf8(bytes)
        .map_err(|_| UpstreamError::Refused("dirent's name is not valid UTF-8"))?;
    Ok((name, val.u64(0)?, FileKind::from_dirent(d_type)))
}


#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// A dirent as upstream lays one out: the inode, the type byte, the name,
    /// and NUL padding to a whole number of words.
    fn dirent(inum: u64, d_type: u8, name: &[u8]) -> Vec<u8> {
        let mut out = inum.to_le_bytes().to_vec();
        out.push(d_type);
        out.extend_from_slice(name);
        while !out.len().is_multiple_of(8) {
            out.push(0);
        }
        out
    }

    fn decode(bytes: &[u8]) -> Result<(&str, u64, FileKind), UpstreamError> {
        decode_dirent(&Raw::new(bytes, "dirent"))
    }

    /// The name is what is left once the last word's trailing NULs come off,
    /// which is the whole of how a dirent states its length.
    #[test]
    fn a_name_is_its_padding_taken_off() {
        let one = dirent(4096, DT_DIR, b"empty");
        assert_eq!(decode(&one), Ok(("empty", 4096, FileKind::Dir)));

        // Exactly filling the last word leaves no NUL to count.
        let flush = dirent(7, DT_REG, b"abcdefg");
        assert_eq!(decode(&flush), Ok(("abcdefg", 7, FileKind::Regular)));

        let long = dirent(9, DT_LNK, b"Documents");
        assert_eq!(decode(&long), Ok(("Documents", 9, FileKind::Symlink)));

        let single = dirent(11, DT_REG, b"a");
        assert_eq!(decode(&single), Ok(("a", 11, FileKind::Regular)));
    }

    /// Every shape a hostile dirent can take is refused by name rather than
    /// returning a name a caller would go on to use as a path component.
    #[test]
    fn a_dirent_that_is_not_one_is_refused() {
        let short = vec![0u8; 8];
        assert!(decode(&short).is_err());

        let ragged = vec![0u8; 12];
        assert!(decode(&ragged).is_err());

        // All padding: no name at all.
        let mut nameless = dirent(3, DT_REG, b"x");
        nameless[9] = 0;
        assert_eq!(
            decode(&nameless),
            Err(UpstreamError::Refused("dirent's name is empty or past BCH_NAME_MAX"))
        );

        let mut casefolded = dirent(3, DT_REG, b"x");
        casefolded[8] |= 0x80;
        assert_eq!(decode(&casefolded), Err(UpstreamError::Refused("dirent is casefolded")));

        let subvol = dirent(3, DT_SUBVOL, b"sub");
        assert_eq!(decode(&subvol), Err(UpstreamError::Refused("dirent points at a subvolume")));

        // A separator inside a name would let one entry name another directory.
        let traversal = dirent(3, DT_REG, b"a/b");
        assert_eq!(
            decode(&traversal),
            Err(UpstreamError::Refused("dirent's name holds a separator or a NUL"))
        );

        let not_utf8 = dirent(3, DT_REG, &[0xff, 0xfe]);
        assert_eq!(decode(&not_utf8), Err(UpstreamError::Refused("dirent's name is not valid UTF-8")));
    }

    /// A `bch_extent_crc32`'s sizes are stored biased by one, so a reader that
    /// forgets the bias reads one sector short of every extent.
    #[test]
    fn a_crc_entry_states_the_extent_it_covers() {
        // csum_type crc32c, compression none, uncompressed 80 sectors,
        // compressed 80, offset 0 — the second extent of the file the oracle
        // writes, as the guest laid it out.
        let word: u32 = 0b10 | (79 << 2) | (79 << 9) | (5 << 24);
        let mut entry = word.to_le_bytes().to_vec();
        entry.extend_from_slice(&0xc522_c42fu32.to_le_bytes());
        let crc = Crc::crc32(&Raw::new(&entry, "crc32")).expect("a crc32 entry");
        assert_eq!(crc.stored_sectors, 80);
        assert_eq!(crc.offset, 0);
        assert_eq!(crc.compression, 0);
        assert_eq!(crc.csum_type, CsumType::Crc32c);
        assert_eq!(crc.csum, (0xc522_c42f, 0));
    }

    /// An inode claiming 256 TiB is refused having asked the allocator for
    /// nothing like it.
    #[test]
    fn a_file_longer_than_its_device_is_refused_without_allocating_it() {
        let device = 1u64 << 20;
        let _ = crate::alloc_probe::take_peak();
        let refused = output_buffer(1 << 48, device);
        let peak = crate::alloc_probe::take_peak();
        assert_eq!(
            refused.err(),
            Some(UpstreamError::Refused("inode claims a file longer than the device it is on"))
        );
        assert!(peak < 1 << 16, "refusing a 256 TiB file asked the allocator for {peak} bytes");

        assert_eq!(output_buffer(0, device).map(|b| b.len()), Ok(0));
        assert_eq!(output_buffer(device, device).map(|b| b.len()), Ok(device as usize));
        assert!(output_buffer(device + 1, device).is_err());
    }

    /// The sizes `sizeof` gives at the pinned commit, and the refusal for a
    /// type the volume gives no size for.
    #[test]
    fn an_entry_this_volume_cannot_size_is_refused() {
        let sizes = super::super::sb::EXTENT_ENTRY_U64S_KNOWN_FOR_TESTS;
        for (kind, want) in [(0u32, 1usize), (1, 1), (2, 2), (3, 3), (4, 1), (5, 1), (6, 1), (7, 1), (8, 1)] {
            assert_eq!(entry_u64s(&sizes, kind), Ok(want), "entry type {kind}");
        }
        assert!(entry_u64s(&sizes, 9).is_err(), "a type the table sizes at zero must be refused");
        assert!(entry_u64s(&sizes, 99).is_err(), "a type past the table must be refused");
    }

    /// An entry ahead of the checksum and the pointer, sized right and sized
    /// one word out.
    ///
    /// `bch2_bkey_extent_flags_set` inserts a `flags` entry at `ptrs.start`, so
    /// `[flags, crc32, ptr]` is a value upstream writes; one word of size error
    /// on it puts the walk on the checksum entry and the pointer out of reach.
    #[test]
    fn an_entry_before_the_checksum_does_not_move_the_pointer() {
        // The type field's width is the type: `flags` is 7 bits, so 1 << 6.
        let flags: u64 = 1 << 6;
        let crc32_lo: u32 = 0b10 | (79 << 2) | (79 << 9) | (5 << 24);
        let ptr: u64 = 1 | (0xE000u64 << 4);

        let mut value = flags.to_le_bytes().to_vec();
        value.extend_from_slice(&crc32_lo.to_le_bytes());
        value.extend_from_slice(&0xc522_c42fu32.to_le_bytes());
        value.extend_from_slice(&ptr.to_le_bytes());
        let val = Raw::new(&value, "extent");

        let right = super::super::sb::EXTENT_ENTRY_U64S_KNOWN_FOR_TESTS;
        let (entry, crc) = find_ptr(&val, &right).expect("the pointer");
        assert_eq!(entry.u64(0), Ok(ptr), "the walk did not land on the pointer");
        assert_eq!(crc.stored_sectors, 80, "the checksum entry was skipped");
        assert_eq!(crc.csum, (0xc522_c42f, 0));

        // One word out: the walk steps over the checksum entry and reads the
        // pointer with no checksum to verify it against.
        let mut wrong = right;
        wrong[6] = 2;
        assert_eq!(
            find_ptr(&val, &wrong).err(),
            Some(UpstreamError::Refused("extent carries no checksum entry")),
            "a `flags` entry sized one word out still produced a checksummed pointer"
        );
    }
}
