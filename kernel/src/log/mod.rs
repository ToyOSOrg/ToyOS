//! The kernel's only log producer: `log!`, `alert!` and `boot_phase!` all
//! expand to [`emit`], the only entry point, which takes only `fmt::Arguments`;
//! there is no byte-oriented entry point, so a partial record is untypeable.

// `-D warnings` in CI clippy makes an undocumented `unsafe` block here an error.
#![warn(clippy::undocumented_unsafe_blocks)]

pub mod console;
pub mod read;
pub mod recovery;
pub mod registry;
pub mod shard;
mod stamp;
#[cfg(feature = "test-actuators")]
pub mod storm;
pub mod user;

use core::sync::atomic::{AtomicBool, Ordering};

pub use stamp::LogStamp;
use toyos_abi::log::{LogRecord, FLAG_UNTIMED, MAX_LOG_SHARDS, MAX_RECORD_MESSAGE};

pub use shard::Shard;
pub use toyos_abi::log::Severity;

/// Set once GS base is valid; before that, reading `gs:` faults.
pub static PERCPU_READY: AtomicBool = AtomicBool::new(false);

/// cpu0's shard and the boot shard: the same, zeroed `static`, not a heap
/// allocation, because `log!` runs before the heap exists and before
/// `PERCPU_READY`.
pub static BOOT_SHARD: Shard = Shard::new();

// The ABI fixes how many shards a cursor can name; the kernel must not exceed it.
const _: () = assert!(crate::sched::MAX_CPUS <= MAX_LOG_SHARDS);

/// The shard `cpu` reserves in, published before that CPU runs an instruction,
/// since a reader finds it only through the registry: the boot shard for CPU
/// 0, and a fresh one, never freed, for every other.
pub fn shard_for(cpu: u32) -> &'static Shard {
    if cpu == 0 {
        return &BOOT_SHARD;
    }
    assert!(
        registry::published(registry::kernel_slots(), cpu as usize - 1).is_none(),
        "log: cpu{cpu} already has a shard, and a second would hide every record written to the first",
    );
    let layout = alloc::alloc::Layout::new::<Shard>();
    // SAFETY: a `Shard` is not zero-sized; the block is never freed.
    let ptr = unsafe { alloc::alloc::alloc_zeroed(layout) }.cast::<Shard>();
    assert!(!ptr.is_null(), "log: no memory for cpu{cpu}'s shard");
    // SAFETY: fresh, zeroed and aligned for a `Shard`, which zeroed is an
    // empty one, live for the machine's life as `publish` needs.
    unsafe {
        registry::publish(registry::kernel_slots(), cpu, ptr);
        &*ptr
    }
}

/// The head the tail is sealed under, read back by `src/bootlog.rs`.
const TAIL_HEAD: &str = "log: this boot's newest records follow, newest first";

/// Seal the stop's own records onto the black box, the one channel a boot's
/// tail has once the stop has begun: every record stamped after `after`,
/// which is the newest one the stop found, newest first.
///
/// **Bounded by the page and by nothing else.** The head and the records may
/// spend what a report may ([`toyos_blackbox::REPORT_BYTES`]), which leaves
/// the reset's own account its reserve; [`toyos_blackbox::Whole`] keeps the
/// newest that fit, counts the rest and says the count last, in the words a
/// death's tail says it ([`toyos_blackbox::DROPPED_OPENS_WITH`]). How many
/// records a stop writes is the machine's to decide, by its CPUs and its disks.
///
/// **The kernel does not wait for `/system/bin/logkeeper`, so it does not know what
/// reached `/log`.** `/system/bin/supervisor` has `logkeeper` flush before it asks for the
/// stop; everything committed after that — the stop's own record, the last
/// word — is on the console, and here, where the next
/// loader pass prints it into `loader.log`.
///
/// Called from the quiesce path under [`crate::blackbox::record_done`], where
/// the page already carries this boot's seal and every lock is still ordinary.
pub fn seal_tail(after: LogStamp) {
    struct Tail<'a>(toyos_blackbox::Whole<'static, &'a mut dyn core::fmt::Write>);
    impl read::RecordSink for Tail<'_> {
        fn put(&mut self, record: &LogRecord) -> bool {
            // No prefix of its own: the loader that prints this page puts one
            // on every line it reads back.
            self.0.put(format_args!("log-tail: {record}"));
            true
        }
    }
    let from = LogStamp::since_zero(after.nanos().saturating_add(1));
    crate::blackbox::append(|out| {
        let _ = writeln!(out, "{TAIL_HEAD}");
        let room = toyos_blackbox::REPORT_BYTES - TAIL_HEAD.len() - 1;
        let mut tail = Tail(toyos_blackbox::Whole::within(out, room, toyos_blackbox::DROPPED_OPENS_WITH));
        read::snapshot_committed(from, read::newest_committed(), &mut tail);
        tail.0.close();
    });
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
}

