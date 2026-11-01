//! The machine's clocks: monotonic since boot, read off the CPU's free-running
//! counter along the line the architecture's boot starts in [`set_counter`]
//! and may retune once it measures the counter's rate better ([`retune`]);
//! wall-clock, the architecture's one reading of the RTC — on x86-64 a CMOS
//! read, which can block for up to a second, and on AArch64 firmware's, which
//! the loader hands over — anchored in [`init_wall`] and answered after as
//! that reading plus [`nanos_since_boot`]; and the log's stamp ([`stamp`]), the
//! same clock counted from the counter's zero, which is where every line of
//! the log — the loader's, the kernel's and every program's — counts from.
//!
//! The line lives on the clock page every process maps
//! (`toyos_abi::clock`), and the kernel reads it there too, so the two clocks
//! are one until the architecture demotes the page ([`demote`]).

use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering::{Acquire, Relaxed, Release}};

use alloc::sync::Arc;
use toyos_abi::clock::{Clock, ClockPage, Line};

use crate::arch::cpu;
use crate::log::LogStamp;
use crate::object::shm::SharedMemObject;
use crate::sync::Lock;
use crate::time::Instant;
use kernel::clock::anchor;
use toyos_wallclock::Civil;

/// The [`Line::mult`] of the rate the CPU states; zero where it states none,
/// or before [`state`].
static STATED_MULT: AtomicU64 = AtomicU64::new(0);

/// The clock page through the direct map, null before [`set_counter`].
static LINES: AtomicPtr<ClockPage> = AtomicPtr::new(core::ptr::null_mut());

/// The clock page as an object, for every address space to map.
static PAGE: AtomicPtr<Arc<SharedMemObject>> = AtomicPtr::new(core::ptr::null_mut());

/// The page's one writer at a time.
static RETUNING: Lock<()> = Lock::new(());

/// The line the one in force replaced, for [`floor_at`]; zeros before the
/// first retune, which no floor exceeds. Published under [`RETUNING`].
static REPLACED: crate::seqlock::Published<3> = crate::seqlock::Published::new();

/// From [`demote`] to [`handed_over`] no retune publishes, so the two lines
/// [`floor_at`] reads stay unchanged through the handover.
static HELD: AtomicBool = AtomicBool::new(false);

/// Take the rate the CPU states, the loader's, for the stamps the log carries
/// before [`set_counter`]. The kernel's entry calls this before its first record.
pub fn state() {
    let stated = cpu::stated_counter_hz().map_or(0, Line::mult_of_hz);
    STATED_MULT.store(stated, Relaxed);
}

/// Start the clock on the counter: its reading at boot and its rate, as a
/// [`Line::mult`]. The architecture's boot calls this once, after it has the
/// rate, and before any address space is built.
///
/// **The stamps go on from where the stated rate left them**: a record before
/// this read the counter at that rate, so the stamp at `boot` is that reading,
/// and no record after this one is stamped earlier than one before it.
pub fn set_counter(boot: u64, mult: u64) {
    let stated = STATED_MULT.load(Relaxed);
    let at_boot = Line { counter: 0, stamp: 0, mult: if stated != 0 { stated } else { mult } }.nanos(boot);
    let page = SharedMemObject::create(crate::mm::charge::boot(), crate::mm::PAGE_SIZE).expect("clock: no frame for the clock page");
    let lines = crate::DirectMap::from_phys(page.page(0).expect("clock: its page is committed whole")).as_mut_ptr::<ClockPage>();
    // SAFETY: the page is this object's own, which no address space maps yet
    // and none unmaps after: held below for the machine's life. Every field of
    // `ClockPage` is an atomic, so a shared reference reads and writes it as
    // atomics while processes read it too.
    let lines = unsafe { &*lines };
    lines.init(at_boot, Line { counter: boot, stamp: at_boot, mult });
    PAGE.store(alloc::boxed::Box::into_raw(alloc::boxed::Box::new(page)), Release);
    LINES.store(lines as *const ClockPage as *mut ClockPage, Release);
}

/// Go on at `mult` from the counter reading `at`: the line from there starts
/// where the clock stands, so it bends and never steps. Nothing while a
/// demotion is handed over ([`demote`]); the caller retunes again later.
pub fn retune(at: u64, mult: u64) {
    let _one = RETUNING.lock();
    if !HELD.load(Relaxed) {
        publish(at, mult);
    }
}

