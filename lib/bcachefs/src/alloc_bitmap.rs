use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::block_io::{BlockBuf, BlockNum, BlockIO, BlockIOExt, BLOCK_SIZE};
use crate::fs::FsError;
use crate::superblock::Superblock;

const BITS_PER_BLOCK: u64 = (BLOCK_SIZE * 8) as u64;

/// A run of blocks the allocator actually reserved.
///
/// `len` is what you got, never what you asked for. The pair this replaced was
/// `(BlockNum, u32)`, which reads as "here is your block, and how many" and was
/// destructured positionally at three sites; one of them then addressed a block
/// past the end of the short run it had just recorded, so a sparse write on a
/// fragmented volume landed on another file. A struct cannot be read as "all
/// of it" by accident.
#[must_use]
#[derive(Debug, Clone, Copy)]
pub struct Run {
    pub start: BlockNum,
    pub len: u32,
}

/// Bitmap-based block allocator.
///
/// The bitmap is stored on disk starting at `bitmap_start` and spanning
/// `bitmap_blocks` blocks. Each bit represents one block: 1 = used, 0 = free.
///
/// **With `shadow` on, no node the last committed tree reaches is written
/// over, and no block it reaches is handed out again, before the next commit
/// lands** (`Mounted::sync`): a btree node is written in place only when the
/// operation in progress took its block ([`Self::in_place`]), and a block
/// given up is free at once only if no commit has named it since it was taken
/// (`fresh`); any other waits in `pending` for the commit. An operation
/// ([`Self::begin`]) gives up its blocks only when it succeeds, and refused
/// gives back every one it took.
/// mkfs runs with `shadow` off: nothing it writes is committed until it ends.
pub struct BitmapAllocator {
    pub bitmap_start: BlockNum,
    pub bitmap_blocks: u64,
    pub total_blocks: u64,
    pub free_blocks: u64,
    pub next_alloc: u64, // cursor — scan starts here, wraps once
    pub shadow: bool,
    /// Taken since the last commit's superblock was handed to the device.
    fresh: Runs,
    /// Given up, and reached by a tree a mount may still find.
    pending: Vec<(u64, u32)>,
    op: Option<Op>,
}

/// Blocks only an operation that [`Reserve::Draw`]s may take while `shadow`
/// is on, so a full volume still copies the nodes a delete or a shrink
/// changes, as many as sixteen between two commits.
pub const NODE_RESERVE: u64 = 16;

/// Whether an operation may take the blocks [`NODE_RESERVE`] keeps: only one
/// that leaves the tree no larger, so a commit gives back all it drew.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Reserve {
    Keep,
    Draw,
}

/// The operation in progress: what it took, and what it gives up if it succeeds.
struct Op {
    reserve: Reserve,
    taken: Runs,
    given: Vec<(u64, u32)>,
}

/// Disjoint runs of blocks, by first block, to one past the last.
#[derive(Default)]
struct Runs(BTreeMap<u64, u64>);

impl Runs {
    fn insert(&mut self, start: u64, len: u64) {
        self.0.insert(start, start + len);
    }

    fn contains(&self, block: u64) -> bool {
        self.0.range(..=block).next_back().is_some_and(|(_, &end)| block < end)
    }

    /// Take `block` out, answering whether it was in.
    fn remove(&mut self, block: u64) -> bool {
        let Some((&start, &end)) = self.0.range(..=block).next_back() else { return false };
        if block >= end {
            return false;
        }
        self.0.remove(&start);
        if start < block {
            self.0.insert(start, block);
        }
        if block + 1 < end {
            self.0.insert(block + 1, end);
        }
        true
    }
}

impl BitmapAllocator {
    /// The allocator of a mounted volume, its free count the last commit's.
    pub fn open(sb: &Superblock) -> Self {
        Self {
            bitmap_start: sb.bitmap_start,
            bitmap_blocks: sb.bitmap_blocks,
            total_blocks: sb.block_count,
            free_blocks: sb.free_blocks,
            next_alloc: sb.next_alloc,
            shadow: true,
            fresh: Runs::default(),
            pending: Vec::new(),
            op: None,
        }
    }

