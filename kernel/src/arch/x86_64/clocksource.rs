//! x86-64's clock: the TSC, at the rate CPUID leaf 15H states where the part
//! states one and at one measured against the HPET where it does not, and held
//! to the HPET while it runs (`kernel::clock::reference`).
//!
//! The clock counts the TSC only where every CPU states it invariant, or under
//! a hypervisor, whose TSC is its own to synthesise and whose CPUID need not
//! say so. Every AP's TSC is bounded against the BSP's as it starts
//! ([`measure_offset`]). While it runs, one CPU a second samples the HPET, the
//! ACPI PM timer and the RTC's latest edge, and every CPU a second checks its
//! `IA32_TSC_ADJUST` and that its TSC is not behind another's ([`check`]).
//!
//! **Where the TSC and the HPET disagree, the other two say which moved.** The
//! PM timer agreeing with the TSC is the HPET moving, and the span is left out.
//! The PM timer agreeing with the HPET leaves the chipset's 14.318 MHz clock
//! or the TSC; the RTC's edges, off its own 32.768 kHz crystal, choose: with
//! the TSC, the span is left out; with the chipset's counters, or saying
//! neither, the TSC stepped. The boot's first forward step is slewed away;
//! a second, one back, a check that finds a CPU's TSC written or behind, or
//! an AP's start that proves its offset demotes the TSC ([`demote`]), and
//! every process's reading of now asks the kernel, which counts by the HPET.
//! A rate that drifts past both bounds panics.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use kernel::clock::reference::{self, Agreement, Arbitration, Calibration, Edges, Failure, Kept, Offset, Reference, Response, Sample, Slew, Third, Watch};
use toyos_abi::clock::Line;

use super::cpu;
use super::pio::{self, Declared};
use toyos_userbound::{Mediated, Ports};
use crate::log;
use crate::mm::Mmio;
use crate::mm::policy::MmioPolicy;
use crate::shootdown::{Reach, Shootdown};
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

#[derive(Clone, Copy)]
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
        self.ahead_of(last.reference, last.before, cpu::counter(), mult)
    }

    /// Where the counter should read at the TSC's `now`, from its reading
    /// `reading` at the TSC's `then`.
    fn ahead_of(&self, reading: u64, then: u64, now: u64, mult: u64) -> u64 {
        let nanos = Line { counter: 0, stamp: 0, mult }.nanos(now.saturating_sub(then));
        reading + (u128::from(nanos) * 1_000_000 / u128::from(self.reference.period_fs)) as u64
    }
}

/// The ACPI PM timer, the second reference: which of the TSC and the HPET
/// moved, where the two disagree. **It and the HPET are both the chipset's
/// (the FCH's on AMD, the PCH's on Intel)**, and may share its clock, so the
/// RTC's edges judge a stall common to both.
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
    /// The RTC's edges, the third reference, on its own crystal.
    edges: Edges,
    /// A step the HPET and the PM timer read the TSC take, waiting for the
    /// RTC's edges after it.
    pending: Option<Pending>,
    /// The steps this boot slewed away, and the slew under way.
    slewed: u32,
    slew: Option<Slewing>,
    /// Spans left out: one reference moved, or the RTC said both did.
    left_out: u64,
}

/// A step the chipset's counters agree the TSC took over `from..to`.
#[derive(Clone, Copy)]
struct Pending {
    step_ns: i64,
    from: Sample,
    to: Sample,
    /// When the sampler gives up waiting for the RTC's edges after it.
    until_ns: u64,
}

/// A slew, and the line it runs on since the counter read `since`: at `ran`
/// where the reference's time runs at `kept`.
#[derive(Clone, Copy)]
struct Slewing {
    slew: Slew,
    since: u64,
    ran: u64,
    kept: u64,
}

/// How long a step waits for the RTC's edges after it before it is taken
/// unconfirmed: [`reference::EDGES_EACH_SIDE`] edges a second apart, and two
/// more for their interrupts.
const RTC_WAIT_NS: u64 = (reference::EDGES_EACH_SIDE as u64 + 2) * 1_000_000_000;

static HELD: Lock<Option<Held>> = Lock::new(None);

/// The last sample every check held, and the clock then: where a demoted
/// clock counts on from by the HPET. Published by the sampler.
static LAST_HELD: crate::seqlock::Published<3> = crate::seqlock::Published::new();

