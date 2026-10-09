//! The kernel's console sink: the one writer of the console wire, `klogd`,
//! and its two drain phases (inline at boot, then the thread).
//!
//! **One writer.** `klogd` puts the kernel's records on the wire, and between
//! them the lines console holders write ([`queue`]) — which on this machine is
//! `/system/bin/logkeeper` alone, rendering each program's line with its tag.
//! It holds the wire ([`serial::wire`]) with interrupts on and
//! preemption allowed, and the registers only for one burst at a time.
//!
//! [`Drain::Inline`] and [`Drain::Thread`] are phases, not fallbacks: exactly
//! one is active, and `Drain::Inline` *is* [`KLOGD`] being null.
//!
//! **The stop takes the wire from `klogd` for good** ([`take_for_the_stop`]):
//! asked, `klogd` lets it go between two writes and never takes it again, and
//! the stop holds it to the machine's end, so every record the stop commits
//! reaches the wire before a CPU is taken down. A `klogd` that keeps it through
//! [`LET_GO`] has the stop write over it, through the registers.

use core::sync::atomic::{AtomicPtr, AtomicU64, AtomicU8, Ordering};

use alloc::sync::Arc;

use toyos_abi::log::LogRecord;
use kernel::sched::task::{WaitClass, WakeCause, WakeReason};
use kernel::sched::park::notify;

use crate::drivers::serial::{self, BackendGuard, PanicUart, MAX_CONSOLE_LINE};
use crate::hw::HW;
use crate::sched::driver::{cpus, irq_off};
use crate::sched::kthread;
use crate::sleeplock::SleepGuard;
use crate::time::{Budget, Deadline, Duration, DEAF_CPU};
use crate::watch;
use crate::sched::payload::KShared;
use crate::scheduler::{self, Parkable};
use crate::sync::Lock;

use super::handoff::Handoff;
use super::read::{drain_ordered, Published, RecordSink};
use super::shard;

const NAME: &str = "klogd";

// `emit` finds `klogd` through this, not the process table: the lookup takes a lock, and `emit` runs inside IRQ handlers and every syscall's locked region.
static KLOGD: AtomicPtr<Arc<KShared>> = AtomicPtr::new(core::ptr::null_mut());

/// Who puts a committed record on the wire.
pub enum Drain {
    /// The producer itself, inline immediately after committing.
    /// Nothing else runs yet: no thread exists before `klogd`'s spawn, and no CPU takes a scheduler pass this early.
    Inline,
    /// `klogd`, woken at the commit of the record it will drain.
    /// Only a commit or a queued line wakes it — no idle loop, no timer.
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

/// One position for every drain context, so a record never reaches the wire
/// twice. Moved only by the holder of the wire, or by the panic path.
static DRAINED: Published = Published::new();

/// Start the thread. Called once, from `kernel_main`, before the scheduler starts.
/// Placement matters: APs spin until the machine is released, so an earlier spawn could not run while the machine has no console.
pub fn start() {
    let sched = kthread::spawn(NAME, body, 0);
    // Leaked: `klogd` never exits, and a producer reading this pointer under lock may not touch a refcount.
    let shared: &'static Arc<KShared> = alloc::boxed::Box::leak(alloc::boxed::Box::new(sched.shared));
    KLOGD.store(shared as *const _ as *mut _, Ordering::Release);
}

/// Post the wake this producer owns; called from `emit` after its publication bracket has closed, and from [`queue`].
pub fn post_wake() {
    let ptr = KLOGD.load(Ordering::Acquire);
    if ptr.is_null() {
        return;
    }
    // SAFETY: leaked once from a `Box`, never cleared; live for the machine's life.
    let shared = unsafe { &*ptr };
    // One read-modify-write of `klogd`'s own word and no lock: it claims a parked or committing `klogd`
    // and flags a running one, whose next commit refuses — the post a producer holding any lock may make.
    irq_off(|guard| {
        notify(shared, WakeCause::new(WakeReason::Woken), cpus(), &HW, guard);
    });
}

/// Put every committed record this machine has not yet spoken on the wire,
/// where the wire is free: the boot before `klogd` runs, and a backend's
/// arrival. Declining loses nothing: the record stays committed, and whoever
/// holds the wire drains it too.
pub fn drain_inline() {
    if !serial::has_console() {
        return;
    }
    let Some(wire) = serial::try_wire() else { return };
    drain_records(&wire, u64::MAX, Yield::Never);
}

/// Whether a drain lets the wire go to the stop that asks for it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Yield {
    /// `klogd`'s: it stops between two writes once the stop asks.
    ToTheStop,
    Never,
}

