//! The kernel's only log producer: `log!`, `alert!` and `boot_phase!` all
//! expand to [`emit`], the only entry point, which takes only `fmt::Arguments`;
//! there is no byte-oriented entry point, so a partial record is untypeable.

// `-D warnings` in CI clippy makes an undocumented `unsafe` block here an error.
#![warn(clippy::undocumented_unsafe_blocks)]

pub mod console;
pub mod nested;
pub mod read;
pub mod registry;
pub mod shard;
#[cfg(feature = "boot-actuators")]
pub mod storm;
pub mod user;

use core::sync::atomic::{AtomicBool, Ordering};

use toyos_abi::log::{LogRecord, FLAG_EARLY, MAX_LOG_SHARDS, MAX_RECORD_MESSAGE};
use crate::time::{Budget, Duration};

pub use shard::Shard;
pub use toyos_abi::log::Level;

/// Set once GS base is valid; before that, reading `gs:` faults.
pub static PERCPU_READY: AtomicBool = AtomicBool::new(false);

/// cpu0's shard and the boot shard: the same, zeroed `static`, not a heap
/// allocation, because `log!` runs before the heap exists and before
/// `PERCPU_READY`.
pub static BOOT_SHARD: Shard = Shard::new();

// The ABI fixes how many shards a cursor can name; the kernel must not exceed it.
const _: () = assert!(crate::sched::MAX_CPUS <= MAX_LOG_SHARDS);

/// Makes an AP's shard reachable to a reader.
/// # Safety
/// `shard` must be a live, initialised [`Shard`] that is never freed.
pub unsafe fn publish_ap_shard(cpu: u32, shard: *mut Shard) {
    // SAFETY: the caller's contract is this one.
    unsafe { registry::publish(registry::kernel_slots(), cpu, shard) };
}

// Distinct from `apic`'s `LOG_FILE_DRAIN`: this bounds an orderly shutdown, not a panic.
const SHUTDOWN_DURABLE: Budget = Budget::of(
    Duration::from_secs(2),
    "the shutdown's last lines are on the console only, and it says so",
);

/// Waits, bounded, for `/system/bin/logd` to make committed records durable.
pub fn wait_for_durable() {
    // Snapshotted once: a re-read would never be satisfied while still committing.
    let want = read::newest_committed_at_ns();
    let deadline = crate::clock::nanos_since_boot().saturating_add(SHUTDOWN_DURABLE.nanos());
    while user::durable_ns() < want {
        if crate::clock::nanos_since_boot() >= deadline {
            crate::log!(
                "shutdown: /log did not answer in {}ms, so this shutdown's last lines are on the \
                 console only",
                SHUTDOWN_DURABLE.duration().millis()
            );
            return;
        }
        // Yields, never spins: at `--smp 1` this is the only CPU logd can run on.
        crate::scheduler::yield_now();
    }
}

/// Every shard a reader can reach, cpu0 first; `None` is a CPU this machine lacks.
pub fn shards() -> [Option<&'static Shard>; MAX_LOG_SHARDS] {
    let mut out = [None; MAX_LOG_SHARDS];
    out[0] = Some(&BOOT_SHARD);
    for (ap, slot) in out[1..].iter_mut().enumerate() {
        *slot = registry::published(registry::kernel_slots(), ap);
    }
    out
}

/// Shards answerable right now: counted once published, not merely declared;
/// it only ever grows, and never past `MAX_LOG_SHARDS`.
pub fn shard_count() -> u32 {
    shards().iter().filter(|shard| shard.is_some()).count() as u32
}

/// Builds a record's message bytes in place for [`emit`]; one pass and one
/// sink, since the console line is rendered from this record through the one
/// formatter in `toyos-abi`.
struct Message<'a> {
    // Borrowed, not owned: `emit` runs on the double-fault stack, no room for a second buffer.
    msg: &'a mut [u8; MAX_RECORD_MESSAGE],
    len: usize,
    // Saturating: dropped silently would make the bound a lie.
    elided: usize,
}

impl core::fmt::Write for Message<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let room = MAX_RECORD_MESSAGE - self.len;
        let bytes = s.as_bytes();
        if bytes.len() <= room {
            self.msg[self.len..self.len + bytes.len()].copy_from_slice(bytes);
            self.len += bytes.len();
            return Ok(());
        }
        // Split on a char boundary: a half-UTF-8 tail renders as mojibake on the panel.
        let mut fit = room;
        while fit > 0 && !s.is_char_boundary(fit) {
            fit -= 1;
        }
        self.msg[self.len..self.len + fit].copy_from_slice(&bytes[..fit]);
        self.len += fit;
        self.elided = self.elided.saturating_add(bytes.len() - fit);
        Ok(())
    }
}

