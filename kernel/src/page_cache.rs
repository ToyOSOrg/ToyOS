//! One page cache per (device, partition) served. Every slot is bound to a
//! [`BlockKey`], so a resident page is written back where it was filled from.
//!
//! Lock order: a cache, then its device (`block::Handle::lock`); never
//! reversed, and no holder of one cache takes another's.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use crate::hasher::{HashMap, KernelHashState};

use crate::block::{self, BlockDevice, BlockError, BlockKey, BlockResult, Partition};
use crate::mm::PAGE_BYTES;
use crate::sync::{Lock, LockGuard};

pub struct Cached {
    cache: Lock<PageCache>,
    part: Partition,
}

/// Wraps `dev` in the read-fault injector when `pc-unbind-selftest` is armed —
/// at registration, so it sits under the one device object consumers share.
pub fn instrumented(dev: Box<dyn BlockDevice>) -> Box<dyn BlockDevice> {
    #[cfg(feature = "boot-actuators")]
    if crate::actuator::pc_unbind_selftest() {
        return Box::new(read_fault::FaultDevice(dev));
    }
    dev
}

pub fn init(part: Partition) -> Arc<Cached> {
    let cache = PageCache::new();
    log!(
        "page cache: device {} partition +{}, {} partition blocks, index sized for {} cached \
         blocks, cap {} slots, {} index bytes",
        part.device_id(),
        part.first_block(),
        part.block_count(),
        cache.index_capacity(),
        cache.max_slots,
        cache.index_bytes()
    );
    Arc::new(Cached { cache: Lock::new(cache), part })
}

/// The block every `BlockDevice` transfers.
const PAGE: u64 = PAGE_BYTES as u64;

/// A cache over the partition `candidate` names, or `None` with the reason
/// logged under `what`. Every number below is one the disk chose, so each
/// refusal says which; nothing here decides what the volume holds.
pub fn over_candidate(candidate: &crate::gpt::Candidate, what: &str) -> Option<Arc<Cached>> {
    let volume = candidate.volume;
    let guid = candidate.guid;
    let Some(handle) = block::open(volume.device) else {
        log!("{what}: candidate {guid} is on device {} and no driver here registered it", volume.device);
        return None;
    };

    let lba = volume.lba_bytes as u64;
    let (Some(start), Some(len)) =
        (volume.start_lba.checked_mul(lba), volume.blocks.checked_mul(lba))
    else {
        log!(
            "{what}: candidate {guid} claims LBA {}+{} of {lba} bytes, which is not a byte range \
             — refusing it",
            volume.start_lba,
            volume.blocks
        );
        return None;
    };
    // Whole device blocks or nothing: a view that began or ended inside one
    // would share it with the table or with the next partition.
    if start % PAGE != 0 || len % PAGE != 0 {
        log!(
            "{what}: candidate {guid} is at {start}+{len} bytes, which is not whole {PAGE}-byte \
             blocks — refusing it"
        );
        return None;
    }
    let device_blocks = handle.block_count();
    let Some(part) = Partition::of(handle, start / PAGE, len / PAGE) else {
        log!(
            "{what}: candidate {guid} is at {start}+{len} on a device of {} bytes — refusing to \
             read past the end of it",
            device_blocks.saturating_mul(PAGE)
        );
        return None;
    };
    Some(init(part))
}

impl Cached {
    pub fn partition(&self) -> &Partition {
        &self.part
    }

    /// Locks cache then device, in that order.
    pub fn lock(&self) -> PageCacheGuard<'_> {
        let cache = self.cache.lock();
        let dev = self.part.handle().lock();
        PageCacheGuard { cache, dev, part: &self.part }
    }

    /// Reads a block of this partition past the cache; locks only the device.
    #[must_use = "a failed read leaves the buffer holding whatever it held before"]
    pub fn raw_read(&self, block: u64, buf: &mut [u8; PAGE_BYTES]) -> BlockResult {
        self.part.read_blocks(block, 1, buf)
    }

    /// Writes a block past the cache's slots but not its flush debt: the bytes
    /// land in the device's write cache, which the next sync owes a flush.
    #[must_use = "a failed write did not reach the device"]
    pub fn raw_write(&self, block: u64, buf: &[u8; PAGE_BYTES]) -> BlockResult {
        let mut guard = self.lock();
        let at = guard.part.locate(block, 1)?;
        guard.cache.flush.record_write();
        guard.dev.write_blocks(at, 1, buf)
    }
}