/// The stop's claim on the wire, which `klogd` answers.
static HANDOFF: Handoff = Handoff::new();

/// Posted by `klogd` once it has let the wire go for the stop.
static LET_GO_POSTED: watch::Watch = watch::Watch::new();

/// How long the stop waits for `klogd` to let the wire go once asked.
///
/// `klogd` lets go between two writes, so what it owes is the write it is in:
/// one rendered line, at most [`LINE_BYTES`], which the slowest wire this
/// kernel programs, the 16550 at 38400 baud, takes in 300 ms. [`DEAF_CPU`]'s
/// span is far past that and is the span after which a CPU that has not run
/// what it was given is a wedged one: a `klogd` that has not let go by then is
/// on one, or holds the wire through a defect.
pub(crate) const LET_GO: Budget = Budget::of(
    Duration::from_nanos(DEAF_CPU.nanos()),
    "the stop says who kept the wire, and writes every record over it through the registers",
);

/// The console as the stop holds it, from the moment it has stopped every
/// console holder to the machine's end. Never dropped: what holds one ends
/// the machine.
pub enum StopWire {
    /// The wire, which `klogd` let go of and never takes again.
    Held(SleepGuard<'static, ()>),
    /// `klogd` kept the wire through [`LET_GO`]; the stop writes over it.
    Kept,
}

impl StopWire {
    /// Every committed record onto the console, and every line console
    /// holders queued where the wire is held.
    pub fn drain(&self) {
        match self {
            Self::Held(wire) => {
                drain_records(wire, u64::MAX, Yield::Never);
                drain_queue(wire, usize::MAX, Yield::Never);
            }
            Self::Kept => serial::drain_over_the_wire(),
        }
    }
}

/// The wire, for the stop once it has stopped every console holder, held from
/// here to the machine's end; and every queued line onto it, so what a holder
/// queued before it was stopped goes on the wire under the boot's last word
/// and never after it.
///
/// Waits on `klogd`'s letting go, bounded by [`LET_GO`]: past it the stop
/// alerts by name and writes over the holder (a line of `klogd`'s still in
/// flight may interleave; nothing is lost but the queued lines, which the
/// alert says). The record backlog stays undrained here, so the sync behind
/// this is not spent behind a slow wire: the boot's last word drains it.
pub fn take_for_the_stop(parkable: &Parkable) -> StopWire {
    // Armed before the ask: a let-go posted between the ask and the park is
    // one the park returns on at once.
    let armed = watch::arm(&LET_GO_POSTED, 0, kernel::sched::task::WaitClass::Other)
        .expect("console: the stop holds no task to park");
    HANDOFF.ask();
    // A staged `klogd` waits for the ask parked, with the wire held; a
    // shipping one never parks holding it.
    #[cfg(feature = "boot-actuators")]
    post_wake();
    let deadline = Deadline::at(crate::clock::now() + LET_GO.duration());
    loop {
        let late = deadline.reached(crate::clock::now());
        if let Some(wire) = serial::try_wire() {
            if late {
                crate::alert!("console: klogd held the wire through the stop's {LET_GO}, and let it go only after");
            }
            drain_queue(&wire, usize::MAX, Yield::Never);
            return StopWire::Held(wire);
        }
        if late {
            break;
        }
        watch::wait_uncancellable(parkable, &armed, deadline);
    }
    crate::alert!(
        "console: {} kept the wire through the stop's {LET_GO}; the lines console holders queued are not \
         on it",
        serial::wire_holder(),
    );
    StopWire::Kept
}

/// Records and queued lines `klogd` takes per hold of the wire, so a console
/// holder's line is never behind the whole backlog of records, nor the other
/// way round.
const CHUNK: u64 = 8;

/// The whole record backlog through the registers the panic path holds.
/// Unbounded because interrupt latency doesn't matter while halting, and the report should be whole.
pub fn drain_locked(guard: &mut BackendGuard) {
    let mut cursor = DRAINED.take();
    let mut sink = Registers { out: guard };
    drain_ordered(&super::shards(), &mut cursor, &mut sink);
    DRAINED.put(&cursor);
}

/// Advances the position with no backend: standing still, an armed waiter would find the same record on every rescan and spin.
/// Safe to advance: shards keep every record for the panel regardless, and a backend arriving later rewinds this position whole.
fn discard_pending() {
    let mut cursor = DRAINED.take();
    let mut sink = Discard;
    drain_ordered(&super::shards(), &mut cursor, &mut sink);
    DRAINED.put(&cursor);
    LOST.store(DRAINED.lost(), Ordering::Relaxed);
}

/// At most `budget` records onto the wire the caller holds. Returns how many went.
fn drain_records(wire: &SleepGuard<'_, ()>, budget: u64, yields: Yield) -> u64 {
    let mut cursor = DRAINED.take();
    let mut sink = Wire { wire, records: 0, budget, yields };
    drain_ordered(&super::shards(), &mut cursor, &mut sink);
    let records = sink.records;
    DRAINED.put(&cursor);
    RECORDS.fetch_add(records, Ordering::Relaxed);
    LOST.store(DRAINED.lost(), Ordering::Relaxed);
    records
}

/// Drain straight to the 16550, over whoever holds its registers.
///
/// # Safety
/// Panic path only, once `serial::panic_flush` found no clean handoff; the position is unsynchronised and a record may reach the wire twice.
pub unsafe fn drain_bypassed(uart: &mut PanicUart) {
    let mut cursor = DRAINED.take();
    let mut sink = Raw { uart };
    drain_ordered(&super::shards(), &mut cursor, &mut sink);
    DRAINED.put(&cursor);
}

/// Which backend the drain has already spoken to, as `serial::Backend`'s discriminant.
static SPOKEN_TO: AtomicU8 = AtomicU8::new(serial::Backend::None as u8);

/// A backend has appeared or changed. Rewind and drain the boot again into the current one.
/// Fires only on an actual change, and the wire targets one backend at a time, so the replay never duplicates onto the backend already spoken to.
pub fn backend_changed() {
    let now = serial::backend() as u8;
    if SPOKEN_TO.swap(now, Ordering::Relaxed) != now {
        DRAINED.rewind();
    }
    drain_inline();
    // post_wake: the rewind above moved the position backwards under a parked `klogd`, with no new commit to wake it.
    post_wake();
}

/// Lines console holders queued for the wire. Fixed, because a console
/// holder's write may neither allocate nor wait: a line that finds it full is
/// counted in [`UNSHOWN`] and said by `klogd`.
const QUEUED_LINES: usize = 64;

struct Queue {
    lines: [[u8; MAX_CONSOLE_LINE]; QUEUED_LINES],
    lens: [u16; QUEUED_LINES],
    /// Whether the entry is a piece of a line the next entry goes on with.
    continues: [bool; QUEUED_LINES],
    head: usize,
    len: usize,
}

static QUEUE: Lock<Queue> = Lock::new(Queue {
    lines: [[0; MAX_CONSOLE_LINE]; QUEUED_LINES],
    lens: [0; QUEUED_LINES],
    continues: [false; QUEUED_LINES],
    head: 0,
    len: 0,
});

/// Whether the wire's last bytes are a queued line's piece its holder goes on
/// with, so a record put on the wire first ends that line. Read and written
/// only by the holder of the wire.
static MID_LINE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// End a queued line left mid-way on the wire, before anything else goes on it.
fn end_the_line(wire: &SleepGuard<'_, ()>) {
    if MID_LINE.swap(false, Ordering::Relaxed) {
        serial::write_wire(wire, b"\n");
    }
}

/// Lines refused a full [`QUEUE`] since `klogd` last said so.
static UNSHOWN: AtomicU64 = AtomicU64::new(0);

/// One console holder's line, or a piece of one it `continues` in the next,
/// for `klogd` to put on the wire; `false` is a full queue, which the holder's
/// write answers as bytes it did not take. Never waits.
pub fn queue(line: &[u8], continues: bool) -> bool {
    let line = &line[..line.len().min(MAX_CONSOLE_LINE)];
    {
        let mut queue = QUEUE.lock();
        if queue.len == QUEUED_LINES {
            return false;
        }
        let at = (queue.head + queue.len) % QUEUED_LINES;
        queue.lines[at][..line.len()].copy_from_slice(line);
        queue.lens[at] = line.len() as u16;
        queue.continues[at] = continues;
        queue.len += 1;
    }
    // The same wake a committed record takes: the store above precedes the
    // fence `signal_after_commit` runs, which `klogd`'s re-scan pairs with.
    if shard::signal_after_commit(shard::log_waiter()) {
        post_wake();
    }
    true
}

/// A line a holder let go of with no room for it: the one kind of console
/// loss, counted and said by `klogd`.
pub fn unshown() {
    UNSHOWN.fetch_add(1, Ordering::Relaxed);
}

/// Whether a console holder's next line would be taken.
pub fn has_room() -> bool {
    QUEUE.lock().len < QUEUED_LINES
}

/// Room in the queue: what a console holder that found it full polls for
/// `WRITABLE` on, posted by `klogd` as it frees room.
pub static SPACE: watch::Watch = watch::Watch::new();

/// At most `budget` queued lines onto the wire the caller holds, each ended
/// with a newline, so a line a holder left unended is not joined to the next.
/// A line's pieces go on together, whatever the budget: they are one line,
/// and a queue's worth past it at most. Answers whether it freed room.
fn drain_queue(wire: &SleepGuard<'_, ()>, budget: usize, yields: Yield) -> bool {
    let mut line = [0u8; MAX_CONSOLE_LINE + 1];
    let mut freed = false;
    let (mut ended, mut taken) = (0, 0);
    // A holder that never ends its line gets a queue's worth past the budget.
    while (ended < budget || MID_LINE.load(Ordering::Relaxed)) && taken < budget.saturating_add(QUEUED_LINES) {
        // A line left mid-way is the stop's to end: `MID_LINE` goes with the wire.
        if yields == Yield::ToTheStop && HANDOFF.asked() {
            break;
        }
        taken += 1;
        let (len, continues) = {
            let mut queue = QUEUE.lock();
            if queue.len == 0 {
                break;
            }
            let at = queue.head;
            let len = queue.lens[at] as usize;
            line[..len].copy_from_slice(&queue.lines[at][..len]);
            queue.head = (at + 1) % QUEUED_LINES;
            queue.len -= 1;
            (len, queue.continues[at])
        };
        freed = true;
        let end = if continues || line[..len].ends_with(b"\n") {
            len
        } else {
            line[len] = b'\n';
            len + 1
        };
        serial::write_wire(wire, &line[..end]);
        MID_LINE.store(continues, Ordering::Relaxed);
        if !continues {
            ended += 1;
        }
    }
    freed
}

/// Empty the queue with no backend to put it on; whether it freed room.
fn discard_queue() -> bool {
    let mut queue = QUEUE.lock();
    let dropped = queue.len;
    queue.len = 0;
    dropped > 0
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
}

impl<F: FnMut(&[u8])> Line<F> {
    fn new(emit: F) -> Self {
        Self { emit, buf: [0; LINE_BYTES], len: 0 }
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
        self.push(s.as_bytes());
        Ok(())
    }
}

/// Render one record as the console line; `logkeeper`'s `/log` sink renders the same line with its wall clock.
pub fn write_line(record: &LogRecord, emit: impl FnMut(&[u8])) {
    use core::fmt::Write;
    let mut line = Line::new(emit);
    let _ = write!(line, "{record}");
    line.finish();
}

/// Records onto a held wire, up to a budget; `put` returns false before the refused record, so the next hold starts there.
struct Wire<'w, 'g> {
    wire: &'w SleepGuard<'g, ()>,
    records: u64,
    budget: u64,
    yields: Yield,
}

impl RecordSink for Wire<'_, '_> {
    fn put(&mut self, record: &LogRecord) -> bool {
        if self.records >= self.budget || (self.yields == Yield::ToTheStop && HANDOFF.asked()) {
            return false;
        }
        end_the_line(self.wire);
        write_line(record, |bytes| serial::write_wire(self.wire, bytes));
        self.records += 1;
        true
    }
}

/// Records through the registers the panic path holds.
struct Registers<'a> {
    out: &'a mut BackendGuard,
}

