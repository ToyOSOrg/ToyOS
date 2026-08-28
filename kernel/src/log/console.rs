//! The kernel's console sink: two drain phases (inline boot, then `klogd`'s
//! thread) and the `klogd` thread itself.
//! `klogd`'s row in `sched::kthread` is [`OnPanic::Halt`]: it is the only
//! console drainer, and its death must not go silent.
//! Records keep committing to their shards regardless of `klogd`; only the
//! live console is lost while it is down.
//! [`Drain::Inline`] and [`Drain::Thread`] are phases, not fallbacks: exactly
//! one is active, and `Drain::Inline` *is* [`KLOGD`] being null.

use core::sync::atomic::{AtomicPtr, AtomicU64, AtomicU8, Ordering};

use alloc::sync::Arc;

use toyos_abi::log::LogRecord;
use toyos_sched::task::{WaitClass, WakeCause, WakeReason};
use toyos_sched::waitq::wake_direct;

use crate::drivers::serial::{self, BackendGuard};
use crate::hw::HW;
use crate::sched::driver::{cpus, irq_off};
use crate::sched::kthread::{self, OnPanic};
use crate::completion;
use crate::sched::payload::{KShared, TaskHandle};
use crate::scheduler;

use super::read::{drain_ordered, Published, RecordSink};
use super::shard;

// klogd, not logd: `/bin/logd` is a separate userland process; one name for both would collide in a dump report.
const NAME: &str = "klogd";

// `emit` finds `klogd` through this, not the process table: the lookup takes a lock, and `emit` runs inside IRQ handlers and every syscall's locked region.
static KLOGD: AtomicPtr<Arc<KShared>> = AtomicPtr::new(core::ptr::null_mut());

// Separate from `KLOGD`: the spawner publishes `KLOGD`, `klogd` itself publishes this before its first park.
// Null between that spawn and `klogd`'s first loop: `Drain::Thread` is already set, and the wake alone covers the gap.
static KLOGD_INBOX: AtomicPtr<Arc<TaskHandle>> = AtomicPtr::new(core::ptr::null_mut());

/// Who puts a committed record on the wire.
pub enum Drain {
    /// The producer itself, inline immediately after committing.
    /// Nothing else runs yet: no thread exists before `klogd`'s spawn, and no CPU takes a scheduler pass this early.
    Inline,
    /// `klogd`, woken at the commit of the record it will drain.
    /// Only a commit wakes it — no idle loop, no timer — and `i8042_no_spurious_wake` depends on that.
    Thread,
}

/// Which phase this machine is in.
pub fn mode() -> Drain {
    if KLOGD.load(Ordering::Acquire).is_null() {
        Drain::Inline
    } else {
        Drain::Thread
    }
}

/// One position for every drain context, so a record never reaches the wire twice.
static DRAINED: Published = Published::new();

/// Start the thread. Called once, from `kernel_main`, before the scheduler starts.
/// Placement matters: APs spin on `SMP_READY` until then, so an earlier spawn could not run while the machine has no console.
pub fn start() {
    let sched = kthread::spawn(NAME, body, 0, OnPanic::Halt);
    // Leaked: `klogd` never exits, and a producer reading this pointer under lock may not touch a refcount.
    let shared: &'static Arc<KShared> = alloc::boxed::Box::leak(alloc::boxed::Box::new(sched.shared));
    KLOGD.store(shared as *const _ as *mut _, Ordering::Release);
}

/// Post the wake this producer owns; called from `emit` after its publication bracket has closed.
pub fn post_wake() {
    let ptr = KLOGD.load(Ordering::Acquire);
    if ptr.is_null() {
        return;
    }
    // SAFETY: leaked once from a `Box`, never cleared; live for the machine's life.
    let shared = unsafe { &*ptr };
    // Preserves invariant W's store-before-claim order (`completion::post_n`), in the one shape that may take no lock.
    // signal, not post: two producers can win the same wake epoch, and post's write would race on one `UnsafeCell<Record>`.
    let inbox = KLOGD_INBOX.load(Ordering::Acquire);
    if !inbox.is_null() {
        // SAFETY: leaked once by `klogd` before its first park, never cleared.
        let handle = unsafe { &*inbox };
        handle.inbox().signal();
    }
    irq_off(|guard| {
        wake_direct(shared, WakeCause::new(WakeReason::Woken), cpus(), &HW, guard);
    });
}

