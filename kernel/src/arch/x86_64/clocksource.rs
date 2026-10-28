//! x86-64's clock: the TSC, at the rate CPUID leaf 15H states where the part
//! states one and at one measured against the HPET where it does not, held to
//! the HPET by every CPU while it runs (`kernel::clock::reference`).
//!
//! The clock counts the TSC only where every CPU states it invariant, or under
//! a hypervisor, whose TSC is its own to synthesise and whose CPUID need not
//! say so; there the HPET is all that holds it. Every CPU's TSC is bounded
//! against the BSP's as it starts ([`measure_offset`]), and a write to it is
//! caught where `IA32_TSC_ADJUST` is there to show one. Each failure panics:
//! no other counter here can take the clock's place, because every process
//! reads the TSC itself.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use kernel::clock::reference::{self, Calibration, Failure, Kept, Offset, Reference, Sample, Watch};
use toyos_abi::clock::Line;

use super::cpu;
use crate::log;
use crate::mm::Mmio;
use crate::mm::policy::MmioPolicy;
use crate::sync::Lock;
use crate::time::{Delay, Duration, AP_START};

const HPET_CAP: u64 = 0x000;
const HPET_CFG: u64 = 0x010;
const HPET_COUNTER: u64 = 0x0F0;
/// `GCAP_ID` bit 13, `COUNT_SIZE_CAP`: the main counter is 64 bits wide
/// (IA-PC HPET Specification 1.0a, §2.3.4).
const COUNT_SIZE_CAP: u64 = 1 << 13;
/// The HPET as a reference: its tick, and the main counter's clock frequency
/// drift the specification's minimum implementation allows, ±0.05% over any
/// interval of 1 ms or more and ±0.2% over any of 100 µs or less (IA-PC HPET
/// Specification 1.0a, §2.2), which is what the TSC may seem to drift against
/// it without having moved. The 0.2% is taken for every span under 1 ms, which
/// the table leaves unstated between 100 µs and 1 ms.
const fn hpet_reference(period_fs: u64) -> Reference {
    Reference { period_fs, tolerance_ppb: 500_000, settled_ns: 1_000_000, short_tolerance_ppb: 2_000_000 }
}

/// Reads of the HPET per boot sample, of which the narrowest bracket is kept: an
/// SMI or a host's preemption inside one read widens that read alone.
const BOOT_READS: usize = 3;

const CALIBRATION: Delay = Delay::to_measure(
    Duration::from_millis(50),
    "the TSC counted against the HPET where no CPUID leaf states its rate; the runtime check refines it",
);
/// Samples across the calibration, so a step inside it is refused.
const CALIBRATION_SAMPLES: u64 = 6;
/// Calibrations the boot tries before it refuses the machine.
const CALIBRATION_TRIES: u32 = 3;

/// What each CPU waits between its samples: about the half millisecond the
/// HPET's tolerance allows a second, so a step of the 1.7 ms seen on the AMD
/// bench fails the sample after it. One read of the HPET, once a second on
/// each CPU, inside its scheduler pass: 14.8 µs on the T14, whose HPET read
/// brackets 35,690 TSC cycles. One and not the boot's narrowest of three:
/// every bound here allows the read's own bracket, so a read an SMI widened
/// costs that sample precision and never a false failure.
const CHECK_EVERY_NS: u64 = 1_000_000_000;

/// Round trips of a cache line between the BSP and each AP as it starts.
const ROUNDS: u32 = 256;

struct Hpet {
    mmio: Mmio,
    wide: bool,
    reference: Reference,
}

impl Hpet {
    /// The main counter, as 64 bits: a 32-bit counter's reading is taken as
    /// the one nearest `near` (`reference::extend`).
    fn read(&self, near: u64) -> u64 {
        if self.wide {
            self.mmio.read_u64(HPET_COUNTER)
        } else {
            reference::extend(near, self.mmio.read_u32(HPET_COUNTER))
        }
    }

    /// The main counter as it reads, a 32-bit one's from zero.
    fn raw(&self) -> u64 {
        if self.wide { self.mmio.read_u64(HPET_COUNTER) } else { u64::from(self.mmio.read_u32(HPET_COUNTER)) }
    }

    /// The narrowest bracket of `reads` readings near `near`.
    fn sample(&self, near: u64, reads: usize) -> Sample {
        let mut best: Option<Sample> = None;
        for _ in 0..reads {
            let before = cpu::rdtsc_ordered();
            let reference = self.read(near);
            let after = cpu::rdtsc_ordered();
            let sample = Sample { before, after, reference };
            if best.is_none_or(|best| sample.width() < best.width()) {
                best = Some(sample);
            }
        }
        best.expect("a sample takes a read")
    }

