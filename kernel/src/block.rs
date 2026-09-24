//! One device object per physical device; a consumer takes a [`Handle`] and a
//! [`Partition`] over it and can name no block outside its span.
//!
//! **A block has one holder.** Every view is made by [`Partition::of`], which
//! refuses a span any live view holds a block of, and the view's hold goes
//! with the last clone of it. The kernel's mounts hold their partitions this
//! way ([`Holder::Kernel`]), and so does a process's partition claim
//! ([`Holder::Claim`]): a mounted partition cannot be claimed, a claimed one
//! cannot be mounted or claimed twice, and no kernel cache ever holds a block
//! a claim writes — so a claim's transfers need no invalidation and read the
//! device.
//!
//! **A flush answers for its writer's own writes.** A device's flush is the
//! whole device's, but every write and flush goes through a [`Locked`] device
//! that knows whose it is: each holder keeps an account of the writes it had
//! reported since its last flush, against the disk's loss count
//! ([`BlockDevice::losses`]), and a flush fails for exactly the holders whose
//! writes were reported before a loss — at each one's own next flush, once,
//! whoever flushes first.
//!
//! Lock order: a consumer's own lock, then [`Handle::lock`]; never the reverse,
//! and never two devices at once. [`DEVICES`] is a leaf taken alone, and a
//! device's holds are taken last, alone or under its device's lock.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::mm::PAGE_SIZE;
use crate::scheduler::Operation;
use crate::sync::{Lock, LockGuard};
use crate::time::{Budget, Cadence, Deadline, Duration};

/// Unique identifier for a block device; [`register`] issues each at most once.
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

    /// How many times this disk has lost writes it reported complete — a
    /// device that left owing a flush and was taken back
    /// (`toyos_xhci::flush`) — as the device that ran the last operation to
    /// return counts it. Never decreases; a disk that is never taken back
    /// answers 0.
    fn losses(&self) -> u64;
}

struct Device {
    id: DeviceId,
    blocks: u64,
    dev: Lock<Box<dyn BlockDevice>>,
    holds: Lock<Holds>,
}

/// Every span a [`Partition`] view holds, and every writer's account.
struct Holds {
    spans: Vec<Span>,
    /// The writes made through [`Handle::lock`], which names no span.
    unspanned: Account,
}

/// One writer's writes on a device since its last flush, for the flush that
/// answers it.
#[derive(Clone, Copy, Default)]
struct Account {
    /// The disk's loss count when this writer's writes since its last flush
    /// that succeeded were reported; `None` for none.
    unflushed: Option<u64>,
    /// Writes of this writer's were reported before the disk lost them.
    lost: bool,
}

impl Account {
    /// A write reported complete while the disk's count was `losses`.
    fn wrote(&mut self, losses: u64) {
        if self.unflushed.is_some_and(|at| at < losses) {
            self.lost = true;
        }
        self.unflushed = Some(losses);
    }

    /// A flush of the device succeeded while its count was `losses`: every
    /// write reported before it is durable, except one reported before a loss.
    fn flushed(&mut self, losses: u64) {
        if self.unflushed.take().is_some_and(|at| at < losses) {
            self.lost = true;
        }
    }

    /// Whether this writer's flush fails for a loss: once.
    fn told(&mut self) -> bool {
        core::mem::take(&mut self.lost)
    }
}

/// Whose writes a [`Locked`] device carries.
#[derive(Clone, Copy)]
enum Writer {
    /// The view whose span begins at this device block.
    View(u64),
    Unspanned,
}

/// A shared handle; there is never a second object for one [`DeviceId`].
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
    let holds = Holds { spans: Vec::new(), unspanned: Account::default() };
    let handle =
        Handle(Arc::new(Device { id, blocks, dev: Lock::new(dev), holds: Lock::new(holds) }));
    devices.insert(id, handle.clone());
    log!("block: device {id} registered, {blocks} blocks");
    Some(handle)
}