/// Put every committed record this machine has not yet spoken on the wire.
pub fn drain_inline() {
    if !serial::has_console() {
        return;
    }
    loop {
        // try_lock, not lock: `BackendGuard::lock` is not reentrant on this CPU, and a Ring 0 exception inside the backend write would spin forever with interrupts off.
        // Declining loses nothing: the record stays committed, and whichever holder scans next drains it too.
        let Some(mut guard) = BackendGuard::try_lock() else { return };
        let records = drain_bounded(&mut guard, CHUNK_RECORDS);
        drop(guard);
        if records < CHUNK_RECORDS {
            return;
        }
    }
}

/// Interrupt-off latency bound, not a batch size: `BackendGuard` holds IF off for its whole life.
const CHUNK_RECORDS: u64 = 8;

/// The whole backlog under one guard, for a caller that already holds it (panic and shutdown flush).
/// Unbounded because interrupt latency doesn't matter while halting or cutting power, and the report should be whole.
pub fn drain_locked(guard: &mut BackendGuard) {
    drain_bounded(guard, u64::MAX);
}

/// Advances the position with no backend: standing still, an armed waiter would find the same record on every rescan and spin.
/// Not in `drain_inline`: that function's other callers (a producer mid-`emit`, the panic path) would pay a per-record shard walk with no backend to justify it.
/// Safe to advance: shards keep every record for the panel regardless, and a backend arriving later rewinds this position whole.
fn discard_pending() {
    let mut cursor = DRAINED.take();
    let mut sink = Discard;
    drain_ordered(&mut cursor, &mut sink);
    DRAINED.put(&cursor);
    LOST.store(DRAINED.lost(), Ordering::Relaxed);
}

/// At most `budget` records to a held backend. Returns how many went.
fn drain_bounded(guard: &mut BackendGuard, budget: u64) -> u64 {
    let mut cursor = DRAINED.take();
    let mut sink = Wire { out: guard, records: 0, budget };
    drain_ordered(&mut cursor, &mut sink);
    let records = sink.records;
    DRAINED.put(&cursor);
    RECORDS.fetch_add(records, Ordering::Relaxed);
    LOST.store(DRAINED.lost(), Ordering::Relaxed);
    records
}

/// Drain with no lock at all, straight to the 16550.
///
/// # Safety
/// Panic path only, after `serial::panic_flush`'s bounded wait for a clean handoff fails; the position is unsynchronised and a record may reach the wire twice.
pub unsafe fn drain_bypassed() {
    let mut cursor = DRAINED.take();
    let mut sink = Raw;
    drain_ordered(&mut cursor, &mut sink);
    DRAINED.put(&cursor);
}

/// Which backend the drain has already spoken to, as `serial::Backend`'s discriminant.
static SPOKEN_TO: AtomicU8 = AtomicU8::new(serial::Backend::None as u8);

/// A backend has appeared or changed. Rewind and drain the boot again into the current one.
/// Fires only on an actual change, and `write_raw` targets one backend at a time, so the replay never duplicates onto the backend already spoken to.
pub fn backend_changed() {
    let now = serial::backend() as u8;
    if SPOKEN_TO.swap(now, Ordering::Relaxed) != now {
        DRAINED.rewind();
    }
    drain_inline();
    // post_wake: the rewind above moved the position backwards under a parked `klogd`, with no new commit to wake it.
    post_wake();
}

/// Sized for the tag, the ABI's widest bracket, the message, and the elision note.
/// A buffer, not a bound: an overlong line spills to the backend under the same guard instead of truncating.
const LINE_BYTES: usize = toyos_abi::log::MAX_RECORD_MESSAGE + 160;

/// One rendered line on its way to the backend.
/// Buffered: writing each `Display` fragment straight through would cost one host round trip per fragment.
struct Line<F: FnMut(&[u8])> {
    emit: F,
    buf: [u8; LINE_BYTES],
    len: usize,
    /// Set once the ABI's leading bracket has been consumed by the tag.
    tagged: bool,
}

impl<F: FnMut(&[u8])> Line<F> {
    fn new(emit: F) -> Self {
        Self { emit, buf: [0; LINE_BYTES], len: 0, tagged: false }
    }

