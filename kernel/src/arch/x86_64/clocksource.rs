//! x86-64's clock: the TSC, at the rate CPUID leaf 15H states where the part
//! states one and at one measured against the HPET where it does not, and held
//! to the HPET while it runs (`kernel::clock::reference`).
//!
//! The clock counts the TSC only where every CPU states it invariant, or under
//! a hypervisor, whose TSC is its own to synthesise and whose CPUID need not
//! say so; there the HPET is all that holds it. Every AP's TSC is bounded
//! against the BSP's as it starts ([`measure_offset`]). While it runs, one CPU
//! a second samples the HPET for the whole machine's rate and steps, over the
//! spans in which some CPU stayed awake; and every CPU a second checks its
//! `IA32_TSC_ADJUST` and that its TSC is not behind another's ([`check`]).
//! Each failure panics: no other counter here can take the clock's place,
//! because every process reads the TSC itself.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use alloc::vec::Vec;

use kernel::clock::reference::{self, Agreement, Arbitration, Calibration, Failure, Kept, Offset, Reference, Sample, Watch};
use toyos_abi::clock::Line;

use super::cpu;
use super::pio::{self, Declared};
use toyos_userbound::{Mediated, Ports};
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

/// Once a second: the step allowance at this gap is about 0.5 ms, under the
/// 1.7 ms step the AMD bench was seen to take. One HPET read a sample: every
/// bound allows its bracket.
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
            reference::extend(near, u64::from(self.mmio.read_u32(HPET_COUNTER)), 32)
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

/// The ACPI PM timer, the second reference: which of the TSC and the HPET
/// moved, where the two disagree. **It and the HPET are both the chipset's
/// (the FCH's on AMD, the PCH's on Intel)**, and may share its clock, so a
/// stall common to both reads as the TSC having moved.
struct PmTimer {
    port: Declared,
    bits: u32,
    reference: Reference,
    last: Sample,
}

/// ACPI 6.5 §4.8.3.3: 3.579545 MHz. ACPI states no tolerance for it; the
/// HPET's is taken, the same chipset's clock.
const PM_TIMER_PERIOD_FS: u64 = 279_365_114;

impl PmTimer {
    fn new(named: toyos_acpi::PmTimer) -> Result<Self, alloc::string::String> {
        let ports = Ports::new(named.port, 4).ok_or("its block runs past the port space")?;
        let port = pio::declare("the ACPI PM timer", ports, Mediated::ReadOnly).map_err(|e| alloc::format!("{e:?}"))?;
        let mut timer = Self {
            port,
            bits: u32::from(named.bits),
            reference: hpet_reference(PM_TIMER_PERIOD_FS),
            last: Sample { before: 0, after: 0, reference: 0 },
        };
        let first = timer.read_raw();
        // A tick is 279 ns: a counter that does not move within 100 µs counts nothing.
        let until = cpu::counter() + crate::clock::counter_ticks(100_000);
        while timer.read_raw() == first {
            if cpu::counter() > until {
                return Err(alloc::format!("port {:#x} reads {first:#x} and does not count", named.port));
            }
        }
        timer.last = timer.sample(timer.read_raw());
        Ok(timer)
    }

    fn read_raw(&self) -> u64 {
        u64::from(cpu::inl(self.port.port(0))) & ((1u64 << self.bits) - 1)
    }

    fn sample(&self, near: u64) -> Sample {
        let before = cpu::rdtsc_ordered();
        let low = self.read_raw();
        let after = cpu::rdtsc_ordered();
        Sample { before, after, reference: reference::extend(near, low, self.bits) }
    }

    /// The next sample, its wraps counted from the last by the kept rate.
    fn next(&self, mult: u64) -> Sample {
        let nanos = Line { counter: 0, stamp: 0, mult }.nanos(cpu::counter().saturating_sub(self.last.before));
        self.sample(self.last.reference + (u128::from(nanos) * 1_000_000 / u128::from(PM_TIMER_PERIOD_FS)) as u64)
    }
}

