//! The one cache every block of a volume passes through: a filesystem's
//! metadata and its files' data alike.
//!
//! **A write lands here and reaches the disk at a flush.** [`Cache::flush`]
//! writes every dirty block, lowest first and in runs, then asks the disk to
//! make them durable; a volume's sync is its own metadata written into the
//! cache and then this. A cache holding more than [`DIRTY_LIMIT`] dirty blocks
//! flushes by itself, so memory bounds the write-back and not the other way
//! round.
//!
//! **Clean blocks are kept up to [`CLEAN_LIMIT`]** and the oldest-read goes
//! first; a dirty block is never dropped.
//!
//! Interior mutability because the bcachefs crate reads through `&self`; the
//! server is one thread, so a `RefCell` and never a lock.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};

use crate::disk::{Disk, DiskError, BLOCK};

/// Clean blocks kept: 64 MiB.
pub const CLEAN_LIMIT: usize = 16 * 1024;

/// Dirty blocks held before the cache flushes by itself: 32 MiB.
pub const DIRTY_LIMIT: usize = 8 * 1024;

/// The longest run one disk request carries, in blocks.
const RUN: usize = 32;

struct Slot {
    data: Box<[u8; BLOCK]>,
    dirty: bool,
}

struct Inner<D> {
    disk: D,
    slots: BTreeMap<u64, Slot>,
    /// Clean blocks in the order they were read or cleaned; stale entries are
    /// skipped when popped.
    order: VecDeque<u64>,
    dirty: usize,
    reads: u64,
    hits: u64,
}

pub struct Cache<D> {
    inner: RefCell<Inner<D>>,
}

/// What a cache has done, for the server's `inspect` line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub reads: u64,
    pub hits: u64,
    pub cached: usize,
    pub dirty: usize,
}

impl<D: Disk> Cache<D> {
    pub fn new(disk: D) -> Self {
        Self {
            inner: RefCell::new(Inner {
                disk,
                slots: BTreeMap::new(),
                order: VecDeque::new(),
                dirty: 0,
                reads: 0,
                hits: 0,
            }),
        }
    }

    pub fn blocks(&self) -> u64 {
        self.inner.borrow().disk.blocks()
    }

    /// The disk, once nothing else holds the cache: what it holds is what the
    /// last flush left.
    pub fn into_disk(self) -> D {
        self.inner.into_inner().disk
    }

    pub fn counts(&self) -> Counts {
        let inner = self.inner.borrow();
        Counts { reads: inner.reads, hits: inner.hits, cached: inner.slots.len(), dirty: inner.dirty }
    }

    /// Blocks `first..first + out.len() / BLOCK`, each from the cache where it
    /// is there and in runs from the disk where it is not.
    pub fn read(&self, first: u64, out: &mut [u8]) -> Result<(), DiskError> {
        assert!(out.len() % BLOCK == 0, "fsd: a cache read of {} bytes", out.len());
        let count = out.len() / BLOCK;
        let mut inner = self.inner.borrow_mut();
        let mut i = 0;
        while i < count {
            let block = first + i as u64;
            inner.reads += 1;
            if let Some(slot) = inner.slots.get(&block) {
                out[i * BLOCK..(i + 1) * BLOCK].copy_from_slice(&slot.data[..]);
                inner.hits += 1;
                i += 1;
                continue;
            }
            // The run of misses from here, which one request fetches.
            let mut run = 1;
            while i + run < count && run < RUN && !inner.slots.contains_key(&(block + run as u64)) {
                run += 1;
            }
            let span = &mut out[i * BLOCK..(i + run) * BLOCK];
            inner.disk.read(block, span)?;
            for (k, chunk) in span.chunks_exact(BLOCK).enumerate() {
                let mut data = Box::new([0u8; BLOCK]);
                data.copy_from_slice(chunk);
                inner.slots.insert(block + k as u64, Slot { data, dirty: false });
                inner.order.push_back(block + k as u64);
            }
            i += run;
        }
        inner.evict();
        Ok(())
    }

    /// One block into `out`.
    pub fn read_block(&self, block: u64, out: &mut [u8; BLOCK]) -> Result<(), DiskError> {
        self.read(block, &mut out[..])
    }