    fn flush(&mut self) {
        if self.len > 0 {
            (self.emit)(&self.buf[..self.len]);
            self.len = 0;
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(LINE_BYTES) {
            if self.len + chunk.len() > LINE_BYTES {
                self.flush();
            }
            self.buf[self.len..self.len + chunk.len()].copy_from_slice(chunk);
            self.len += chunk.len();
        }
    }

    fn finish(mut self) {
        self.push(b"\n");
        self.flush();
    }
}

impl<F: FnMut(&[u8])> core::fmt::Write for Line<F> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        if !self.tagged {
            self.tagged = true;
            // The tag replaces `LogRecord`'s leading bracket instead of duplicating its fields; if the bracket goes, the raw fragment passes through whole.
            let rest = s.strip_prefix('[').unwrap_or(s);
            self.push(rest.as_bytes());
            return Ok(());
        }
        self.push(s.as_bytes());
        Ok(())
    }
}

/// Render one record as the console line; `logd`'s `/log` sink renders the same line with a different prefix.
pub fn write_line(record: &LogRecord, emit: impl FnMut(&[u8])) {
    use core::fmt::Write;
    let mut line = Line::new(emit);
    line.push(b"[kernel ");
    let _ = write!(line, "{record}");
    line.finish();
}

/// Records through a backend the caller holds, up to a budget; `put` returns false before the refused record, so the next acquisition starts there.
struct Wire<'a> {
    out: &'a mut BackendGuard,
    records: u64,
    budget: u64,
}

impl RecordSink for Wire<'_> {
    fn put(&mut self, record: &LogRecord) -> bool {
        if self.records >= self.budget {
            return false;
        }
        write_line(record, |bytes| self.out.write_raw(bytes));
        self.records += 1;
        true
    }
}

/// Advances the position with no backend; no bytes are built.
struct Discard;

impl RecordSink for Discard {
    fn put(&mut self, _record: &LogRecord) -> bool {
        true
    }
}

/// Records straight to the 16550, for the bypass. No lock, bounded per byte.
struct Raw;

impl RecordSink for Raw {
    fn put(&mut self, record: &LogRecord) -> bool {
        write_line(record, serial::panic_raw);
        true
    }
}

extern "C" fn body(_arg: u64) -> ! {
    // First, before any drain: stages a panic inside a kernel thread to test the panic handler's branch.
    #[cfg(feature = "boot-actuators")]
    if crate::actuator::klogd_panic() {
        panic!("klogd-panic: the console drainer died");
    }

    let parkable = scheduler::Parkable::at_entry();
    let handle = crate::sched::driver::current_handle().expect("klogd runs as a task");
    // Signals without a lock or watch list; `post_wake` explains why a record can't be written here instead.
    KLOGD_INBOX.store(
        alloc::boxed::Box::leak(alloc::boxed::Box::new(handle.clone())) as *const Arc<TaskHandle>
            as *mut _,
        Ordering::Release,
    );
    loop {
        // Bounded per chunk so interrupts stay off for at most `CHUNK_RECORDS` lines; `discard_pending` covers machines with no backend.
        if serial::has_console() {
            drain_inline();
        } else {
            discard_pending();
        }

        // The one point with committed records just observed that may take a lock; `emit` may not.
        // Outside `drain_inline`: that function's other callers (a producer mid-`emit`, the panic path) may not touch `INBOXES`.
        super::user::post_readiness();

        // A completion post cannot drop a wake: it stores the record before claiming, so a miss here is caught by `wait`'s own recheck.
        let Some(armed) = completion::arm(
            completion::Subject::of(handle.watch()),
            completion::Token::new(0),
            WaitClass::Other,
        ) else {
            continue;
        };
        // Safe with no backend because `discard_pending` still advances the position each pass.
        if shard::arm_waiter(shard::log_waiter(), || DRAINED.any_pending()) {
            continue;
        }
        // No deadline: a spurious wake costs a re-drain; a missing one is what W3's fences prevent.
        PARKS.fetch_add(1, Ordering::Relaxed);
        // `klogd` is never killed, so this cancel arm is unreachable.
        let _ = completion::wait(&parkable, &armed, crate::time::Deadline::never());
    }
}

/// Three counters read by `sched::dump`: records drained, records lost, and parks.
static RECORDS: AtomicU64 = AtomicU64::new(0);
static LOST: AtomicU64 = AtomicU64::new(0);
static PARKS: AtomicU64 = AtomicU64::new(0);

/// `(records drained, records lost, parks)`, via three relaxed loads.
pub fn stats() -> (u64, u64, u64) {
    (
        RECORDS.load(Ordering::Relaxed),
        LOST.load(Ordering::Relaxed),
        PARKS.load(Ordering::Relaxed),
    )
}
