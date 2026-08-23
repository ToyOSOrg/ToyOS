use crate::mm::PAGE_SIZE;
use crate::scheduler::Operation;
use crate::time::{Budget, Cadence, Deadline, Duration};

/// Unique identifier for a block device, used as page cache key.
pub type DeviceId = u32;

/// How long one operation on a block device may spend inside the device before
/// it is refused.
///
/// **The number is the caller's and not the driver's, and that split is the
/// whole point.** A driver's own bound covers *one* device round trip —
/// `USB_TIMEOUT_NS` is 2 s in `drivers/xhci` — and says nothing about the
/// composition above it: one `read_blocks` of N blocks is `ceil(N / 8)` SCSI
/// commands, each of which may be issued three times with a Reset Recovery
/// between the attempts, and each of those is three phases with a bound of its
/// own. So a device that answers *every* transfer, just slowly, holds a caller
/// for as long as the work takes and there is nothing above the driver that
/// says how long that may be. This is that number.
///
/// **It is what makes a shipped daemon's give-up policy reachable**, which is
/// the reason it exists rather than a consequence of it. `/bin/logd`'s
/// `LOG_WRITE_BUDGET` is 5 s and it is measured in userland *around the
/// syscall*: a syscall that has not returned cannot be given up on, so every
/// bound below it is what decides whether that policy runs at all. Its own doc
/// used to name `USB_TIMEOUT_NS` as the thing that turns a stick that stopped
/// answering into an `Err` — true of a dead device and never true of a slow
/// one, because that bound is never reached by a device that answers.
///
/// **2 s, and the derivation is two terms.** Below: one whole
/// `USB_TIMEOUT_NS`, so a caller that has spent more than a single transfer's
/// entire allowance on commands that are *completing* is talking to a device
/// too slow to serve. Above: the refusal is
/// taken between commands and never inside one, so the overshoot is the command
/// in flight — one more transfer bound at worst — and `2 + 2` leaves a second
/// of the daemon's 5 s for it to notice with.
///
/// **What the derivation does not cover, and this doc used to claim it did.**
/// The clock is [`crate::clock::now`], which is the TSC, and a TCG guest's TSC
/// advances with the *host's* real time; [`begin_operation`] is also called
/// above the `XHCI` ticket lock, so lock-wait and any host descheduling of the
/// vCPU thread are charged to this budget. A healthy device therefore can reach
/// it — the same is now recorded one layer down for `USB_TIMEOUT_NS` — and what
/// the caller is told when it does is the sentence below.
///
/// **The term the two-part derivation is missing is the recovery, and since
/// owner ruling 2026-08-23 the recovery lives one level up.** "One whole
/// `USB_TIMEOUT_NS`" is the allowance for transfers that *complete*; a transfer
/// that breached its own bound did not, and what the driver does next is a Reset
/// Recovery (`xhci/wait/msc.rs`'s `scsi`, and its own doc on why recovering and
/// then reporting failure loses a write the device would have taken). With this
/// budget equal to that bound, one breached transfer spends all of it, so the
/// *re-issue* after a timeout-induced break is never this operation's: it is
/// refused unissued, the operation answers [`BlockError::BudgetExpired`], and
/// the retry belongs to the caller above the locks — `object/ops.rs`'s `fsync`
/// loop, bounded by [`DEADMAN`] — where the CPU is not pinned and can yield
/// between attempts. Measured 2026-08-22, 1 red in 73 full 12-wide suites,
/// `esp_filesystem`'s `fsync` on `/log`; the identical break was absorbed by
/// the in-driver retry on CI on 2026-08-13, before this constant existed.
///
/// **This number is a slowness detector and never a death sentence, and it is
/// the pin.** `issues/audio/disk-wait-pins-a-cpu.md`: for the whole of one
/// operation the CPU sits up to four ticket spinlocks deep with preemption off,
/// so this constant is the longest a single pinned stretch may grow — raising
/// it lengthens an audio-path stall directly, which is why the retries a slow
/// device needs are bought as *more short operations* rather than one long one.
/// Not an agent's number to move.
///
/// **A [`Budget`] and not a [`crate::time::Tripwire`]**: expiry is a degraded
/// answer, named. The operation is refused, the device is *not* marked failed —
/// nothing was in flight when the refusal was taken — and the caller is told
/// which of the two happened: [`BlockError::BudgetExpired`], which reaches
/// userland as `SyscallError::WouldBlock` and never as
/// [`SyscallError::Io`](toyos_abi::syscall::SyscallError::Io). Until
/// 2026-08-22 it did not, and `/bin/logd` ended a boot's log for a stick that
/// was answering.
pub const OPERATION: Budget = Budget::of(
    Duration::from_secs(2),
    "the block-device operation is refused as one that would block, and the \
     caller's own give-up policy decides whether to ask again",
);