/// Where the TSC was demoted: the HPET, its reading then and the clock by it,
/// and its last reading since, which the next extends from.
struct Demoted {
    hpet: Hpet,
    at_hpet: u64,
    at_stamp: u64,
    last_tsc: u64,
    last_hpet: u64,
    mult: u64,
}

static DEMOTED: Lock<Option<Demoted>> = Lock::new(None);
/// Taken by the first [`demote`]; every later one only logs.
static IS_DEMOTED: AtomicBool = AtomicBool::new(false);
/// What no answer since the demotion reads before: every online CPU's clock
/// once it saw the page marked ([`HANDOVER`]), and every answer given.
static DEMOTED_LATEST: AtomicU64 = AtomicU64::new(0);
/// The demotion's one round: each online CPU, once the page is marked, floors
/// [`DEMOTED_LATEST`] at its own clock ([`floor_here`]).
static HANDOVER: Shootdown = Shootdown::new();
/// Every CPU a process may have read the page on has floored
/// [`DEMOTED_LATEST`]: no process is answered before.
static HANDED_OVER: AtomicBool = AtomicBool::new(false);
/// A finding after the demotion has been logged; later ones are not.
static FOUND_SINCE: AtomicBool = AtomicBool::new(false);
static DEMOTED_ANSWERED: AtomicBool = AtomicBool::new(false);
/// The rate the watch keeps, for a demotion on any CPU to retune the line to.
static KEPT_MULT: AtomicU64 = AtomicU64::new(0);

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

/// The handover's answer for each of the `cpus` CPUs the firmware names.
pub fn size(cpus: usize) {
    HANDOVER.size(cpus);
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
        Err(why) => log!("clock: no ACPI PM timer to tell the TSC from the HPET ({why}); the RTC alone tells them apart"),
    }
    LAST_HELD.publish([anchor.before, anchor.reference, crate::clock::stamp_at(anchor.before)]);
    *HPET_COPY.lock() = Some(hpet);
    KEPT_MULT.store(kept.mult, Ordering::Relaxed);
    *HELD.lock() = Some(Held {
        watch: Watch::new(hpet.reference, anchor, kept),
        hpet,
        pm: pm.ok(),
        edges: Edges::new(),
        pending: None,
        slewed: 0,
        slew: None,
        left_out: 0,
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
/// `cpu_id`'s TSC can be from this one's (`reference::Offset`). Demotes the TSC
/// on an offset the trips prove, and panics on an AP that stops answering.
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
        Err(proven) => demote(format_args!(
            "cpu{cpu_id}'s TSC is {} cpu0's by at least {} cycles, which {ROUNDS} round trips prove ({offset:?})",
            if proven < 0 { "behind" } else { "ahead of" },
            proven.unsigned_abs(),
        )),
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
        if adjust != started {
            local.adjust.store(adjust, Ordering::Relaxed);
            demote(format_args!(
                "cpu{cpu_id}'s TSC was written: IA32_TSC_ADJUST reads {} and read {} before",
                adjust as i64,
                started as i64,
            ));
        }
    }
    let latest = LATEST.0.load(Ordering::Acquire);
    let mine = cpu::rdtsc_ordered();
    if mine < latest {
        demote(format_args!(
            "cpu{cpu_id}'s TSC read {mine}, {} cycles behind the latest another CPU's check read: this CPU's TSC \
             trails another's, or every CPU's stepped back",
            latest - mine,
        ));
    } else {
        LATEST.0.fetch_max(mine, Ordering::AcqRel);
    }

    let due = NEXT_SAMPLE_NS.load(Ordering::Relaxed);
    if now_ns < due || NEXT_SAMPLE_NS.compare_exchange(due, now_ns + CHECK_EVERY_NS, Ordering::Relaxed, Ordering::Relaxed).is_err() {
        return;
    }
    let Some(mut held) = HELD.try_lock() else {
        return;
    };
    if let Some(held) = held.as_mut() {
        sample(held, now_ns);
    }
}