    /// `data`, whole blocks from `first`, into the cache: on the disk at the
    /// next flush.
    pub fn write(&self, first: u64, data: &[u8]) -> Result<(), DiskError> {
        assert!(data.len() % BLOCK == 0, "fsd: a cache write of {} bytes", data.len());
        let over = {
            let mut inner = self.inner.borrow_mut();
            if first.checked_add((data.len() / BLOCK) as u64).is_none_or(|end| end > inner.disk.blocks()) {
                return Err(DiskError::Range);
            }
            for (k, chunk) in data.chunks_exact(BLOCK).enumerate() {
                let block = first + k as u64;
                let newly = match inner.slots.get_mut(&block) {
                    Some(slot) => {
                        slot.data.copy_from_slice(chunk);
                        !std::mem::replace(&mut slot.dirty, true)
                    }
                    None => {
                        let mut buf = Box::new([0u8; BLOCK]);
                        buf.copy_from_slice(chunk);
                        inner.slots.insert(block, Slot { data: buf, dirty: true });
                        true
                    }
                };
                if newly {
                    inner.dirty += 1;
                }
            }
            inner.dirty > DIRTY_LIMIT
        };
        if over {
            self.flush()?;
        }
        Ok(())
    }

    /// Every dirty block to the disk, lowest first and in runs, then the disk
    /// made durable. A block the disk refused stays dirty.
    pub fn flush(&self) -> Result<(), DiskError> {
        let mut inner = self.inner.borrow_mut();
        let dirty: Vec<u64> = inner.slots.iter().filter(|(_, s)| s.dirty).map(|(b, _)| *b).collect();
        let mut i = 0;
        let mut buf = vec![0u8; RUN * BLOCK];
        while i < dirty.len() {
            let first = dirty[i];
            let mut run = 1;
            while i + run < dirty.len() && run < RUN && dirty[i + run] == first + run as u64 {
                run += 1;
            }
            for k in 0..run {
                let slot = &inner.slots[&(first + k as u64)];
                buf[k * BLOCK..(k + 1) * BLOCK].copy_from_slice(&slot.data[..]);
            }
            inner.disk.write(first, &buf[..run * BLOCK])?;
            for k in 0..run {
                let block = first + k as u64;
                inner.slots.get_mut(&block).expect("listed above").dirty = false;
                inner.order.push_back(block);
                inner.dirty -= 1;
            }
            i += run;
        }
        inner.disk.flush()?;
        inner.evict();
        Ok(())
    }
}

impl<D> Inner<D> {
    /// Drop the oldest clean blocks past [`CLEAN_LIMIT`].
    fn evict(&mut self) {
        while self.slots.len() - self.dirty > CLEAN_LIMIT {
            let Some(block) = self.order.pop_front() else { break };
            if self.slots.get(&block).is_some_and(|s| !s.dirty) {
                self.slots.remove(&block);
            }
        }
        // A queue of mostly stale entries is compacted, so it stays the size
        // of what it orders.
        if self.order.len() > 4 * (CLEAN_LIMIT + DIRTY_LIMIT) {
            let slots = &self.slots;
            self.order.retain(|b| slots.get(b).is_some_and(|s| !s.dirty));
        }
    }
}

/// The cache as the bcachefs crate reads it: shared, since the volume reads
/// files' data through the same cache its btree lives in.
pub struct Shared<D>(pub std::rc::Rc<Cache<D>>);

impl<D> Clone for Shared<D> {
    fn clone(&self) -> Self {
        Self(std::rc::Rc::clone(&self.0))
    }
}

impl<D: Disk> bcachefs::BlockIO for Shared<D> {
    fn read_block(&self, block: bcachefs::BlockNum, buf: &mut bcachefs::BlockBuf) -> Result<(), bcachefs::DeviceError> {
        self.0.read(block.raw(), buf.as_bytes_mut()).map_err(|e| bcachefs::DeviceError::classify(&e))
    }