impl RecordSink for Registers<'_> {
    fn put(&mut self, record: &LogRecord) -> bool {
        write_line(record, |bytes| self.out.write_raw(bytes));
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

/// Records straight to the 16550, for the bypass. Bounded per byte.
struct Raw<'a> {
    uart: &'a mut PanicUart,
}

impl RecordSink for Raw<'_> {
    fn put(&mut self, record: &LogRecord) -> bool {
        write_line(record, |bytes| self.uart.write(bytes));
        true
    }
}

extern "C" fn body(_arg: u64) -> ! {

    let parkable = scheduler::Parkable::at_entry();
    let handle = crate::sched::driver::current_handle().expect("klogd runs as a task");
    loop {
        #[cfg(feature = "boot-actuators")]
        staged::hold_if_staged(&parkable, &handle);
        let freed = if serial::has_console() {
            // A chunk of each per hold, with interrupts on throughout.
            let wire = serial::wire(&parkable);
            drain_records(&wire, CHUNK, Yield::ToTheStop);
            drain_queue(&wire, CHUNK as usize, Yield::ToTheStop)
        } else {
            discard_pending();
            discard_queue()
        };
        // With the wire let go of: the read the stop's try pairs with.
        if HANDOFF.asked() {
            let_go(&parkable, &handle);
        }
        // A holder that found the queue full waits on room; this is the room.
        if freed {
            SPACE.post();
        }
        let unshown = UNSHOWN.swap(0, Ordering::Relaxed);
        if unshown > 0 {
            crate::log!(
                "console: {unshown} line(s) a console holder wrote went unshown: they came faster \
                 than this console takes them, and the log has them"
            );
        }

        // The one point with committed records just observed that may take a lock; `emit` may not.
        // Outside `drain_inline`: that function's other callers (a producer mid-`emit`, the panic path) may not take a watch's lock.
        super::user::post_readiness();

        // Registered each pass, after the drain and before the recheck: the
        // wire's lock may park this thread on its own watch, and a task waits
        // on one watch at a time. `post_wake` notifies this thread's word
        // directly; nothing else posts this watch but the thread's own end.
        let armed = watch::arm(handle.watch(), 0, WaitClass::Other).expect("klogd runs as a task");
        // Safe with no backend because `discard_pending` still advances the position each pass.
        if shard::arm_waiter(shard::log_waiter(), || {
            // Under the lock `queue` stores under, ahead of the fence its wake takes.
            DRAINED.any_pending() || QUEUE.lock().len > 0
        }) {
            continue;
        }
        // No deadline: a spurious wake costs a re-drain; a missing one is what W3's fences prevent.
        PARKS.fetch_add(1, Ordering::Relaxed);
        // `klogd` is never killed, so this cancel arm is unreachable.
        let _ = watch::wait(&parkable, &armed, crate::time::Deadline::never());
    }
}

