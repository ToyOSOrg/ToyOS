//! A clean volume, read: its superblock chosen among the copies, its
//! btrees from `clean`'s roots, and files, directories and symlinks over
//! them.

use alloc::vec::Vec;

use toyos_blockdev::{BlockDev, BLOCK};
use toyos_crypto::{scrypt, siphash};

use super::bkey::{Bpos, Key};
use super::crypt::{MasterKey, Nonce};
use super::node::{self, Expect, Node};
use super::superblock::{Layout, Root, Superblock, HEADER, LAYOUT, MAX_BYTES, PRIMARY};
use super::value::{inode_field, inode_flag, Dirent, DirentType, Extent, InodeValue, NodePtr, Subvolume};
use super::{kind, le32, Btree, Error, Result, SECTOR};
use crate::crc32c::{crc32c, crc32c_zero};

/// A clean volume open for reading.
pub struct Volume<D> {
    dev: D,
    sb: Superblock,
    key: Option<MasterKey>,
    /// The root subvolume's snapshot, the only one read.
    snapshot: u32,
    root: u64,
}

/// A file's attributes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inode {
    pub ino: u64,
    pub mode: u16,
    pub size: u64,
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
    pub rdev: u32,
    pub generation: u32,
    /// Access, change, modification and creation, in nanoseconds since the
    /// Unix epoch.
    pub atime: i64,
    pub ctime: i64,
    pub mtime: i64,
    pub otime: i64,
    /// The journal sequence the inode was last written in.
    pub journal_seq: u64,
    value: InodeValue,
}

/// One name in a directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    pub name: Vec<u8>,
    pub ino: u64,
    pub kind: DirentType,
    /// Where a listing resumes after this entry.
    pub next: u64,
}

/// The longest symlink this reader reads, its NUL included: a path's
/// length on the systems that make them.
pub(crate) const MAX_SYMLINK: u64 = 4096;

const S_IFMT: u16 = 0o170000;
const S_IFDIR: u16 = 0o040000;

impl Inode {
    pub fn is_dir(&self) -> bool {
        self.mode & S_IFMT == S_IFDIR
    }
}

impl<D: BlockDev> Volume<D> {
    /// An unencrypted volume, or one formatted without a passphrase.
    pub async fn open(dev: D) -> Result<Self> {
        let sb = read_superblock(&dev).await?;
        let key = match &sb.crypt {
            Some(crypt) => Some(MasterKey::stored(crypt).ok_or(Error::Locked)?),
            None => None,
        };
        Self::with(dev, sb, key).await
    }

    /// An encrypted volume, its key unwrapped under `passphrase` with no more
    /// scrypt cost than `limits`, which the superblock asks and the caller
    /// bounds.
    pub async fn unlock(dev: D, passphrase: &[u8], limits: scrypt::Limits) -> Result<Self> {
        let sb = read_superblock(&dev).await?;
        let crypt = sb.crypt.ok_or(Error::Invalid("a passphrase for an unencrypted volume"))?;
        let key = MasterKey::unwrap(&crypt, &sb.header.internal_uuid, passphrase, limits)?;
        Self::with(dev, sb, Some(key)).await
    }

    async fn with(dev: D, sb: Superblock, key: Option<MasterKey>) -> Result<Self> {
        if sb.encrypted() != sb.crypt.is_some() {
            return Err(Error::Corrupt("encryption option and crypt field disagree"));
        }
        if !sb.clean_flag() || sb.clean.is_none() {
            return Err(Error::Unsupported("a volume not cleanly unmounted: its journal needs replay"));
        }
        let m = &sb.member;
        if m.nbuckets * u64::from(m.bucket_sectors) > dev.blocks() * (BLOCK / SECTOR) as u64 {
            return Err(Error::Corrupt("member's buckets past the device"));
        }
        let mut volume = Volume { dev, sb, key, snapshot: 0, root: 0 };
        let (k, v) = volume.get(Btree::Subvolumes, Bpos::new(0, u64::from(super::mkfs::ROOT_SUBVOLUME), 0)).await?.ok_or(Error::Corrupt("no root subvolume"))?;
        if k.kind != kind::SUBVOLUME {
            return Err(Error::Corrupt("root subvolume of another type"));
        }
        let subvolume = Subvolume::decode(&v)?;
        // A snapshotted subvolume's keys live in its ancestors' snapshots,
        // which this reader does not walk.
        if subvolume.snapshot != super::mkfs::ROOT_SNAPSHOT {
            return Err(Error::Unsupported("a root subvolume that has been snapshotted"));
        }
        volume.snapshot = subvolume.snapshot;
        volume.root = subvolume.inode;
        Ok(volume)
    }