pub fn open(id: DeviceId) -> Option<Handle> {
    DEVICES.lock().get(&id).cloned()
}

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

    /// The device itself, with its queue serialised for as long as the guard
    /// lives, for a writer that holds no view of it.
    pub fn lock(&self) -> Locked<'_> {
        Locked { dev: self.0.dev.lock(), device: &*self.0, writer: Writer::Unspanned }
    }
}

/// A device with its queue serialised, whose writes and flushes are one
/// writer's: a flush through it fails if a write of that writer's was reported
/// before the disk lost it, and for no other writer's.
pub struct Locked<'a> {
    dev: LockGuard<'a, Box<dyn BlockDevice>>,
    device: &'a Device,
    writer: Writer,
}

impl Locked<'_> {
    /// `f` over this writer's account, under the device's holds.
    fn account<R>(&self, f: impl FnOnce(&mut Account, Option<Holder>) -> R) -> R {
        let mut holds = self.device.holds.lock();
        match self.writer {
            Writer::Unspanned => f(&mut holds.unspanned, None),
            Writer::View(first) => {
                let span = holds
                    .spans
                    .iter_mut()
                    .find(|span| span.first == first)
                    .expect("a live view's span is held until its last clone drops");
                let holder = span.holder;
                f(&mut span.account, Some(holder))
            }
        }
    }
}

impl BlockDevice for Locked<'_> {
    fn device_id(&self) -> DeviceId {
        self.dev.device_id()
    }

    fn block_count(&self) -> u64 {
        self.dev.block_count()
    }

    fn read_blocks(&mut self, lba: u64, count: u32, buf: &mut [u8]) -> BlockResult {
        self.dev.read_blocks(lba, count, buf)
    }

    fn write_blocks(&mut self, lba: u64, count: u32, buf: &[u8]) -> BlockResult {
        let done = self.dev.write_blocks(lba, count, buf);
        if done.is_ok() {
            let losses = self.dev.losses();
            self.account(|account, _| account.wrote(losses));
        }
        done
    }

    fn flush(&mut self) -> BlockResult {
        self.dev.flush()?;
        let losses = self.dev.losses();
        {
            let mut holds = self.device.holds.lock();
            holds.unspanned.flushed(losses);
            for span in &mut holds.spans {
                span.account.flushed(losses);
            }
        }
        let Some(holder) = self.account(|account, holder| account.told().then_some(holder)) else {
            return Ok(());
        };
        let whose = match holder {
            Some(holder) => alloc::format!("{holder}"),
            None => alloc::string::String::from("a writer holding no view"),
        };
        log!(
            "block: device {}: writes {whose} made before its disk came back owing a flush \
             may not have survived, and its flush says so",
            self.device.id
        );
        Err(BlockError::Device)
    }

    fn losses(&self) -> u64 {
        self.dev.losses()
    }
}

/// A block of one partition of one device, minted only by [`Partition::key`].
/// `partition` is judged (`pc-partition-offset`): it puts a write where the
/// slot was filled from. `device` is not, and no test can see it — one cache
/// serves one partition, so two devices in one map is already unrepresentable.
/// The field order is the sort order, so a run of one view's keys is a run on
/// the device.
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

/// Who holds a span of a device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Holder {
    /// Something in this kernel — a mount, a probe — named for the log.
    Kernel(&'static str),
    /// A process, through a partition claim.
    Claim,
}

impl core::fmt::Display for Holder {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Kernel(what) => write!(f, "the kernel ({what})"),
            Self::Claim => f.write_str("a process's partition claim"),
        }
    }
}

/// One held span, `first..end` in device blocks, never empty, and its
/// holder's account of its writes.
struct Span {
    first: u64,
    end: u64,
    holder: Holder,
    account: Account,
}

/// A span's hold, shared by every clone of the view that took it: a transfer
/// in flight on a clone keeps the span held after the view it was cloned from
/// has gone.
struct Hold {
    device: Arc<Device>,
    first: u64,
}