/// `klogd`'s end: the wire is the stop's, and this thread parks for the
/// machine's life, a commit's wake answered by parking again.
fn let_go(parkable: &Parkable, handle: &crate::sched::payload::TaskHandle) -> ! {
    // The backlog this leaves reaches the wire at the boot's last word.
    crate::log!("console: klogd let the wire go to the stop");
    LET_GO_POSTED.post();
    loop {
        let armed = watch::arm(handle.watch(), 0, WaitClass::Other).expect("klogd runs as a task");
        let _ = watch::wait(parkable, &armed, crate::time::Deadline::never());
    }
}

#[cfg(feature = "boot-actuators")]
pub use staged::hold_the_wire as stage_a_held_wire;

/// `klogd` holding the wire across the stop, which only an actuator stages.
#[cfg(feature = "boot-actuators")]
mod staged {
    use core::sync::atomic::{AtomicBool, Ordering};

    use kernel::sched::task::WaitClass;
    use crate::sched::payload::TaskHandle;

    use crate::drivers::serial;
    use crate::scheduler::Parkable;
    use crate::time::Deadline;
    use crate::watch;

    /// Set by the stop: `klogd` takes the wire at its next pass.
    static STAGED: AtomicBool = AtomicBool::new(false);
    /// Set by `klogd` once it holds the wire, and posted.
    static HOLDING: AtomicBool = AtomicBool::new(false);
    static HELD: watch::Watch = watch::Watch::new();