    pub fn superblock(&self) -> &Superblock {
        &self.sb
    }

    pub(crate) fn dev(&self) -> &D {
        &self.dev
    }

    /// The root directory's inode number.
    pub fn root(&self) -> u64 {
        self.root
    }

    pub(crate) fn roots(&self) -> &[Root] {
        &self.sb.clean.as_ref().expect("opened clean").roots
    }

    fn root_of(&self, btree: Btree) -> Option<&Root> {
        self.roots().iter().find(|r| r.btree == btree)
    }

    /// The node `ptr` points at, `level` of `btree`, spanning `[min, max]`.
    pub(crate) async fn node(&self, btree: Btree, level: u8, ptr: &NodePtr, max: Bpos) -> Result<Node> {
        let sectors = u32::from(ptr.written);
        if sectors > self.sb.node_sectors() || ptr.ptr.dev != 0 {
            return Err(Error::Corrupt("btree pointer past its node, or to another device"));
        }
        self.in_one_bucket(ptr.ptr.sector, u64::from(self.sb.node_sectors()))?;
        let bytes = read_bytes(&self.dev, ptr.ptr.sector * SECTOR as u64, sectors as usize * SECTOR).await?;
        let expect = Expect { btree, level, seq: ptr.seq, min: ptr.min, max };
        node::decode(&self.sb, self.key.as_ref(), &expect, bytes)
    }

    /// A pointed range lies in one bucket of the device.
    fn in_one_bucket(&self, sector: u64, sectors: u64) -> Result<()> {
        let m = &self.sb.member;
        let bucket = u64::from(m.bucket_sectors);
        let first = sector / bucket;
        if first < u64::from(m.first_bucket) || first >= m.nbuckets || sector % bucket + sectors > bucket {
            return Err(Error::Corrupt("pointer outside one bucket of the device"));
        }
        Ok(())
    }

    /// Visits `btree`'s keys from `from` in order until `visit` answers
    /// `false`.
    pub(crate) async fn walk(&self, btree: Btree, from: Bpos, mut visit: impl FnMut(&Key, &[u8]) -> Result<bool>) -> Result<()> {
        let Some(root) = self.root_of(btree) else { return Ok(()) };
        let mut stack: Vec<(Node, usize)> = Vec::new();
        let node = self.node(btree, root.level, &root.ptr, root.key.pos).await?;
        let start = node.keys.partition_point(|(k, _)| k.pos < from);
        stack.push((node, start));
        while let Some((node, i)) = stack.last_mut() {
            let Some((key, value)) = node.keys.get(*i) else {
                stack.pop();
                continue;
            };
            *i += 1;
            if node.level == 0 {
                if !visit(key, value)? {
                    return Ok(());
                }
                continue;
            }
            if key.kind != kind::BTREE_PTR_V2 {
                return Err(Error::Unsupported("interior key other than btree_ptr_v2, such as a v1 btree_ptr"));
            }
            let (level, ptr, max) = (node.level - 1, NodePtr::decode(value)?, key.pos);
            let child = self.node(btree, level, &ptr, max).await?;
            let start = child.keys.partition_point(|(k, _)| k.pos < from);
            stack.push((child, start));
        }
        Ok(())
    }

    /// The key at `pos` in `btree`, if any.
    pub(crate) async fn get(&self, btree: Btree, pos: Bpos) -> Result<Option<(Key, Vec<u8>)>> {
        let mut found = None;
        self.walk(btree, pos, |k, v| {
            if k.pos == pos {
                found = Some((*k, v.to_vec()));
            }
            Ok(false)
        })
        .await?;
        Ok(found)
    }

