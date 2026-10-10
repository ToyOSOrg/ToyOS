//! The machine's clocks: monotonic since boot, read off the CPU's free-running
//! counter at the period the architecture's boot gives [`set_counter`] (on
//! x86-64, measured against the HPET); wall-clock, read from the
//! architecture's RTC exactly once — a CMOS read can block for up to a
//! second — in [`init_wall`], and answered after as that reading plus
//! [`nanos_since_boot`]; and the log's stamp ([`stamp`]), the same clock
//! counted from the counter's zero, which is where every line of the log —
//! the loader's, the kernel's and every program's — counts from.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering::{Acquire, Relaxed, Release}};

use crate::arch::cpu;
use crate::log::LogStamp;
use crate::time::Instant;

static TSC_BOOT: AtomicU64 = AtomicU64::new(0);
static TSC_PERIOD_FS: AtomicU64 = AtomicU64::new(0);
/// The counter's zero to `TSC_BOOT`, in nanoseconds.
static STAMP_AT_BOOT: AtomicU64 = AtomicU64::new(0);
/// One counter tick at the rate the CPU states, in femtoseconds; zero where it
/// states none, or before [`state`].
static STATED_FS: AtomicU64 = AtomicU64::new(0);

/// Take the rate the CPU states, the loader's, for the stamps the log carries
/// before [`set_counter`]. The kernel's entry calls this before its first record.
pub fn state() {
    let stated = cpu::stated_counter_hz().map_or(0, |hz| 1_000_000_000_000_000 / hz);
    STATED_FS.store(stated, Relaxed);
}

/// Start the clock on the counter: its reading at boot and its measured period.
/// The architecture's boot calls this once, after it has the period.
///
/// **The stamps go on from where the stated rate left them**: a record before
/// this read the counter at that rate, so the stamp at `boot` is that reading,
/// and no record after this one is stamped earlier than one before it.
pub fn set_counter(boot: u64, period_fs: u64) {
    let stated = STATED_FS.load(Relaxed);
    let at_boot = ticks_to_nanos(boot, if stated != 0 { stated } else { period_fs });
    STAMP_AT_BOOT.store(at_boot, Relaxed);
    TSC_BOOT.store(boot, Relaxed);
    TSC_PERIOD_FS.store(period_fs, Relaxed);
    publish_page(boot, period_fs, at_boot);
}

fn ticks_to_nanos(ticks: u64, period_fs: u64) -> u64 {
    ((ticks as u128 * period_fs as u128) / 1_000_000) as u64
}

/// Now as a log line's time: nanoseconds since the counter's zero — power-on,
/// or the reset since — at the clock's rate, and before [`set_counter`] at the
/// rate the CPU states. `None` where it states none and the clock has not
/// started: no rate reads the counter yet. Never panics, as
/// [`nanos_since_boot`].
pub fn stamp() -> Option<LogStamp> {
    if calibrated() {
        return Some(LogStamp::since_zero(STAMP_AT_BOOT.load(Relaxed).saturating_add(nanos_since_boot())));
    }
    let stated = STATED_FS.load(Relaxed);
    (stated != 0).then(|| LogStamp::since_zero(ticks_to_nanos(cpu::counter(), stated)))
}

/// The clock page's frame, or 0 before [`set_counter`]: the one every address space
/// maps read-only at [`toyos_abi::clock::CLOCK_PAGE`].
static PAGE_PHYS: AtomicU64 = AtomicU64::new(0);

/// Lay the calibration out on a frame of its own, for every process to read
/// the clock this module reads without asking it. Laid out before any address
/// space can map it, and never written again.
fn publish_page(counter_at_boot: u64, period_fs: u64, stamp_at_boot: u64) {
    use toyos_abi::clock::{ClockPage, CLOCK_MAGIC};
    let bytes = crate::mm::PAGE_2M as usize;
    // Held for the machine's life: every process maps it.
    let frame = crate::process::PageAlloc::new(bytes)
        .expect("clock: no 2 MiB frame for the clock page");
    // SAFETY: a fresh allocation this function owns, `bytes` long, that no
    // address space maps yet; zeroed whole because all of it is mapped, and
    // the page is written through raw pointers because it becomes a user
    // mapping.
    unsafe {
        core::ptr::write_bytes(frame.ptr(), 0, bytes);
        core::ptr::write_volatile(
            frame.ptr() as *mut ClockPage,
            ClockPage { magic: CLOCK_MAGIC, counter_at_boot, period_fs, stamp_at_boot },
        );
    }
    PAGE_PHYS.store(frame.phys(), Release);
    #[expect(clippy::disallowed_methods, reason = "every address space maps the clock page for the machine's life")]
    core::mem::forget(frame);
}

