//! One device object per physical device, registered under the [`DeviceId`] it
//! answers to; every consumer takes a [`Handle`] and a [`Partition`] over it,
//! so none owns a device and none can name a block outside its span.
//!
//! Lock order: a consumer's own lock, then [`Handle::lock`]; never the reverse,
//! and never two devices at once. [`DEVICES`] is a leaf taken alone.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
#[cfg(feature = "boot-actuators")]
use alloc::vec::Vec;

use crate::mm::PAGE_SIZE;
use crate::scheduler::Operation;
use crate::sync::{Lock, LockGuard};
use crate::time::{Budget, Cadence, Deadline, Duration};

/// Unique identifier for a block device; the page cache keys every block on it.
pub type DeviceId = u32;

/// How long one operation on a block device may spend inside the device before it is refused.
// Bounds the longest stretch held pinned with preemption off; raising it lengthens audio-path stalls directly.
pub const OPERATION: Budget = Budget::of(
    Duration::from_secs(2),
    "the block-device operation is refused as one that would block, and the \
     caller's own give-up policy decides whether to ask again",
);

/// Total time a sequence of block-device operations may spend before the volume is declared failed.
pub const DEADMAN: Budget = Budget::of(
    Duration::from_secs(120),
    "the run of retries ends, the volume is declared failed, and the caller is \
     told with a device error rather than another ask-again",
);

/// How soon the retry loop may ask again after its first refused attempt.
pub const RETRY_SOONEST: Cadence = Cadence::every(
    Duration::from_millis(10),
    "one quantum parked between attempts; the refusal itself issued nothing",
);

/// The ceiling the retry interval doubles up to.
pub const RETRY_SLOWEST: Cadence = Cadence::every(
    OPERATION.duration(),
    "between two pinned attempts the machine gets at least as long as one \
     attempt may pin",
);

/// Backoff for attempt `attempt` (>= 2): doubles from `RETRY_SOONEST` to `RETRY_SLOWEST`.
pub(crate) fn backoff_step(attempt: u32) -> Duration {
    Duration::from_nanos(
        RETRY_SOONEST
            .nanos()
            .saturating_mul(1u64 << (attempt - 2).min(32))
            .min(RETRY_SLOWEST.nanos()),
    )
}

/// Parks the calling task between two refused block-operation attempts; the first call only yields.
/// Must not be called by a caller already holding a completion arm — arming a second one panics.
pub(crate) fn between_attempts(attempt: u32) {
    if attempt <= 1 {
        crate::scheduler::yield_now();
        return;
    }
    let parkable = crate::scheduler::Parkable::at_entry();
    let Some(handle) = crate::sched::driver::current_handle() else {
        return;
    };
    let deadline = Deadline::at(crate::clock::now() + backoff_step(attempt));
    let _ = crate::completion::wait_until(
        &parkable,
        crate::completion::Subject::of(handle.watch()),
        crate::completion::Token::new(0),
        toyos_sched::task::WaitClass::Other,
        deadline,
        || false,
    );
}

/// Declares the running context inside one block-device operation, bounded by `OPERATION`, until the guard drops.
// An absolute deadline, not a relative duration: it crosses into a driver that loops, and re-basing per command would bound each command instead of the whole operation.
#[must_use = "the operation lasts exactly as long as this guard"]
pub fn begin_operation() -> Operation {
    Operation::begin(Deadline::at(crate::clock::now() + OPERATION.duration()))
}

/// Why an operation on this trait did not complete.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockError {
    /// The device itself failed the operation.
    Device,
    /// Refused before it was attempted because the operation's time budget expired; safe to retry.
    BudgetExpired,
}

impl BlockError {
    /// Combines two failures from one composed operation: `Device` always wins.
    pub fn worse(self, other: Self) -> Self {
        match (self, other) {
            (Self::Device, _) | (_, Self::Device) => Self::Device,
            _ => Self::BudgetExpired,
        }
    }
}

pub type BlockResult = Result<(), BlockError>;

/// Block-oriented storage device interface; all I/O is in whole 4KB blocks.
pub trait BlockDevice: Send {
    fn device_id(&self) -> DeviceId;
    fn block_count(&self) -> u64;