    /// The inode numbered `ino`.
    pub async fn inode(&self, ino: u64) -> Result<Inode> {
        let (key, value) = self.get(Btree::Inodes, Bpos::new(0, ino, self.snapshot)).await?.ok_or(Error::NotFound)?;
        match key.kind {
            kind::INODE_V3 => {}
            kind::INODE | kind::INODE_V2 => return Err(Error::Unsupported("inode in a format before inode_v3")),
            _ => return Err(Error::NotFound),
        }
        let v = InodeValue::decode(&value)?;
        let mode = v.mode();
        let stored = v.field(inode_field::NLINK) as u32;
        let nlink = if v.flags & inode_flag::UNLINKED != 0 {
            0
        } else {
            stored.checked_add(if mode & S_IFMT == S_IFDIR { 2 } else { 1 }).ok_or(Error::Corrupt("inode link count past 2^32"))?
        };
        let time = |f| self.time(v.field(f));
        Ok(Inode {
            ino,
            mode,
            size: v.size,
            nlink,
            uid: v.field(inode_field::UID) as u32,
            gid: v.field(inode_field::GID) as u32,
            rdev: v.field(inode_field::RDEV) as u32,
            generation: v.field(inode_field::GENERATION) as u32,
            atime: time(inode_field::ATIME),
            ctime: time(inode_field::CTIME),
            mtime: time(inode_field::MTIME),
            otime: time(inode_field::OTIME),
            journal_seq: v.journal_seq,
            value: v,
        })
    }

    /// A stored time, signed units since the time base, in
    /// nanoseconds since the epoch.
    fn time(&self, stored: u64) -> i64 {
        let precision = i128::from(self.sb.header.time_precision);
        let ns = (i128::from(stored as i64) + i128::from(self.sb.header.time_base) / precision) * precision;
        ns.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
    }

    async fn directory(&self, dir: u64) -> Result<Inode> {
        let inode = self.inode(dir).await?;
        if inode.is_dir() {
            Ok(inode)
        } else {
            Err(Error::NotDirectory)
        }
    }

    /// The entry `name` in directory `dir`: its name's hash slot, then the run
    /// of entries and whiteouts after it.
    pub async fn lookup(&self, dir: u64, name: &[u8]) -> Result<Option<DirEntry>> {
        let inode = self.directory(dir).await?;
        let start = Bpos::new(dir, hash(&inode.value, name)?, self.snapshot);
        let mut expected = start;
        let mut found = None;
        let mut failed = None;
        self.walk(Btree::Dirents, start, |k, v| {
            if k.pos != expected {
                return Ok(false);
            }
            if k.kind == kind::DIRENT {
                match entry(k, v) {
                    Ok(e) if e.name == name => {
                        found = Some(e);
                        return Ok(false);
                    }
                    Ok(_) => {}
                    Err(e) => {
                        failed = Some(e);
                        return Ok(false);
                    }
                }
            } else if k.kind != kind::HASH_WHITEOUT {
                return Ok(false);
            }
            match k.pos.offset.checked_add(1) {
                Some(offset) => expected.offset = offset,
                None => return Ok(false),
            }
            Ok(true)
        })
        .await?;
        failed.map_or(Ok(found), Err)
    }

    /// Up to `max` entries of `dir` from `cursor` on, in hash order, and
    /// whether more follow. Cursor 0 starts the listing; an entry's `next`
    /// resumes after it.
    pub async fn readdir(&self, dir: u64, cursor: u64, max: usize) -> Result<(Vec<DirEntry>, bool)> {
        self.directory(dir).await?;
        let mut out = Vec::new();
        let mut more = false;
        self.walk(Btree::Dirents, Bpos::new(dir, cursor, 0), |k, v| {
            if k.pos.inode != dir {
                return Ok(false);
            }
            if k.pos.snapshot != self.snapshot || k.kind != kind::DIRENT {
                return Ok(true);
            }
            if out.len() == max {
                more = true;
                return Ok(false);
            }
            out.push(entry(k, v)?);
            Ok(true)
        })
        .await?;
        Ok((out, more))
    }

    /// The target of the symlink `ino`.
    pub async fn readlink(&self, ino: u64) -> Result<Vec<u8>> {
        let inode = self.inode(ino).await?;
        if inode.mode & S_IFMT != 0o120000 || inode.size == 0 {
            return Err(Error::Invalid("readlink of a node that is no symlink"));
        }
        if inode.size > MAX_SYMLINK {
            return Err(Error::Unsupported("symlink target past 4 KiB"));
        }
        let mut target = alloc::vec![0u8; inode.size as usize];
        self.read(ino, 0, &mut target).await?;
        // The target is stored with one NUL after it.
        if target.pop() != Some(0) {
            return Err(Error::Corrupt("symlink target without its NUL"));
        }
        Ok(target)
    }