    /// Read the free count off the bitmap: a stop after the last commit
    /// leaves it holding fewer than the superblock counts.
    pub fn count_free(&mut self, io: &dyn BlockIO) -> Result<(), FsError> {
        let mut free_blocks = 0;
        let mut buf = BlockBuf::zeroed();
        for i in 0..self.total_blocks.div_ceil(BITS_PER_BLOCK) {
            io.read(BlockNum::new(self.bitmap_start.raw() + i), &mut buf)?;
            let bits = (self.total_blocks - i * BITS_PER_BLOCK).min(BITS_PER_BLOCK);
            let (whole, rest) = buf.0[..bits.div_ceil(8) as usize].split_at((bits / 8) as usize);
            free_blocks += whole.iter().map(|b| b.count_zeros() as u64).sum::<u64>();
            free_blocks += rest.first().map_or(0, |b| (!b & ((1u8 << (bits % 8)) - 1)).count_ones() as u64);
        }
        self.free_blocks = free_blocks;
        Ok(())
    }

    /// Begin one operation on the tree, which [`Self::succeed`] or
    /// [`Self::fail`] ends.
    pub fn begin(&mut self, reserve: Reserve) {
        let op = Op { reserve, taken: Runs::default(), given: Vec::new() };
        assert!(self.op.replace(op).is_none(), "an operation began inside another");
    }

    /// What an allocation may take now: every free block, but for those
    /// [`NODE_RESERVE`] keeps from all but an operation that draws on it.
    pub(crate) fn spare(&self) -> u64 {
        match !self.shadow || self.op.as_ref().is_some_and(|op| op.reserve == Reserve::Draw) {
            true => self.free_blocks,
            false => self.free_blocks.saturating_sub(NODE_RESERVE),
        }
    }

    /// Whether a node at `block` may be written where it is.
    pub fn in_place(&self, block: BlockNum) -> bool {
        !self.shadow || self.op.as_ref().is_some_and(|op| op.taken.contains(block.raw()))
    }

    /// The operation took effect: what it gave up is given up now. A bit the
    /// device would not clear is a leaked block, and no reason to call an
    /// operation that took effect refused.
    pub fn succeed(&mut self, io: &dyn BlockIO) {
        let op = self.op.take().expect("an operation in progress");
        for (start, count) in op.given {
            let _ = self.give(io, start, count);
        }
    }

    /// The operation was refused: every block it took is free again, and what
    /// it would have given up stays its owner's. A bit the device would not
    /// clear is a leaked block, and the refusal already in hand is the answer.
    pub fn fail(&mut self, io: &dyn BlockIO) {
        let op = self.op.take().expect("an operation in progress");
        for (start, end) in op.taken.0 {
            for block in start..end {
                self.fresh.remove(block);
                let _ = self.set_free(io, BlockNum::new(block));
            }
        }
    }

    /// A superblock naming the tree as it stands may land from here on, so
    /// nothing taken so far is free at once when given up.
    pub fn seal(&mut self) {
        self.fresh = Runs::default();
    }

    /// Blocks given up that a commit landing makes free.
    pub fn pending(&self) -> u64 {
        self.pending.iter().map(|&(_, count)| count as u64).sum()
    }

    /// A commit landed: the blocks only an older tree reached are free.
    pub fn committed(&mut self, io: &dyn BlockIO) -> Result<(), FsError> {
        while let Some((start, count)) = self.pending.pop() {
            for i in 0..count {
                if let Err(e) = self.set_free(io, BlockNum::new(start + i as u64)) {
                    self.pending.push((start + i as u64, count - i));
                    return Err(e);
                }
            }
        }
        Ok(())
    }