    /// Reads `count` contiguous blocks starting at `lba` into `buf` (`buf.len()` must equal `count as usize * 4096`).
    #[must_use = "a failed read leaves the buffer holding whatever it held before"]
    fn read_blocks(&mut self, lba: u64, count: u32, buf: &mut [u8]) -> BlockResult;

    /// Writes `count` contiguous blocks starting at `lba` from `buf` (`buf.len()` must equal `count as usize * 4096`).
    #[must_use = "a failed write did not reach the device"]
    fn write_blocks(&mut self, lba: u64, count: u32, buf: &[u8]) -> BlockResult;

    /// Flush any hardware write caches to persistent storage.
    #[must_use = "a failed flush means the writes before it are not durable"]
    fn flush(&mut self) -> BlockResult;
}

/// One physical device, and the lock serialising its queue.
struct Device {
    id: DeviceId,
    blocks: u64,
    dev: Lock<Box<dyn BlockDevice>>,
}

/// A shared handle to one physical device; there is never a second object for
/// one [`DeviceId`].
#[derive(Clone)]
pub struct Handle(Arc<Device>);

static DEVICES: Lock<BTreeMap<DeviceId, Handle>> = Lock::new(BTreeMap::new());

/// Registers `dev` under the id it answers to, or refuses when that id is taken:
/// two devices sharing a number would serve each other's blocks out of one
/// cache, which a plain insert here would arrange in silence.
#[must_use = "a refused registration leaves the device unreachable"]
pub fn register(dev: Box<dyn BlockDevice>) -> Option<Handle> {
    let id = dev.device_id();
    let blocks = dev.block_count();
    let mut devices = DEVICES.lock();
    if let Some(held) = devices.get(&id) {
        log!(
            "block: device {id} is already registered with {} blocks; refusing a second device \
             claiming that number ({blocks} blocks) — one cache keys its pages on it",
            held.0.blocks
        );
        return None;
    }
    let handle = Handle(Arc::new(Device { id, blocks, dev: Lock::new(dev) }));
    devices.insert(id, handle.clone());
    log!("block: device {id} registered, {blocks} blocks");
    Some(handle)
}

/// The registered device with this id.
pub fn open(id: DeviceId) -> Option<Handle> {
    DEVICES.lock().get(&id).cloned()
}

/// Every device registered so far, by number.
#[cfg(feature = "boot-actuators")]
pub fn registered() -> Vec<Handle> {
    DEVICES.lock().values().cloned().collect()
}

impl Handle {
    pub fn device_id(&self) -> DeviceId {
        self.0.id
    }

    /// What the driver reported at registration; never re-asked, so a view's bound cannot move under it.
    pub fn block_count(&self) -> u64 {
        self.0.blocks
    }

    /// The device itself, with its queue serialised for as long as the guard lives.
    pub fn lock(&self) -> LockGuard<'_, Box<dyn BlockDevice>> {
        self.0.dev.lock()
    }
}

/// A block of one partition of one device: the whole of a cached page's
/// identity, minted only by [`Partition::key`], so a key naming one view's
/// block cannot be built by another. The field order is the sort order, so a
/// run of one view's keys is a run on the device.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct BlockKey {
    device: DeviceId,
    partition: u64,
    block: u64,
}

impl BlockKey {
    /// Where the block is on the device, which is what a transfer takes.
    pub fn device_block(self) -> u64 {
        self.partition + self.block
    }
}

/// One consumer's view of one span of a device, in whole [`BlockDevice`] blocks:
/// a read at `block_count()` is refused by name, never served from past its end.
#[derive(Clone)]
pub struct Partition {
    handle: Handle,
    first_block: u64,
    blocks: u64,
}

impl Partition {
    /// The whole device as one view.
    pub fn whole(handle: Handle) -> Self {
        let blocks = handle.block_count();
        Self { handle, first_block: 0, blocks }
    }

    /// `blocks` blocks from `first_block`, or `None` when that span is off the device.
    pub fn of(handle: Handle, first_block: u64, blocks: u64) -> Option<Self> {
        let end = first_block.checked_add(blocks)?;
        (end <= handle.block_count()).then_some(Self { handle, first_block, blocks })
    }

    pub fn device_id(&self) -> DeviceId {
        self.handle.device_id()
    }