/// The total a *sequence* of block-device operations may spend making one
/// durability request true before the volume is declared failed.
///
/// **The deadman, and the one legal reader is the retry loop above every
/// lock** — `object/ops.rs`'s `fsync`. [`OPERATION`] bounds one pinned attempt;
/// this bounds the run of attempts, taken between them, where no spinlock is
/// held and the loop yields the CPU. Its job is catching a *hung* device — one
/// that answers resets and never completes work — not a slow one: a slow device
/// keeps answering attempts inside their own budgets and never comes near it.
///
/// **Expiry is the third of exactly three ways a volume may be declared
/// failed**, beside a device error status and a reset escalation that itself
/// failed. A single attempt's elapsed time is never one of them: a timeout
/// means "not durable *yet*", and only the device's own word means "cannot be
/// made durable". PostgreSQL post-fsyncgate, ZFS (`zio_slow_io_ms` against its
/// 300 s hung-I/O deadman) and Linux's SCSI/NVMe error handling all draw the
/// same line, and this kernel drew it on the other side once: 1 red in 73
/// suites, a boot's log ended for a stick that answered every transfer.
///
/// **120 s, and the derivation is three fences.** Below: it must hold the worst
/// *recoverable* stall ever recorded on this path with room to spare — the
/// 2026-08-13 stick answered SYNCHRONIZE CACHE 280 ms after a 2 s transport
/// break, the 2026-08-22 red spent 2.1 s inside one `SYS_FSYNC`, and the
/// healthy distribution [`census`] measures sits well under one attempt's
/// bound (2026-08-23, dev host, full 12-wide `cargo test`, busiest guest's 647
/// flushes: p50 ≤ 512 µs, p99 ≤ 16 ms, max 87.5 ms — 23× under `OPERATION`) —
/// so 120 s is ~30 whole hung-attempt cycles at the backoff's ceiling, not a
/// tuned fit. Above: ZFS ships 300 s for the same job, and a laptop being
/// flashed from this stick deserves an answer while somebody is still
/// watching; 120 s is the round number between. It deliberately no longer
/// fits inside `/bin/logd`'s old 5 s syscall-side bound: that bound's slowness
/// half moved here, and logd's policy now treats a slow-but-answered round as
/// degraded rather than dead.
pub const DEADMAN: Budget = Budget::of(
    Duration::from_secs(120),
    "the run of retries ends, the volume is declared failed, and the caller is \
     told with a device error rather than another ask-again",
);

/// How soon the retry loop may ask again after its first refused attempt.
///
/// One scheduler quantum: the common producer of a refused budget is the
/// caller's own lost time — lock-wait, or the host descheduling the vCPU — and
/// both are usually over within one. The first retry is nearly free either
/// way: a refusal is taken before anything is issued.
pub const RETRY_SOONEST: Cadence = Cadence::every(
    Duration::from_millis(10),
    "one quantum parked between attempts; the refusal itself issued nothing",
);

/// The ceiling the retry interval doubles up to.
///
/// Exactly one [`OPERATION`]: every attempt against a hung-but-resetting
/// device can pin a CPU for up to that long
/// (`issues/audio/disk-wait-pins-a-cpu.md`), so a floor of the same width
/// between attempts caps the pin's duty cycle at half — the audio path is
/// never held more than every other slice of the run, however long the
/// deadman lets it go on.
pub const RETRY_SLOWEST: Cadence = Cadence::every(
    OPERATION.duration(),
    "between two pinned attempts the machine gets at least as long as one \
     attempt may pin",
);