    /// Give up `count` blocks from `start`: free now if no commit has named
    /// them, and otherwise once the next one lands. A block whose bit the
    /// device would not clear is leaked, and the rest are given up still.
    fn give(&mut self, io: &dyn BlockIO, start: u64, count: u32) -> Result<(), FsError> {
        let mut refused = Ok(());
        for block in start..start + count as u64 {
            if !self.shadow || self.fresh.remove(block) {
                if let Err(e) = self.set_free(io, BlockNum::new(block)) {
                    refused = refused.and(Err(e));
                }
            } else {
                match self.pending.last_mut() {
                    Some((at, n)) if *at + *n as u64 == block => *n += 1,
                    _ => self.pending.push((block, 1)),
                }
            }
        }
        refused
    }

    /// Where a block's bit lives, and which bit of that byte it is.
    fn bit_of(&self, block: BlockNum) -> (BlockNum, usize, u8) {
        let byte_idx = block.raw() / 8;
        (
            BlockNum::new(self.bitmap_start.raw() + byte_idx / BLOCK_SIZE as u64),
            (byte_idx % BLOCK_SIZE as u64) as usize,
            (block.raw() % 8) as u8,
        )
    }

    /// Mark a single block as used in the bitmap.
    fn set_used(&self, io: &dyn BlockIO, block: BlockNum) -> Result<(), FsError> {
        let (bitmap_block, byte_off, bit) = self.bit_of(block);
        let mut buf = BlockBuf::zeroed();
        io.read(bitmap_block, &mut buf)?;
        buf.0[byte_off] |= 1 << bit;
        io.write(bitmap_block, &buf)
    }

    /// Mark a single block as free in the bitmap.
    ///
    /// The in-memory counters move only once the bitmap block is on the
    /// device: a free the device refused is not a free, and counting it would
    /// leave the allocator handing out a block the bitmap still calls taken.
    fn set_free(&mut self, io: &dyn BlockIO, block: BlockNum) -> Result<(), FsError> {
        let (bitmap_block, byte_off, bit) = self.bit_of(block);
        let mut buf = BlockBuf::zeroed();
        io.read(bitmap_block, &mut buf)?;
        buf.0[byte_off] &= !(1 << bit);
        io.write(bitmap_block, &buf)?;

        self.free_blocks += 1;
        if block.raw() < self.next_alloc {
            self.next_alloc = block.raw();
        }
        Ok(())
    }

    /// Mark a contiguous range of blocks as used, one write per bitmap block.
    /// A write the device refuses clears again what the run had marked, as far
    /// as the device lets it, so a run is not left marked and named by nothing.
    fn set_range_used(&self, io: &dyn BlockIO, start: BlockNum, count: u64) -> Result<(), FsError> {
        let end = start.raw() + count;
        let mut block = start.raw();
        while block < end {
            match self.set_bits(io, block, end, true) {
                Ok(next) => block = next,
                Err(e) => {
                    let mut undo = start.raw();
                    while undo < block {
                        match self.set_bits(io, undo, block, false) {
                            Ok(next) => undo = next,
                            Err(_) => break,
                        }
                    }
                    return Err(e);
                }
            }
        }
        Ok(())
    }

    /// Set or clear, in one read-modify-write, the bits of `from..end` that
    /// lie in `from`'s bitmap block; the first block past them.
    fn set_bits(&self, io: &dyn BlockIO, from: u64, end: u64, used: bool) -> Result<u64, FsError> {
        let (bitmap_block, _, _) = self.bit_of(BlockNum::new(from));
        let mut buf = BlockBuf::zeroed();
        io.read(bitmap_block, &mut buf)?;
        let mut block = from;
        while block < end {
            let (holding, byte_off, bit) = self.bit_of(BlockNum::new(block));
            if holding != bitmap_block {
                break;
            }
            match used {
                true => buf.0[byte_off] |= 1 << bit,
                false => buf.0[byte_off] &= !(1 << bit),
            }
            block += 1;
        }
        io.write(bitmap_block, &buf)?;
        Ok(block)
    }

    /// Allocate a single block.
    pub fn alloc_block(&mut self, io: &dyn BlockIO) -> Result<BlockNum, FsError> {
        Ok(self.alloc_exact(io, 1)?.start)
    }