/// The IF/TF-off bracket must span the `xadd` through publication, or a
/// writer preempted mid-body can be overwritten before its record is visible:
/// the `xadd` is atomic against a same-CPU interrupt only, not against another
/// CPU, so this holds only while the CPU keeps ownership of the shard across
/// the whole bracket, since work stealing is enabled.
fn reserve(guard: &crate::arch::IrqGuard) -> (Origin, u64) {
    if !PERCPU_READY.load(Ordering::Relaxed) {
        // SAFETY: nothing else is running, so this CPU owns the boot shard.
        let seq = unsafe { BOOT_SHARD.reserve(guard) };
        let origin = Origin { shard: &BOOT_SHARD, cpu: 0, tid: 0, pid: 0 };
        return (origin, seq);
    }

    let (shard, seq, cpu, tid, pid) = crate::arch::percpu::reserve_log_slot(guard);
    // SAFETY: this CPU's own `PerCpu` pointer, valid before the CPU takes an instruction.
    let shard: &'static Shard = unsafe { &*shard };
    (Origin { shard, cpu: cpu as u16, tid: on_a_thread(tid), pid: on_a_thread(pid) }, seq)
}

/// `0` means "no thread"; `PerCpu`'s own sentinel is `u32::MAX`, translated at
/// this boundary rather than carried inward, so no downstream consumer has to
/// know the raw idle-CPU sentinel.
///
/// A process's first thread has `Tid(0)`, which also renders as absent —
/// tracked at `issues/a-record-cannot-name-thread-zero.md`, fixed
/// in the ABI's formatter rather than here.
fn on_a_thread(id: u32) -> u32 {
    if id == u32::MAX { 0 } else { id }
}

/// The only producer: formats, then stamps, reserves and publishes under one bracket.
pub fn emit(severity: Severity, args: core::fmt::Arguments) {
    let mut record = LogRecord { severity: severity as u8, ..LogRecord::EMPTY };

    // Formatting runs outside every critical section: no lock, device or gs: access.
    let mut message = Message { msg: &mut record.msg, len: 0, elided: 0 };
    let _ = core::fmt::Write::write_fmt(&mut message, args);
    record.len = message.len as u16;
    record.elided = message.elided.min(u16::MAX as usize) as u16;

    let guard = crate::arch::IrqGuard::close();
    // Stamped inside the bracket: outside it, ordering by seq and by at_ns
    // could disagree. The NMI handler never logs and #MC halts rather than
    // returning, which is what closes the two paths IF/TF masking alone
    // cannot.
    match crate::clock::stamp() {
        Some(at) => record.at_ns = at.nanos(),
        None => record.flags = FLAG_UNTIMED,
    }
    let (origin, seq) = reserve(&guard);
    record.seq = seq;
    record.pid = origin.pid;
    record.tid = origin.tid;
    record.cpu = origin.cpu;

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

    // After the commit, so the record this call made is the one on the panel.
    crate::drivers::panic_console::early_checkpoint();
}

/// A line of ordinary kernel log.
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        $crate::log::emit($crate::log::Severity::Info, format_args!($($arg)*))
    };
}

/// A refusal, a corruption, or a fault; the panel paints the row red for it.
#[macro_export]
macro_rules! alert {
    ($($arg:tt)*) => {
        $crate::log::emit($crate::log::Severity::Alert, format_args!($($arg)*))
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
            $crate::log::Severity::Info,
            format_args!(
                "Boot: {} ({}ms)",
                $name,
                ($crate::clock::nanos_since_boot() - $since) / 1_000_000
            ),
        );
        $crate::drivers::panic_console::boot_checkpoint();
        // The deadline seals where the machine was, and this is the only word
        // for it: a phase not in `deadline::PHASES` does not compile.
        $crate::deadline::reached($crate::deadline::index_of($name));
    }};
}