    /// The stop's, before it takes anything down: `klogd` takes the wire, and
    /// this returns once it has.
    pub fn hold_the_wire(parkable: &Parkable) {
        let armed = watch::arm(&HELD, 0, WaitClass::Other).expect("the stop holds a task");
        STAGED.store(true, Ordering::Release);
        super::post_wake();
        // No bound in guest time, which runs on while a host starves the
        // guest: the harness's ceiling, scaled by the host, is this wait's.
        while !HOLDING.load(Ordering::Acquire) {
            watch::wait_uncancellable(parkable, &armed, Deadline::never());
        }
        crate::log!("console: klogd holds the wire as the stop begins, staged");
    }

    /// `klogd`'s, at the top of its loop: once staged, it takes the wire and
    /// keeps it — until the stop asks for it, then lets it go and goes on as
    /// a shipping `klogd` does, or, with `wire-kept-through-the-stop`, for good.
    pub fn hold_if_staged(parkable: &Parkable, handle: &TaskHandle) {
        if !STAGED.swap(false, Ordering::Acquire) {
            return;
        }
        let wire = serial::wire(parkable);
        HOLDING.store(true, Ordering::Release);
        HELD.post();
        let kept = crate::actuator::wire_kept_through_the_stop();
        loop {
            let armed = watch::arm(handle.watch(), 0, WaitClass::Other).expect("klogd runs as a task");
            if !kept && super::HANDOFF.asked() {
                break;
            }
            let _ = watch::wait(parkable, &armed, Deadline::never());
        }
        drop(wire);
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
