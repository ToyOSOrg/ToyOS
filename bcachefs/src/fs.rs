use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::marker::PhantomData;

use crate::alloc_bitmap::BitmapAllocator;
use crate::block_io::{BlockBuf, BlockNum, BlockIO, BlockIOExt, BLOCK_SIZE};
use crate::btree::{self, Entry, Key, KeyType, Node};
use crate::superblock::Superblock;

/// Extent: a contiguous run of blocks on disk.
///
/// **An extent that came off a disk is a range of the volume, and
/// [`decode_leaf_value`] is what makes that true of it.** The fields are bare
/// integers and they cross the crate boundary — `kernel/src/file_backing.rs`
/// turns `start_block` into a block to read on every page fault — so nothing
/// downstream has anything to compare them against. The comparison happens
/// once, at the decode.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Extent {
    pub start_block: u64,
    pub block_count: u32,
    pub _reserved: u32,
}

const EXTENT_SIZE: usize = 16;

/// Append `count` blocks at `start`, extending the last extent when the run
/// continues it.
///
/// Without this a file needed one extent per *page*: the allocator hands back
/// consecutive blocks for consecutive single-block requests, and every one of
/// them was pushed as a fresh 16-byte extent into a value that has to fit in a
/// 4 KiB btree node. That is what capped a file at ~250 pages and panicked the
/// kernel one page later. A sequentially written file of any size is now one
/// extent, and what remains bounded is the number of *discontiguous runs*.
fn push_extent(extents: &mut Vec<Extent>, start: u64, count: u32) {
    if let Some(last) = extents.last_mut() {
        // `checked_add` on both halves. On the count, because it is a u32 and a
        // run past 16 TiB has to become a second extent instead of wrapping. On
        // the address, because the extent list this appends to can have come
        // off the disk — the kernel's write path resolves a page against the
        // list `decode_leaf_value` handed it — and `u64::MAX + 1` in a kernel
        // built with `overflow-checks` is a panic from a number an image chose.
        if last.start_block.checked_add(last.block_count as u64) == Some(start) {
            if let Some(merged) = last.block_count.checked_add(count) {
                last.block_count = merged;
                return;
            }
        }
    }
    extents.push(Extent { start_block: start, block_count: count, _reserved: 0 });
}

/// Filesystem error type with rich context.
#[derive(Debug)]
pub enum FsError {
    BadMagic { expected: [u8; 4], got: [u8; 4] },
    UnsupportedVersion(u32),
    ChecksumMismatch { block: BlockNum, stored: u32, computed: u32 },
    CorruptedKey(u16),
    CorruptedNode(BlockNum),
    /// A block number off the end of what it was read out of. On-disk pointers
    /// are the disk's claim about itself; the arbiter is whichever bound is
    /// smaller — the device for a btree child pointer, the *volume* for a
    /// file's extents, because the volume is what the allocator's bitmap is
    /// sized from.
    BlockOffDevice { block: u64, device_blocks: u64 },
    /// A file claiming more blocks than the volume has — an extent list naming
    /// them, or a declared size needing them. The disk's arithmetic about
    /// itself, and it sizes what this crate then allocates.
    NotEnoughBlocks { needed: u64, available: u64 },
    /// A descent that has gone deeper than any tree over this device can be,
    /// which means it is following a cycle.
    TreeTooDeep(BlockNum),
    /// A superblock that does not describe the device it was read from. The
    /// CRC only says the bytes are the bytes somebody wrote.
    BadSuperblock { field: &'static str },
    /// The device refused a transfer. Distinct from every corruption variant
    /// above: those say the bytes are wrong, these say there are no bytes.
    DeviceRead(BlockNum),
    DeviceWrite(BlockNum),
    DeviceSync,
    NotFound,
    NoSpace { requested: u32, available: u64 },
    NameTooLong { len: usize, max: usize },
    /// A value no node could hold. Reachable from ordinary userland writes:
    /// the extent list lives inline in the value, one entry per discontiguous
    /// run of the file.
    EntryTooLarge { size: usize, max: usize },
    /// A node whose entries do not fit the block. Defence in depth behind
    /// `EntryTooLarge` — nothing should reach it.
    NodeOverfull { used: usize, max: usize },
}

pub struct ReadOnly;
pub struct ReadWrite;

/// A formatted but not yet mounted filesystem. Used for building images (mkfs).
pub struct Formatted<IO: BlockIO> {
    io: IO,
    sb: Superblock,
    alloc: BitmapAllocator,
}

/// A mounted filesystem. Mode is ReadOnly or ReadWrite.
pub struct Mounted<IO: BlockIO, Mode = ReadWrite> {
    io: IO,
    sb: Superblock,
    alloc: BitmapAllocator,
    _mode: PhantomData<Mode>,
}

// --- Hash ---

fn siphash_2_4(data: &[u8], key: [u8; 8]) -> u64 {
    // Simplified SipHash-2-4
    let k = u64::from_le_bytes(key);
    let mut v0 = 0x736f6d6570736575u64 ^ k;
    let mut v1 = 0x646f72616e646f6du64 ^ k;
    let mut v2 = 0x6c7967656e657261u64 ^ k;
    let mut v3 = 0x7465646279746573u64 ^ k;

    let len = data.len();
    let blocks = len / 8;

    for i in 0..blocks {
        let mut word = [0u8; 8];
        word.copy_from_slice(&data[i * 8..i * 8 + 8]);
        let m = u64::from_le_bytes(word);
        v3 ^= m;
        for _ in 0..2 {
            sip_round(&mut v0, &mut v1, &mut v2, &mut v3);
        }
        v0 ^= m;
    }

    let mut last = (len as u64) << 56;
    let remainder = &data[blocks * 8..];
    for (i, &byte) in remainder.iter().enumerate() {
        last |= (byte as u64) << (i * 8);
    }

    v3 ^= last;
    for _ in 0..2 {
        sip_round(&mut v0, &mut v1, &mut v2, &mut v3);
    }
    v0 ^= last;

    v2 ^= 0xff;
    for _ in 0..4 {
        sip_round(&mut v0, &mut v1, &mut v2, &mut v3);
    }

    v0 ^ v1 ^ v2 ^ v3
}

fn sip_round(v0: &mut u64, v1: &mut u64, v2: &mut u64, v3: &mut u64) {
    *v0 = v0.wrapping_add(*v1);
    *v1 = v1.rotate_left(13);
    *v1 ^= *v0;
    *v0 = v0.rotate_left(32);
    *v2 = v2.wrapping_add(*v3);
    *v3 = v3.rotate_left(16);
    *v3 ^= *v2;
    *v0 = v0.wrapping_add(*v3);
    *v3 = v3.rotate_left(21);
    *v3 ^= *v0;
    *v2 = v2.wrapping_add(*v1);
    *v1 = v1.rotate_left(17);
    *v1 ^= *v2;
    *v2 = v2.rotate_left(32);
}

fn hash_name(seed: &[u8; 16], name: &str) -> (u64, u64) {
    let mut seed1 = [0u8; 8];
    let mut seed2 = [0u8; 8];
    seed1.copy_from_slice(&seed[0..8]);
    seed2.copy_from_slice(&seed[8..16]);
    (
        siphash_2_4(name.as_bytes(), seed1),
        siphash_2_4(name.as_bytes(), seed2),
    )
}

fn make_key(seed: &[u8; 16], name: &str, key_type: KeyType) -> Key {
    let (h, hi) = hash_name(seed, name);
    Key {
        name_hash: h,
        name_hash_hi: hi,
        key_type,
    }
}

// --- Leaf value encoding/decoding ---

const MAX_NAME_LEN: usize = 512;

/// A leaf value's type tag *is* its key type, so nothing carries the two
/// separately. They were written out by hand at three sites as `1` and `2`,
/// with `if key_type == File { 1 } else { 2 }` twice — one place for them to
/// drift, and no way to notice.
const _: () = assert!(
    KeyType::File as u8 == 1 && KeyType::Symlink as u8 == 2,
    "decode_leaf_value reads 1 as a file and 2 as a symlink",
);

/// Encode a file/symlink leaf value.
fn encode_leaf_value(
    entry_type: KeyType,
    name: &str,
    size: u64,
    mtime: u64,
    extents: &[Extent],
) -> Vec<u8> {
    let name_bytes = name.as_bytes();
    let name_len = name_bytes.len();
    // 1 (entry_type) + 2 (name_len) + 8 (size) + 8 (mtime) + name + extents
    let extent_bytes = extents.len() * EXTENT_SIZE;
    let total = 1 + 2 + 8 + 8 + name_len + extent_bytes;
    let mut val = vec![0u8; total];

    val[0] = entry_type as u8;
    val[1..3].copy_from_slice(&(name_len as u16).to_le_bytes());
    val[3..11].copy_from_slice(&size.to_le_bytes());
    val[11..19].copy_from_slice(&mtime.to_le_bytes());
    val[19..19 + name_len].copy_from_slice(name_bytes);

    let mut off = 19 + name_len;
    for ext in extents {
        val[off..off + 8].copy_from_slice(&ext.start_block.to_le_bytes());
        val[off + 8..off + 12].copy_from_slice(&ext.block_count.to_le_bytes());
        val[off + 12..off + 16].copy_from_slice(&ext._reserved.to_le_bytes());
        off += EXTENT_SIZE;
    }

    val
}

/// Decoded leaf value with owned strings.
pub enum LeafValue {
    File {
        name: String,
        size: u64,
        mtime: u64,
        extents: Vec<Extent>,
    },
    Symlink {
        name: String,
        size: u64,
        mtime: u64,
        extents: Vec<Extent>,
    },
}

impl LeafValue {
    pub fn name(&self) -> &str {
        match self {
            LeafValue::File { name, .. } => name,
            LeafValue::Symlink { name, .. } => name,
        }
    }