    /// Whether `block` is free.
    pub fn is_free(&self, io: &dyn BlockIO, block: BlockNum) -> Result<bool, FsError> {
        let (bitmap_block, byte_off, bit) = self.bit_of(block);
        let mut buf = BlockBuf::zeroed();
        io.read(bitmap_block, &mut buf)?;
        Ok(buf.0[byte_off] & (1 << bit) == 0)
    }

    /// Reserve as much of `wanted` as one contiguous run can cover, scanning
    /// from block `from`.
    ///
    /// The run is never empty and may be shorter than asked for, so every
    /// caller has to loop or has to be wrong.
    pub fn alloc_up_to(&mut self, io: &dyn BlockIO, from: u64, wanted: u32) -> Result<Run, FsError> {
        let spare = self.spare();
        if spare == 0 {
            return Err(FsError::NoSpace { requested: wanted, available: 0 });
        }
        // A zero-length run would let a caller's loop spin without progress.
        let wanted = wanted.max(1).min(u32::try_from(spare).unwrap_or(u32::MAX));
        let (start, len) = self.longest_free_run(io, from % self.total_blocks, wanted)?;
        self.reserve(io, start, len.min(wanted))
    }

    /// Reserve a run at or after block `from`, of at most `wanted` blocks: the
    /// first free run at least `min` long, or else the longest one seen once
    /// the scan has gone round the volume or examined `budget` blocks. Each
    /// block examined until a run is chosen comes off `budget`. `None` if no
    /// free block was seen within it.
    pub fn alloc_first_fit(
        &mut self,
        io: &dyn BlockIO,
        from: u64,
        wanted: u32,
        min: u32,
        budget: &mut u64,
    ) -> Result<Option<Run>, FsError> {
        let spare = self.spare();
        if spare == 0 {
            return Err(FsError::NoSpace { requested: wanted, available: 0 });
        }
        let wanted = wanted.max(1).min(u32::try_from(spare).unwrap_or(u32::MAX));
        let min = min.clamp(1, wanted);
        let total = self.total_blocks;
        let chosen = {
            let mut cached = (u64::MAX, BlockBuf::zeroed());
            let mut free = |pos: u64| -> Result<bool, FsError> {
                let (bitmap_block, byte_off, bit) = self.bit_of(BlockNum::new(pos));
                if cached.0 != bitmap_block.raw() {
                    io.read(bitmap_block, &mut cached.1)?;
                    cached.0 = bitmap_block.raw();
                }
                Ok(cached.1 .0[byte_off] & (1 << bit) == 0)
            };
            let from = from % total;
            // (start, length) of the run being walked, and of the longest seen.
            let (mut run, mut best) = ((0u64, 0u32), (0u64, 0u32));
            let mut found = None;
            for pos in (from..total).chain(0..from) {
                // Block 0 does not follow the volume's last block.
                if pos == 0 {
                    run.1 = 0;
                }
                if *budget == 0 {
                    break;
                }
                *budget -= 1;
                if !free(pos)? {
                    run.1 = 0;
                    continue;
                }
                if run.1 == 0 {
                    run.0 = pos;
                }
                run.1 += 1;
                if run.1 > best.1 {
                    best = run;
                }
                if run.1 >= min {
                    found = Some(run);
                    break;
                }
            }
            match found {
                Some((start, mut len)) => {
                    while len < wanted && start + u64::from(len) < total && free(start + u64::from(len))? {
                        len += 1;
                    }
                    Some((start, len))
                }
                None => (best.1 > 0).then_some(best),
            }
        };
        match chosen {
            Some((start, len)) => self.reserve(io, start, len).map(Some),
            None => Ok(None),
        }
    }

    /// Reserve all of `count` or nothing, for callers that cannot place a
    /// short run. Nothing is marked used unless the whole run is there.
    pub fn alloc_exact(&mut self, io: &dyn BlockIO, count: u32) -> Result<Run, FsError> {
        let spare = self.spare();
        if spare < count as u64 {
            return Err(FsError::NoSpace { requested: count, available: spare });
        }
        let (start, len) = self.longest_free_run(io, self.next_alloc, count)?;
        if len < count {
            return Err(FsError::NoSpace {
                requested: count,
                available: self.free_blocks,
            });
        }
        self.reserve(io, start, count)
    }