    /// Bytes of file `ino` from `offset` into `out`, as many as the file
    /// holds there. Holes read as zeros.
    pub async fn read(&self, ino: u64, offset: u64, out: &mut [u8]) -> Result<usize> {
        let inode = self.inode(ino).await?;
        let len = inode.size.saturating_sub(offset).min(out.len() as u64) as usize;
        let out = &mut out[..len];
        out.fill(0);
        if len == 0 {
            return Ok(0);
        }
        let (start, end) = (offset, offset + len as u64);
        let mut extents = Vec::new();
        self.walk(Btree::Extents, Bpos::new(ino, start / SECTOR as u64 + 1, 0), |k, v| {
            if k.pos.inode != ino {
                return Ok(false);
            }
            let first = extent_start(k)?;
            if first >= end {
                return Ok(false);
            }
            if k.pos.snapshot == self.snapshot {
                extents.push((*k, v.to_vec()));
            }
            Ok(true)
        })
        .await?;
        for (key, value) in extents {
            let first = extent_start(&key)?;
            let last = first + u64::from(key.size) * SECTOR as u64;
            let (from, to) = (first.max(start), last.min(end));
            if from >= to {
                continue;
            }
            let dst = &mut out[(from - start) as usize..(to - start) as usize];
            match key.kind {
                kind::EXTENT => self.extent(&key, &value, from - first, dst).await?,
                kind::INLINE_DATA => {
                    // The bytes from the key's first; past the value,
                    // zeros.
                    let skip = (from - first) as usize;
                    let have = value.get(skip..).unwrap_or(&[]);
                    let n = have.len().min(dst.len());
                    dst[..n].copy_from_slice(&have[..n]);
                }
                kind::RESERVATION | kind::WHITEOUT | kind::EXTENT_WHITEOUT => {}
                kind::REFLINK_P => return Err(Error::Unsupported("reflinked extent")),
                kind::ERROR => return Err(Error::Corrupt("extent recorded as lost")),
                _ => return Err(Error::Corrupt("extents key of no extent type")),
            }
        }
        Ok(len)
    }

    /// `dst.len()` bytes of the extent `key` from `skip` bytes into its
    /// range, its checksum verified or its MAC verified and decrypted.
    async fn extent(&self, key: &Key, value: &[u8], skip: u64, dst: &mut [u8]) -> Result<()> {
        let extent = Extent::decode(value)?;
        let crc = extent.crc.ok_or(Error::Unsupported("extent without a checksum entry"))?;
        if extent.ptr.unwritten {
            return Ok(());
        }
        if crc.compression != 0 || crc.compressed != crc.uncompressed {
            return Err(Error::Unsupported("compressed extent"));
        }
        if extent.ptr.dev != 0 || extent.ptr.cached {
            return Err(Error::Unsupported("extent on another device, or cached"));
        }
        if u64::from(crc.offset) + u64::from(key.size) > u64::from(crc.uncompressed) {
            return Err(Error::Corrupt("extent past its checksummed range"));
        }
        self.in_one_bucket(extent.ptr.sector, u64::from(crc.compressed))?;
        // An encrypted volume MACs all its data.
        if self.sb.encrypted() && !matches!(crc.csum_type, 3 | 4) {
            return Err(Error::Corrupt("unauthenticated extent on an encrypted volume"));
        }
        let mut data = read_bytes(&self.dev, extent.ptr.sector * SECTOR as u64, crc.compressed as usize * SECTOR).await?;
        match crc.csum_type {
            0 => {}
            // Over the whole encoded extent, the value zero-extended to the
            // entry's checksum bytes.
            1 | 5 => {
                let value = if crc.csum_type == 1 { crc32c(&data) } else { crc32c_zero(&data) };
                let mut want = [0u8; 16];
                want[..4].copy_from_slice(&value.to_le_bytes());
                if want != crc.csum {
                    return Err(Error::Checksum("extent"));
                }
            }
            // An 80-bit MAC in a crc64 entry, a 128-bit one in a
            // crc128.
            3 | 4 if crc.csum_len == if crc.csum_type == 3 { 10 } else { 16 } => {
                let master = self.key.as_ref().ok_or(Error::Locked)?;
                let nonce = Nonce::extent(key.version.hi, key.version.lo, crc.nonce);
                master.verify(nonce, &data, &crc.csum[..crc.csum_len], "extent")?;
                master.xor(nonce, &mut data)?;
            }
            _ => return Err(Error::Unsupported("data checksum type")),
        }
        let at = crc.offset as usize * SECTOR + skip as usize;
        dst.copy_from_slice(&data[at..at + dst.len()]);
        Ok(())
    }
}