/// [`retune`]'s publication, under [`RETUNING`].
fn publish(at: u64, mult: u64) {
    let page = lines().expect("clock: retuned before it started");
    let old = page.read().expect("clock: the page `set_counter` laid out carries no clock").line;
    REPLACED.publish([old.counter, old.stamp, old.mult]);
    page.publish(Line { counter: at, stamp: old.stamp_of(at), mult });
}

/// The rate the line in force runs at.
pub fn mult() -> u64 {
    clock().expect("clock: read before it started").line.mult
}

/// The clock at the counter reading `at`, as a log line's time.
pub fn stamp_at(at: u64) -> u64 {
    clock().expect("clock: read before it started").stamp_of(at)
}

/// Send every process's reading of now to the kernel from here on
/// (`toyos_abi::clock::ClockPage::demoted`), and hold every retune until
/// [`handed_over`]: a page reader that found the page unmarked read the line
/// in force or an older one, and [`floor_at`] reads the latest two.
pub fn demote() {
    let _one = RETUNING.lock();
    HELD.store(true, Relaxed);
    lines().expect("clock: demoted before it started").demote();
}

/// The clock at the counter reading `at` by the line in force and by the one
/// it replaced, whichever is later: no earlier than a page reader's reading
/// of a counter before `at` along either. Lock-free, for a kick handler.
pub fn floor_at(at: u64) -> u64 {
    let now = stamp_at(at);
    let [counter, stamp, mult] = REPLACED.snapshot().expect("clock: the replaced line is published only under `RETUNING`, which a hold keeps");
    now.max(Line { counter, stamp, mult }.stamp_of(at))
}

/// End [`demote`]'s hold, going on at `mult` from the counter reading `at`.
pub fn handed_over(at: u64, mult: u64) {
    let _one = RETUNING.lock();
    HELD.store(false, Relaxed);
    publish(at, mult);
}

fn lines() -> Option<&'static ClockPage> {
    let page = LINES.load(Acquire);
    // SAFETY: `set_counter` stored a pointer into a page it holds for the
    // machine's life, and the pointer is never cleared.
    (!page.is_null()).then(|| unsafe { &*page })
}

fn clock() -> Option<Clock> {
    lines().and_then(ClockPage::read)
}

/// The clock and the counter read inside its line's read section, so a line
/// replaced while this CPU was interrupted is read again. The kernel's own
/// clock stays on the counter whether or not the page is demoted.
fn now_on_line() -> Option<(Clock, u64)> {
    lines().and_then(|page| page.read_now(cpu::counter_ordered)).map(|(clock, now, _)| (clock, now))
}

/// Now as a log line's time: nanoseconds since the counter's zero — power-on,
/// or the reset since — at the clock's rate, and before [`set_counter`] at the
/// rate the CPU states. `None` where it states none and the clock has not
/// started: no rate reads the counter yet. Never panics, as
/// [`nanos_since_boot`].
pub fn stamp() -> Option<LogStamp> {
    if let Some((clock, now)) = now_on_line() {
        return Some(LogStamp::since_zero(clock.stamp_of(now)));
    }
    let stated = STATED_MULT.load(Relaxed);
    (stated != 0).then(|| LogStamp::since_zero(Line { counter: 0, stamp: 0, mult: stated }.nanos(cpu::counter())))
}

/// Map the clock page into a fresh address space, read-only, at the address
/// the ABI names.
pub fn map_page(space: &mut crate::mm::space::AddressSpace) {
    let page = PAGE.load(Acquire);
    assert!(!page.is_null(), "clock: an address space was built before the clock page");
    // SAFETY: `set_counter` stored a box it never frees, and the pointer is
    // never cleared.
    let page = Arc::clone(unsafe { &*page });
    let at = crate::mm::space::At::Fixed(crate::UserAddr::new(toyos_abi::clock::CLOCK_PAGE));
    let len = page.size();
    space.map(page, 0, len, kernel::vm::Prot::Read, at).expect("clock: the clock page's address is a fresh space's");
}

/// Whether [`nanos_since_boot`] measures anything yet; false before [`set_counter`].
pub fn calibrated() -> bool {
    !LINES.load(Relaxed).is_null()
}

/// Nanoseconds since boot; lock-free, no MMIO, and never panics — `log::emit`
/// reads it from inside a bracket where panicking would reenter the log.
/// Zero before [`set_counter`], and a CPU whose counter trails boot reads boot.
pub fn nanos_since_boot() -> u64 {
    now_on_line().map_or(0, |(clock, now)| clock.nanos_since_boot(now))
}