    /// Mark a run used and move the cursor past it.
    ///
    /// The counters follow the bitmap write for the reason `set_free`'s do: a
    /// reservation the device would not record is not a reservation, and
    /// deducting it would lose blocks nothing ever gets back.
    fn reserve(&mut self, io: &dyn BlockIO, start: u64, len: u32) -> Result<Run, FsError> {
        let start_block = BlockNum::new(start);
        self.set_range_used(io, start_block, len as u64)?;
        self.fresh.insert(start, len as u64);
        if let Some(op) = &mut self.op {
            op.taken.insert(start, len as u64);
        }
        self.free_blocks -= len as u64;
        self.next_alloc = start + len as u64;
        if self.next_alloc >= self.total_blocks {
            self.next_alloc = 0;
        }
        Ok(Run { start: start_block, len })
    }

    /// The longest free run found scanning from block `start_pos`, wrapping
    /// once, stopping early once `wanted` blocks are in hand.
    fn longest_free_run(&self, io: &dyn BlockIO, start_pos: u64, wanted: u32) -> Result<(u64, u32), FsError> {
        if self.free_blocks == 0 {
            return Err(FsError::NoSpace {
                requested: wanted,
                available: 0,
            });
        }

        let total = self.total_blocks;
        let mut best_start = None;
        let mut best_count = 0u32;

        // Scan from start_pos, wrap once
        let mut pos = start_pos;
        let mut wrapped = false;
        let mut run_start = None;
        let mut run_count = 0u32;

        // Cache the current bitmap block to avoid re-reading for every bit
        let mut cached_bitmap_block = u64::MAX;
        let mut cached_buf = BlockBuf::zeroed();

        loop {
            if wrapped && pos >= start_pos {
                break;
            }
            if pos >= total {
                if wrapped {
                    break;
                }
                wrapped = true;
                pos = 0;
                run_start = None;
                run_count = 0;
                continue;
            }

            // Read bitmap block if not cached
            let byte_idx = pos / 8;
            let bblock = self.bitmap_start.raw() + byte_idx / BLOCK_SIZE as u64;
            if bblock != cached_bitmap_block {
                io.read(BlockNum::new(bblock), &mut cached_buf)?;
                cached_bitmap_block = bblock;
            }

            let byte_off = (byte_idx % BLOCK_SIZE as u64) as usize;
            let bit_idx = pos % 8;
            let is_free = (cached_buf.0[byte_off] >> bit_idx) & 1 == 0;

            if is_free {
                if run_start.is_none() {
                    run_start = Some(pos);
                    run_count = 0;
                }
                run_count += 1;

                if run_count >= wanted {
                    // Found exactly what we wanted
                    best_start = run_start;
                    best_count = run_count;
                    break;
                }

                if run_count > best_count {
                    best_start = run_start;
                    best_count = run_count;
                }
            } else {
                run_start = None;
                run_count = 0;
            }

            pos += 1;
        }

        let start = best_start.ok_or(FsError::NoSpace {
            requested: wanted,
            available: self.free_blocks,
        })?;

        Ok((start, best_count))
    }

    /// Give up a contiguous range of blocks: inside an operation, once it
    /// succeeds.
    pub fn free_range(
        &mut self,
        io: &dyn BlockIO,
        start: BlockNum,
        count: u32,
    ) -> Result<(), FsError> {
        match &mut self.op {
            Some(op) => {
                op.given.push((start.raw(), count));
                Ok(())
            }
            None => self.give(io, start.raw(), count),
        }
    }