/// Give the CPU away between two refused block-operation attempts.
///
/// The one place the retry cadence is spent, shared by every loop that turns a
/// [`BlockError::BudgetExpired`] into another attempt on a fresh budget:
/// `object/ops.rs`'s `SYS_FSYNC` and `writeback`'s close-time drain. The first
/// retry only yields — the budget was usually spent by lock-wait or a
/// descheduled vCPU, both over by the next slice — and every later one parks,
/// doubling from [`RETRY_SOONEST`] to [`RETRY_SLOWEST`] so a hung-but-resetting
/// device costs the machine a pinned attempt at most every other attempt-width.
///
/// **Nothing here holds a lock and nothing here is pinned**, which is the whole
/// reason the loop that calls it lives above every lock: `attempt` is this run's
/// count, and the park is on the caller's own task watch, where nothing posts,
/// so the deadline is the whole of the wait. A context with no task handle (a
/// boot phase) cannot park and returns at once — its caller must be a task, and
/// both callers are.
pub(crate) fn between_attempts(attempt: u32) {
    if attempt <= 1 {
        crate::scheduler::yield_now();
        return;
    }
    let step = RETRY_SOONEST
        .nanos()
        .saturating_mul(1u64 << (attempt - 2).min(32))
        .min(RETRY_SLOWEST.nanos());
    let parkable = crate::scheduler::Parkable::at_entry();
    let Some(handle) = crate::sched::driver::current_handle() else {
        return;
    };
    let deadline = Deadline::at(crate::clock::now() + Duration::from_nanos(step));
    let _ = crate::completion::wait_until(
        &parkable,
        crate::completion::Subject::of(handle.watch()),
        crate::completion::Token::new(0),
        toyos_sched::task::WaitClass::Other,
        deadline,
        || false,
    );
}

/// Declare the running context inside one block-device operation, bounded by
/// [`OPERATION`], until the guard drops.
///
/// Established by the [`BlockDevice`] implementation, which is the layer that
/// knows one call is one operation, and *recovered* by the driver below it
/// rather than handed to it: [`Operation`] carries why owner ruling 1B put the
/// deadline on the running context instead of in an argument, and what else
/// rides the same word.
///
/// **A [`Deadline`] because it is absolute**: it crosses into a driver that
/// loops, and a relative duration re-based at each command would bound every
/// command instead of the operation.
#[must_use = "the operation lasts exactly as long as this guard"]
pub fn begin_operation() -> Operation {
    Operation::begin(Deadline::at(crate::clock::now() + OPERATION.duration()))
}

/// An operation this trait did not complete, and which of two reasons it was.
///
/// **Two variants and not one bit, because they are not the same fact and the
/// machine's one consumer acts on them differently.** This was one bit until
/// 2026-08-22, on the ground that "above this trait there is exactly one thing
/// to do with the answer — stop, and do not believe the buffer". That is true
/// of the *buffer* and false of the caller: `/bin/logd` gives its volume up
/// permanently on any `Err` from `SYS_FSYNC`, which is right for a stick that
/// cannot flush and wrong for a refusal that means "your budget, not this
/// stick". The measurement that split them (2026-08-22): 1 red in 73 full
/// 12-wide suites, a boot whose peers were up in 1,385 ms spending
/// `syscall_wall=2108ms` inside one `SYS_FSYNC`, and a boot's log ended for a
/// device that was fine.
///
/// It is still not a *vocabulary*. Which endpoint stalled, what the sense key
/// was and whether the device answered at all stay in the driver's own log
/// line, where a triage reads them; the one distinction here is the one a
/// caller can act on.
///
/// It is not [`SyscallError`] because a driver has no business naming a
/// syscall's return. The conversion happens where the two meet:
/// `vfs::FileSystem` answers `SyscallError`, [`SyscallError::Io`] is the
/// variant that exists for [`BlockError::Device`], and
/// [`SyscallError::WouldBlock`] is the one that exists for
/// [`BlockError::BudgetExpired`] — a word the ABI already had, which
/// `rust/library/std/src/sys/fs/toyos.rs` already maps to
/// `io::ErrorKind::WouldBlock`.
///
/// [`SyscallError`]: toyos_abi::syscall::SyscallError
/// [`SyscallError::Io`]: toyos_abi::syscall::SyscallError::Io
/// [`SyscallError::WouldBlock`]: toyos_abi::syscall::SyscallError::WouldBlock
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockError {
    /// The device did not do it. A transfer issued and not completed, a status
    /// the command carried, a controller recovery gave up on, a request past
    /// the end of the medium, a volume slot that is not mounted — every fact
    /// about the hardware or the volume.
    Device,
    /// The operation's [`OPERATION`] budget expired, so this was refused
    /// **before it was attempted**.
    ///
    /// Not a fact about the device: nothing was in flight when the refusal was
    /// taken, no disk was marked failed, and the transport is exactly as the
    /// last operation left it. A caller that can afford to ask again later
    /// loses nothing by doing so, and one that cannot is no worse off than the
    /// single bit left it.
    BudgetExpired,
}