    pub fn size(&self) -> u64 {
        match self {
            LeafValue::File { size, .. } => *size,
            LeafValue::Symlink { size, .. } => *size,
        }
    }

    pub fn mtime(&self) -> u64 {
        match self {
            LeafValue::File { mtime, .. } => *mtime,
            LeafValue::Symlink { mtime, .. } => *mtime,
        }
    }

    pub fn extents(&self) -> &[Extent] {
        match self {
            LeafValue::File { extents, .. } => extents,
            LeafValue::Symlink { extents, .. } => extents,
        }
    }
}

/// Decode a file/symlink leaf value, refusing one that does not fit the volume
/// it was read out of.
///
/// **`volume_blocks` is not decoration, and this is the only place it is
/// applied.** An extent's `start_block` and `block_count` are numbers off the
/// disk, and three things downstream take them as read: the kernel's demand
/// paging turns one into a device block on every page fault
/// (`kernel/src/file_backing.rs`), [`read_extents`] sizes an allocation from
/// the file size beside them, and every delete hands them to
/// [`BitmapAllocator::free_range`] — which turns a block number into a bitmap
/// block to *read, modify and write*. That last one is why the bound is the
/// volume's block count and not the device's: `bit_of` derives the bitmap block
/// as `bitmap_start + block / 32768`, and the superblock guarantees the bitmap
/// covers the volume and nothing beyond it. On a 128-block volume, block 32769
/// lands on byte 0 of block 2 — the root node — and freeing it clears bit 1 of
/// `BTND`'s `B`.
///
/// So the refusals are here, at the one place a leaf value becomes typed data,
/// rather than at each of the sites that consume one.
fn decode_leaf_value(value: &[u8], volume_blocks: u64) -> Result<LeafValue, FsError> {
    if value.len() < 19 {
        return Err(FsError::CorruptedKey(0));
    }

    let entry_type = value[0];
    let name_len = u16::from_le_bytes([value[1], value[2]]) as usize;
    let size = u64::from_le_bytes(value[3..11].try_into().unwrap());
    let mtime = u64::from_le_bytes(value[11..19].try_into().unwrap());

    if 19 + name_len > value.len() {
        return Err(FsError::CorruptedKey(0));
    }

    let name_str = core::str::from_utf8(&value[19..19 + name_len])
        .map_err(|_| FsError::CorruptedKey(0))?;
    let name = String::from(name_str);

    let extent_data = &value[19 + name_len..];
    let extent_count = extent_data.len() / EXTENT_SIZE;
    let mut extents = Vec::with_capacity(extent_count);
    let mut named_blocks = 0u64;
    for i in 0..extent_count {
        let off = i * EXTENT_SIZE;
        let start_block = u64::from_le_bytes(extent_data[off..off + 8].try_into().unwrap());
        let block_count = u32::from_le_bytes(extent_data[off + 8..off + 12].try_into().unwrap());

        // Every block the run names has to be a block of this volume, and the
        // *end* is where that is decided: a start on the last block passes on
        // its own and still runs off the edge.
        let end = start_block.checked_add(block_count as u64);
        if end.is_none_or(|end| end > volume_blocks) {
            return Err(FsError::BlockOffDevice { block: start_block, device_blocks: volume_blocks });
        }

        // And a file cannot hold more blocks than the volume has: every block
        // of a file is one of the volume's, so the total is bounded even where
        // no single run is out of range. Without this, 254 in-range runs of
        // 4 GiB each are what `read_extents` sizes its `Vec` against.
        named_blocks = named_blocks.saturating_add(block_count as u64);
        if named_blocks > volume_blocks {
            return Err(FsError::NotEnoughBlocks { needed: named_blocks, available: volume_blocks });
        }

        extents.push(Extent { start_block, block_count, _reserved: 0 });
    }

    match entry_type {
        1 => Ok(LeafValue::File { name, size, mtime, extents }),
        2 => Ok(LeafValue::Symlink { name, size, mtime, extents }),
        _ => Err(FsError::CorruptedKey(entry_type as u16)),
    }
}

/// Allocate blocks and write `data` into them, returning the extent list.
///
/// The allocator answers with a run that may be shorter than the request, so
/// covering `data` takes a loop — and a run reserved by an earlier turn of that
/// loop is a block the bitmap calls taken that no entry names, once a later
/// turn fails. Every run goes back before the error does.
fn write_data(
    io: &dyn BlockIO,
    alloc: &mut BitmapAllocator,
    data: &[u8],
) -> Result<Vec<Extent>, FsError> {
    if data.is_empty() {
        return Ok(Vec::new());
    }

    let blocks_needed = data.len().div_ceil(BLOCK_SIZE) as u32;
    let mut extents: Vec<Extent> = Vec::new();
    let mut remaining = blocks_needed;
    let mut data_offset = 0usize;

    while remaining > 0 {
        let run = match alloc.alloc_up_to(io, remaining) {
            Ok(run) => run,
            Err(err) => return Err(give_back(io, alloc, &extents, err)),
        };
        push_extent(&mut extents, run.start.raw(), run.len);

        let mut buf = BlockBuf::zeroed();
        for i in 0..run.len as u64 {
            buf.0.fill(0);
            let chunk_end = (data_offset + BLOCK_SIZE).min(data.len());
            if data_offset < data.len() {
                let len = chunk_end - data_offset;
                buf.0[..len].copy_from_slice(&data[data_offset..chunk_end]);
            }
            if let Err(err) = io.write(BlockNum::new(run.start.raw() + i), &buf) {
                return Err(give_back(io, alloc, &extents, err));
            }
            data_offset += BLOCK_SIZE;
        }

        remaining -= run.len;
    }

    Ok(extents)
}

/// Hand back the runs a failed [`write_data`] had already reserved, and return
/// the failure that stopped it.
///
/// Best effort by construction: this runs because something has already gone
/// wrong, and a bitmap write that also fails has no better answer to give than
/// the error already in hand.
fn give_back(
    io: &dyn BlockIO,
    alloc: &mut BitmapAllocator,
    extents: &[Extent],
    err: FsError,
) -> FsError {
    for ext in extents {
        let _ = alloc.free_range(io, BlockNum::new(ext.start_block), ext.block_count);
    }
    err
}

/// Read file data from a list of extents.
///
/// **`size` sizes the `Vec` and `size` is a number off the disk**, so it is
/// held against the volume: a file cannot be longer than the thing it is
/// stored on. Without that line a leaf value declaring `u64::MAX` asked the
/// allocator for `u64::MAX` bytes — in the kernel, from a symlink `read_link`
/// follows, where anything past `mm::MAX_HEAP_ALLOC` is not an `Err` but an
/// assertion.
///
/// **The volume and not the blocks the file names**, which is the tighter
/// bound and the wrong one: a file is allowed to end in a hole. `set_len` past
/// the last block dirties no page, so the flush allocates nothing and records
/// a size the extents do not reach — `fs_truncate_persist` grows a one-page
/// `/home` file to 3 MiB — and the loop below is written for exactly that,
/// leaving the tail as the zeros the hole is.
fn read_extents(
    io: &dyn BlockIO,
    extents: &[Extent],
    size: u64,
    volume_blocks: u64,
) -> Result<Vec<u8>, FsError> {
    let needed_blocks = size.div_ceil(BLOCK_SIZE as u64);
    if needed_blocks > volume_blocks {
        return Err(FsError::NotEnoughBlocks { needed: needed_blocks, available: volume_blocks });
    }

    let mut data = vec![0u8; size as usize];
    let mut offset = 0usize;
    let mut buf = BlockBuf::zeroed();

    for ext in extents {
        for i in 0..ext.block_count as u64 {
            if offset >= size as usize {
                break;
            }
            io.read(BlockNum::new(ext.start_block + i), &mut buf)?;
            let remaining = size as usize - offset;
            let to_copy = remaining.min(BLOCK_SIZE);
            data[offset..offset + to_copy].copy_from_slice(&buf.0[..to_copy]);
            offset += to_copy;
        }
    }

    Ok(data)
}

// --- Formatted (for mkfs / image building) ---

impl<IO: BlockIO> Formatted<IO> {
    /// Format a new filesystem on the given block device.
    pub fn format(io: IO) -> Result<Self, FsError> {
        let block_count = io.block_count();

        // Layout: [superblock(1)] [bitmap] [journal(reserved, 0 for now)] [data...] [sb_backup(1)]
        let bitmap_blocks = block_count.div_ceil(BLOCK_SIZE as u64 * 8);
        let bitmap_start = BlockNum::new(1);
        // Reserve journal space but don't use it in Phase 1
        let journal_start = BlockNum::new(1 + bitmap_blocks);
        let journal_blocks = 0u32; // Phase 2 will set this to 64

        let metadata_blocks = 1 + bitmap_blocks + journal_blocks as u64;

        // Create empty root leaf node
        let root_block_num = metadata_blocks; // first data block is the root node
        let total_metadata = metadata_blocks + 1; // +1 for root node

        let alloc = BitmapAllocator::format(
            &io,
            bitmap_start,
            bitmap_blocks,
            block_count,
            total_metadata,
        )?;

        // An empty node's entries occupy zero bytes, so the only failure
        // `write` has left here is the device's.
        Node::Leaf(Vec::new()).write(&io, BlockNum::new(root_block_num))?;

        // Generate random-ish hash seed from block count (deterministic for reproducible builds)
        let mut hash_seed = [0u8; 16];
        let seed_val = block_count.wrapping_mul(0x517cc1b727220a95);
        hash_seed[0..8].copy_from_slice(&seed_val.to_le_bytes());
        hash_seed[8..16].copy_from_slice(&seed_val.wrapping_mul(0x6c62272e07bb0142).to_le_bytes());

        let sb = Superblock {
            block_count,
            root_node: BlockNum::new(root_block_num),
            next_alloc: total_metadata,
            free_blocks: alloc.free_blocks,
            bitmap_start,
            bitmap_blocks,
            journal_start,
            journal_blocks,
            journal_head: 0,
            flags: 0, // not clean until sync
            hash_seed,
        };

        sb.write(&io)?;

        Ok(Self { io, sb, alloc })
    }