/// Leave the TSC for the HPET for the rest of the boot, or, after that, log
/// what was found. Every process's reading of now then asks the kernel
/// ([`stamp_now`]), which counts by the HPET from the last held sample's clock,
/// and answers none before every online CPU's clock once it saw the page
/// marked ([`hand_over`]). The line goes back to the kept rate, unslewed, and
/// the kernel's own clock and deadlines stay on it.
fn demote(why: core::fmt::Arguments) {
    if IS_DEMOTED.swap(true, Ordering::SeqCst) {
        if !FOUND_SINCE.swap(true, Ordering::Relaxed) {
            log!("clock: DEMOTED TSC: {why}; later findings are not logged");
        }
        return;
    }
    // A publication in progress ends within its few stores.
    let [held_tsc, held_hpet, held_stamp] = loop {
        if let Some(words) = LAST_HELD.snapshot() {
            break words;
        }
        core::hint::spin_loop();
    };
    let hpet = HPET_COPY.lock().expect("the clock started before any check");
    let mult = KEPT_MULT.load(Ordering::Relaxed);
    let first = cpu::counter_ordered();
    let at_hpet = hpet.read(hpet.ahead_of(held_hpet, held_tsc, first, mult));
    let at_stamp = held_stamp + hpet.reference.ns_of(at_hpet.saturating_sub(held_hpet));
    *DEMOTED.lock() = Some(Demoted { hpet, at_hpet, at_stamp, last_tsc: first, last_hpet: at_hpet, mult });
    crate::clock::demote();
    core::sync::atomic::fence(Ordering::SeqCst);
    hand_over();
    let after = cpu::counter_ordered();
    let on_line = crate::clock::stamp_at(after);
    crate::clock::handed_over(after, mult);
    HANDED_OVER.store(true, Ordering::Release);
    log!(
        "clock: DEMOTED: {why}, so the TSC no longer keeps the clock: every process's reading of now asks the kernel, \
         which counts by the HPET from {at_stamp} ns, {} ns from the TSC's clock and held at every CPU's clock at \
         the handover till it catches up; the kernel's own clock and deadlines stay on the TSC, at the kept rate",
        at_stamp as i64 - on_line as i64,
    );
}

/// The handover's round, on the page marked: every online CPU floors
/// [`DEMOTED_LATEST`] at its own clock, this one first. A page reader that
/// found the page unmarked read its counter before its CPU's floor, on that
/// CPU's TSC, so no answer reads before its reading along the line in force
/// or the one it replaced, however far that TSC leads. A reading along an
/// older line is the remainder
/// `issues/a-demoted-tsc-keeps-the-kernels-clock-apart-from-processes.md`
/// records.
/// Before the APs are released no process has run on them, so this CPU
/// floors alone and none is asked.
///
/// Never logs, as `tlb::wait_for`: a CPU blocked on the console's lock with
/// interrupts closed could not answer.
fn hand_over() {
    if !crate::smp::answering() {
        return floor_here();
    }
    let generation = HANDOVER.issue(Reach::Narrow);
    serve_here();
    super::apic::kick_all_but_self();
    for cpu in 0..crate::smp::cpu_count() as usize {
        awaiting("answer to the handover from every CPU", || HANDOVER.served(cpu, generation));
    }
}

/// This CPU's answer to the handover, if it owes one: the kick handler's and
/// `tlb::poll`'s, so a `Lock` spinner answers it. Not every spinner with
/// interrupts closed does; the round ends because the demoter answers every
/// round it is asked meanwhile ([`awaiting`]).
pub fn serve_here() {
    // The hint, not the mask, with no round in flight.
    if !HANDOVER.owes(super::percpu::cpu_id() as usize) {
        return;
    }
    let _closed = super::IrqGuard::close();
    HANDOVER.serve_if_owed(super::percpu::cpu_id() as usize, |_| floor_here());
}

/// Floor every answer after the demotion at this CPU's clock now, along the
/// line in force and the one it replaced (`clock::floor_at`): after the mark,
/// so after the counter of every page reader on this CPU that found the page
/// unmarked, whichever of the two it read.
fn floor_here() {
    DEMOTED_LATEST.fetch_max(crate::clock::floor_at(cpu::counter_ordered()), Ordering::AcqRel);
}

/// Spin until `done`, answering every round this CPU owes meanwhile
/// (`tlb::poll`: the TLB's, `SMI_CMD`'s and the handover's), as every spinner
/// with interrupts closed must; and panic past `time::DEAF_CPU`: a CPU that
/// takes no kick in five seconds is wedged.
fn awaiting(what: &str, done: impl Fn() -> bool) {
    const SPINS_PER_DEADLINE_CHECK: u32 = 1024;
    if done() {
        return;
    }
    let deaf = crate::time::DEAF_CPU.nanos();
    let deadline = crate::clock::nanos_since_boot().saturating_add(deaf);
    let mut spins = 0u32;
    while !done() {
        super::tlb::poll();
        core::hint::spin_loop();
        spins += 1;
        if spins == SPINS_PER_DEADLINE_CHECK {
            spins = 0;
            assert!(crate::clock::nanos_since_boot() < deadline, "clock: no {what} in {deaf} ns since the TSC was demoted");
        }
    }
}

