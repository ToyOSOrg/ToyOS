//! The machine's clocks: monotonic since boot, read off the CPU's free-running
//! counter at the period the architecture's boot gives [`set_counter`] (on
//! x86-64, measured against the HPET); and wall-clock, read from the
//! architecture's RTC exactly once — a CMOS read can block for up to a
//! second — in [`init_wall`], and answered after as that reading plus
//! [`nanos_since_boot`].

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering::{Acquire, Relaxed, Release}};

use crate::arch::cpu;
use crate::time::Instant;

static TSC_BOOT: AtomicU64 = AtomicU64::new(0);
static TSC_PERIOD_FS: AtomicU64 = AtomicU64::new(0);

/// Start the clock on the counter: its reading at boot and its measured period.
/// The architecture's boot calls this once, after it has the period.
pub fn set_counter(boot: u64, period_fs: u64) {
    TSC_BOOT.store(boot, Relaxed);
    TSC_PERIOD_FS.store(period_fs, Relaxed);
    publish_page(boot, period_fs);
}

/// The clock page's frame, or 0 before [`set_counter`]: the one every address space
/// maps read-only at [`toyos_abi::clock::CLOCK_PAGE`].
static PAGE_PHYS: AtomicU64 = AtomicU64::new(0);

/// Lay the calibration out on a frame of its own, for every process to read
/// the clock this module reads without asking it. Laid out before any address
/// space can map it, and never written again.
fn publish_page(counter_at_boot: u64, period_fs: u64) {
    use toyos_abi::clock::{ClockPage, CLOCK_MAGIC};
    let bytes = crate::mm::PAGE_2M as usize;
    // Held for the machine's life: every process maps it.
    let frame = crate::process::PageAlloc::new(bytes, crate::mm::pmm::Category::SharedMemory)
        .expect("clock: no 2 MiB frame for the clock page");
    // SAFETY: a fresh allocation this function owns, `bytes` long, that no
    // address space maps yet; zeroed whole because all of it is mapped, and
    // the page is written through raw pointers because it becomes a user
    // mapping.
    unsafe {
        core::ptr::write_bytes(frame.ptr(), 0, bytes);
        core::ptr::write_volatile(
            frame.ptr() as *mut ClockPage,
            ClockPage { magic: CLOCK_MAGIC, counter_at_boot, period_fs },
        );
    }
    PAGE_PHYS.store(frame.phys(), Release);
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
    let delta = cpu::counter().saturating_sub(TSC_BOOT.load(Relaxed));
    let period_fs = TSC_PERIOD_FS.load(Relaxed);
    ((delta as u128 * period_fs as u128) / 1_000_000) as u64
}

/// The same reading as an [`Instant`], the type arithmetic on it is allowed in.
/// The one bridge between hardware and `crate::time`, which stays `core`-only for `kernel-loom`.
pub fn now() -> Instant {
    Instant::from_nanos_since_boot(nanos_since_boot())
}

/// The [`cpu::rdtsc`] value `nanos` in the future, for a wait loop that must
/// not call the nanosecond clock.
pub fn tsc_deadline(nanos: u64) -> u64 {
    cpu::counter().saturating_add(tsc_ticks(nanos))
}

/// `nanos` as a count of TSC ticks: a span converted once and then compared
/// against `rdtsc` differences, which is what a sampler that may not divide
/// needs. Before [`init`] the period is unknown and this is zero, so a bound
/// derived from it is one its arm has to refuse.
pub fn tsc_ticks(nanos: u64) -> u64 {
    let period_fs = TSC_PERIOD_FS.load(Relaxed);
    if period_fs == 0 {
        return 0;
    }
    ((nanos as u128 * 1_000_000) / period_fs as u128) as u64
}

/// The span a count of [`tsc_ticks`] stands for, for a caller that measured
/// before there was a period to measure with and converts once, afterwards.
/// Zero while the period is unknown, so a span taken on a machine that never
/// calibrated reads as no time rather than as an invented one.
pub fn nanos_of_ticks(ticks: u64) -> u64 {
    ((ticks as u128 * TSC_PERIOD_FS.load(Relaxed) as u128) / 1_000_000) as u64
}

/// Polls `ready` until it holds or `nanos` pass; `false` is the deadline.
/// Reads the TSC, not [`nanos_since_boot`], because that clock's out-of-line divide
/// would appear as `dump_nmi_probe`'s red under an NMI sample.
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

    BOOT_SECS.store(civil.to_unix_secs().saturating_sub(nanos_since_boot() / 1_000_000_000), Relaxed);
    WALL_KNOWN.store(true, Release);
    log!("clock: the RTC reads {civil} UTC");
}

/// Unix seconds, now. `None` if the RTC never answered.
pub fn utc_secs() -> Option<u64> {
    WALL_KNOWN
        .load(Acquire)
        .then(|| BOOT_SECS.load(Relaxed) + nanos_since_boot() / 1_000_000_000)
}