/// What the HPET's sampler holds, once the boot has started the clock.
struct Held {
    hpet: Hpet,
    watch: Watch,
    pm: Option<PmTimer>,
    /// Spans left out, and of them those across which the HPET, and the PM
    /// timer, disagreed with the TSC: what the left-out spans would have read.
    unheld_hpet_apart: u64,
    unheld_pm_apart: u64,
    /// Each CPU's [`crate::idle::Local::deep_periods`] at the last sample.
    idle: Vec<(u64, u64)>,
}

impl Held {
    /// Whether some CPU stayed out of every declared idle state from the last
    /// sample to now: no package state deeper than its own can have held in
    /// between, so the HPET counted throughout. Where every CPU spent part of
    /// the span in one, the package may have reached a state that stops the
    /// HPET, and the span is not held.
    fn some_cpu_stayed_awake(&mut self) -> bool {
        let cpus = crate::smp::cpu_count() as usize;
        self.idle.resize(cpus, (0, 0));
        let mut awake = false;
        for (cpu, was) in self.idle.iter_mut().enumerate() {
            let local = crate::cpu_local::get(cpu as u32).expect("a CPU the roster counts has its record");
            let now = local.idle.deep_periods();
            awake |= now == *was && now.0 == now.1;
            *was = now;
        }
        awake
    }
}

static HELD: Lock<Option<Held>> = Lock::new(None);

/// Whether CPUID states `IA32_TSC_ADJUST`, read once on the BSP.
static TSC_ADJUST: AtomicBool = AtomicBool::new(false);

/// When the HPET is next sampled, by whichever CPU's pass gets there first.
static NEXT_SAMPLE_NS: AtomicU64 = AtomicU64::new(0);

/// The latest TSC any CPU's check read, on a line of its own.
#[repr(align(64))]
struct Latest(AtomicU64);

static LATEST: Latest = Latest(AtomicU64::new(0));

/// Each CPU's part of the check: when it next checks itself, and the
/// `IA32_TSC_ADJUST` it started with.
pub struct Local {
    due_ns: AtomicU64,
    adjust: AtomicU64,
}

impl Local {
    pub const fn new() -> Self {
        Self { due_ns: AtomicU64::new(0), adjust: AtomicU64::new(0) }
    }
}

