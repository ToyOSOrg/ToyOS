use crate::mm::PAGE_SIZE;
use crate::scheduler::Operation;
use crate::time::{Budget, Cadence, Deadline, Duration};

/// Unique identifier for a block device, used as page cache key.
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


/// Blocks the filesystem metadata cache may hold.
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