    /// Create a file on the formatted filesystem (used during mkfs).
    pub fn create(&mut self, name: &str, data: &[u8], mtime: u64) -> Result<(), FsError> {
        if name.is_empty() || name.len() > MAX_NAME_LEN {
            return Err(FsError::NameTooLong { len: name.len(), max: MAX_NAME_LEN });
        }

        let extents = write_data(&self.io, &mut self.alloc, data)?;
        let value = encode_leaf_value(KeyType::File, name, data.len() as u64, mtime, &extents);
        let key = make_key(&self.sb.hash_seed, name, KeyType::File);
        let entry = Entry { key, value };

        self.sb.root_node = btree::insert(&self.io, &mut self.alloc, self.sb.root_node, entry)?;

        Ok(())
    }

    /// Create a symlink on the formatted filesystem.
    pub fn create_symlink(&mut self, name: &str, target: &str, mtime: u64) -> Result<(), FsError> {
        if name.is_empty() || name.len() > MAX_NAME_LEN {
            return Err(FsError::NameTooLong { len: name.len(), max: MAX_NAME_LEN });
        }

        let target_bytes = target.as_bytes();
        let extents = write_data(&self.io, &mut self.alloc, target_bytes)?;
        let value = encode_leaf_value(KeyType::Symlink, name, target_bytes.len() as u64, mtime, &extents);
        let key = make_key(&self.sb.hash_seed, name, KeyType::Symlink);
        let entry = Entry { key, value };

        self.sb.root_node = btree::insert(&self.io, &mut self.alloc, self.sb.root_node, entry)?;

        Ok(())
    }

    /// Finalize the filesystem: write superblock with clean flag.
    pub fn sync(&mut self) -> Result<(), FsError> {
        self.sb.free_blocks = self.alloc.free_blocks;
        self.sb.next_alloc = self.alloc.next_alloc;
        self.sb.set_clean(true);
        self.sb.write(&self.io)?;
        self.io.flush()
    }

    /// Mount this formatted filesystem for read-write access.
    pub fn mount(self) -> Mounted<IO, ReadWrite> {
        Mounted {
            io: self.io,
            sb: self.sb,
            alloc: self.alloc,
            _mode: PhantomData,
        }
    }

    /// Mount this formatted filesystem for read-only access.
    pub fn mount_readonly(self) -> Mounted<IO, ReadOnly> {
        Mounted {
            io: self.io,
            sb: self.sb,
            alloc: self.alloc,
            _mode: PhantomData,
        }
    }

    /// Consume and return the underlying IO (for extracting the image bytes).
    pub fn into_io(mut self) -> Result<IO, FsError> {
        self.sync()?;
        Ok(self.io)
    }
}

// --- Mounted (read operations, available for both ReadOnly and ReadWrite) ---

impl<IO: BlockIO, Mode> Mounted<IO, Mode> {
    /// Open an existing filesystem from disk.
    pub fn open(io: IO) -> Result<Mounted<IO, Mode>, FsError> {
        let sb = Superblock::read(&io)?;
        let alloc = BitmapAllocator {
            bitmap_start: sb.bitmap_start,
            bitmap_blocks: sb.bitmap_blocks,
            total_blocks: sb.block_count,
            free_blocks: sb.free_blocks,
            next_alloc: sb.next_alloc,
        };
        Ok(Mounted {
            io,
            sb,
            alloc,
            _mode: PhantomData,
        })
    }

