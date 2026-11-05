//! The bound on a CPU that has stopped taking interrupts altogether, carried by
//! the one interrupt it cannot mask.
//!
//! `crate::deadline` is the bound on a *machine*, and its whole requirement is
//! that some CPU still takes an interrupt: it is polled from the timer entry.
//! A machine on which no CPU does is named in that module's header as what it
//! does not cover, and it is not hypothetical — the T14 sat past 420 s with a
//! 120 ms-derived bound armed and nothing polled it, which is a hand on the
//! power button. This is the other half: the local APIC's performance-counter
//! LVT in NMI delivery mode, armed off a counter that overflows about once a
//! second, on every CPU whose CPUID states a PMU (SDM Vol. 3A §12.5.1 for the
//! LVT, Vol. 3B §20.2.2 for the counters, Vol. 2A CPUID leaf 0AH for whether
//! there are any). An NMI is delivered whatever `IF` holds, so the sample
//! reaches the one CPU nothing else can.
//!
//! **Its subject is one CPU and not the machine**, which is the whole
//! difference: it ends a machine whose other cores are healthy and taking
//! interrupts, because a core that has taken none for its bound will never run
//! a thread again and nothing else here would say so.
//!
//! **What a sample compares is progress, not liveness.**
//! `crate::irq_census::taken_here` is every interrupt this CPU has taken except
//! the NMI, in both rings — the exclusion is load-bearing, since the sample
//! arrives as an NMI and has counted itself before it reads that — so a count
//! that has not moved is a CPU that has taken nothing at all. A CPU whose count
//! is stale for [`toyos_tco::hard_lockup_bound_ms`] of
//! the bound this boot named, *and* whose sampled frame has `IF` clear, is
//! stuck: it writes the reset register through `power::reset_now`.
//!
//! # The discipline this file is written under
//!
//! Every path below runs on IST2 inside an NMI: no lock, no allocation, no
//! `log!` — the interrupted context may hold the log ring's own shard, and
//! `build/sourcegate.rs`'s `nmi_does_not_log` is the gate. The sampling path is
//! also on every CPU every second of every boot, so it is atomics and one
//! `cpu::counter` read and no division at all.
//!
//! # What it does not cover, stated rather than implied
//!
//! - **A halted CPU.** The counter is unhalted cycles, so a CPU asleep in `hlt`
//!   generates no sample. That is the right shape and not a gap: a halted CPU
//!   is not spinning on anything, and one that is woken takes the interrupt
//!   that wakes it.
//! - **The span before `clock::init`,** which has no counter period to convert a
//!   bound with, and before `irqchip::init`, which has no LVT to arm. The same
//!   floor `crate::deadline` states.
//! - **A CPU with `IF` set that no timer ever interrupts.** Nothing resets it,
//!   deliberately, and what keeps that from being a hole is
//!   `hw::KernelHw::idle_wait`: the `stop_timer` that pairs with the halt is
//!   undone on the way *out* of it, so a CPU executing at all has a one-shot
//!   armed and takes it — whether or not the wake that ended its halt was the
//!   timer's.

use core::fmt;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

use crate::arch::{cpu, percpu, pmu, trap};

const SAMPLE_NS: u64 = toyos_tco::HARD_LOCKUP_SAMPLE_NS;

/// The bound in counter ticks, or 0 for a boot that armed none. Written on the BSP
/// before any AP exists.
static BOUND_TICKS: AtomicU64 = AtomicU64::new(0);

/// The performance counter's reload period, in `cpu::counter` ticks, the unit
/// `pmu::arm` takes.
static PERIOD: AtomicU64 = AtomicU64::new(0);

/// Whether the panic path has taken this machine. A panicked kernel holds its
/// panel with `IF` clear for [`toyos_tco::PANIC_BOUND_MS`] and is not wedged —
/// somebody is reading it — so the detector stands down rather than resetting a
/// machine out from under its own report.
static STOOD_DOWN: AtomicBool = AtomicBool::new(false);

/// A CPU's words, all of them written by that CPU from its own NMI.
pub struct Local {
    /// The progress count at its last sample, and when that count last moved.
    progress: AtomicU64,
    still_since: AtomicU64,
    armed_pmu: AtomicBool,
}

impl Local {
    pub fn new() -> Self {
        Self { progress: AtomicU64::new(0), still_since: AtomicU64::new(0), armed_pmu: AtomicBool::new(false) }
    }
}

fn of(cpu: usize) -> &'static Local {
    &crate::cpu_local::of(cpu as u32).hardlockup
}

/// Turn the bound this boot named into a bound on one CPU, and arm the BSP.
///
/// Called from `deadline::start` with the deadline's own bound: one parameter
/// carries both, so a boot that shortens the one it gave itself shortens what a
/// deaf CPU gets with it and the two can never disagree.
///
/// **Says nothing itself, and hands its caller the words.** Every other path in
/// this file is reached from an NMI, where a log record would reenter the ring
/// the interrupted context may be mid-publish of, and `build/sourcegate.rs`'s
/// `nmi_does_not_log` holds the whole file to that. Its caller runs in ordinary
/// context and writes the record.
#[must_use]
pub fn start(deadline_ms: u64) -> Armed {
    let ms = toyos_tco::hard_lockup_bound_ms(deadline_ms);
    let ticks = crate::clock::counter_ticks(ms.saturating_mul(1_000_000));
    if ms == 0 || ticks == 0 {
        return Armed { ms: 0, sampled: false };
    }
    BOUND_TICKS.store(ticks, Relaxed);
    PERIOD.store(crate::clock::counter_ticks(SAMPLE_NS), Relaxed);
    arm_this_cpu();
    Armed { ms, sampled: has_a_counter(percpu::cpu_id() as usize) }
}