/// The HPET, for [`demote`] on whichever CPU's check finds the TSC moved.
static HPET_COPY: Lock<Option<Hpet>> = Lock::new(None);

/// Now as a log line's time, for a process: the TSC's clock, or, once
/// demoted, the HPET's count on from where the TSC was demoted, never before
/// the handover's floor. The counter is read before the mark, as a page reader
/// reads it.
pub fn stamp_now() -> u64 {
    let on_line = crate::clock::stamp_at(cpu::counter_ordered());
    if !IS_DEMOTED.load(Ordering::SeqCst) {
        return on_line;
    }
    awaiting("end to the handover", || HANDED_OVER.load(Ordering::Acquire));
    let stamp = {
        let mut demoted = DEMOTED.lock();
        let d = demoted.as_mut().expect("handed over before the demotion was recorded");
        let now = cpu::counter_ordered();
        let reading = d.hpet.read(d.hpet.ahead_of(d.last_hpet, d.last_tsc, now, d.mult));
        (d.last_tsc, d.last_hpet) = (now, reading);
        d.at_stamp + d.hpet.reference.ns_of(reading.saturating_sub(d.at_hpet))
    };
    let answered = DEMOTED_LATEST.fetch_max(stamp, Ordering::AcqRel).max(stamp);
    if !DEMOTED_ANSWERED.swap(true, Ordering::Relaxed) {
        log!("clock: a process's reading of now asked the kernel since the TSC was demoted, and read {answered} ns");
    }
    answered
}

/// One sample of the HPET, of the PM timer beside it and of the RTC's
/// latest edge: each span the TSC and the HPET agree over is held; where
/// they do not, the PM timer and then the RTC say which moved.
fn sample(held: &mut Held, now_ns: u64) {
    let kept = held.watch.kept();
    let last = held.watch.last();
    let hpet = held.hpet.sample(held.hpet.near(last, kept.mult), 1);
    let pm = held.pm.as_ref().map(|pm| (pm.last, pm.next(kept.mult)));
    let edge = super::rtc::last_edge();
    #[cfg(feature = "boot-actuators")]
    let (hpet, pm, edge) = staged(hpet, pm, edge);
    if let Some((seen, at)) = edge {
        held.edges.push(seen, at);
    }
    let pm_agreement = held.pm.as_ref().zip(pm).map(|(timer, (from, to))| {
        let span = reference::Span::between(timer.reference, from, to);
        let tolerance = kept.bound_ppb + timer.reference.tolerance_over(timer.reference.span_ns(from, to));
        reference::agree_over(timer.reference, span, kept.mult, tolerance)
    });
    if let (Some(timer), Some((_, to))) = (held.pm.as_mut(), pm) {
        timer.last = to;
    }
    match held.watch.take(hpet, true) {
        Ok(report) => {
            LAST_HELD.publish([hpet.before, hpet.reference, crate::clock::stamp_at(hpet.before)]);
            if let Some(report) = report {
                log_report(held, report);
                #[cfg(feature = "boot-actuators")]
                stage_first(held.hpet.reference.period_fs);
            }
        }
        Err(Failure::Step { from, to, agreement }) => {
            let spans = spans(agreement, pm_agreement, from, to, pm);
            let _ = held.watch.take(hpet, false);
            held.left_out += 1;
            match pm_agreement.map(|second| reference::arbitrate(agreement, second)) {
                Some(Arbitration::FirstMoved) => log!("clock: the HPET moved and the TSC did not, so this span is left out: {spans}"),
                Some(Arbitration::AllDisagree) => {
                    log!("clock: the TSC, the HPET and the ACPI PM timer all disagree, so this span is left out: {spans}")
                }
                Some(Arbitration::CounterMoved) | None => {
                    if let Some(earlier) = held.pending.take() {
                        settle(held, earlier);
                    }
                    log!(
                        "clock: the TSC reads {} ns from the chipset's counters over the last span; the RTC's edges \
                         after it will say which moved: {spans}",
                        agreement.apart_ns,
                    );
                    held.pending = Some(Pending { step_ns: agreement.apart_ns, from, to, until_ns: now_ns + RTC_WAIT_NS });
                }
            }
        }
        Err(Failure::Drift { since, to, agreement }) => panic!(
            "clock: the TSC and the HPET disagree by {} ns over the {} ns the HPET counted in held spans, {} allowed: \
             their rates are further apart than the kept rate's bound and the HPET's tolerance (the open span from \
             {since:?} to {to:?})",
            agreement.apart_ns,
            agreement.reference_ns,
            agreement.allowed_ns,
        ),
    }
    if let Some(pending) = held.pending.filter(|p| held.edges.complete_after(p.to.after) || now_ns >= p.until_ns) {
        held.pending = None;
        settle(held, pending);
    }
    KEPT_MULT.store(held.watch.kept().mult, Ordering::Relaxed);
    slew(held);
}

