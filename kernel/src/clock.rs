//! The machine's clocks: monotonic since boot, calibrated from the HPET at
//! boot and read off the TSC after; and wall-clock, read from the CMOS RTC
//! exactly once — a CMOS read can block for up to a second — in
//! [`init_wall`], and answered after as that reading plus [`nanos_since_boot`].

use core::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering::{Acquire, Relaxed, Release}};

use crate::mm::paging::MmioPolicy;
use crate::arch::cpu;
use crate::time::{Delay, Duration, Instant};

const HPET_CAP: u64 = 0x000;
const HPET_CFG: u64 = 0x010;
const HPET_COUNTER: u64 = 0x0F0;

static TSC_BOOT: AtomicU64 = AtomicU64::new(0);
static TSC_PERIOD_FS: AtomicU64 = AtomicU64::new(0);

pub fn init(hpet_base: u64) {
    let hpet = crate::mm::paging::map_mmio(hpet_base, 0x1000, MmioPolicy::Uncacheable);

    let cap = hpet.read_u64(HPET_CAP);
    let hpet_period_fs = cap >> 32;
    assert!(hpet_period_fs > 0, "HPET: invalid counter period");

    let cfg = hpet.read_u64(HPET_CFG);
    hpet.write_u64(HPET_CFG, cfg | 1);

    const CALIBRATION: Delay = Delay::to_measure(
        Duration::from_millis(50),
        "TSC ticks counted against the HPET; longer is a better ratio and boot time is what it costs",
    );
    let calibration_ns = CALIBRATION.nanos();
    let calibration_hpet_ticks = calibration_ns * 1_000_000 / hpet_period_fs;

    let hpet_start = hpet.read_u64(HPET_COUNTER);
    let tsc_start = cpu::rdtsc();
    let hpet_target = hpet_start + calibration_hpet_ticks;
    while hpet.read_u64(HPET_COUNTER) < hpet_target {}
    let tsc_end = cpu::rdtsc();
    let hpet_end = hpet.read_u64(HPET_COUNTER);

    let hpet_elapsed_fs = (hpet_end - hpet_start) as u128 * hpet_period_fs as u128;
    let tsc_delta = tsc_end - tsc_start;
    let tsc_period_fs = (hpet_elapsed_fs / tsc_delta as u128) as u64;

    TSC_BOOT.store(tsc_start, Relaxed);
    TSC_PERIOD_FS.store(tsc_period_fs, Relaxed);

    let tsc_freq_mhz = 1_000_000_000_000_000u64 / tsc_period_fs / 1_000_000;
    log!("TSC: {}MHz (period={}fs, calibrated over {}ms)", tsc_freq_mhz, tsc_period_fs, calibration_ns / 1_000_000);
}

/// Whether [`nanos_since_boot`] measures anything yet; false before [`init`].
pub fn calibrated() -> bool {
    TSC_PERIOD_FS.load(Relaxed) != 0
}

/// Nanoseconds since boot; lock-free, no MMIO, and never panics — `log::emit`
/// reads it from inside a bracket where panicking would reenter the log.
/// Saturating, not wrapping: a trailing CPU reads as oldest, not lying newest after a 584-year wrap.
pub fn nanos_since_boot() -> u64 {
    let delta = cpu::rdtsc().saturating_sub(TSC_BOOT.load(Relaxed));
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
    let period_fs = TSC_PERIOD_FS.load(Relaxed);
    let ticks = (nanos as u128 * 1_000_000) / period_fs.max(1) as u128;
    cpu::rdtsc().saturating_add(ticks as u64)
}

/// Polls `ready` until it holds or `nanos` pass; `false` is the deadline.
/// Reads the TSC, not [`nanos_since_boot`], because that clock's out-of-line divide
/// would appear as `src/redlist.rs`'s `dump_nmi_probe` red under an NMI sample.
/// Before [`init`] the TSC period is zero and the wait is unbounded.
pub fn settles(nanos: u64, ready: impl Fn() -> bool) -> bool {
    let until = tsc_deadline(nanos);
    while !ready() {
        if cpu::rdtsc() >= until {
            return false;
        }
        core::hint::spin_loop();
    }
    true
}

/// Unix seconds, in the machine's own zone, at `nanos_since_boot() == 0`.
static BOOT_LOCAL_SECS: AtomicU64 = AtomicU64::new(0);
/// Seconds to add to the machine's zone to get UTC (`Localtime = UTC - TimeZone`).
static UTC_OFFSET_SECS: AtomicI64 = AtomicI64::new(0);
/// Whether the two above mean anything; zero is a valid instant and offset, not a sentinel.
static WALL_KNOWN: AtomicBool = AtomicBool::new(false);

/// Reads the RTC once, after [`init`], and anchors the wall clock to it.
pub fn init_wall(century_reg: Option<u8>, utc_offset_minutes: Option<i32>) {
    // OVMF never names a zone, so `rtc_zone_east` is a test actuator forcing
    // UTC+2 (`Localtime = UTC - TimeZone`, so east is negative: -120).
    let utc_offset_minutes =
        if crate::actuator::rtc_zone_east() { Some(-120) } else { utc_offset_minutes };

    let civil = match crate::rtc::read(century_reg) {
        Ok(civil) => civil,
        Err(fault) => {
            log!("clock: this machine will not say what time it is — {fault}");
            return;
        }
    };

    let local = civil.to_unix_secs();
    let offset_secs = utc_offset_minutes.unwrap_or(0) as i64 * 60;
    BOOT_LOCAL_SECS.store(local.saturating_sub(nanos_since_boot() / 1_000_000_000), Relaxed);
    UTC_OFFSET_SECS.store(offset_secs, Relaxed);
    WALL_KNOWN.store(true, Release);

    match utc_offset_minutes {
        Some(minutes) => log!("clock: the RTC reads {civil}, {minutes} minutes from UTC by firmware"),
        None => log!("clock: the RTC reads {civil}; firmware named no zone, so it is taken as UTC"),
    }
}

/// Local wall-clock time — what FAT stamps use, since FAT stores local time
/// by specification. `None` if the RTC never answered.
pub fn local_secs() -> Option<u64> {
    WALL_KNOWN
        .load(Acquire)
        .then(|| BOOT_LOCAL_SECS.load(Relaxed) + nanos_since_boot() / 1_000_000_000)
}

/// The same instant in Unix seconds (UTC) — what `SYS_CLOCK_EPOCH` serves.
pub fn utc_secs() -> Option<u64> {
    let local = local_secs()?;
    Some(local.saturating_add_signed(UTC_OFFSET_SECS.load(Relaxed)))
}