    /// Initialize bitmap on disk: zero all bitmap blocks, then mark metadata blocks as used.
    pub fn format(
        io: &dyn BlockIO,
        bitmap_start: BlockNum,
        bitmap_blocks: u64,
        total_blocks: u64,
        metadata_blocks: u64,
    ) -> Result<Self, FsError> {
        let zero = BlockBuf::zeroed();
        for i in 0..bitmap_blocks {
            io.write(BlockNum::new(bitmap_start.raw() + i), &zero)?;
        }

        let mut alloc = Self {
            bitmap_start,
            bitmap_blocks,
            total_blocks,
            free_blocks: total_blocks - metadata_blocks,
            next_alloc: metadata_blocks,
            shadow: false,
            fresh: Runs::default(),
            pending: Vec::new(),
            op: None,
        };

        // Metadata blocks (superblock, bitmap, journal area), and the backup
        // superblock at the far end.
        for i in 0..metadata_blocks {
            alloc.set_used(io, BlockNum::new(i))?;
        }
        alloc.set_used(io, BlockNum::new(total_blocks - 1))?;
        alloc.free_blocks -= 1;

        Ok(alloc)
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
    use crate::block_io::{DeviceError, VecBlockIO};
    use core::cell::{Cell, RefCell};

    /// A bitmap device that records the blocks written to it, and refuses a
    /// write to `refuse` once armed.
    struct Counted {
        io: VecBlockIO,
        written: RefCell<Vec<u64>>,
        refuse: Cell<Option<u64>>,
    }

    impl BlockIO for Counted {
        fn read_block(&self, block: BlockNum, buf: &mut BlockBuf) -> Result<(), DeviceError> {
            self.io.read_block(block, buf)
        }
        fn write_block(&self, block: BlockNum, buf: &BlockBuf) -> Result<(), DeviceError> {
            if self.refuse.get() == Some(block.raw()) {
                return Err(DeviceError::FAILED);
            }
            self.written.borrow_mut().push(block.raw());
            self.io.write_block(block, buf)
        }
        fn block_count(&self) -> u64 {
            self.io.block_count()
        }
        fn sync(&self) -> Result<(), DeviceError> {
            Ok(())
        }
    }

    /// A volume of two bitmap blocks; the device holds only the bitmap.
    fn two_bitmap_blocks() -> (Counted, BitmapAllocator) {
        let io = Counted { io: VecBlockIO::new(2), written: RefCell::default(), refuse: Cell::new(None) };
        let alloc = BitmapAllocator::format(&io, BlockNum::new(0), 2, 40_000, 2).expect("format");
        io.written.borrow_mut().clear();
        (io, alloc)
    }

    /// A run across the boundary of two bitmap blocks writes each once and
    /// marks its blocks, and only those.
    #[test]
    fn a_run_across_two_bitmap_blocks_writes_each_once() {
        let (io, mut alloc) = two_bitmap_blocks();
        let free = alloc.free_blocks;
        let at = BITS_PER_BLOCK - 8;
        let mut budget = u64::MAX;
        let run = alloc.alloc_first_fit(&io, at, 16, 1, &mut budget).expect("a run").expect("a free block");
        assert_eq!((run.start.raw(), run.len), (at, 16));
        assert_eq!(*io.written.borrow(), [0, 1]);
        for (block, taken) in [(at - 1, false), (at, true), (at + 15, true), (at + 16, false)] {
            assert_eq!(alloc.is_free(&io, BlockNum::new(block)).unwrap(), !taken, "block {block}");
        }
        alloc.count_free(&io).expect("count");
        assert_eq!(alloc.free_blocks, free - 16);
    }

    /// A run whose second bitmap block the device refuses is refused, and
    /// what it marked in the first is cleared again.
    #[test]
    fn a_run_refused_halfway_leaves_nothing_marked() {
        let (io, mut alloc) = two_bitmap_blocks();
        let free = alloc.free_blocks;
        io.refuse.set(Some(1));
        let mut budget = u64::MAX;
        assert!(alloc.alloc_first_fit(&io, BITS_PER_BLOCK - 8, 16, 1, &mut budget).is_err());
        alloc.count_free(&io).expect("count");
        assert_eq!(alloc.free_blocks, free, "blocks stayed marked that nothing names");
    }
}