    pub fn first_block(&self) -> u64 {
        self.first_block
    }

    pub fn block_count(&self) -> u64 {
        self.blocks
    }

    pub fn handle(&self) -> &Handle {
        &self.handle
    }

    /// The identity of `block` in this view, or a refusal past its end.
    pub fn key(&self, block: u64) -> Result<BlockKey, BlockError> {
        self.locate(block, 1)?;
        Ok(BlockKey { device: self.device_id(), partition: self.first_block, block })
    }

    /// The device block `block` names, or a refusal past the view's end.
    pub fn locate(&self, block: u64, count: u32) -> Result<u64, BlockError> {
        match block.checked_add(count as u64) {
            Some(end) if end <= self.blocks => Ok(self.first_block + block),
            _ => Err(self.past_end(block, count)),
        }
    }

    #[cold]
    #[inline(never)]
    fn past_end(&self, block: u64, count: u32) -> BlockError {
        log!(
            "block: device {} partition at +{}: refusing {count} block(s) at {block} on a view \
             of {} blocks",
            self.handle.device_id(),
            self.first_block,
            self.blocks
        );
        BlockError::Device
    }

    #[must_use = "a failed read leaves the buffer holding whatever it held before"]
    pub fn read_blocks(&self, block: u64, count: u32, buf: &mut [u8]) -> BlockResult {
        let at = self.locate(block, count)?;
        self.handle.lock().read_blocks(at, count, buf)
    }

    #[must_use = "a failed write did not reach the device"]
    pub fn write_blocks(&self, block: u64, count: u32, buf: &[u8]) -> BlockResult {
        let at = self.locate(block, count)?;
        self.handle.lock().write_blocks(at, count, buf)
    }

    #[must_use = "a failed flush means the writes before it are not durable"]
    pub fn flush(&self) -> BlockResult {
        self.handle.lock().flush()
    }
}

/// The duplicate-id control (`block-duplicate-id`): the impostor answers to a
/// registered number and fills every read with its own mark, so a registry that
/// took it is caught serving that mark for a device it is not.
#[cfg(feature = "boot-actuators")]
pub fn duplicate_id_selftest() {
    use alloc::vec;

    const MARK: &[u8] = b"impostor";

    struct Impostor {
        id: DeviceId,
        blocks: u64,
    }

    impl BlockDevice for Impostor {
        fn device_id(&self) -> DeviceId {
            self.id
        }
        fn block_count(&self) -> u64 {
            self.blocks
        }
        fn read_blocks(&mut self, _lba: u64, _count: u32, buf: &mut [u8]) -> BlockResult {
            buf.fill(0);
            buf[..MARK.len()].copy_from_slice(MARK);
            Ok(())
        }
        fn write_blocks(&mut self, _lba: u64, _count: u32, _buf: &[u8]) -> BlockResult {
            Ok(())
        }
        fn flush(&mut self) -> BlockResult {
            Ok(())
        }
    }

    let before = registered();
    let Some(first) = before.first().cloned() else {
        log!("block-duplicate-id: FAIL (this boot registered no block device)");
        return;
    };
    let id = first.device_id();
    let refused = register(Box::new(Impostor { id, blocks: first.block_count() })).is_none();

    let mut buf = vec![0u8; PAGE_SIZE as usize];
    let served = open(id).is_some_and(|h| h.lock().read_blocks(0, 1, &mut buf).is_ok());
    let by_impostor = buf[..MARK.len()] == *MARK;
    log!(
        "block-duplicate-id: device {id} claimed twice, second registration refused={refused}, \
         devices {} before and {} after, block 0 served={served} by_impostor={by_impostor}",
        before.len(),
        registered().len()
    );
}

/// Blocks the filesystem metadata cache may hold, per cache instance: it is
/// sized from memory and never from the device, so N instances claim N times
/// this and nothing evicts across them.
/// Must stay under 14,336 or the hashbrown index crosses the 16,384-bucket bound `nvme_large_device` asserts.
pub fn metadata_cache_blocks() -> usize {
    if crate::actuator::test_small_caches() {
        return 64;
    }
    let (total, _) = crate::mm::pmm::stats();
    (((total / 32) / PAGE_SIZE) as usize).clamp(64, 4096)
}