/// What the RTC says of a pending step, and what the clock does about it.
fn settle(held: &mut Held, pending: Pending) {
    let kept = held.watch.kept();
    let apart = held.edges.apart_across(pending.from.before, pending.to.after, kept.mult);
    let rtc = match apart {
        Some(apart) => alloc::format!("the RTC's edges read {apart} ns"),
        None => alloc::string::String::from("no RTC edge either side"),
    };
    let step = pending.step_ns;
    let confirmed = match reference::third_says(step, apart) {
        Third::References => {
            return log!(
                "clock: the chipset's counters moved {step} ns from the TSC and {rtc}, with the TSC, so the span \
                 was left out"
            );
        }
        Third::Counter => "confirmed by the RTC",
        Third::Unsure => "unconfirmed by the RTC",
    };
    held.edges.forget_before(pending.to.after);
    if IS_DEMOTED.load(Ordering::Acquire) {
        return log!("clock: TSC STEP: the demoted TSC stepped {step} ns against the HPET and the PM timer, {confirmed} ({rtc}); not slewed");
    }
    match reference::respond(step, held.slewed) {
        Response::Slew => {
            held.slewed += 1;
            held.slew = Some(Slewing { slew: Slew::new(step), since: cpu::counter(), ran: crate::clock::mult(), kept: kept.mult });
            log!(
                "clock: TSC STEP: the TSC stepped {step} ns forward against the HPET and the PM timer, {confirmed} \
                 ({rtc}); the clock slews it away at most {} ppm, most of it in about {} ms",
                reference::SLEW_MOST_PPB / 1_000,
                u128::from(step.unsigned_abs()) * u128::from(reference::PPB) / reference::SLEW_MOST_PPB as u128 / 1_000_000,
            );
        }
        Response::Demote => {
            demote(format_args!(
                "the TSC stepped {step} ns against the HPET and the PM timer, {confirmed} ({rtc}), after {} step(s) slewed",
                held.slewed,
            ));
        }
    }
}

/// The slew under way, paid to now and set for the next second; or, with
/// none, the kept rate where the watch retuned it.
fn slew(held: &mut Held) {
    let kept = held.watch.kept().mult;
    if IS_DEMOTED.load(Ordering::Acquire) {
        held.slew = None;
    }
    let Some(mut slewing) = held.slew else {
        if crate::clock::mult() != kept {
            crate::clock::retune(cpu::counter(), kept);
        }
        return;
    };
    let now = cpu::counter();
    slewing.slew.ran(now - slewing.since, slewing.kept, slewing.ran);
    let ran = slewing.slew.mult(kept, CHECK_EVERY_NS);
    crate::clock::retune(now, ran);
    if slewing.slew.settled() {
        held.slew = None;
        log!("clock: the TSC's step is slewed away, {} ns left", slewing.slew.debt_ns());
        #[cfg(feature = "boot-actuators")]
        stage_again();
    } else {
        held.slew = Some(Slewing { since: now, ran, kept, ..slewing });
    }
}