/// Start the clock on the TSC, at its stated rate or one measured against the
/// HPET at `hpet_base`, and hand the HPET to the runtime check.
pub fn start(hpet_base: u64, pm_timer: Result<toyos_acpi::PmTimer, alloc::string::String>) {
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
    let pm = pm_timer.and_then(PmTimer::new);
    match &pm {
        Ok(pm) => log!("clock: the ACPI PM timer at {:#x}, {} bits, tells the TSC from the HPET where they disagree", pm.port.port(0).number(), pm.bits),
        Err(why) => log!("clock: no ACPI PM timer to tell the TSC from the HPET ({why}); a step between them panics"),
    }
    *HELD.lock() = Some(Held {
        watch: Watch::new(hpet.reference, anchor, kept),
        hpet,
        pm: pm.ok(),
        unheld_hpet_apart: 0,
        unheld_pm_apart: 0,
        idle: Vec::new(),
    });
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

/// This CPU's checks, at most once a [`CHECK_EVERY_NS`], and the machine's
/// HPET sample on the first CPU due: called from every scheduler pass, where a
/// CPU not due pays one load.
///
/// A CPU checks that its `IA32_TSC_ADJUST` is the one it started with, and
/// that its TSC reads no earlier than the latest any CPU's check read before
/// this one loaded it: a reading behind it is this CPU's TSC proven behind by
/// the difference (`reference::Offset`'s argument, one way).
pub fn check(now_ns: u64) {
    let local = &crate::cpu_local::here().arch.clock;
    if now_ns < local.due_ns.load(Ordering::Relaxed) {
        return;
    }
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
    let latest = LATEST.0.load(Ordering::Acquire);
    let mine = cpu::rdtsc_ordered();
    assert!(
        mine >= latest,
        "clock: cpu{cpu_id}'s TSC read {mine}, {} cycles behind the {latest} another CPU's check read before it",
        latest - mine,
    );
    LATEST.0.fetch_max(mine, Ordering::AcqRel);

    let due = NEXT_SAMPLE_NS.load(Ordering::Relaxed);
    if now_ns < due || NEXT_SAMPLE_NS.compare_exchange(due, now_ns + CHECK_EVERY_NS, Ordering::Relaxed, Ordering::Relaxed).is_err() {
        return;
    }
    let Some(mut held) = HELD.try_lock() else {
        return;
    };
    if let Some(held) = held.as_mut() {
        sample(held, cpu_id);
    }
}

/// One sample of the HPET, and of the PM timer beside it, held to the TSC
/// where the span since the last can be.
fn sample(held: &mut Held, cpu_id: u32) {
    let awake = held.some_cpu_stayed_awake();
    let kept = held.watch.kept();
    let last = held.watch.last();
    let hpet = held.hpet.sample(held.hpet.near(last, kept.mult), 1);
    let pm = held.pm.as_ref().map(|pm| (pm.last, pm.next(kept.mult)));
    #[cfg(feature = "boot-actuators")]
    let (hpet, pm) = staged(hpet, pm, false, held.hpet.reference.period_fs);
    let against = |reference: Reference, from: Sample, to: Sample| {
        let span = reference::Span::between(reference, from, to);
        let tolerance = kept.bound_ppb + reference.tolerance_over(reference.span_ns(from, to));
        reference::agree_over(reference, span, kept.mult, tolerance)
    };
    let pm_reference = held.pm.as_ref().map(|pm| pm.reference);
    let pm_agreement = pm.zip(pm_reference).map(|((from, to), reference)| against(reference, from, to));
    if let (Some(timer), Some((_, to))) = (held.pm.as_mut(), pm) {
        timer.last = to;
    }
    if !awake {
        held.unheld_hpet_apart += u64::from(!against(held.hpet.reference, last, hpet).holds());
        held.unheld_pm_apart += u64::from(pm_agreement.is_some_and(|a| !a.holds()));
        let _ = held.watch.take(hpet, false);
        return;
    }
    match held.watch.take(hpet, true) {
        Ok(None) => {}
        Ok(Some(report)) => {
            #[cfg(feature = "boot-actuators")]
            staged(hpet, pm, true, held.hpet.reference.period_fs);
            let kept = Line { counter: 0, stamp: 0, mult: report.kept.mult };
            if report.retuned {
                crate::clock::retune(report.kept.mult);
            }
            let measured = Line { counter: 0, stamp: 0, mult: report.measured.mult }.millihertz();
            let (span_s, bound) = (report.measured.span_ns / 1_000_000_000, report.measured.bound_ppb);
            let unheld = alloc::format!(
                "{} spans unheld, across which the HPET read apart from the TSC in {} and the PM timer in {}",
                report.unheld, held.unheld_hpet_apart, held.unheld_pm_apart,
            );
            match (report.kept.measured, report.retuned) {
                (false, _) => log!(
                    "clock: over {span_s} s the TSC counted {measured} mHz against the HPET, within {bound} ppb; \
                     the clock keeps CPUID 15H's {} mHz; {unheld}",
                    kept.millihertz(),
                ),
                (true, retuned) => log!(
                    "clock: over {span_s} s the TSC counted {measured} mHz against the HPET, within {bound} ppb; \
                     the clock {} {} mHz, within {} ppb; {unheld}",
                    if retuned { "now keeps" } else { "keeps" },
                    kept.millihertz(),
                    report.kept.bound_ppb,
                ),
            }
        }
        Err(Failure::Step { from, to, agreement }) => {
            let spans = spans(agreement, pm_agreement, from, to, pm);
            match pm_agreement.map(|second| reference::arbitrate(agreement, second)) {
                Some(Arbitration::FirstMoved) => {
                    log!("clock: the HPET moved and the TSC did not, so this span is left out: {spans}");
                    let _ = held.watch.take(hpet, false);
                }
                Some(Arbitration::AllDisagree) => {
                    log!("clock: the TSC, the HPET and the ACPI PM timer all disagree, so this span is left out: {spans}");
                    let _ = held.watch.take(hpet, false);
                }
                Some(Arbitration::CounterMoved) => panic!(
                    "clock: the TSC and the HPET disagree by {} ns over the {} ns the HPET counted since the last \
                     sample, {} allowed, and the ACPI PM timer agrees with the HPET: two of the chipset's counters \
                     agree against the TSC, which moved, or both stalled together (cpu{cpu_id}; {spans})",
                    agreement.apart_ns,
                    agreement.reference_ns,
                    agreement.allowed_ns,
                ),
                None => panic!(
                    "clock: the TSC and the HPET disagree by {} ns over the {} ns the HPET counted since the last \
                     sample, {} allowed, and no PM timer says which moved (cpu{cpu_id}; {spans})",
                    agreement.apart_ns,
                    agreement.reference_ns,
                    agreement.allowed_ns,
                ),
            }
        }
        Err(Failure::Drift { since, to, agreement }) => panic!(
            "clock: the TSC and the HPET disagree by {} ns over the {} ns the HPET counted in held spans, {} allowed: \
             their rates are further apart than the kept rate's bound and the HPET's tolerance (cpu{cpu_id}, the \
             open span from {since:?} to {to:?})",
            agreement.apart_ns,
            agreement.reference_ns,
            agreement.allowed_ns,
        ),
    }
}

/// All three counters' spans, for a disagreement's record.
fn spans(
    hpet: Agreement,
    pm: Option<Agreement>,
    from: Sample,
    to: Sample,
    pm_samples: Option<(Sample, Sample)>,
) -> alloc::string::String {
    let pm = match (pm, pm_samples) {
        (Some(a), Some((pm_from, pm_to))) => alloc::format!(
            "the PM timer counted {} ns and reads {} ns apart, {} allowed ({pm_from:?} to {pm_to:?})",
            a.reference_ns, a.apart_ns, a.allowed_ns,
        ),
        _ => "no PM timer".into(),
    };
    alloc::format!(
        "the TSC counted {} ticks, the HPET {} ns and reads {} ns apart, {} allowed ({from:?} to {to:?}); {pm}",
        to.before.wrapping_sub(from.before),
        hpet.reference_ns,
        hpet.apart_ns,
        hpet.allowed_ns,
    )
}

/// `tsc-step` and `hpet-stall`: from the first report on, every sample reads
/// either the TSC 10 ms ahead of where it is, in both references' readings,
/// or the HPET 10 ms behind, as a TSC that stepped and an HPET that stood
/// still. A hypervisor's TSC may ignore a write (QEMU's TCG does), so each is
/// staged in the readings the sampler takes.
#[cfg(feature = "boot-actuators")]
fn staged(hpet: Sample, pm: Option<(Sample, Sample)>, reported: bool, period_fs: u64) -> (Sample, Option<(Sample, Sample)>) {
    static TSC_AHEAD: AtomicU64 = AtomicU64::new(0);
    static HPET_BEHIND: AtomicU64 = AtomicU64::new(0);
    if reported && TSC_AHEAD.load(Ordering::Relaxed) == 0 && HPET_BEHIND.load(Ordering::Relaxed) == 0 {
        if crate::actuator::tsc_step() {
            let ahead = crate::clock::counter_ticks(10_000_000);
            TSC_AHEAD.store(ahead, Ordering::Relaxed);
            log!("tsc-step: the TSC reads {ahead} cycles ahead from here");
        }
        if crate::actuator::hpet_stall() {
            let behind = 10_000_000_000_000 / period_fs;
            HPET_BEHIND.store(behind, Ordering::Relaxed);
            log!("hpet-stall: the HPET reads {behind} ticks behind from here");
        }
    }
    let ahead = TSC_AHEAD.load(Ordering::Relaxed);
    let forward = |s: Sample| Sample { before: s.before + ahead, after: s.after + ahead, ..s };
    let behind = HPET_BEHIND.load(Ordering::Relaxed);
    let hpet = Sample { reference: hpet.reference - behind, ..forward(hpet) };
    (hpet, pm.map(|(from, to)| (from, forward(to))))
}