pub struct PageCacheGuard<'a> {
    cache: LockGuard<'a, PageCache>,
    dev: LockGuard<'a, Box<dyn BlockDevice>>,
    part: &'a Partition,
}

impl PageCacheGuard<'_> {
    pub fn block_count(&self) -> u64 {
        self.part.block_count()
    }

    pub fn read(&mut self, block: u64) -> Result<&[u8], BlockError> {
        let Self { cache, dev, part } = self;
        cache.read(part, dev.as_mut(), block)
    }

    pub fn write_new(&mut self, block: u64) -> Result<&mut [u8], BlockError> {
        let Self { cache, dev, part } = self;
        cache.write_new(part, dev.as_mut(), block)
    }

    pub fn sync(&mut self) -> BlockResult {
        let Self { cache, dev, .. } = self;
        cache.sync(dev.as_mut())
    }
}

/// 256 pages per chunk so each chunk allocation is exactly 1MB.
const PAGES_PER_CHUNK: usize = 256;
const CHUNK_SIZE: usize = PAGES_PER_CHUNK * 4096;

pub struct PageCache {
    /// Maps a block's identity → slot index; sized by the cached set, never by the partition's block count.
    block_to_slot: HashMap<BlockKey, u32>,
    /// Maps slot index → the block it holds, for sync and eviction; `None` is a slot bound to nothing.
    slot_to_block: Vec<Option<BlockKey>>,
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
}

impl PageCache {
    fn new() -> Self {
        let max_slots = block::metadata_cache_blocks();
        Self {
            // Reserved up front so `index_capacity` reports the real ceiling immediately.
            block_to_slot: HashMap::with_capacity_and_hasher(max_slots, KernelHashState::new()),
            slot_to_block: Vec::with_capacity(max_slots),
            dirty: Vec::with_capacity(max_slots),
            referenced: Vec::with_capacity(max_slots),
            chunks: Vec::with_capacity(max_slots.div_ceil(PAGES_PER_CHUNK)),
            hand: 0,
            max_slots,
            evictions: 0,
            flush: crate::durability::Owed::new(),
        }
    }

    /// Blocks the index has room for; must stay far below the partition's block count.
    pub fn index_capacity(&self) -> usize {
        self.block_to_slot.capacity()
    }

    /// What that index costs: a `(key, slot)` pair plus a control byte per
    /// bucket, over hashbrown's 7/8 load factor.
    pub fn index_bytes(&self) -> usize {
        let buckets = self.index_capacity().div_ceil(7) * 8;
        buckets * (core::mem::size_of::<(BlockKey, u32)>() + 1)
    }