fn log_report(held: &Held, report: reference::Report) {
    let kept = Line { counter: 0, stamp: 0, mult: report.kept.mult };
    let measured = Line { counter: 0, stamp: 0, mult: report.measured.mult }.millihertz();
    let (span_s, bound) = (report.measured.span_ns / 1_000_000_000, report.measured.bound_ppb);
    let left_out = alloc::format!(
        "{} spans left out; {} TSC step(s) slewed{}",
        held.left_out,
        held.slewed,
        held.slew.map_or_else(alloc::string::String::new, |s| alloc::format!(", {} ns still to slew", s.slew.debt_ns())),
    );
    match (report.kept.measured, report.retuned) {
        (false, _) => log!(
            "clock: over {span_s} s the TSC counted {measured} mHz against the HPET, within {bound} ppb; \
             the clock keeps CPUID 15H's {} mHz; {left_out}",
            kept.millihertz(),
        ),
        (true, retuned) => log!(
            "clock: over {span_s} s the TSC counted {measured} mHz against the HPET, within {bound} ppb; \
             the clock {} {} mHz, within {} ppb; {left_out}",
            if retuned { "now keeps" } else { "keeps" },
            kept.millihertz(),
            report.kept.bound_ppb,
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

/// The PM timer's span, and the RTC's latest edge, as one sample reads them.
#[cfg(feature = "boot-actuators")]
type PmSpan = Option<(Sample, Sample)>;
#[cfg(feature = "boot-actuators")]
type Edge = Option<(u64, u64)>;

/// `tsc-step` and `hpet-stall`: from the first report on, every reading reads
/// either the TSC 10 ms ahead of where it is, the HPET's, the PM timer's and
/// the RTC's edges alike, or the HPET 10 ms behind, as a TSC that stepped and
/// an HPET that stood still; and the TSC 10 ms further once its first step is
/// slewed away. A hypervisor's TSC may ignore a write (QEMU's TCG does), so
/// each is staged in the readings the sampler takes, from the counter it
/// took effect at.
#[cfg(feature = "boot-actuators")]
mod stage {
    use core::sync::atomic::{AtomicU64, Ordering};

    pub static STEPS_AT: [AtomicU64; 2] = [AtomicU64::new(u64::MAX), AtomicU64::new(u64::MAX)];
    pub static STALLED_AT: AtomicU64 = AtomicU64::new(u64::MAX);
    pub static STEP_TICKS: AtomicU64 = AtomicU64::new(0);
    pub static STALL_TICKS: AtomicU64 = AtomicU64::new(0);

    /// What a counter read at `at` reads, staged.
    pub fn ahead(at: u64) -> u64 {
        let steps = STEPS_AT.iter().filter(|s| s.load(Ordering::Relaxed) <= at).count() as u64;
        at + steps * STEP_TICKS.load(Ordering::Relaxed)
    }
}

#[cfg(feature = "boot-actuators")]
fn staged(hpet: Sample, pm: PmSpan, edge: Edge) -> (Sample, PmSpan, Edge) {
    use stage::*;
    let forward = |s: Sample| Sample { before: ahead(s.before), after: ahead(s.after), ..s };
    let stalled = if hpet.before >= STALLED_AT.load(Ordering::Relaxed) { STALL_TICKS.load(Ordering::Relaxed) } else { 0 };
    let hpet = Sample { reference: hpet.reference - stalled, ..forward(hpet) };
    (hpet, pm.map(|(from, to)| (from, forward(to))), edge.map(|(seen, at)| (seen, ahead(at))))
}

/// `tsc-step`'s and `hpet-stall`'s first, at the clock's first report.
#[cfg(feature = "boot-actuators")]
fn stage_first(period_fs: u64) {
    use stage::*;
    let now = cpu::counter();
    if STEPS_AT[0].load(Ordering::Relaxed) != u64::MAX || STALLED_AT.load(Ordering::Relaxed) != u64::MAX {
        return;
    }
    if crate::actuator::tsc_step() {
        let ahead = crate::clock::counter_ticks(10_000_000);
        STEP_TICKS.store(ahead, Ordering::Relaxed);
        STEPS_AT[0].store(now, Ordering::Relaxed);
        log!("tsc-step: the TSC reads {ahead} cycles ahead from here");
    }
    if crate::actuator::hpet_stall() {
        let behind = 10_000_000_000_000 / period_fs;
        STALL_TICKS.store(behind, Ordering::Relaxed);
        STALLED_AT.store(now, Ordering::Relaxed);
        log!("hpet-stall: the HPET reads {behind} ticks behind from here");
    }
}

/// `tsc-step`'s second step, once the first is slewed away.
#[cfg(feature = "boot-actuators")]
fn stage_again() {
    if crate::actuator::tsc_step() && stage::STEPS_AT[1].load(Ordering::Relaxed) == u64::MAX {
        stage::STEPS_AT[1].store(cpu::counter(), Ordering::Relaxed);
        log!("tsc-step: the TSC reads {} cycles further ahead from here", stage::STEP_TICKS.load(Ordering::Relaxed));
    }
}