/// Pages the file data cache may hold.
pub fn file_cache_pages() -> usize {
    if crate::actuator::test_small_caches() {
        return 64;
    }
    let (total, _) = crate::mm::pmm::stats();
    (((total / 64) / PAGE_SIZE) as usize).clamp(2048, 65536)
}

/// Flush-latency census backing the `OPERATION`/`DEADMAN` budgets.
pub mod census {
    use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

    use super::DeviceId;

    /// Distinct devices the census can hold apart; extra devices alias into the last slot.
    const DEVICES: usize = 4;
    /// log2-µs latency buckets: bucket `i` holds flushes under `2^i` µs; the last holds everything from 2s up.
    const BUCKETS: usize = 32;

    struct Slot {
        /// The device id plus one, so zero means empty.
        id: AtomicU32,
        flushes: AtomicU64,
        /// Operations refused on the caller's budget (`BlockError::BudgetExpired`).
        expiries: AtomicU64,
    }

    static SLOTS: [Slot; DEVICES] = [const {
        Slot { id: AtomicU32::new(0), flushes: AtomicU64::new(0), expiries: AtomicU64::new(0) }
    }; DEVICES];
    static LATENCY: [AtomicU64; BUCKETS] = [const { AtomicU64::new(0) }; BUCKETS];
    static MAX_NS: AtomicU64 = AtomicU64::new(0);
    /// Last-reported event total; suppresses a repeat print when nothing new happened.
    static REPORTED: AtomicU64 = AtomicU64::new(0);

    fn slot(device: DeviceId) -> &'static Slot {
        let key = device + 1;
        for slot in &SLOTS {
            match slot.id.compare_exchange(0, key, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => return slot,
                Err(held) if held == key => return slot,
                Err(_) => {}
            }
        }
        &SLOTS[DEVICES - 1]
    }

    /// One device flush completed (either way), taking `nanos` of wall clock.
    pub fn flush_took(device: DeviceId, nanos: u64) {
        slot(device).flushes.fetch_add(1, Ordering::Relaxed);
        let micros = nanos / 1_000;
        let bucket = (64 - u64::leading_zeros(micros | 1) as usize).min(BUCKETS - 1);
        LATENCY[bucket].fetch_add(1, Ordering::Relaxed);
        MAX_NS.fetch_max(nanos, Ordering::Relaxed);
    }

    /// One operation on `device` was refused on the caller's budget.
    pub fn budget_expired(device: DeviceId) {
        slot(device).expiries.fetch_add(1, Ordering::Relaxed);
    }

    /// Latency ceiling (µs) at or below which `want` percent of `total` samples fall.
    fn percentile(counts: &[u64; BUCKETS], total: u64, want: u64) -> u64 {
        let mut seen = 0u64;
        for (i, &count) in counts.iter().enumerate() {
            seen += count;
            if seen * 100 >= total * want {
                return 1u64 << i;
            }
        }
        1u64 << (BUCKETS - 1)
    }

    /// Prints the census once per batch of new events; called at process exit.
    pub fn print_if_moved() {
        let mut counts = [0u64; BUCKETS];
        let mut total = 0u64;
        for (bucket, count) in LATENCY.iter().zip(counts.iter_mut()) {
            *count = bucket.load(Ordering::Relaxed);
            total += *count;
        }
        let mut events = total;
        for slot in &SLOTS {
            events += slot.expiries.load(Ordering::Relaxed);
        }
        if events == 0 || REPORTED.swap(events, Ordering::Relaxed) == events {
            return;
        }
        for slot in &SLOTS {
            let id = slot.id.load(Ordering::Relaxed);
            if id == 0 {
                continue;
            }
            crate::log!(
                "flush-census: dev={} flushes={} expiries={}",
                id - 1,
                slot.flushes.load(Ordering::Relaxed),
                slot.expiries.load(Ordering::Relaxed),
            );
        }
        if total > 0 {
            crate::log!(
                "flush-census: p50<={}us p99<={}us max={}us of {total} flushes",
                percentile(&counts, total, 50),
                percentile(&counts, total, 99),
                MAX_NS.load(Ordering::Relaxed) / 1_000,
            );
        }
    }
}