/// Map the clock page into a fresh address space, read-only, at the address
/// the ABI names. A region of its own, so no `mmap` can land on it and a
/// fault in it is refused rather than filled.
pub fn map_page(space: &mut crate::mm::paging::AddressSpace) {
    use crate::mm::policy::{CachePolicy, Prot};
    let phys = PAGE_PHYS.load(Acquire);
    assert!(phys != 0, "clock: an address space was built before the clock page");
    let at = crate::UserAddr::new(toyos_abi::clock::CLOCK_PAGE);
    space.map_range(at, phys, crate::mm::PAGE_2M, Prot::Read, CachePolicy::Normal);
    space.insert_region(
        at,
        crate::vma::Region { size: crate::mm::PAGE_2M, kind: crate::vma::RegionKind::Mapped },
    );
}

/// Whether [`nanos_since_boot`] measures anything yet; false before [`init`].
pub fn calibrated() -> bool {
    TSC_PERIOD_FS.load(Relaxed) != 0
}


/// Nanoseconds since boot; lock-free, no MMIO, and never panics — `log::emit`
/// reads it from inside a bracket where panicking would reenter the log.
/// Saturating, not wrapping: a trailing CPU reads as oldest, not lying newest after a 584-year wrap.
pub fn nanos_since_boot() -> u64 {
    ticks_to_nanos(cpu::counter().saturating_sub(TSC_BOOT.load(Relaxed)), TSC_PERIOD_FS.load(Relaxed))
}

/// The same reading as an [`Instant`], the type arithmetic on it is allowed in.
/// The one bridge between hardware and `crate::time`, which stays `core`-only for `kernel-loom`.
pub fn now() -> Instant {
    Instant::from_nanos_since_boot(nanos_since_boot())
}

/// The [`cpu::rdtsc`] value `nanos` in the future, for a wait loop that must
/// not call the nanosecond clock.
pub fn tsc_deadline(nanos: u64) -> u64 {
    cpu::counter().saturating_add(counter_ticks(nanos))
}

/// `nanos` as a count of [`cpu::counter`] ticks: a span converted once and then compared
/// against counter differences, which is what a sampler that may not divide
/// needs. Before [`init`] the period is unknown and this is zero, so a bound
/// derived from it is one its arm has to refuse.
pub fn counter_ticks(nanos: u64) -> u64 {
    let period_fs = TSC_PERIOD_FS.load(Relaxed);
    if period_fs == 0 {
        return 0;
    }
    ((nanos as u128 * 1_000_000) / period_fs as u128) as u64
}

/// The span a count of [`counter_ticks`] stands for, for a caller that measured
/// before there was a period to measure with and converts once, afterwards.
/// Zero while the period is unknown, so a span taken on a machine that never
/// calibrated reads as no time rather than as an invented one.
pub fn nanos_of_ticks(ticks: u64) -> u64 {
    ticks_to_nanos(ticks, TSC_PERIOD_FS.load(Relaxed))
}

/// Polls `ready` until it holds or `nanos` pass; `false` is the deadline.
/// Reads the TSC, not [`nanos_since_boot`], because that clock's out-of-line divide
/// would land an NMI sample's `rip` in `compiler_builtins` rather than in the wait.
///
/// **Before [`init`] there is no period to measure a span with, so this asks
/// `ready` once and answers it.** A wait nothing can bound is the one thing an
/// unattended machine may not enter: a device denied its recovery window is a
/// `false` its caller reports, and a spin nothing ends reports nothing at all.
pub fn settles(nanos: u64, ready: impl Fn() -> bool) -> bool {
    if !calibrated() {
        return ready();
    }
    let until = tsc_deadline(nanos);
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

/// Reads the RTC, which keeps UTC, once, after [`init`], and anchors the wall
/// clock to it.
pub fn init_wall(century_reg: Option<u8>) {
    let civil = match crate::arch::rtc::read(century_reg) {
        Ok(civil) => civil,
        Err(fault) => {
            log!("clock: this machine will not say what time it is — {fault}");
            return;
        }
    };

    BOOT_SECS.store(civil.to_unix_secs().saturating_sub(nanos_since_boot() / NANOS_PER_SEC), Relaxed);
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