/// The same reading as an [`Instant`], the type arithmetic on it is allowed in.
/// The one bridge between hardware and `crate::time`, which stays `core`-only for `kernel-loom`.
pub fn now() -> Instant {
    Instant::from_nanos_since_boot(nanos_since_boot())
}

/// The [`cpu::counter`] reading `nanos` in the future, for a wait loop that must
/// not call the nanosecond clock.
pub fn counter_deadline(nanos: u64) -> u64 {
    cpu::counter().saturating_add(counter_ticks(nanos))
}

/// `nanos` as a count of [`cpu::counter`] ticks: a span converted once and then compared
/// against counter differences, which is what a sampler that may not divide
/// needs. Before [`set_counter`] the rate is unknown and this is zero, so a bound
/// derived from it is one its arm has to refuse.
pub fn counter_ticks(nanos: u64) -> u64 {
    clock().map_or(0, |clock| clock.line.ticks(nanos))
}

/// The span a count of [`counter_ticks`] stands for, for a caller that measured
/// before there was a rate to measure with and converts once, afterwards.
/// Zero while the rate is unknown, so a span taken on a machine that never
/// calibrated reads as no time rather than as an invented one.
pub fn nanos_of_ticks(ticks: u64) -> u64 {
    clock().map_or(0, |clock| clock.line.nanos(ticks))
}

/// Polls `ready` until it holds or `nanos` pass; `false` is the deadline.
/// Reads the counter, not [`nanos_since_boot`], because that clock's out-of-line divide
/// would land an NMI sample's PC in `compiler_builtins` rather than in the wait.
///
/// **Before [`set_counter`] there is no rate to measure a span with, so this asks
/// `ready` once and answers it.** A wait nothing can bound is the one thing an
/// unattended machine may not enter: a device denied its recovery window is a
/// `false` its caller reports, and a spin nothing ends reports nothing at all.
pub fn settles(nanos: u64, ready: impl Fn() -> bool) -> bool {
    if !calibrated() {
        return ready();
    }
    let until = counter_deadline(nanos);
    while !ready() {
        if cpu::counter() >= until {
            return false;
        }
        core::hint::spin_loop();
    }
    true
}

/// Unix seconds at `nanos_since_boot() == 0`.
static BOOT_SECS: AtomicU64 = AtomicU64::new(0);
/// Whether the above means anything; zero is a valid instant, not a sentinel.
static WALL_KNOWN: AtomicBool = AtomicBool::new(false);

/// Anchors the wall clock, once, after [`set_counter`], to the architecture's
/// reading of the RTC, which keeps UTC: the instant, and the counter when it
/// was true, which on a machine whose loader read it is before boot.
pub fn init_wall(reading: Result<(Civil, u64), impl core::fmt::Display>) {
    let (civil, at) = match reading {
        Ok(reading) => reading,
        Err(fault) => {
            log!("clock: this machine will not say what time it is — {fault}");
            return;
        }
    };

    let clock = clock().expect("clock: the wall clock anchored before the counter started");
    BOOT_SECS.store(anchor(civil.to_unix_secs(), clock.stamp_of(at), clock.stamp_at_boot), Relaxed);
    WALL_KNOWN.store(true, Release);
    log!("clock: the RTC reads {civil} UTC");
}

/// Unix seconds, now — what `SYS_CLOCK_EPOCH` serves: the whole seconds of
/// [`utc_nanos`]. `None` if the RTC never answered.
pub fn utc_secs() -> Option<u64> {
    utc_nanos().map(|nanos| nanos / NANOS_PER_SEC)
}

const NANOS_PER_SEC: u64 = 1_000_000_000;

/// Nanoseconds since the Unix epoch, UTC: the RTC's whole-second reading carried
/// on by the counter, so its resolution is the counter's and its accuracy the
/// RTC's second.
pub fn utc_nanos() -> Option<u64> {
    WALL_KNOWN
        .load(Acquire)
        .then(|| BOOT_SECS.load(Relaxed).saturating_mul(NANOS_PER_SEC).saturating_add(nanos_since_boot()))
}

/// What a file written now is stamped with (`toyos_abi::syscall::Stat::mtime`):
/// [`utc_nanos`], and 0 — undated — on a machine whose RTC never answered.
pub fn mtime_now() -> u64 {
    utc_nanos().unwrap_or(0)
}

