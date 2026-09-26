//! The TSC, calibrated against the HPET: x86-64's free-running counter has no
//! stated rate every part can be trusted for, so the boot measures it.

use crate::log;
use crate::mm::paging::MmioPolicy;
use crate::time::{Delay, Duration};

use super::cpu;

const HPET_CAP: u64 = 0x000;
const HPET_CFG: u64 = 0x010;
const HPET_COUNTER: u64 = 0x0F0;

/// Measure the TSC against the HPET at `hpet_base` and start the clock on it.
pub fn calibrate_counter(hpet_base: u64) {
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
    let tsc_start = cpu::counter();
    let hpet_target = hpet_start + calibration_hpet_ticks;
    log!(
        "clock: HPET at {:#x} enabled, period={}fs, counter reads {}, calibrating over {} ticks",
        hpet_base,
        hpet_period_fs,
        hpet_start,
        calibration_hpet_ticks,
    );

    // A main counter that does not advance would spin here forever, and this is
    // the boot's last wait before it has a clock: the only unit available to
    // bound it is the TSC's own, so the budget is the calibration converted at
    // a frequency no x86-64 part reaches, which makes it an over-estimate of
    // the cycles the calibration can legitimately take on any machine.
    const TSC_CEILING_HZ: u64 = 10_000_000_000;
    // Times two, so a machine merely slower than the ceiling is not refused for it.
    let stall_budget_cycles = 2 * calibration_ns * (TSC_CEILING_HZ / 1_000_000_000);
    while hpet.read_u64(HPET_COUNTER) < hpet_target {
        assert!(
            cpu::counter().wrapping_sub(tsc_start) <= stall_budget_cycles,
            "clock: the HPET main counter at {:#x} did not reach {} in {} TSC cycles (it started \
             at {} and reads {}), so this machine offers no clock to calibrate against",
            hpet_base,
            hpet_target,
            stall_budget_cycles,
            hpet_start,
            hpet.read_u64(HPET_COUNTER),
        );
    }
    let tsc_end = cpu::counter();
    let hpet_end = hpet.read_u64(HPET_COUNTER);

    let hpet_elapsed_fs = (hpet_end - hpet_start) as u128 * hpet_period_fs as u128;
    let tsc_delta = tsc_end - tsc_start;
    let tsc_period_fs = (hpet_elapsed_fs / tsc_delta as u128) as u64;

    crate::clock::set_counter(tsc_start, tsc_period_fs);

    let tsc_freq_mhz = 1_000_000_000_000_000u64 / tsc_period_fs / 1_000_000;
    log!("TSC: {}MHz (period={}fs, calibrated over {}ms)", tsc_freq_mhz, tsc_period_fs, calibration_ns / 1_000_000);

    // **The one cross-source check this machine offers.** Everything else the
    // kernel times is derived from the measurement just taken, so it could only
    // agree with itself; CPUID 15H/16H is the part's own statement of the same
    // frequency, arrived at by neither the HPET nor this counting loop, and the
    // parts-per-million between the two is what a metal profile can hold a
    // ceiling against.
    let measured_hz = 1_000_000_000_000_000u64 / tsc_period_fs;
    match cpu::stated_counter_hz() {
        Some(stated) => {
            let apart = measured_hz.abs_diff(stated);
            log!(
                "clock: TSC measured {measured_hz}Hz against the HPET, CPUID states {stated}Hz, \
                 {}ppm apart",
                apart * 1_000_000 / stated,
            );
        }
        None => log!(
            "clock: TSC measured {measured_hz}Hz against the HPET; CPUID leaves 15H and 16H \
             stating no frequency, so nothing independent confirms it"
        ),
    }
}