/// The byte an extent key's range starts at: its position is its end.
fn extent_start(k: &Key) -> Result<u64> {
    let first = k.pos.offset.checked_sub(u64::from(k.size)).ok_or(Error::Corrupt("extent larger than its end"))?;
    k.pos.offset.checked_mul(SECTOR as u64).ok_or(Error::Corrupt("extent past 2^64 bytes"))?;
    Ok(first * SECTOR as u64)
}

/// A dirent key and value as an entry.
fn entry(k: &Key, v: &[u8]) -> Result<DirEntry> {
    let dirent = Dirent::decode(v)?;
    if dirent.kind == DirentType::Subvolume {
        return Err(Error::Unsupported("subvolume dirent"));
    }
    let next = k.pos.offset.checked_add(1).ok_or(Error::Corrupt("dirent at the last offset"))?;
    Ok(DirEntry { name: dirent.name.to_vec(), ino: dirent.target, kind: dirent.kind, next })
}

/// The hash slot of `name` under directory `dir`'s seed: SipHash-2-4 keyed
/// with the seed and zero, shifted to 63 bits, never 0 or 1, which are `.`
/// and `..`.
pub(crate) fn hash(dir: &InodeValue, name: &[u8]) -> Result<u64> {
    if dir.hash_type() != 3 {
        return Err(Error::Unsupported("directory name hash other than siphash"));
    }
    let mut key = [0u8; 16];
    key[..8].copy_from_slice(&dir.hash_seed.to_le_bytes());
    let mut h = siphash::siphash24(&key, name) >> 1;
    if dir.flags & inode_flag::DIRENT_31_BIT != 0 {
        h &= (1 << 31) - 1;
    }
    Ok(h.max(2))
}

/// `len` bytes from byte `at`, read through the device's whole blocks.
pub(crate) async fn read_bytes(dev: &impl BlockDev, at: u64, len: usize) -> Result<Vec<u8>> {
    let first = at / BLOCK as u64;
    let skip = (at % BLOCK as u64) as usize;
    let blocks = (skip + len).div_ceil(BLOCK);
    let mut buf = alloc::vec![[0u8; BLOCK]; blocks];
    dev.read(first, &mut buf).await?;
    Ok(buf.as_flattened()[skip..skip + len].to_vec())
}

/// The primary copy's layout, or the layout copy's when the primary
/// is unreadable; then of every copy it lists, the valid one of highest
/// sequence, the later on a tie.
pub(crate) async fn read_superblock(dev: &impl BlockDev) -> Result<Superblock> {
    let primary = read_copy(dev, PRIMARY).await;
    let layout = match &primary {
        Ok(sb) => sb.header.layout.clone(),
        Err(Error::Device(e)) => return Err(Error::Device(*e)),
        Err(_) => Layout::decode(&read_bytes(dev, LAYOUT * SECTOR as u64, Layout::BYTES).await?)?,
    };
    let mut best: Option<Superblock> = primary.as_ref().ok().cloned();
    let mut failure = primary.err();
    for &offset in &layout.offsets {
        if offset == PRIMARY {
            continue;
        }
        match read_copy(dev, offset).await {
            Ok(sb) if best.as_ref().is_none_or(|b| sb.header.seq >= b.header.seq) => best = Some(sb),
            Ok(_) => {}
            Err(Error::Device(e)) => return Err(Error::Device(e)),
            Err(e) => failure = failure.or(Some(e)),
        }
    }
    best.ok_or(failure.unwrap_or(Error::Corrupt("no superblock copy")))
}

async fn read_copy(dev: &impl BlockDev, sector: u64) -> Result<Superblock> {
    let device = dev.blocks() * BLOCK as u64;
    let fits = |len: usize| sector.checked_mul(SECTOR as u64).and_then(|at| at.checked_add(len as u64)).is_some_and(|end| end <= device);
    if !fits(BLOCK) {
        return Err(Error::Corrupt("superblock copy past the device"));
    }
    let at = sector * SECTOR as u64;
    let head = read_bytes(dev, at, BLOCK).await?;
    let total = HEADER + 8 * le32(&head, 124) as usize;
    if total <= head.len() {
        return Superblock::decode(&head, sector);
    }
    if total > MAX_BYTES || !fits(total) {
        return Err(Error::Unsupported("superblock past 64 KiB or past the device"));
    }
    Superblock::decode(&read_bytes(dev, at, total).await?, sector)
}