impl BlockError {
    /// Which of two failures stands when one operation composes several.
    ///
    /// [`Device`](Self::Device) wins: an operation one of whose parts the
    /// device refused is a failed operation whatever else expired, and the
    /// honest answer for the whole is the worse of the halves. `page_cache::sync`
    /// is the caller — one write-back is many runs plus a flush.
    pub fn worse(self, other: Self) -> Self {
        match (self, other) {
            (Self::Device, _) | (_, Self::Device) => Self::Device,
            _ => Self::BudgetExpired,
        }
    }
}

pub type BlockResult = Result<(), BlockError>;

/// Block-oriented storage device interface.
///
/// All I/O is in whole 4KB blocks. No byte-level addressing — that's the
/// filesystem's job. The page cache sits between the filesystem and this trait.
///
/// Every method is fallible because every implementation is: an NVMe command
/// carries a status, and a USB stick can stall, refuse, or be pulled out
/// mid-transfer. When these returned `()` the NVMe driver discarded six
/// completion statuses and the page cache filled a slot from a read that had
/// not happened — which is worse than losing the data, because the slot was
/// already labelled with the new block's number and the *previous tenant's*
/// bytes were then served under it.
pub trait BlockDevice: Send {
    fn device_id(&self) -> DeviceId;
    fn block_count(&self) -> u64;

    /// Read `count` contiguous blocks starting at `lba` into `buf`.
    /// `buf.len()` must equal `count as usize * 4096`.
    ///
    /// On `Err` the contents of `buf` are whatever they were before the call.
    #[must_use = "a failed read leaves the buffer holding whatever it held before"]
    fn read_blocks(&mut self, lba: u64, count: u32, buf: &mut [u8]) -> BlockResult;

    /// Write `count` contiguous blocks starting at `lba` from `buf`.
    /// `buf.len()` must equal `count as usize * 4096`.
    #[must_use = "a failed write did not reach the device"]
    fn write_blocks(&mut self, lba: u64, count: u32, buf: &[u8]) -> BlockResult;

    /// Flush any hardware write caches to persistent storage.
    #[must_use = "a failed flush means the writes before it are not durable"]
    fn flush(&mut self) -> BlockResult;
}

// How much of RAM the two caches above this trait may hold, in 4 KiB pages.
//
// Both numbers are hard ceilings, not targets. Linux lets its page cache take
// the whole machine because it has a pressure signal and a reclaim path to
// give it back on demand; ToyOS has neither (`issues/isolation/no-physical-memory-fairness.md` ("No physical
// memory fairness"), so a cache that grows to fit the workload is a cache
// that starves userland with no way to stop it. Until there is a pressure
// signal, the ceiling has to be a number the machine can lose outright.
//
// The `test-small-caches` overrides exist because the honest ceilings are
// tens of megabytes: a test that reached them by doing real I/O would spend
// minutes proving what 256 KiB proves in a second. The eviction code they
// drive is the shipped code — only the bound moves.