    /// Where the counter should read now, from `last` and the rate the clock
    /// keeps: what a 32-bit counter's wraps are counted against.
    fn near(&self, last: Sample, mult: u64) -> u64 {
        let nanos = Line { counter: 0, stamp: 0, mult }.nanos(cpu::counter().saturating_sub(last.before));
        last.reference + (u128::from(nanos) * 1_000_000 / u128::from(self.reference.period_fs)) as u64
    }
}

/// What the runtime check holds, once the boot has started the clock.
struct Held {
    hpet: Hpet,
    watch: Watch,
}

static HELD: Lock<Option<Held>> = Lock::new(None);

/// Whether CPUID states `IA32_TSC_ADJUST`, read once on the BSP.
static TSC_ADJUST: AtomicBool = AtomicBool::new(false);

/// Each CPU's part of the check: when it next samples, and the
/// `IA32_TSC_ADJUST` it started with.
pub struct Local {
    due_ns: AtomicU64,
    adjust: AtomicU64,
    /// What `tsc-step` adds to this CPU's readings.
    #[cfg(feature = "boot-actuators")]
    staged: AtomicU64,
}

impl Local {
    pub const fn new() -> Self {
        Self {
            due_ns: AtomicU64::new(0),
            adjust: AtomicU64::new(0),
            #[cfg(feature = "boot-actuators")]
            staged: AtomicU64::new(0),
        }
    }
}

/// Start the clock on the TSC, at its stated rate or one measured against the
/// HPET at `hpet_base`, and hand the HPET to the runtime check.
pub fn start(hpet_base: u64) {
    admit("cpu0");
    TSC_ADJUST.store(cpu::has_tsc_adjust(), Ordering::Relaxed);
    note_adjust();

    let mmio = crate::mm::paging::map_mmio(hpet_base, 0x1000, MmioPolicy::Uncacheable);
    let cap = mmio.read_u64(HPET_CAP);
    let period_fs = cap >> 32;
    assert!(period_fs > 0, "HPET: invalid counter period");
    let cfg = mmio.read_u64(HPET_CFG);
    mmio.write_u64(HPET_CFG, cfg | 1);
    let hpet = Hpet { mmio, wide: cap & COUNT_SIZE_CAP != 0, reference: hpet_reference(period_fs) };
    log!(
        "clock: HPET at {hpet_base:#x} enabled, period={period_fs}fs, {} bits, counter reads {}",
        if hpet.wide { 64 } else { 32 },
        hpet.raw(),
    );

    let (anchor, kept) = match cpu::stated_counter_hz() {
        Some(hz) => {
            let anchor = hpet.sample(hpet.raw(), BOOT_READS);
            log!(
                "clock: the TSC counts at {hz} Hz, as CPUID 15H states, held to the HPET from a reading {} cycles wide",
                anchor.width()
            );
            (anchor, Kept { mult: Line::mult_of_hz(hz), bound_ppb: 1, measured: false })
        }
        None => calibrate(&hpet),
    };
    crate::clock::set_counter(anchor.before, kept.mult);
    *HELD.lock() = Some(Held { watch: Watch::new(hpet.reference, anchor, kept), hpet });
}

/// The TSC's rate against the HPET over [`CALIBRATION`], first sample to last,
/// refused where a sample between leaves the line, and the first sample.
fn calibrate(hpet: &Hpet) -> (Sample, Kept) {
    let span_ticks = CALIBRATION.nanos() * 1_000_000 / hpet.reference.period_fs;
    for attempt in 1..=CALIBRATION_TRIES {
        // Every sample is within the window of the first, far under the 2^31
        // ticks a 32-bit counter's reading is taken near.
        let first = hpet.sample(hpet.raw(), BOOT_READS);
        let mut samples = [first; CALIBRATION_SAMPLES as usize];
        for (i, slot) in samples.iter_mut().enumerate().skip(1) {
            let target = first.reference + span_ticks * i as u64 / (CALIBRATION_SAMPLES - 1);
            wait_for(hpet, target, first);
            *slot = hpet.sample(first.reference, BOOT_READS);
        }
        match reference::calibrate(hpet.reference, &samples) {
            Ok(rate) => {
                let line = Line { counter: 0, stamp: 0, mult: rate.mult };
                log!(
                    "clock: the TSC counts at {} mHz against the HPET over {} ms, within {} ppb; CPUID 15H states no rate, so the HPET goes on refining it",
                    line.millihertz(),
                    rate.span_ns / 1_000_000,
                    rate.bound_ppb,
                );
                return (first, Kept { mult: rate.mult, bound_ppb: rate.bound_ppb, measured: true });
            }
            Err(Calibration::Unmeasurable(why)) => {
                log!("clock: calibration {attempt} measured nothing ({why:?}): {samples:?}")
            }
            Err(Calibration::Disturbed { index, agreement }) => log!(
                "clock: calibration {attempt} refused: sample {index} sits {} ns off the line, {} allowed: {samples:?}",
                agreement.apart_ns,
                agreement.allowed_ns,
            ),
        }
    }
    panic!("clock: {CALIBRATION_TRIES} calibrations of the TSC against the HPET were each disturbed, so this machine keeps no time");
}