    fn write_block(&self, block: bcachefs::BlockNum, buf: &bcachefs::BlockBuf) -> Result<(), bcachefs::DeviceError> {
        self.0.write(block.raw(), buf.as_bytes()).map_err(|e| bcachefs::DeviceError::classify(&e))
    }

    fn block_count(&self) -> u64 {
        self.0.blocks()
    }

    fn sync(&self) -> Result<(), bcachefs::DeviceError> {
        self.0.flush().map_err(|e| bcachefs::DeviceError::classify(&e))
    }
}

/// Every refusal here was attempted: nothing in this server refuses on a clock.
impl bcachefs::TransferError for DiskError {
    fn refused_before_attempt(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disk::Ram;

    /// A disk that counts what reaches it.
    struct Counting {
        ram: Ram,
        reads: u32,
        writes: u32,
        flushes: u32,
    }

    impl Disk for Counting {
        fn blocks(&self) -> u64 {
            self.ram.blocks()
        }
        fn read(&mut self, first: u64, out: &mut [u8]) -> Result<(), DiskError> {
            self.reads += 1;
            self.ram.read(first, out)
        }
        fn write(&mut self, first: u64, data: &[u8]) -> Result<(), DiskError> {
            self.writes += 1;
            self.ram.write(first, data)
        }
        fn flush(&mut self) -> Result<(), DiskError> {
            self.flushes += 1;
            self.ram.flush()
        }
    }

    fn cache() -> Cache<Counting> {
        Cache::new(Counting { ram: Ram::new(1024), reads: 0, writes: 0, flushes: 0 })
    }

    #[test]
    fn a_write_reaches_the_disk_at_the_flush_and_not_before() {
        let c = cache();
        c.write(7, &[0xAB; BLOCK]).unwrap();
        assert_eq!(c.inner.borrow().disk.writes, 0);
        let mut out = [0u8; BLOCK];
        c.read_block(7, &mut out).unwrap();
        assert_eq!(out, [0xAB; BLOCK], "a dirty block is served from the cache");
        c.flush().unwrap();
        let inner = c.inner.borrow();
        assert_eq!((inner.disk.writes, inner.disk.flushes, inner.dirty), (1, 1, 0));
        let mut on_disk = [0u8; BLOCK];
        drop(inner);
        c.inner.borrow_mut().disk.ram.read(7, &mut on_disk).unwrap();
        assert_eq!(on_disk, [0xAB; BLOCK]);
    }

    #[test]
    fn contiguous_misses_are_one_request_and_a_second_read_is_none() {
        let c = cache();
        let mut out = vec![0u8; 10 * BLOCK];
        c.read(100, &mut out).unwrap();
        assert_eq!(c.inner.borrow().disk.reads, 1);
        c.read(100, &mut out).unwrap();
        assert_eq!(c.inner.borrow().disk.reads, 1);
        assert_eq!(c.counts().hits, 10);
    }

    #[test]
    fn dirty_runs_go_out_in_runs() {
        let c = cache();
        for b in [3u64, 4, 5, 9, 10] {
            c.write(b, &[b as u8; BLOCK]).unwrap();
        }
        c.flush().unwrap();
        assert_eq!(c.inner.borrow().disk.writes, 2, "3..=5 and 9..=10");
    }

    #[test]
    fn a_write_past_the_disk_is_refused() {
        let c = cache();
        assert_eq!(c.write(1024, &[0; BLOCK]), Err(DiskError::Range));
        assert_eq!(c.counts().dirty, 0);
    }

    #[test]
    fn clean_blocks_are_bounded_and_dirty_ones_are_kept() {
        let c = Cache::new(Counting { ram: Ram::new((CLEAN_LIMIT + 64) as u64), reads: 0, writes: 0, flushes: 0 });
        c.write(0, &[1; BLOCK]).unwrap();
        let mut out = vec![0u8; BLOCK];
        for b in 1..(CLEAN_LIMIT as u64 + 32) {
            c.read(b, &mut out).unwrap();
        }
        let counts = c.counts();
        assert!(counts.cached - counts.dirty <= CLEAN_LIMIT);
        assert_eq!(counts.dirty, 1);
        c.read(0, &mut out).unwrap();
        assert_eq!(out, vec![1; BLOCK]);
    }
}
