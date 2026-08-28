use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use hashbrown::HashMap;

use crate::block::{self, BlockDevice, BlockError, BlockResult, DeviceId};
use crate::sync::Lock;

// Lock ordering: BLOCK_CACHE → BLOCK_DEV (never reversed).
static BLOCK_CACHE: Lock<Option<PageCache>> = Lock::new(None);
static BLOCK_DEV: Lock<Option<Box<dyn BlockDevice>>> = Lock::new(None);

/// Initialize the page cache, taking ownership of the block device.
pub fn init(dev: Box<dyn BlockDevice>) {
    let block_count = dev.block_count();
    let cache = PageCache::new(block_count, dev.device_id());
    log!("page cache: {} device blocks, index sized for {} cached blocks, cap {} slots",
        block_count, cache.index_capacity(), cache.max_slots);
    *BLOCK_CACHE.lock() = Some(cache);
    *BLOCK_DEV.lock() = Some(dev);
}

/// Locks cache then device, in that order, for metadata operations.
pub fn lock() -> PageCacheGuard {
    let cache = BLOCK_CACHE.lock();
    let dev = BLOCK_DEV.lock();
    PageCacheGuard { cache, dev }
}

pub struct PageCacheGuard {
    cache: crate::sync::LockGuard<'static, Option<PageCache>>,
    dev: crate::sync::LockGuard<'static, Option<Box<dyn BlockDevice>>>,
}

impl PageCacheGuard {
    pub fn cache_and_dev(&mut self) -> (&mut PageCache, &mut dyn BlockDevice) {
        let cache = self.cache.as_mut().expect("page cache not initialized");
        let dev = self.dev.as_mut().expect("block device not initialized");
        (cache, dev.as_mut())
    }

    pub fn block_count(&self) -> u64 {
        self.cache.as_ref().expect("page cache not initialized").block_count()
    }
}

impl core::ops::Deref for PageCacheGuard {
    type Target = PageCache;
    fn deref(&self) -> &PageCache { self.cache.as_ref().expect("page cache not initialized") }
}

impl core::ops::DerefMut for PageCacheGuard {
    fn deref_mut(&mut self) -> &mut PageCache { self.cache.as_mut().expect("page cache not initialized") }
}

/// Reads a block directly from disk, bypassing the cache; locks only the device.
#[must_use = "a failed read leaves the buffer holding whatever it held before"]
pub fn raw_block_read(block: u64, buf: &mut [u8; 4096]) -> BlockResult {
    let mut dev = BLOCK_DEV.lock();
    let dev = dev.as_mut().expect("block device not initialized");
    dev.read_blocks(block, 1, buf)
}

/// Writes a block directly to disk, bypassing the cache's slots but not its
/// flush debt: the bytes land in the device's write cache, which the next [`PageCache::sync`] owes a flush.
#[must_use = "a failed write did not reach the device"]
pub fn raw_block_write(block: u64, buf: &[u8; 4096]) -> BlockResult {
    let mut guard = lock();
    let (cache, dev) = guard.cache_and_dev();
    cache.flush.record_write();
    dev.write_blocks(block, 1, buf)
}

// No device reaches u64::MAX blocks, so this value is safe as a sentinel.
const NO_BLOCK: u64 = u64::MAX;

/// 256 pages per chunk so each chunk allocation is exactly 1MB.
const PAGES_PER_CHUNK: usize = 256;
const CHUNK_SIZE: usize = PAGES_PER_CHUNK * 4096;

pub struct PageCache {
    /// Maps block number → slot index; sized by the cached set, never by the device block count.
    block_to_slot: HashMap<u64, u32>,
    /// Maps slot index → block number, for sync and eviction.
    slot_to_block: Vec<u64>,
    dirty: Vec<bool>,
    /// CLOCK's second-chance bit: set on every hit, cleared when the hand passes.
    /// Without it the cache degenerates to FIFO and can evict the superblock like any other slot.
    referenced: Vec<bool>,
    /// Allocated in fixed 1MB chunks rather than one buffer, to avoid reallocating the whole cache as it grows.
    /// `Box<[u8]>`, not `Box<[u8; CHUNK_SIZE]>`: the array type would build a 1 MiB stack temporary before boxing.
    chunks: Vec<Box<[u8]>>,
    hand: u32,
    max_slots: usize,
    evictions: u64,
    /// The device flush [`Self::sync`] still owes: raised by every write that
    /// reached the device's cache, settled only by a `dev.flush()` that
    /// returned `Ok` — so an empty dirty set never skips a flush that is owed.
    flush: crate::durability::Owed,
    /// The device's size; nothing in this struct may size an allocation by it.
    block_count: u64,
    _device_id: DeviceId,
}