    /// Decode a leaf value against *this volume's* block count.
    ///
    /// The one way a leaf value is decoded inside a mount, so the bound cannot
    /// be forgotten at one of the dozen sites that ask for one. `sb.block_count`
    /// and not `io.block_count()`: the volume is what the bitmap is sized from,
    /// and `Superblock::check` has already refused a volume larger than its
    /// device.
    fn decode(&self, value: &[u8]) -> Result<LeafValue, FsError> {
        decode_leaf_value(value, self.sb.block_count)
    }

    /// Find a file entry by name. Tries File key first, then Symlink.
    fn find_by_name(&self, name: &str) -> Result<Option<(Key, Vec<u8>)>, FsError> {
        // Try as File first (most common)
        let key = make_key(&self.sb.hash_seed, name, KeyType::File);
        if let Some(value) = btree::search(&self.io, self.sb.root_node, &key)? {
            let leaf = self.decode(&value)?;
            if leaf.name() == name {
                return Ok(Some((key, value)));
            }
        }

        // Try as Symlink
        let key = make_key(&self.sb.hash_seed, name, KeyType::Symlink);
        if let Some(value) = btree::search(&self.io, self.sb.root_node, &key)? {
            let leaf = self.decode(&value)?;
            if leaf.name() == name {
                return Ok(Some((key, value)));
            }
        }

        Ok(None)
    }

    /// Read a file's contents by name.
    pub fn read_file(&self, name: &str) -> Result<Vec<u8>, FsError> {
        let (_, value) = self.find_by_name(name)?.ok_or(FsError::NotFound)?;
        let leaf = self.decode(&value)?;
        match leaf {
            LeafValue::File { size, extents, .. } | LeafValue::Symlink { size, extents, .. } => {
                read_extents(&self.io, &extents, size, self.sb.block_count)
            }
        }
    }

    /// Read a symlink's target by name. `None` when the name is not a symlink.
    pub fn read_link(&self, name: &str) -> Result<Option<String>, FsError> {
        let Some((_, value)) = self.find_by_name(name)? else { return Ok(None) };
        match self.decode(&value)? {
            LeafValue::Symlink { size, extents, .. } => {
                let data = read_extents(&self.io, &extents, size, self.sb.block_count)?;
                Ok(String::from_utf8(data).ok())
            }
            _ => Ok(None),
        }
    }

    /// Modification time of a file. `None` when nothing answers to the name.
    pub fn file_mtime(&self, name: &str) -> Result<Option<u64>, FsError> {
        Ok(self.leaf(name)?.map(|leaf| leaf.mtime()))
    }

    /// The decoded entry `name` answers to, if any.
    fn leaf(&self, name: &str) -> Result<Option<LeafValue>, FsError> {
        match self.find_by_name(name)? {
            Some((_, value)) => Ok(Some(self.decode(&value)?)),
            None => Ok(None),
        }
    }

    /// List all files. Returns (name, size) pairs.
    pub fn list(&self) -> Result<Vec<(String, u64)>, FsError> {
        let entries = btree::collect_all(&self.io, self.sb.root_node)?;
        let mut result = Vec::new();
        for entry in &entries {
            if let Ok(leaf) = self.decode(&entry.value) {
                result.push((String::from(leaf.name()), leaf.size()));
            }
        }
        Ok(result)
    }

    /// Convert back to Formatted state (for testing — insert more files after reading).
    pub fn into_formatted(self) -> Formatted<IO> {
        Formatted {
            io: self.io,
            sb: self.sb,
            alloc: self.alloc,
        }
    }

    /// Return the extents and file size for a file.
    /// Used by the kernel to construct a FileBacking for demand-paged loading.
    pub fn file_extents(&self, name: &str) -> Result<Option<(Vec<Extent>, u64)>, FsError> {
        Ok(self.leaf(name)?.map(|leaf| (leaf.extents().to_vec(), leaf.size())))
    }

    /// Check if a name is a symlink.
    pub fn is_symlink(&self, name: &str) -> Result<bool, FsError> {
        Ok(self.find_by_name(name)?.is_some_and(|(key, _)| key.key_type == KeyType::Symlink))
    }

}

// --- ReadWrite-only operations ---

impl<IO: BlockIO> Mounted<IO, ReadWrite> {
    /// Create a file, replacing whatever answered to `name`.
    pub fn create(&mut self, name: &str, data: &[u8], mtime: u64) -> Result<(), FsError> {
        self.put(name, KeyType::File, data, mtime)
    }

    /// Create a symlink, replacing whatever answered to `name`.
    pub fn create_symlink(&mut self, name: &str, target: &str) -> Result<(), FsError> {
        self.put(name, KeyType::Symlink, target.as_bytes(), 0)
    }

    /// Put `name` on the volume, displacing whatever answered to it.
    ///
    /// The new entry goes in before the old one comes out, for the reason
    /// `rename` does it: freeing first is the old file destroyed when
    /// `write_data` or `btree::insert` then fails, and a full volume is a
    /// failure any caller can provoke. Measured before it did: on a 64-block
    /// volume, a 5-block file overwritten by a 400-block one left the volume
    /// empty and the 5 blocks unreachable.
    ///
    /// What that costs is that the replacement's blocks and the original's are
    /// both allocated at once, so an overwrite on a nearly full volume can now
    /// fail where it used to succeed. An error is the cheaper half of that
    /// trade.
    fn put(
        &mut self,
        name: &str,
        key_type: KeyType,
        data: &[u8],
        mtime: u64,
    ) -> Result<(), FsError> {
        if name.is_empty() || name.len() > MAX_NAME_LEN {
            return Err(FsError::NameTooLong { len: name.len(), max: MAX_NAME_LEN });
        }

        let displaced = match self.find_by_name(name)? {
            Some((key, value)) => Some((key, self.decode(&value)?.extents().to_vec())),
            None => None,
        };

        let extents = write_data(&self.io, &mut self.alloc, data)?;
        let value = encode_leaf_value(key_type, name, data.len() as u64, mtime, &extents);
        let key = make_key(&self.sb.hash_seed, name, key_type);
        self.sb.root_node = btree::insert(
            &self.io, &mut self.alloc,
            self.sb.root_node,
            Entry { key, value },
        )?;

        self.retire_displaced(displaced, key)
    }

    /// Remove the entry the insert of `new_key` did not replace, and free the
    /// blocks of whatever answered to that name before.
    ///
    /// The insert replaces the destination only where the two keys agree. A
    /// file written over a symlink keys differently, and the entry left behind
    /// would answer to the name forever with blocks nothing could reach.
    fn retire_displaced(
        &mut self,
        displaced: Option<(Key, Vec<Extent>)>,
        new_key: Key,
    ) -> Result<(), FsError> {
        let Some((old_key, old_extents)) = displaced else { return Ok(()) };
        if old_key != new_key {
            btree::delete(&self.io, self.sb.root_node, &old_key)?;
        }
        for ext in &old_extents {
            self.alloc.free_range(&self.io, BlockNum::new(ext.start_block), ext.block_count)?;
        }
        Ok(())
    }

    /// Delete a file or symlink by name. Returns true if found and deleted.
    pub fn delete(&mut self, name: &str) -> Result<bool, FsError> {
        self.delete_by_name(name)
    }

    /// Delete all entries whose name starts with the given prefix.
    pub fn delete_prefix(&mut self, prefix: &str) -> Result<(), FsError> {
        let entries = btree::collect_all(&self.io, self.sb.root_node)?;

        for entry in &entries {
            // An entry that does not decode cannot be matched against the
            // prefix, so it is not one of the entries this was asked to remove.
            let Ok(leaf) = self.decode(&entry.value) else { continue };
            if !leaf.name().starts_with(prefix) {
                continue;
            }
            // Remove first, free second, and free nothing when the removal did
            // not happen: an entry that survives still names its blocks, and
            // handing them to the next file gives two entries one block.
            //
            // `collect_all` visits every child; a descent takes the one path
            // `find_child` chooses. In a tree whose child keys agree with the
            // keys beneath them those two find the same entries, so a removal
            // that comes back empty is the disk contradicting itself.
            if btree::delete(&self.io, self.sb.root_node, &entry.key)?.is_none() {
                return Err(FsError::CorruptedNode(self.sb.root_node));
            }
            for ext in leaf.extents() {
                self.alloc.free_range(&self.io, BlockNum::new(ext.start_block), ext.block_count)?;
            }
        }
        Ok(())
    }