/// Where this record goes and who is writing it.
struct Origin {
    shard: &'static Shard,
    cpu: u16,
    tid: u32,
    pid: u32,
    flags: u8,
}

/// The IF/TF-off bracket must span the `xadd` through publication, or a
/// writer preempted mid-body can be overwritten before its record is visible:
/// the `xadd` is atomic against a same-CPU interrupt only, not against another
/// CPU, so this holds only while the CPU keeps ownership of the shard across
/// the whole bracket, since work stealing is enabled.
fn reserve(guard: &crate::arch::LogCommitGuard) -> (Origin, u64) {
    if !PERCPU_READY.load(Ordering::Relaxed) {
        // SAFETY: nothing else is running, so this CPU owns the boot shard.
        let seq = unsafe { BOOT_SHARD.reserve(guard) };
        let origin = Origin { shard: &BOOT_SHARD, cpu: 0, tid: 0, pid: 0, flags: FLAG_EARLY };
        return (origin, seq);
    }

    let (shard, seq, cpu, tid, pid) = crate::arch::percpu::reserve_log_slot(guard);
    // SAFETY: this CPU's own `PerCpu` pointer, valid before the CPU takes an instruction.
    let shard: &'static Shard = unsafe { &*shard };
    (Origin { shard, cpu: cpu as u16, tid: on_a_thread(tid), pid: on_a_thread(pid), flags: 0 }, seq)
}

/// `0` means "no thread"; `PerCpu`'s own sentinel is `u32::MAX`, translated at
/// this boundary rather than carried inward, so no downstream consumer has to
/// know the raw idle-CPU sentinel.
///
/// A process's first thread has `Tid(0)`, which also renders as absent —
/// tracked at `issues/diagnostics/a-record-cannot-name-thread-zero.md`, fixed
/// in the ABI's formatter rather than here.
fn on_a_thread(id: u32) -> u32 {
    if id == u32::MAX { 0 } else { id }
}

/// The only producer: formats, then stamps, reserves and publishes under one bracket.
pub fn emit(level: Level, args: core::fmt::Arguments) {
    let mut record = LogRecord { level: level as u8, ..LogRecord::EMPTY };

    // Formatting runs outside every critical section: no lock, device or gs: access.
    let mut message = Message { msg: &mut record.msg, len: 0, elided: 0 };
    let _ = core::fmt::Write::write_fmt(&mut message, args);
    record.len = message.len as u16;
    record.elided = message.elided.min(u16::MAX as usize) as u16;

    let guard = crate::arch::LogCommitGuard::close();
    // Stamped inside the bracket: outside it, ordering by seq and by at_ns
    // could disagree. The NMI handler never logs and #MC halts rather than
    // returning, which is what closes the two paths IF/TF masking alone
    // cannot.
    record.at_ns = crate::clock::nanos_since_boot();
    let (origin, seq) = reserve(&guard);
    record.seq = seq;
    record.pid = origin.pid;
    record.tid = origin.tid;
    record.cpu = origin.cpu;
    record.flags = origin.flags;

    // SAFETY: seq came from this shard's own reserve, committed exactly once under this guard.
    unsafe { origin.shard.commit(seq, &record, &guard) };
    drop(guard);

    // The two `Drain` modes are boot phases, not interchangeable fallbacks,
    // and `console::mode` is the single word read for it rather than a flag
    // kept beside it.
    match console::mode() {
        // Nothing else can run yet: the producer is the drainer, which is
        // what makes a boot that wedges before the idle loop say everything
        // it had logged.
        console::Drain::Inline => console::drain_inline(),
        // `emit` may take no lock, so the wake is a fence-guarded post, not a queue wake.
        console::Drain::Thread => {
            if shard::signal_after_commit(shard::log_waiter()) {
                console::post_wake();
            }
        }
    }
}

/// A line of ordinary kernel log.
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        $crate::log::emit($crate::log::Level::Info, format_args!($($arg)*))
    };
}

/// A refusal, a corruption, or a fault; the panel paints the row red for this level.
#[macro_export]
macro_rules! alert {
    ($($arg:tt)*) => {
        $crate::log::emit($crate::log::Level::Alert, format_args!($($arg)*))
    };
}

/// Logs a boot phase's elapsed time and repaints the console; `$since` 0
/// measures from boot. Logging and repainting are bundled because a wedge
/// without a panic calls nothing, so a checkpoint the console never shows is
/// not a checkpoint.
#[macro_export]
macro_rules! boot_phase {
    ($name:literal, $since:expr) => {{
        $crate::log::emit(
            $crate::log::Level::Phase,
            format_args!(
                "Boot: {} ({}ms)",
                $name,
                ($crate::clock::nanos_since_boot() - $since) / 1_000_000
            ),
        );
        $crate::drivers::panic_console::boot_checkpoint();
    }};
}