impl PageCache {
    fn new(block_count: u64, device_id: DeviceId) -> Self {
        let max_slots = block::metadata_cache_blocks();
        Self {
            // Reserved up front so `index_capacity` reports the real ceiling immediately.
            block_to_slot: HashMap::with_capacity(max_slots),
            slot_to_block: Vec::with_capacity(max_slots),
            dirty: Vec::with_capacity(max_slots),
            referenced: Vec::with_capacity(max_slots),
            chunks: Vec::with_capacity(max_slots.div_ceil(PAGES_PER_CHUNK)),
            hand: 0,
            max_slots,
            evictions: 0,
            flush: crate::durability::Owed::new(),
            block_count,
            _device_id: device_id,
        }
    }

    pub fn block_count(&self) -> u64 {
        self.block_count
    }

    /// Blocks the index has room for; must stay far below `block_count`.
    pub fn index_capacity(&self) -> usize {
        self.block_to_slot.capacity()
    }

    /// Returns `None` only when every resident slot is dirty and write-back failed.
    fn alloc_slot(&mut self, dev: &mut dyn BlockDevice, block: u64) -> Option<u32> {
        let slot = if self.slot_to_block.len() < self.max_slots {
            let slot = self.slot_to_block.len() as u32;
            self.slot_to_block.push(block);
            self.dirty.push(false);
            self.referenced.push(false);
            if slot as usize / PAGES_PER_CHUNK >= self.chunks.len() {
                self.chunks.push(vec![0u8; CHUNK_SIZE].into_boxed_slice());
            }
            slot
        } else {
            let slot = self.take_victim(dev)?;
            self.block_to_slot.remove(&self.slot_to_block[slot as usize]);
            self.slot_to_block[slot as usize] = block;
            self.dirty[slot as usize] = false;
            self.evictions += 1;
            // Logs once per full cache turnover, not on a fixed count, so the cadence scales with the bound.
            if self.evictions == 1 || self.evictions.is_multiple_of(self.max_slots as u64) {
                log!("page cache: {} evictions, {}/{} slots resident",
                    self.evictions, self.slot_to_block.len(), self.max_slots);
            }
            slot
        };
        self.referenced[slot as usize] = true;
        self.block_to_slot.insert(block, slot);
        Some(slot)
    }

    /// Clears the block's index entry so a later reader misses instead of reading stale data.
    fn unbind(&mut self, slot: u32, block: u64) {
        self.block_to_slot.remove(&block);
        self.slot_to_block[slot as usize] = NO_BLOCK;
        self.dirty[slot as usize] = false;
        self.referenced[slot as usize] = false;
    }

    /// Frees a slot for reuse, writing back first when every clean slot is exhausted.
    /// Writing back early is safe: there is no journal, so eviction only reorders writes that were never ordered.
    fn take_victim(&mut self, dev: &mut dyn BlockDevice) -> Option<u32> {
        if let Some(slot) = self.clock_pick() {
            return Some(slot);
        }
        // Every resident slot is dirty; one write-back clears them all unless the device refuses.
        if self.sync(dev).is_err() {
            log!("page cache: write-back failed; no slot could be freed");
        }
        self.clock_pick()
    }

    /// CLOCK second-chance eviction over two revolutions of the hand.
    /// First revolution clears reference bits; second finds a victim unless every slot is dirty.
    fn clock_pick(&mut self) -> Option<u32> {
        let n = self.slot_to_block.len() as u32;
        for _ in 0..2 * n {
            let slot = self.hand as usize;
            self.hand = if self.hand + 1 == n { 0 } else { self.hand + 1 };
            if self.dirty[slot] {
                continue;
            }
            if self.referenced[slot] {
                self.referenced[slot] = false;
                continue;
            }
            return Some(slot as u32);
        }
        None
    }