    /// Sync filesystem state to disk.
    pub fn sync(&mut self) -> Result<(), FsError> {
        self.sb.free_blocks = self.alloc.free_blocks;
        self.sb.next_alloc = self.alloc.next_alloc;
        self.sb.set_clean(true);
        self.sb.write(&self.io)?;
        self.io.flush()
    }

    /// Delete a file/symlink by name, freeing its data blocks. Returns true if found.
    ///
    /// `find_by_name` answers the "is this the entry we mean?" question, which
    /// used to be asked after the removal: `btree::delete` took the entry out
    /// and *then* the decoded name was compared, so a key collision destroyed
    /// an unrelated file, leaked its blocks and returned `false` — telling the
    /// caller nothing had happened. It also answers it once for both key
    /// types, where the old shape fell through from File to Symlink after a
    /// non-matching removal and could take two entries out in one call.
    fn delete_by_name(&mut self, name: &str) -> Result<bool, FsError> {
        let Some((key, value)) = self.find_by_name(name)? else { return Ok(false) };
        let extents = self.decode(&value)?.extents().to_vec();

        // `find_by_name` reached this key by the descent `btree::delete` is
        // about to repeat, so an empty removal is not "no such file" — it is a
        // tree that answers two ways.
        if btree::delete(&self.io, self.sb.root_node, &key)?.is_none() {
            return Err(FsError::CorruptedNode(self.sb.root_node));
        }
        for ext in &extents {
            self.alloc.free_range(&self.io, BlockNum::new(ext.start_block), ext.block_count)?;
        }
        Ok(true)
    }

    /// Rename a file or symlink.
    ///
    /// The new entry goes in before the old one comes out, so a crash between
    /// the two leaves the file under both names rather than under neither. What
    /// that ordering costs is that the insert *is* the removal of whatever
    /// `new_name` named — same name and same type is the same key, and
    /// `btree::insert` replaces on an equal key — so the displaced entry has to
    /// be read out of the tree before the insert. Asking for it afterwards, by
    /// name, answers with the file that was just renamed and frees its extents.
    pub fn rename(&mut self, old_name: &str, new_name: &str) -> Result<(), FsError> {
        // Every other name-taking entry point bounds its name; this one did
        // not, and `user_ptr::MAX_USER_STR` lets 64 KiB of it through.
        if new_name.is_empty() || new_name.len() > MAX_NAME_LEN {
            return Err(FsError::NameTooLong { len: new_name.len(), max: MAX_NAME_LEN });
        }

        let (old_key, old_value) = self.find_by_name(old_name)?
            .ok_or(FsError::NotFound)?;
        let leaf = self.decode(&old_value)?;
        let new_key = make_key(&self.sb.hash_seed, new_name, old_key.key_type);

        // What `new_name` names now. `find_by_name` matches on the decoded
        // name, so an entry answering to both names is one entry — a rename
        // onto itself, with nothing to displace and nothing to free.
        let displaced = match self.find_by_name(new_name)? {
            Some((key, _)) if key == old_key => None,
            Some((key, value)) => Some((key, self.decode(&value)?.extents().to_vec())),
            None => None,
        };

        let new_value = encode_leaf_value(
            old_key.key_type, new_name, leaf.size(), leaf.mtime(), leaf.extents(),
        );
        self.sb.root_node = btree::insert(
            &self.io, &mut self.alloc,
            self.sb.root_node,
            Entry { key: new_key, value: new_value },
        )?;

        self.retire_displaced(displaced, new_key)?;

        // The source's blocks stay allocated: the new entry holds the same
        // extent list. Nothing to delete when the two names share a key — the
        // entry under it is the one the insert just wrote.
        if new_key != old_key {
            btree::delete(&self.io, self.sb.root_node, &old_key)?;
        }

        Ok(())
    }

    /// Update file metadata (size, mtime, extents) without rewriting data.
    pub fn update_metadata(
        &mut self,
        name: &str,
        new_extents: &[Extent],
        size: u64,
        mtime: u64,
    ) -> Result<(), FsError> {
        let (old_key, old_value) = self.find_by_name(name)?
            .ok_or(FsError::NotFound)?;
        let leaf = self.decode(&old_value)?;

        let extents = if new_extents.is_empty() { leaf.extents() } else { new_extents };
        let new_value = encode_leaf_value(old_key.key_type, leaf.name(), size, mtime, extents);
        let new_entry = Entry { key: old_key, value: new_value };

        // No delete first. The key is unchanged and `btree::insert` replaces on
        // an equal key, so the delete bought nothing and cost the file: a
        // pre-check for `EntryTooLarge` does not cover `insert`'s other
        // rejection, a split with no free block to split into, and that one
        // left the entry deleted and never put back. Blocks the caller drops
        // from the extent list are still leaked.
        self.sb.root_node = btree::insert(
            &self.io, &mut self.alloc,
            self.sb.root_node,
            new_entry,
        )?;
        Ok(())
    }