impl Drop for Hold {
    fn drop(&mut self) {
        // `first` names one span, since no two overlap and none is empty.
        self.device.holds.lock().spans.retain(|span| span.first != self.first);
    }
}

/// Why [`Partition::of`] made no view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewRefused {
    /// The span is empty or runs off the device.
    OffDevice,
    /// A block of it is already held.
    Held(Holder),
}

/// One consumer's view of one span of a device, in whole [`BlockDevice`] blocks:
/// a read at `block_count()` is refused by name, never served from past its end.
#[derive(Clone)]
pub struct Partition {
    handle: Handle,
    first_block: u64,
    blocks: u64,
    _hold: Arc<Hold>,
}

impl Partition {
    /// `blocks` blocks from `first_block`, held for `holder` until the last
    /// clone of the view drops — or refused when the span is empty, off the
    /// device, or overlaps a span another view holds.
    pub fn of(
        handle: Handle,
        first_block: u64,
        blocks: u64,
        holder: Holder,
    ) -> Result<Self, ViewRefused> {
        let end = first_block
            .checked_add(blocks)
            .filter(|&end| blocks != 0 && end <= handle.block_count())
            .ok_or(ViewRefused::OffDevice)?;
        let mut holds = handle.0.holds.lock();
        if let Some(held) =
            holds.spans.iter().find(|span| span.first < end && first_block < span.end)
        {
            return Err(ViewRefused::Held(held.holder));
        }
        let account = Account::default();
        holds.spans.push(Span { first: first_block, end, holder, account });
        drop(holds);
        let hold = Arc::new(Hold { device: handle.0.clone(), first: first_block });
        Ok(Self { handle, first_block, blocks, _hold: hold })
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

    /// The identity of `block` in this view, or a refusal past its end.
    pub fn key(&self, block: u64) -> Result<BlockKey, BlockError> {
        self.locate(block, 1)?;
        Ok(BlockKey { device: self.device_id(), partition: self.first_block, block })
    }

    /// Whether `count` blocks from `block` end inside the view — asked of a
    /// caller's numbers, so a refusal is the caller's answer and not a line in
    /// the kernel's log.
    pub fn fits(&self, block: u64, count: u32) -> bool {
        block.checked_add(count as u64).is_some_and(|end| end <= self.blocks)
    }

    /// The device, serialised, with every write and flush through it this
    /// view's own.
    pub fn lock(&self) -> Locked<'_> {
        let device = &*self.handle.0;
        Locked { dev: device.dev.lock(), device, writer: Writer::View(self.first_block) }
    }

    /// The device block `block` names, or a refusal past the view's end.
    pub fn locate(&self, block: u64, count: u32) -> Result<u64, BlockError> {
        if self.fits(block, count) {
            Ok(self.first_block + block)
        } else {
            Err(self.past_end(block, count))
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
        self.lock().read_blocks(at, count, buf)
    }

    #[must_use = "a failed write did not reach the device"]
    pub fn write_blocks(&self, block: u64, count: u32, buf: &[u8]) -> BlockResult {
        let at = self.locate(block, count)?;
        self.lock().write_blocks(at, count, buf)
    }

    #[must_use = "a failed flush means the writes before it are not durable"]
    pub fn flush(&self) -> BlockResult {
        self.lock().flush()
    }
}

/// The duplicate-id control (`block-duplicate-id`): the impostor fills every
/// read with its own mark, so a registry that took it is caught serving that
/// mark for a device it is not.
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
        fn losses(&self) -> u64 {
            0
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

/// Blocks the metadata cache may hold, per instance: sized from memory and
/// never from the device, so N instances claim N times this and none evicts
/// across them. N is the role partitions a boot caches — one today, from
/// `page_cache::init`'s one call site — and a foreign volume adds none, since a
/// FAT mount reads through `fat32_adapter::FatDevice` and never a `Cached`.
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