/// Spin until the HPET reads `target`. A main counter that does not advance
/// would spin here forever, and this is the boot's last wait before it has a
/// clock: the only unit available to bound it is the TSC's own, so the budget
/// is the calibration converted at a frequency no x86-64 part reaches, twice.
fn wait_for(hpet: &Hpet, target: u64, first: Sample) {
    const TSC_CEILING_HZ: u64 = 10_000_000_000;
    let budget = 2 * CALIBRATION.nanos() * (TSC_CEILING_HZ / 1_000_000_000);
    while hpet.read(first.reference) < target {
        assert!(
            cpu::counter().wrapping_sub(first.after) <= budget,
            "clock: the HPET main counter did not reach {target} in {budget} TSC cycles (it read {} at the \
             first sample and reads {}), so this machine offers no clock to calibrate against",
            first.reference,
            hpet.read(first.reference),
        );
    }
}

/// Refuse a CPU whose TSC this clock cannot keep time on.
fn admit(cpu: &str) {
    assert!(cpu::has_rdtscp(), "clock: {cpu} has no RDTSCP, which every read the clock bounds is ordered by");
    if cpu::invariant_tsc() {
        return;
    }
    assert!(
        cpu::under_hypervisor(),
        "clock: {cpu} does not state its TSC invariant (CPUID.80000007H:EDX[8]), so its rate may change with its \
         power state and no clock can be kept on it"
    );
    log!("clock: {cpu}'s hypervisor does not state its TSC invariant; the HPET alone holds it");
}

fn note_adjust() {
    if TSC_ADJUST.load(Ordering::Relaxed) {
        crate::cpu_local::here().arch.clock.adjust.store(cpu::rdmsr(cpu::IA32_TSC_ADJUST), Ordering::Relaxed);
    }
}

/// This AP's part of its start, before it echoes: the same refusal as the BSP's.
pub fn start_ap(cpu_id: u32) {
    admit(&alloc::format!("cpu{cpu_id}"));
    note_adjust();
}

/// The round trips' line: whose turn it is, odd the AP's and even the BSP's,
/// and the counter its last writer read just before it passed the turn on. A
/// turn and not the counter's own value says whose word it is, because a
/// hypervisor's counter can read the same twice.
#[repr(align(64))]
struct Trip {
    turn: AtomicU64,
    counter: AtomicU64,
}

static TRIP: Trip = Trip { turn: AtomicU64::new(0), counter: AtomicU64::new(0) };
/// The AP's least trip out, once it has made every one.
static LEAST_OUT: AtomicU64 = AtomicU64::new(0);
static OUT_DONE: AtomicBool = AtomicBool::new(false);

/// Pass the turn on as `turn`, with the counter read just before.
fn pass(turn: u64) {
    TRIP.counter.store(cpu::rdtsc_ordered(), Ordering::Relaxed);
    TRIP.turn.store(turn, Ordering::Release);
}

/// Wait for `turn`, and answer the counter its writer read and the trip:
/// this CPU's counter now, less that one.
fn arrive(turn: u64) -> Option<i64> {
    if !crate::clock::settles(AP_START.nanos(), || TRIP.turn.load(Ordering::Acquire) == turn) {
        return None;
    }
    let theirs = TRIP.counter.load(Ordering::Relaxed);
    Some(cpu::rdtsc_ordered().wrapping_sub(theirs) as i64)
}

/// The BSP's half of [`ROUNDS`] round trips with the AP just started: how far
/// `cpu_id`'s TSC can be from this one's (`reference::Offset`). Panics on an
/// offset the trips prove, and on an AP that stops answering.
pub fn measure_offset(cpu_id: u32) {
    let mut least_back = i64::MAX;
    for round in 0..u64::from(ROUNDS) {
        pass(2 * round + 1);
        let back = arrive(2 * round + 2);
        let back = back.unwrap_or_else(|| panic!("clock: cpu{cpu_id} stopped answering the round trips that bound its TSC at round {round}"));
        least_back = least_back.min(back);
    }
    let done = crate::clock::settles(AP_START.nanos(), || OUT_DONE.load(Ordering::Acquire));
    assert!(done, "clock: cpu{cpu_id} never reported its round trips");
    OUT_DONE.store(false, Ordering::Relaxed);
    TRIP.turn.store(0, Ordering::Relaxed);
    let offset = Offset { least_out: LEAST_OUT.load(Ordering::Relaxed) as i64, least_back };
    match offset.bound() {
        Ok((lo, hi)) => log!("clock: cpu{cpu_id}'s TSC is within {lo}..={hi} cycles of cpu0's, over {ROUNDS} round trips"),
        Err(proven) => panic!(
            "clock: cpu{cpu_id}'s TSC is {} cpu0's by at least {} cycles, which {ROUNDS} round trips prove ({offset:?})",
            if proven < 0 { "behind" } else { "ahead of" },
            proven.unsigned_abs(),
        ),
    }
}