/// What [`start`] armed, as the line a boot is read back by.
pub struct Armed {
    ms: u64,
    sampled: bool,
}

impl fmt::Display for Armed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { ms, sampled } = *self;
        // Each of the three named rather than silently unarmed: a machine whose
        // CPUID states no architectural PMU has this bound only where something
        // else sends it the NMI the counter would have, and a reader of the boot
        // has to know which of them they are looking at.
        match (ms, sampled) {
            (0, _) => f.write_str(
                "hard lockup: no bound — this boot named no deadline to derive one from",
            ),
            (ms, true) => write!(
                f,
                "hard lockup: {ms} ms, sampled every {} ms by each cpu's own performance counter, \
                 after which a cpu that has taken no interrupt resets the machine",
                SAMPLE_NS / 1_000_000,
            ),
            (ms, false) => write!(
                f,
                "hard lockup: {ms} ms, but CPUID states no architectural performance counter on \
                 this machine, so nothing samples a cpu that stops taking interrupts"
            ),
        }
    }
}

/// Take the baseline for this CPU and arm its counter, if it has one.
///
/// Every CPU, called once as it joins: the counters and the LVT are per logical
/// CPU, and a CPU nobody armed is one this bound does not cover.
pub fn arm_this_cpu() {
    if BOUND_TICKS.load(Relaxed) == 0 {
        return;
    }
    let me = percpu::cpu_id() as usize;
    // The baseline first and unconditionally: a CPU with no counter of its own
    // is still sampled by any NMI that reaches it, and a sample against an
    // unwritten baseline would read as a CPU that has been stuck since boot.
    of(me).progress.store(crate::irq_census::taken_here(), Relaxed);
    of(me).still_since.store(cpu::counter(), Relaxed);
    if !pmu::arm(PERIOD.load(Relaxed)) {
        return;
    }
    of(me).armed_pmu.store(true, Relaxed);
}

/// Stand the detector down for the rest of this machine's life.
///
/// Called from `panic::halt_all_cpus`, which is every fatal path's one funnel: a
/// panicked kernel pages its panel with `IF` clear, under a bound of its own,
/// and a reader holding the machine open is not a machine to reset. One relaxed
/// store, and the LVT then stays masked of its own accord — hardware masks it
/// on delivery and only [`sample`]'s re-arm clears it.
pub fn stand_down() {
    STOOD_DOWN.store(true, Relaxed);
}

/// One sample of the CPU this NMI landed on: whether it has taken anything
/// since the last one, and whether that has gone on too long.
///
/// Called from `arch::trap::nmi`'s `note` and nowhere else. Returns on every NMI
/// that is not this CPU's own overflow, so the diagnostic senders — the blocked
/// task dump's probe — cost one load and one compare.
pub fn sample(flags: u64) {
    if BOUND_TICKS.load(Relaxed) == 0 || STOOD_DOWN.load(Relaxed) {
        return;
    }
    let me = percpu::cpu_id() as usize;
    if !(of(me).armed_pmu.load(Relaxed) && pmu::overflowed()) {
        return;
    }
    let now = cpu::counter();

    let taken = crate::irq_census::taken_here();
    let moved = of(me).progress.swap(taken, Relaxed) != taken;
    // `IF` set is the whole difference between this bound and the deadline's: a
    // CPU that can still take an interrupt is one the timer entry's poll
    // reaches, and this mechanism is not about it.
    if moved || trap::frame_interrupts_enabled(flags) {
        of(me).still_since.store(now, Relaxed);
    } else if now.wrapping_sub(of(me).still_since.load(Relaxed)) >= BOUND_TICKS.load(Relaxed) {
        locked_up()
    }
    // **After the decision and never before it.** Re-arming clears the mask
    // hardware set on delivery; leaving it set is what stops a second NMI
    // landing on the stack this one is still standing on while it resets, which
    // `arch::trap::nmi`'s `nested_nmi` would answer by stopping the machine
    // without resetting it.
    pmu::rearm(PERIOD.load(Relaxed));
}

/// Hand the machine back, from the stuck CPU's own NMI. No CPU is asked
/// anything from here: an NMI sent to a sibling already inside its own would
/// enter `nested_nmi` and stop the machine instead of resetting it.
fn locked_up() -> ! {
    if !crate::deadline::claim_the_reset() {
        // Another CPU is already resetting.
        cpu::halt();
    }
    crate::power::reset_now()
}

/// Whether `cpu` armed a counter of its own.
fn has_a_counter(cpu: usize) -> bool {
    of(cpu).armed_pmu.load(Relaxed)
}