    /// Returns `None` only when every resident slot is dirty and write-back failed.
    fn alloc_slot(&mut self, dev: &mut dyn BlockDevice, key: BlockKey) -> Option<u32> {
        let slot = if self.slot_to_block.len() < self.max_slots {
            let slot = self.slot_to_block.len() as u32;
            self.slot_to_block.push(Some(key));
            self.dirty.push(false);
            self.referenced.push(false);
            if slot as usize / PAGES_PER_CHUNK >= self.chunks.len() {
                self.chunks.push(vec![0u8; CHUNK_SIZE].into_boxed_slice());
            }
            slot
        } else {
            let slot = self.take_victim(dev)?;
            if let Some(evicted) = self.slot_to_block[slot as usize] {
                self.block_to_slot.remove(&evicted);
            }
            self.slot_to_block[slot as usize] = Some(key);
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
        self.block_to_slot.insert(key, slot);
        Some(slot)
    }

    /// Clears the block's index entry so a later reader misses instead of reading stale data.
    fn unbind(&mut self, slot: u32, key: BlockKey) {
        self.block_to_slot.remove(&key);
        self.slot_to_block[slot as usize] = None;
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

    fn slot_of(&self, key: BlockKey) -> Option<u32> {
        self.block_to_slot.get(&key).copied()
    }

    fn read(
        &mut self,
        part: &Partition,
        dev: &mut dyn BlockDevice,
        block: u64,
    ) -> Result<&[u8], BlockError> {
        let key = part.key(block)?;
        if let Some(slot) = self.slot_of(key) {
            self.referenced[slot as usize] = true;
            return Ok(self.slot_data(slot));
        }
        let slot = self.alloc_slot(dev, key).ok_or(BlockError::Device)?;
        let page = self.slot_data_mut(slot);
        if let Err(e) = dev.read_blocks(key.device_block(), 1, page) {
            self.unbind(slot, key);
            return Err(e);
        }
        Ok(self.slot_data(slot))
    }

    fn write_new(
        &mut self,
        part: &Partition,
        dev: &mut dyn BlockDevice,
        block: u64,
    ) -> Result<&mut [u8], BlockError> {
        let key = part.key(block)?;
        let slot = match self.slot_of(key) {
            Some(slot) => {
                self.referenced[slot as usize] = true;
                slot
            }
            None => self.alloc_slot(dev, key).ok_or(BlockError::Device)?,
        };
        self.dirty[slot as usize] = true;
        let page = self.slot_data_mut(slot);
        // A reused slot still holds the evicted block's bytes; zero it before returning.
        page.fill(0);
        Ok(page)
    }

    /// Writes every dirty slot back, coalescing runs of consecutive blocks.
    /// Every address comes from the key the slot was filled under, so a page is
    /// written where it was read from and nowhere else.
    fn sync(&mut self, dev: &mut dyn BlockDevice) -> BlockResult {
        let mut pending: Vec<u32> = (0..self.slot_to_block.len() as u32)
            .filter(|&s| self.dirty[s as usize])
            .collect();

        // Nothing to write is not nothing to flush: a raw write, or a failed predecessor's runs, is still owed.
        if pending.is_empty() && !self.flush.is_owed() {
            return Ok(());
        }
        pending.sort_unstable_by_key(|&s| self.slot_to_block[s as usize]);

        // A dirty slot is bound by construction: `unbind` clears the dirty bit
        // with the key, so nothing here can be resident-and-unbound.
        let at = |cache: &Self, slot: u32| {
            cache.slot_to_block[slot as usize]
                .expect("a dirty slot holds the block it was filled under")
                .device_block()
        };

        let mut buf = vec![0u8; 32 * 4096];
        // Errors combine via `worse`, not first-wins, so a caller sees the one that blocks retry.
        let mut failed: Option<BlockError> = None;
        let mut i = 0;
        while i < pending.len() {
            let start = at(self, pending[i]);
            let mut count = 1usize;

            while i + count < pending.len()
                && at(self, pending[i + count]) == start + count as u64
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

/// Read-fault injection (`pc-unbind-selftest`): refuse one armed block, count served reads of another.
#[cfg(feature = "boot-actuators")]
mod read_fault {
    use core::sync::atomic::{AtomicU64, Ordering};

    use alloc::boxed::Box;

    use crate::block::{BlockDevice, BlockError, BlockResult, DeviceId};

    // `u64::MAX` disarms; no device reaches it.
    pub(super) static FAIL_BLOCK: AtomicU64 = AtomicU64::new(u64::MAX);
    pub(super) static WATCH_BLOCK: AtomicU64 = AtomicU64::new(u64::MAX);
    pub(super) static SERVED: AtomicU64 = AtomicU64::new(0);

    pub(super) struct FaultDevice(pub Box<dyn BlockDevice>);

    fn covers(lba: u64, count: u32, block: u64) -> bool {
        lba <= block && block - lba < count as u64
    }

    impl BlockDevice for FaultDevice {
        fn device_id(&self) -> DeviceId {
            self.0.device_id()
        }

        fn block_count(&self) -> u64 {
            self.0.block_count()
        }

        fn read_blocks(&mut self, lba: u64, count: u32, buf: &mut [u8]) -> BlockResult {
            if covers(lba, count, FAIL_BLOCK.load(Ordering::Relaxed)) {
                return Err(BlockError::Device);
            }
            let read = self.0.read_blocks(lba, count, buf);
            if read.is_ok() && covers(lba, count, WATCH_BLOCK.load(Ordering::Relaxed)) {
                SERVED.fetch_add(1, Ordering::Relaxed);
            }
            read
        }

        fn write_blocks(&mut self, lba: u64, count: u32, buf: &[u8]) -> BlockResult {
            self.0.write_blocks(lba, count, buf)
        }

        fn flush(&mut self) -> BlockResult {
            self.0.flush()
        }
    }
}

/// The un-index control, behind `pc-unbind-selftest`, for `PageCache::read`'s
/// unbind-on-failed-fill. The count is the assertion: after a refused fill, the
/// next read of the same block reaches the device exactly once — a slot left
/// bound answers from its last tenant and reaches it zero times. The byte
/// comparison against the device (past the cache) is the differential half.
/// One guard held throughout, so nothing touches the armed block mid-sequence.
#[cfg(feature = "boot-actuators")]
pub fn unbind_selftest(cached: &Cached) {
    use core::sync::atomic::Ordering;

    let mut guard = cached.lock();
    // The highest non-resident block: read by nothing so far, or long evicted.
    let Some((block, key)) = (0..guard.block_count())
        .rev()
        .filter_map(|b| guard.part.key(b).ok().map(|k| (b, k)))
        .find(|(_, k)| !guard.cache.block_to_slot.contains_key(k))
    else {
        log!("pc-unbind-selftest: FAIL (every device block is resident)");
        return;
    };

    read_fault::SERVED.store(0, Ordering::Relaxed);
    read_fault::WATCH_BLOCK.store(key.device_block(), Ordering::Relaxed);
    read_fault::FAIL_BLOCK.store(key.device_block(), Ordering::Relaxed);
    let refused = guard.read(block).is_err();
    read_fault::FAIL_BLOCK.store(u64::MAX, Ordering::Relaxed);
    if !refused {
        log!("pc-unbind-selftest: FAIL (the injected read fault never fired)");
        return;
    }

    let mut reread = vec![0u8; PAGE_BYTES].into_boxed_slice();
    match guard.read(block) {
        Ok(data) => reread.copy_from_slice(data),
        Err(_) => {
            log!("pc-unbind-selftest: FAIL (the re-read after the failed fill was refused)");
            return;
        }
    }
    let served = read_fault::SERVED.load(Ordering::Relaxed);
    read_fault::WATCH_BLOCK.store(u64::MAX, Ordering::Relaxed);

    let mut raw = vec![0u8; PAGE_BYTES].into_boxed_slice();
    if guard.dev.read_blocks(key.device_block(), 1, &mut raw).is_err() {
        log!("pc-unbind-selftest: FAIL (the ground-truth device read was refused)");
        return;
    }

    if served == 1 && reread[..] == raw[..] {
        log!("pc-unbind-selftest: PASS (block {block}: 1 device read after the failed fill, bytes match the device)");
    } else {
        log!(
            "pc-unbind-selftest: FAIL (block {block}: {served} device reads after the failed \
             fill, bytes {}the device's — the slot stayed bound to a block it never read)",
            if reread[..] == raw[..] { "match " } else { "differ from " }
        );
    }
}

/// The partition-offset control (`pc-partition-offset`): a cache over a view
/// that does not begin at block 0 writes through the view's block numbers, and
/// the bytes have to land at the device's. Read back past the cache and past
/// the view, so only the key the slot was filled under could have offset it.
#[cfg(feature = "boot-actuators")]
pub fn partition_offset_selftest(handle: &block::Handle) {
    let Some(part) = block::Partition::of(handle.clone(), offset_probe::FIRST, offset_probe::BLOCKS)
    else {
        log!("pc-partition-offset: FAIL (the view does not fit on device {})", handle.device_id());
        return;
    };
    let cached = init(part);
    {
        let mut guard = cached.lock();
        let Ok(page) = guard.write_new(offset_probe::AT) else {
            log!("pc-partition-offset: FAIL (the view refused a write at its own block)");
            return;
        };
        page[..offset_probe::MARK.len()].copy_from_slice(offset_probe::MARK);
        if guard.sync().is_err() {
            log!("pc-partition-offset: FAIL (the write-back was refused)");
            return;
        }
    }

    let mut at_offset = vec![0u8; PAGE_BYTES];
    let mut at_zero = vec![0u8; PAGE_BYTES];
    let device_block = offset_probe::FIRST + offset_probe::AT;
    if handle.lock().read_blocks(device_block, 1, &mut at_offset).is_err() {
        log!("pc-partition-offset: FAIL (device block {device_block} would not read back)");
        return;
    }
    if handle.lock().read_blocks(offset_probe::AT, 1, &mut at_zero).is_err() {
        log!("pc-partition-offset: FAIL (device block {} would not read back)", offset_probe::AT);
        return;
    }
    let marked = |b: &[u8]| b[..offset_probe::MARK.len()] == *offset_probe::MARK;
    log!(
        "pc-partition-offset: view +{} block {} landed_at_{device_block}={} at_block_{}={}",
        offset_probe::FIRST,
        offset_probe::AT,
        marked(&at_offset),
        offset_probe::AT,
        marked(&at_zero)
    );
}

/// Mirrored by `tests/common/storage.rs`, which reads the same two device
/// blocks off the image after the guest has gone.
#[cfg(feature = "boot-actuators")]
mod offset_probe {
    pub const FIRST: u64 = 3000;
    pub const BLOCKS: u64 = 64;
    pub const AT: u64 = 7;
    pub const MARK: &[u8] = b"TOYOS-PARTITION-OFFSET";
}