    /// Resolve a page index to a block number, allocating blocks to reach it.
    ///
    /// The allocator answers with a run that may be shorter than the request,
    /// so covering a page means looping until the extents reach it — the same
    /// loop `write_data` has always had. Reading the short run as a complete
    /// one returned a block past the end of the extent that had just been
    /// recorded: the page's write went to a block belonging to another file,
    /// and a later read of the same page resolved somewhere else again.
    pub fn resolve_or_alloc_block(
        &mut self,
        extents: &mut Vec<Extent>,
        page_idx: u32,
    ) -> Result<u64, FsError> {
        if let Some(block) = block_for(extents, page_idx) {
            return Ok(block);
        }

        let target = page_idx as u64;
        let mut covered: u64 = extents.iter().map(|e| e.block_count as u64).sum();
        while covered <= target {
            let want = (target - covered + 1).min(u32::MAX as u64) as u32;
            let run = self.alloc.alloc_up_to(&self.io, want)?;
            push_extent(extents, run.start.raw(), run.len);
            covered += run.len as u64;
        }

        block_for(extents, page_idx).ok_or(FsError::NotFound)
    }
}

/// The block holding `page_idx`, if the extents already reach that far.
///
/// The one definition of where a page lives, used both to answer a resolve and
/// to answer it again after allocating — so an allocation that came up short
/// cannot produce a block the lookup would not agree with.
///
/// Total arithmetic, for the reason [`push_extent`]'s is: the list walked here
/// can be one a disk wrote, and a `+` that wraps is a panic rather than a
/// wrong answer. `None` is already "the extents do not reach that page", which
/// is what a list whose own arithmetic does not close is.
fn block_for(extents: &[Extent], page_idx: u32) -> Option<u64> {
    let mut cursor = 0u64;
    for ext in extents {
        let end = cursor.checked_add(ext.block_count as u64)?;
        if (page_idx as u64) < end {
            return ext.start_block.checked_add(page_idx as u64 - cursor);
        }
        cursor = end;
    }
    None
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
    use crate::block_io::VecBlockIO;
    use crate::btree::NODE_MAGIC;
    use crate::crc32c::crc32c;

    /// A real volume with a real file on it, as raw bytes to be tampered with.
    ///
    /// Every crafted block below is resealed with the checksum the format asks
    /// for, so the parser accepts all of them as authentic. That is the whole
    /// point: whoever writes the image writes the CRC.
    fn image(blocks: u64) -> Vec<u8> {
        let mut fs = Formatted::format(VecBlockIO::new(blocks)).expect("format");
        fs.create("victim.txt", b"a file that was already here", 1).expect("create");
        fs.into_io().expect("sync").into_vec()
    }

    fn seal_node(raw: &mut [u8], block: u64) {
        let at = block as usize * BLOCK_SIZE;
        let crc = crc32c(&raw[at + crate::btree::CRC_START..at + BLOCK_SIZE]);
        raw[at + 4..at + 8].copy_from_slice(&crc.to_le_bytes());
    }

    fn seal_superblock(raw: &mut [u8], block: u64) {
        let at = block as usize * BLOCK_SIZE;
        let crc = crc32c(&raw[at + Superblock::CRC_START..at + BLOCK_SIZE]);
        raw[at + 8..at + 12].copy_from_slice(&crc.to_le_bytes());
    }

    /// Patch a little-endian `u64` into both copies of the superblock.
    fn patch_superblock(raw: &mut [u8], blocks: u64, off: usize, val: u64) {
        for sb_block in [0, blocks - 1] {
            let at = sb_block as usize * BLOCK_SIZE;
            raw[at + off..at + off + 8].copy_from_slice(&val.to_le_bytes());
            seal_superblock(raw, sb_block);
        }
    }

    /// Point both superblocks at `root`, and set the depth field to its maximum
    /// while we are here — that field is what three recursions used to descend
    /// on, and a disk saying 65535 is the shape that killed the kernel.
    fn set_root(raw: &mut [u8], blocks: u64, root: u64) {
        for sb_block in [0, blocks - 1] {
            let at = sb_block as usize * BLOCK_SIZE;
            raw[at + 24..at + 32].copy_from_slice(&root.to_le_bytes());
            raw[at + 32..at + 34].copy_from_slice(&u16::MAX.to_le_bytes());
            seal_superblock(raw, sb_block);
        }
    }

    /// An interior node at `block` with one child pointer, `value_len` bytes
    /// wide, naming `child`.
    fn craft_interior(raw: &mut [u8], block: u64, child: u64, value_len: u32) {
        let at = block as usize * BLOCK_SIZE;
        raw[at..at + BLOCK_SIZE].fill(0);
        raw[at..at + 4].copy_from_slice(&NODE_MAGIC);
        raw[at + 8..at + 10].copy_from_slice(&1u16.to_le_bytes());
        raw[at + 10..at + 12].copy_from_slice(&1u16.to_le_bytes());

        let entry = at + 32;
        raw[entry + 18..entry + 22].copy_from_slice(&value_len.to_le_bytes());
        let value = entry + 24;
        let bytes = child.to_le_bytes();
        let n = (value_len as usize).min(bytes.len());
        raw[value..value + n].copy_from_slice(&bytes[..n]);
        seal_node(raw, block);
    }

    fn mount(raw: Vec<u8>) -> Result<Mounted<VecBlockIO, ReadOnly>, FsError> {
        Mounted::<_, ReadOnly>::open(VecBlockIO::from_vec(raw))
    }

    fn mount_rw(raw: Vec<u8>) -> Result<Mounted<VecBlockIO, ReadWrite>, FsError> {
        Mounted::<_, ReadWrite>::open(VecBlockIO::from_vec(raw))
    }

    fn read_u64_at(raw: &[u8], off: usize) -> u64 {
        u64::from_le_bytes(raw[off..off + 8].try_into().unwrap())
    }

    /// A leaf with no entries. Legal, and a descent that reaches it answers
    /// "not here" for every key there is.
    fn craft_empty_leaf(raw: &mut [u8], block: u64) {
        let at = block as usize * BLOCK_SIZE;
        raw[at..at + BLOCK_SIZE].fill(0);
        raw[at..at + 4].copy_from_slice(&NODE_MAGIC);
        seal_node(raw, block);
    }

    /// An interior node at `block` whose children are `(key, block)` in order.
    fn craft_children(raw: &mut [u8], block: u64, children: &[(Key, u64)]) {
        let at = block as usize * BLOCK_SIZE;
        raw[at..at + BLOCK_SIZE].fill(0);
        raw[at..at + 4].copy_from_slice(&NODE_MAGIC);
        raw[at + 8..at + 10].copy_from_slice(&1u16.to_le_bytes());
        raw[at + 10..at + 12].copy_from_slice(&(children.len() as u16).to_le_bytes());
        for (i, (key, child)) in children.iter().enumerate() {
            let entry = at + 32 + i * 32;
            raw[entry..entry + 8].copy_from_slice(&key.name_hash.to_le_bytes());
            raw[entry + 8..entry + 16].copy_from_slice(&key.name_hash_hi.to_le_bytes());
            raw[entry + 16..entry + 18].copy_from_slice(&(key.key_type as u16).to_le_bytes());
            raw[entry + 18..entry + 22].copy_from_slice(&8u32.to_le_bytes());
            raw[entry + 24..entry + 32].copy_from_slice(&child.to_le_bytes());
        }
        seal_node(raw, block);
    }

    fn mark_used(raw: &mut [u8], bitmap_block: u64, blocks: &[u64]) {
        let at = bitmap_block as usize * BLOCK_SIZE;
        for &b in blocks {
            raw[at + (b / 8) as usize] |= 1 << (b % 8);
        }
    }

    /// Rewrite the root leaf as one `victim.txt` entry with the type, size and
    /// extents given, and reseal it.
    ///
    /// The key bytes are left exactly as [`image`] wrote them, so the name
    /// still hashes to this entry and every lookup reaches the crafted value.
    /// The extents go in raw, as `(start_block, block_count)` — that is the
    /// whole point: these are the numbers a disk gets to choose.
    fn craft_entry(raw: &mut [u8], root: u64, entry_type: u8, size: u64, extents: &[(u64, u32)]) {
        const NAME: &[u8] = b"victim.txt";
        let at = root as usize * BLOCK_SIZE;
        let entry = at + 32;
        let value = entry + 24;
        let val_len = 19 + NAME.len() + extents.len() * EXTENT_SIZE;
        assert!(
            value - at + val_len <= BLOCK_SIZE,
            "a {val_len}-byte value does not fit the block this crafts",
        );

        // Level 0, one entry: a leaf.
        raw[at + 8..at + 10].copy_from_slice(&0u16.to_le_bytes());
        raw[at + 10..at + 12].copy_from_slice(&1u16.to_le_bytes());
        raw[entry + 18..entry + 22].copy_from_slice(&(val_len as u32).to_le_bytes());
        raw[value..at + BLOCK_SIZE].fill(0);

        raw[value] = entry_type;
        raw[value + 1..value + 3].copy_from_slice(&(NAME.len() as u16).to_le_bytes());
        raw[value + 3..value + 11].copy_from_slice(&size.to_le_bytes());
        raw[value + 11..value + 19].copy_from_slice(&1u64.to_le_bytes());
        raw[value + 19..value + 19 + NAME.len()].copy_from_slice(NAME);

        let mut off = value + 19 + NAME.len();
        for (start_block, block_count) in extents {
            raw[off..off + 8].copy_from_slice(&start_block.to_le_bytes());
            raw[off + 8..off + 12].copy_from_slice(&block_count.to_le_bytes());
            off += EXTENT_SIZE;
        }
        seal_node(raw, root);
    }

    #[test]
    fn a_delete_of_a_name_that_is_not_here_destroys_nothing() {
        // The stored entry keeps its value — the name inside it is still
        // victim.txt — and answers to the key `ghost` hashes to. That is the
        // 2^-128 collision, staged rather than waited for.
        let blocks = 128;
        let mut raw = image(blocks);
        let mut seed = [0u8; 16];
        seed.copy_from_slice(&raw[90..106]);
        let root = read_u64_at(&raw, 24);
        let (h, hi) = hash_name(&seed, "ghost");

        let entry = root as usize * BLOCK_SIZE + 32;
        raw[entry..entry + 8].copy_from_slice(&h.to_le_bytes());
        raw[entry + 8..entry + 16].copy_from_slice(&hi.to_le_bytes());
        seal_node(&mut raw, root);

        let mut fs = mount_rw(raw).expect("mount");
        assert!(
            fs.list().expect("list").iter().any(|(n, _)| n == "victim.txt"),
            "the craft did not leave victim.txt on the volume",
        );

        assert!(!fs.delete("ghost").expect("delete"), "nothing on this volume is named ghost");

        assert!(
            fs.list().expect("list").iter().any(|(n, _)| n == "victim.txt"),
            "deleting a name that does not exist destroyed the entry it collided with",
        );
    }

    #[test]
    fn a_delete_prefix_that_removed_nothing_frees_nothing() {
        // The tree's shape is on the disk, so a child key can disagree with
        // the keys below it. `collect_all` visits every child and finds the
        // entry; a descent takes one path and does not. The entry survives —
        // and must keep its blocks, or the allocator hands them to the next
        // file while something still points at them.
        let blocks = 128;
        let raw = image(blocks);
        let victim: Vec<u64> = mount(raw.clone())
            .expect("mount")
            .file_extents("victim.txt")
            .expect("file_extents")
            .expect("victim.txt is on the volume")
            .0
            .iter()
            .map(|e| e.start_block)
            .collect();

        let mut raw = raw;
        let leaf = read_u64_at(&raw, 24);
        let (empty_leaf, new_root) = (leaf + 2, leaf + 3);
        craft_empty_leaf(&mut raw, empty_leaf);
        craft_children(
            &mut raw,
            new_root,
            &[
                (Key::ZERO, empty_leaf),
                (Key { name_hash: u64::MAX, name_hash_hi: u64::MAX, key_type: KeyType::Symlink }, leaf),
            ],
        );
        mark_used(&mut raw, 1, &[empty_leaf, new_root]);
        let free = read_u64_at(&raw, 44) - 2;
        patch_superblock(&mut raw, blocks, 44, free);
        patch_superblock(&mut raw, blocks, 36, new_root + 1);
        patch_superblock(&mut raw, blocks, 24, new_root);

        let mut fs = mount_rw(raw).expect("mount");
        assert!(
            fs.list().expect("list").iter().any(|(n, _)| n == "victim.txt"),
            "the craft did not leave victim.txt on the volume",
        );
        assert!(
            fs.find_by_name("victim.txt").expect("search").is_none(),
            "the craft is not the shape under test: a descent still reaches victim.txt",
        );

        let free_before = fs.alloc.free_blocks;
        match fs.delete_prefix("victim") {
            Err(FsError::CorruptedNode(_)) => {}
            other => panic!("expected CorruptedNode, got {other:?}"),
        }
        assert_eq!(
            fs.alloc.free_blocks, free_before,
            "delete_prefix freed the blocks of an entry it did not remove",
        );

        fs.create("other.bin", &[0xAA; BLOCK_SIZE], 0).expect("create");
        let (other, _) = fs.file_extents("other.bin").expect("file_extents").expect("other.bin");
        for ext in &other {
            for i in 0..ext.block_count as u64 {
                assert!(
                    !victim.contains(&(ext.start_block + i)),
                    "other.bin was given block {} — victim.txt still names it",
                    ext.start_block + i,
                );
            }
        }
    }

    #[test]
    fn a_root_that_points_at_itself_is_refused_not_followed() {
        let blocks = 128;
        let mut raw = image(blocks);
        craft_interior(&mut raw, 3, 3, 8);
        set_root(&mut raw, blocks, 3);

        let fs = mount(raw).expect("the volume still describes its device");
        match fs.list() {
            Err(FsError::TreeTooDeep(_)) => {}
            other => panic!("expected TreeTooDeep, got {other:?}"),
        }
    }

    #[test]
    fn an_interior_value_too_short_for_a_block_number_is_refused() {
        let blocks = 128;
        let mut raw = image(blocks);
        craft_interior(&mut raw, 3, 4, 4);
        set_root(&mut raw, blocks, 3);

        let fs = mount(raw).expect("mount");
        match fs.list() {
            Err(FsError::CorruptedNode(_)) => {}
            other => panic!("expected CorruptedNode, got {other:?}"),
        }
    }

    #[test]
    fn a_child_pointer_past_the_end_of_the_device_is_refused() {
        let blocks = 128;
        let mut raw = image(blocks);
        craft_interior(&mut raw, 3, u64::MAX, 8);
        set_root(&mut raw, blocks, 3);

        let fs = mount(raw).expect("mount");
        match fs.list() {
            Err(FsError::BlockOffDevice { .. }) => {}
            other => panic!("expected BlockOffDevice, got {other:?}"),
        }
    }

    #[test]
    fn a_superblock_claiming_more_blocks_than_the_device_has_is_refused() {
        let blocks = 128;
        let mut raw = image(blocks);
        patch_superblock(&mut raw, blocks, 12, 1 << 40);

        match mount(raw) {
            Err(FsError::BadSuperblock { field }) => assert_eq!(field, "block_count"),
            Err(other) => panic!("expected BadSuperblock, got {other:?}"),
            Ok(_) => panic!("mounted a superblock describing a device 8192 times this one"),
        }
    }

    #[test]
    fn a_bitmap_outside_the_volume_is_refused() {
        // `set_used`, `set_free` and `is_free` all compute
        // `bitmap_start + byte_idx / 4096` and check it against nothing, so a
        // mounted volume with this field wrong writes its bitmap over whatever
        // is at those blocks.
        let blocks = 128;
        let mut raw = image(blocks);
        patch_superblock(&mut raw, blocks, 52, 1 << 40);

        match mount(raw) {
            Err(FsError::BadSuperblock { field }) => assert_eq!(field, "bitmap_start"),
            Err(other) => panic!("expected BadSuperblock, got {other:?}"),
            Ok(_) => panic!("mounted a volume whose bitmap is not on the device"),
        }
    }

    #[test]
    fn a_bitmap_too_small_to_cover_the_volume_is_refused() {
        // One bit per block is not negotiable: a shorter bitmap means blocks
        // whose free/used state is read out of whatever follows it.
        let blocks = 128;
        let mut raw = image(blocks);
        patch_superblock(&mut raw, blocks, 60, 0);

        match mount(raw) {
            Err(FsError::BadSuperblock { field }) => assert_eq!(field, "bitmap_blocks"),
            Err(other) => panic!("expected BadSuperblock, got {other:?}"),
            Ok(_) => panic!("mounted a volume with no bitmap at all"),
        }
    }

    #[test]
    fn an_untampered_volume_still_mounts_and_reads() {
        let blocks = 128;
        let fs = mount(image(blocks)).expect("a volume this crate wrote must mount");
        assert_eq!(fs.read_file("victim.txt").expect("read"), b"a file that was already here");
    }

    #[test]
    fn an_extent_past_the_end_of_the_volume_is_refused() {
        // A run that *begins* on the last block and ends one past it. The
        // start alone is in range, which is why the comparison is on the end.
        let blocks = 128;
        let mut raw = image(blocks);
        let root = read_u64_at(&raw, 24);
        craft_entry(&mut raw, root, 1, 2 * BLOCK_SIZE as u64, &[(blocks - 1, 2)]);

        let fs = mount(raw).expect("mount");
        match fs.read_file("victim.txt") {
            Err(FsError::BlockOffDevice { block, device_blocks }) => {
                assert_eq!((block, device_blocks), (blocks - 1, blocks));
            }
            // Never `{other:?}` over a read's `Ok`: an unrefused one holds
            // whatever the crafted size asked for, and the panic message is
            // where that lands — 2 GB of it, measured, the first time.
            Err(other) => panic!("expected BlockOffDevice, got {other:?}"),
            Ok(data) => panic!("a run ending past the volume was read: {} bytes", data.len()),
        }
    }

    #[test]
    fn a_run_ending_on_the_volumes_last_block_is_accepted() {
        // The other side of the same comparison: a file whose run ends exactly
        // at the volume's end is a legal file, and a bound that refuses it is
        // an off-by-one nothing else would catch.
        let blocks = 128;
        let mut raw = image(blocks);
        let root = read_u64_at(&raw, 24);
        craft_entry(&mut raw, root, 1, BLOCK_SIZE as u64, &[(blocks - 1, 1)]);

        let fs = mount(raw).expect("mount");
        let data = fs.read_file("victim.txt").expect("a run of the last block is on the volume");
        assert_eq!(data.len(), BLOCK_SIZE);
    }

    #[test]
    fn a_file_naming_more_blocks_than_the_volume_has_is_refused() {
        // Every run here is inside the volume; there are simply too many of
        // them. This is the bound that keeps `read_extents`' `Vec` under the
        // volume's own size — without it, 254 in-range runs of 4 GiB each are
        // what a declared size is measured against.
        let blocks = 128;
        let mut raw = image(blocks);
        let root = read_u64_at(&raw, 24);
        let extents: Vec<(u64, u32)> = (0..64).map(|_| (3u64, 4u32)).collect();
        craft_entry(&mut raw, root, 1, BLOCK_SIZE as u64, &extents);

        let fs = mount(raw).expect("mount");
        match fs.read_file("victim.txt") {
            Err(FsError::NotEnoughBlocks { needed, available }) => {
                // 33 runs of 4 is the first total past 128.
                assert_eq!((needed, available), (132, blocks));
            }
            Err(other) => panic!("expected NotEnoughBlocks, got {other:?}"),
            Ok(data) => panic!(
                "a file naming 256 blocks of a {blocks}-block volume was read: {} bytes",
                data.len(),
            ),
        }
    }

    #[test]
    fn a_size_longer_than_the_volume_never_reaches_the_allocator() {
        // The kernel-reachable shape: `read_link` is on the adapter, and the
        // target it returns is `read_extents(size)` with a `size` that came off
        // the disk. A gibibyte is small enough that a host allocator hands it
        // over and 500 times what the kernel's asserts on
        // (`mm::MAX_HEAP_ALLOC`, 2_093_056), so the peak allocation is the
        // instrument here and the return value is not.
        let blocks = 128;
        let mut raw = image(blocks);
        let root = read_u64_at(&raw, 24);
        let size = 1u64 << 30;
        craft_entry(&mut raw, root, 2, size, &[(3, 1)]);

        let fs = mount(raw).expect("mount");
        crate::alloc_probe::take_peak();
        let target = fs.read_link("victim.txt");
        let peak = crate::alloc_probe::take_peak();

        // The allocation is asserted first: it is the harm, and it happened
        // before there was a return value to look at.
        assert!(
            peak <= BLOCK_SIZE,
            "a symlink declaring {size} bytes asked the allocator for {peak}",
        );
        match target {
            Err(FsError::NotEnoughBlocks { needed, available }) => {
                assert_eq!((needed, available), (size / BLOCK_SIZE as u64, blocks));
            }
            Err(other) => panic!("expected NotEnoughBlocks, got {other:?}"),
            Ok(target) => panic!(
                "a symlink declaring {size} bytes resolved to a {}-byte target",
                target.map_or(0, |t| t.len()),
            ),
        }
    }

    #[test]
    fn a_file_ending_in_a_hole_still_reads_as_zeros() {
        // The legal case the bound above must not take with it. `set_len` past
        // the last block dirties no page, so a flush records a size the extents
        // do not reach — `fs_truncate_persist` grows a one-page `/home` file to
        // 3 MiB — and the tail of that file is a hole, which is zeros and not a
        // refusal.
        let blocks = 128;
        let mut raw = image(blocks);
        let root = read_u64_at(&raw, 24);
        craft_entry(&mut raw, root, 1, 4 * BLOCK_SIZE as u64, &[(3, 1)]);

        let fs = mount(raw).expect("mount");
        let data = fs.read_file("victim.txt").expect("a file may end in a hole");
        assert_eq!(data.len(), 4 * BLOCK_SIZE);
        assert!(
            data[BLOCK_SIZE..].iter().all(|&b| b == 0),
            "the hole past the file's one block is not zeros",
        );
    }

    #[test]
    fn an_extent_outside_the_volume_is_refused_before_the_bitmap_is_touched() {
        // What a delete does with an extent: `free_range` → `set_free` →
        // `bit_of`, which is `bitmap_start + block / 32768` and is held against
        // nothing. This volume's bitmap is one block, at block 1, so every
        // block from 32768 up has its "bit" in block 2 — the root node. 32769
        // picks bit 1 of byte 0, and byte 0 is the `B` of `BTND` (0x42), whose
        // bit 1 is set: clearing it leaves `@TND` and the volume has no root.
        let blocks = 128;
        let mut raw = image(blocks);
        let root = read_u64_at(&raw, 24);
        assert_eq!(root, 2, "the bit arithmetic above is keyed to the root's block");
        craft_entry(&mut raw, root, 1, BLOCK_SIZE as u64, &[(32769, 1)]);

        let at = root as usize * BLOCK_SIZE;
        let before = raw[at..at + BLOCK_SIZE].to_vec();

        let mut fs = mount_rw(raw).expect("mount");
        let deleted = fs.delete("victim.txt");

        // The bytes first: what the delete *returned* is the smaller half of
        // this, and asserting it first would hide the damage behind a panic.
        let after = fs.into_formatted().into_io().expect("sync").into_vec();
        assert_eq!(&after[at..at + 4], &NODE_MAGIC[..], "the root node stopped being a node");
        assert!(
            after[at..at + BLOCK_SIZE] == before[..],
            "a delete of an extent outside the volume wrote to the root node",
        );

        match deleted {
            Err(FsError::BlockOffDevice { block, device_blocks }) => {
                assert_eq!((block, device_blocks), (32769, blocks));
            }
            other => panic!("expected BlockOffDevice, got {other:?}"),
        }
    }

    #[test]
    fn a_page_resolved_against_an_extent_at_the_end_of_the_address_space_is_total() {
        // Two halves of one hole. The list `resolve_or_alloc_block` extends is
        // the one `decode_leaf_value` handed out, so the door is where the
        // refusal belongs — and the arithmetic behind the door is total anyway,
        // because `u64::MAX + 1` under the kernel's `overflow-checks` is a
        // panic rather than a wrong answer.
        let blocks = 128;
        let mut raw = image(blocks);
        let root = read_u64_at(&raw, 24);
        craft_entry(&mut raw, root, 1, BLOCK_SIZE as u64, &[(u64::MAX, 1)]);

        let mut fs = mount_rw(raw).expect("mount");

        // The arithmetic first, on a list handed in directly: past the door,
        // `push_extent` adds `start_block + block_count` and `block_for`
        // accumulates the counts, and neither may be a panic.
        let mut extents = Vec::from([Extent {
            start_block: u64::MAX,
            block_count: 1,
            _reserved: 0,
        }]);
        let block = fs.resolve_or_alloc_block(&mut extents, 1).expect("a block for page 1");
        assert!(block < blocks, "page 1 resolved to block {block}, which is not on this volume");

        // And the door itself: nothing gets to hand that list to the kernel.
        match fs.file_extents("victim.txt") {
            Err(FsError::BlockOffDevice { block, device_blocks }) => {
                assert_eq!((block, device_blocks), (u64::MAX, blocks));
            }
            other => panic!("expected BlockOffDevice, got {other:?}"),
        }
    }
}
