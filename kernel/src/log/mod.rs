//! The kernel's only log producer: `log!`, `alert!` and `boot_phase!` all
//! expand to [`emit`], the only entry point, which takes only `fmt::Arguments`;
//! there is no byte-oriented entry point, so a partial record is untypeable.

// `-D warnings` in CI clippy makes an undocumented `unsafe` block here an error.
#![warn(clippy::undocumented_unsafe_blocks)]

pub mod console;
pub mod nested;
pub mod read;
pub mod recovery;
pub mod registry;
pub mod shard;
#[cfg(feature = "boot-actuators")]
pub mod storm;
pub mod user;

use core::sync::atomic::{AtomicBool, Ordering};

use toyos_abi::log::{LogRecord, FLAG_EARLY, MAX_LOG_SHARDS, MAX_RECORD_MESSAGE};

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

/// Makes an AP's shard reachable to a reader.
/// # Safety
/// `shard` must be a live, initialised [`Shard`] that is never freed.
pub unsafe fn publish_ap_shard(cpu: u32, shard: *mut Shard) {
    // SAFETY: the caller's contract is this one.
    unsafe { registry::publish(registry::kernel_slots(), cpu, shard) };
}

/// The newest records the stop's account carries whole. The page's account
/// reserve is what bounds it: a tail that spent the reserve would leave the
/// reset's own account under it nowhere to go.
const TAIL_RECORDS: usize = 16;

/// Seal the newest of this boot's records onto the black box, the one channel
/// a boot's own tail has once the stop has begun.
///
/// **The kernel does not wait for `/system/bin/logd`, so it does not know what
/// reached `/log`.** `/system/bin/init` has `logd` flush before it asks for the
/// stop; everything committed after that — the stop's own record, `Syncing
/// filesystems...`, the last word — is on the console, and here, where the next
/// loader pass prints it into `loader.log`.
///
/// Called from the quiesce path under [`crate::blackbox::record_done`], where
/// the page already carries this boot's seal and every lock is still ordinary.
pub fn seal_tail() {
    struct Tail<'a> {
        out: &'a mut dyn core::fmt::Write,
        left: usize,
    }
    impl read::RecordSink for Tail<'_> {
        fn put(&mut self, record: &LogRecord) -> bool {
            // No prefix of its own: the loader that prints this page puts one
            // on every line it reads back.
            let _ = writeln!(self.out, "log-tail: {record}");
            self.left -= 1;
            self.left > 0
        }
    }
    crate::blackbox::append(|out| {
        let _ = writeln!(out, "log: the newest {TAIL_RECORDS} records of this boot follow, newest first");
        let mut tail = Tail { out, left: TAIL_RECORDS };
        read::snapshot_committed(0, read::newest_committed_at_ns(), &mut tail);
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
pub fn emit(severity: Severity, args: core::fmt::Arguments) {
    let mut record = LogRecord { severity: severity as u8, ..LogRecord::EMPTY };

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

    // Between the drain and the repaint, so the record this call just put on the
    // console is the one the panel does not have.
    #[cfg(feature = "boot-actuators")]
    if HALT_BEFORE_REPAINT.load(Ordering::Relaxed) {
        crate::arch::cpu::halt();
    }

    // After the commit, so the record this call made is the one on the panel.
    crate::drivers::panic_console::early_checkpoint();
}

/// Armed by the `test-early-halt` actuator one record ahead of where it wants
/// the boot to stop.
#[cfg(feature = "boot-actuators")]
static HALT_BEFORE_REPAINT: AtomicBool = AtomicBool::new(false);

#[cfg(feature = "boot-actuators")]
pub fn halt_before_the_next_repaint() {
    HALT_BEFORE_REPAINT.store(true, Ordering::Relaxed);
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

/// Records one site may say in [`LIMIT_WINDOW_NS`] before the rest of the
/// window's are counted instead ([`log_limited!`]).
pub const LIMIT_BURST: u64 = 16;
pub const LIMIT_WINDOW_NS: u64 = 1_000_000_000;

/// `log!` for a site a program can drive at any rate: past [`LIMIT_BURST`]
/// records a second the site's records are counted, the last one said before
/// that says so, and the next one said carries the count
/// (`toyos_elide::limit`).
#[macro_export]
macro_rules! log_limited {
    ($($arg:tt)*) => {{
        static LIMIT: toyos_elide::limit::Limit =
            toyos_elide::limit::Limit::new($crate::log::LIMIT_BURST, $crate::log::LIMIT_WINDOW_NS);
        match LIMIT.admit($crate::clock::nanos_since_boot()) {
            toyos_elide::limit::Admit::Suppress => {}
            toyos_elide::limit::Admit::Say { suppressed, last } => $crate::log::emit(
                $crate::log::Severity::Info,
                format_args!(
                    "{}{}",
                    format_args!($($arg)*),
                    $crate::log::Limited { suppressed, last },
                ),
            ),
        }
    }};
}

/// What a limited site's record adds to its line: nothing, or what the limit did.
pub struct Limited {
    pub suppressed: u64,
    pub last: bool,
}

impl core::fmt::Display for Limited {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.suppressed > 0 {
            write!(f, " (after {} like it suppressed)", self.suppressed)?;
        }
        if self.last {
            write!(f, " (the rest like it this second are suppressed)")?;
        }
        Ok(())
    }
}