    fn slot_data(&self, slot: u32) -> &[u8] {
        let chunk_idx = slot as usize / PAGES_PER_CHUNK;
        let page_in_chunk = slot as usize % PAGES_PER_CHUNK;
        let off = page_in_chunk * 4096;
        &self.chunks[chunk_idx][off..off + 4096]
    }

    fn slot_data_mut(&mut self, slot: u32) -> &mut [u8] {
        let chunk_idx = slot as usize / PAGES_PER_CHUNK;
        let page_in_chunk = slot as usize % PAGES_PER_CHUNK;
        let off = page_in_chunk * 4096;
        &mut self.chunks[chunk_idx][off..off + 4096]
    }

    fn slot_of(&self, block: u64) -> Option<u32> {
        self.block_to_slot.get(&block).copied()
    }

    pub fn read(&mut self, dev: &mut dyn BlockDevice, block: u64) -> Result<&[u8], BlockError> {
        if let Some(slot) = self.slot_of(block) {
            self.referenced[slot as usize] = true;
            return Ok(self.slot_data(slot));
        }
        let slot = self.alloc_slot(dev, block).ok_or(BlockError::Device)?;
        let page = self.slot_data_mut(slot);
        if let Err(e) = dev.read_blocks(block, 1, page) {
            self.unbind(slot, block);
            return Err(e);
        }
        Ok(self.slot_data(slot))
    }

    pub fn write_new(
        &mut self,
        dev: &mut dyn BlockDevice,
        block: u64,
    ) -> Result<&mut [u8], BlockError> {
        let slot = match self.slot_of(block) {
            Some(slot) => {
                self.referenced[slot as usize] = true;
                slot
            }
            None => self.alloc_slot(dev, block).ok_or(BlockError::Device)?,
        };
        self.dirty[slot as usize] = true;
        let page = self.slot_data_mut(slot);
        // A reused slot still holds the evicted block's bytes; zero it before returning.
        page.fill(0);
        Ok(page)
    }

    /// Writes every dirty slot back, coalescing runs of consecutive blocks.
    pub fn sync(&mut self, dev: &mut dyn BlockDevice) -> BlockResult {
        let mut pending: Vec<u32> = (0..self.slot_to_block.len() as u32)
            .filter(|&s| self.dirty[s as usize])
            .collect();

        // Nothing to write is not nothing to flush: a raw write, or a failed predecessor's runs, is still owed.
        if pending.is_empty() && !self.flush.is_owed() {
            return Ok(());
        }
        pending.sort_unstable_by_key(|&s| self.slot_to_block[s as usize]);

        let mut buf = vec![0u8; 32 * 4096];
        // Errors combine via `worse`, not first-wins, so a caller sees the one that blocks retry.
        let mut failed: Option<BlockError> = None;
        let mut i = 0;
        while i < pending.len() {
            let start = self.slot_to_block[pending[i] as usize];
            let mut count = 1usize;

            while i + count < pending.len()
                && self.slot_to_block[pending[i + count] as usize] == start + count as u64
                && count < 32
            {
                count += 1;
            }

            for j in 0..count {
                let page = self.slot_data(pending[i + j]);
                buf[j * 4096..(j + 1) * 4096].copy_from_slice(page);
            }

            // A failed run stays dirty for retry; the loop continues rather than aborting on it.
            match dev.write_blocks(start, count as u32, &buf[..count * 4096]) {
                Ok(()) => {
                    self.flush.record_write();
                    for j in 0..count {
                        self.dirty[pending[i + j] as usize] = false;
                    }
                }
                Err(e) => {
                    log!("page cache: write-back of {count} blocks at {start} failed");
                    failed = Some(failed.map_or(e, |had| had.worse(e)));
                }
            }

            i += count;
        }

        // The lock is held throughout, so the snapshot covers every write above.
        let upto = self.flush.snapshot();
        match dev.flush() {
            Ok(()) => self.flush.settle(upto),
            Err(e) => {
                log!("page cache: flush failed; the write-back above is not durable");
                failed = Some(failed.map_or(e, |had| had.worse(e)));
            }
        }
        failed.map_or(Ok(()), Err)
    }
}