/// Blocks the filesystem metadata cache may hold.
///
/// Metadata residency is a property of the filesystem, not of the machine:
/// formatting the T14's 244 GB namespace leaves ~1900 blocks resident (the
/// number `nvme_large_device` writes back at shutdown), and a mounted
/// filesystem touches far fewer. 4096 blocks is 16 MiB — a little over 2x
/// that peak — so the steady state never evicts, and a cold walk of a btree
/// bigger than the cache degrades to re-reads instead of growing forever.
///
/// RAM enters only as a floor for machines too small to spare 16 MiB, where
/// the filesystem's appetite stops being the binding constraint. It must also
/// stay under 14,336 or the hashbrown index crosses the 16,384-bucket bound
/// `nvme_large_device` asserts.
pub fn metadata_cache_blocks() -> usize {
    if crate::actuator::test_small_caches() {
        return 64;
    }
    let (total, _) = crate::mm::pmm::stats();
    (((total / 32) / PAGE_SIZE) as usize).clamp(64, 4096)
}

/// Pages the file data cache may hold.
///
/// This one *is* a fraction of RAM: unlike metadata, the hot file set is a
/// property of what userland is doing, and there is no smaller number that is
/// right for both a 512 MiB box and a 32 GiB laptop. 1/64 of usable RAM is
/// 64 MiB on the 4 GiB test guest and 256 MiB at the upper clamp — small
/// enough that losing all of it is invisible, large enough to hold every
/// binary the system boots.
pub fn file_cache_pages() -> usize {
    if crate::actuator::test_small_caches() {
        return 64;
    }
    let (total, _) = crate::mm::pmm::stats();
    (((total / 64) / PAGE_SIZE) as usize).clamp(2048, 65536)
}

/// The flush-latency census: what the machine's device flushes actually cost,
/// printed so [`OPERATION`] and [`DEADMAN`] are derived from data rather than
/// from an argument about data.
///
/// The `syscall_cost`/interrupt-census shape: counters fed on the path they
/// measure, one printed line at a moment a running guest reaches — a process
/// exit — because the harness ends every guest by killing QEMU and a
/// shutdown-only instrument reaches no capture. The feeding is a handful of
/// relaxed atomics on the *flush* path only, which no audio-path work shares.
///
/// Latency lands in power-of-two microsecond buckets, so the line's `p50`/`p99`
/// are each bucket ceilings — good to a factor of two, which is what sizing a
/// two-orders-of-magnitude deadman needs — and `max` is exact.
pub mod census {
    use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

    use super::DeviceId;

    /// Distinct devices the census can hold apart. The machines this kernel
    /// boots carry at most an NVMe namespace and a couple of sticks; a fifth
    /// device folds into the last slot and the line says so with its id.
    const DEVICES: usize = 4;
    /// log2-µs latency buckets: bucket `i` holds flushes that took under
    /// `2^i` µs, and the last holds everything from 2 s up.
    const BUCKETS: usize = 32;

    struct Slot {
        /// The device id plus one, so zero means empty.
        id: AtomicU32,
        flushes: AtomicU64,
        /// Operations (read, write or flush) this device refused on the
        /// caller's budget — [`super::BlockError::BudgetExpired`], the count
        /// the slow-vs-failed split turns on.
        expiries: AtomicU64,
    }

    static SLOTS: [Slot; DEVICES] = [const {
        Slot { id: AtomicU32::new(0), flushes: AtomicU64::new(0), expiries: AtomicU64::new(0) }
    }; DEVICES];
    static LATENCY: [AtomicU64; BUCKETS] = [const { AtomicU64::new(0) }; BUCKETS];
    static MAX_NS: AtomicU64 = AtomicU64::new(0);
    /// Everything the print above has already reported, so an exit with no news
    /// prints nothing.
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

    /// The latency value at or below which `want` of `total` samples fall,
    /// as the ceiling of its bucket in microseconds.
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

    /// Say what has been measured, once per batch of news. Called at process
    /// exit — the one recurring moment a running guest reaches.
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