/// The AP's half of [`measure_offset`], after its echo.
pub fn answer_offset() {
    let mut least_out = i64::MAX;
    for round in 0..u64::from(ROUNDS) {
        let out = arrive(2 * round + 1);
        let out = out.unwrap_or_else(|| {
            panic!("clock: cpu{} waited for a round trip the BSP never sent, round {round}", super::percpu::cpu_id())
        });
        least_out = least_out.min(out);
        pass(2 * round + 2);
    }
    LEAST_OUT.store(least_out as u64, Ordering::Relaxed);
    OUT_DONE.store(true, Ordering::Release);
}

/// This CPU's sample against the HPET, at most once a [`CHECK_EVERY_NS`]:
/// called from every scheduler pass, where a CPU not due pays one load.
pub fn check(now_ns: u64) {
    let local = &crate::cpu_local::here().arch.clock;
    if now_ns < local.due_ns.load(Ordering::Relaxed) {
        return;
    }
    // Another CPU sampling now: this one samples on its next pass.
    let Some(mut held) = HELD.try_lock() else {
        return;
    };
    let Some(held) = held.as_mut() else {
        return;
    };
    local.due_ns.store(now_ns + CHECK_EVERY_NS, Ordering::Relaxed);
    let cpu_id = super::percpu::cpu_id();
    if TSC_ADJUST.load(Ordering::Relaxed) {
        let started = local.adjust.load(Ordering::Relaxed);
        let adjust = cpu::rdmsr(cpu::IA32_TSC_ADJUST);
        assert!(
            adjust == started,
            "clock: cpu{cpu_id}'s TSC was written: IA32_TSC_ADJUST reads {} and read {} when it started",
            adjust as i64,
            started as i64,
        );
    }
    let sample = held.hpet.sample(held.hpet.near(held.watch.last(), held.watch.kept().mult), 1);
    #[cfg(feature = "boot-actuators")]
    let sample = staged(local, sample, false);
    match held.watch.take(sample) {
        Ok(None) => {}
        Ok(Some(report)) => {
            #[cfg(feature = "boot-actuators")]
            staged(local, sample, true);
            let kept = Line { counter: 0, stamp: 0, mult: report.kept.mult };
            if report.retuned {
                crate::clock::retune(report.kept.mult);
            }
            log!(
                "clock: over {} s the TSC counted {} mHz against the HPET, within {} ppb; the clock {} {} mHz, within {} ppb",
                report.measured.span_ns / 1_000_000_000,
                Line { counter: 0, stamp: 0, mult: report.measured.mult }.millihertz(),
                report.measured.bound_ppb,
                if report.retuned { "now keeps" } else if report.kept.measured { "keeps" } else { "keeps CPUID 15H's" },
                kept.millihertz(),
                report.kept.bound_ppb,
            );
        }
        Err(Failure::Step { from, to, agreement }) => panic!(
            "clock: cpu{cpu_id}'s TSC stepped: {} ns apart from the HPET over {} ns since the last sample, {} allowed \
             ({from:?} to {to:?})",
            agreement.apart_ns,
            agreement.reference_ns,
            agreement.allowed_ns,
        ),
        Err(Failure::Drift { from, to, agreement }) => panic!(
            "clock: cpu{cpu_id}'s TSC drifted from the rate the clock keeps: {} ns apart from the HPET over {} ns, {} \
             allowed ({from:?} to {to:?})",
            agreement.apart_ns,
            agreement.reference_ns,
            agreement.allowed_ns,
        ),
    }
}

/// `tsc-step`: from the first report on, the first CPU to report reads its
/// TSC 10 ms ahead of where it is. A hypervisor's TSC may ignore a write
/// (QEMU's TCG does), so the step is staged in the readings the check takes.
#[cfg(feature = "boot-actuators")]
fn staged(local: &Local, sample: Sample, reported: bool) -> Sample {
    static STEPPED: AtomicBool = AtomicBool::new(false);
    if crate::actuator::tsc_step() && reported && !STEPPED.swap(true, Ordering::Relaxed) {
        let ahead = crate::clock::counter_ticks(10_000_000);
        local.staged.store(ahead, Ordering::Relaxed);
        log!("tsc-step: cpu{}'s TSC reads {ahead} cycles ahead from here", super::percpu::cpu_id());
    }
    let ahead = local.staged.load(Ordering::Relaxed);
    Sample { before: sample.before + ahead, after: sample.after + ahead, ..sample }
}
