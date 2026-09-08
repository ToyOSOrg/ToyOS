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
//! a thread again and nothing else here would say so. Run 24 on the T14 is the
//! measurement — seven cores taking interrupts, one deaf inside the shutdown
//! syscall, and this is what ended it, half a bound before the machine's own.
//!
//! **What a sample compares is progress, not liveness.**
//! `crate::irq_census::taken_here` is every interrupt this CPU has taken except
//! the NMI, in both rings — the exclusion is load-bearing, since the sample
//! arrives as an NMI and has counted itself before it reads that — so a count
//! that has not moved is a CPU that has taken nothing at all. A CPU whose count
//! is stale for [`toyos_tco::hard_lockup_bound_ms`] of
//! the bound this boot named, *and* whose sampled frame has `IF` clear, is
//! stuck: it seals a `WEDGED` record naming itself, its `rip` and `rsp` from the
//! NMI frame, the lock it is spinning on if `Lock::lock` recorded one, a line
//! for every other CPU, and the tail of the log ring — then writes the reset
//! register through `acpi::reset_now`.
//!
//! # The discipline this file is written under
//!
//! Every path below runs on IST2 inside an NMI: no lock, no allocation, no
//! `log!` — the interrupted context may hold the log ring's own shard, and
//! `src/sourcegate.rs`'s `nmi_does_not_log` is the gate. The sampling path is
//! also on every CPU every second of every boot, so it is atomics and one
//! `rdtsc` and no division at all; `src/redlist.rs`'s `dump_nmi_probe` is the
//! instrument that once caught a 128-bit divide on an NMI-sampled path. The
//! report divides, and only after the machine is already being ended.
//!
//! # What it does not cover, stated rather than implied
//!
//! - **A halted CPU.** The counter is unhalted cycles, so a CPU asleep in `hlt`
//!   generates no sample. That is the right shape and not a gap: a halted CPU
//!   is not spinning on anything, and one that is woken takes the interrupt
//!   that wakes it.
//! - **The span before `clock::init`,** which has no TSC period to convert a
//!   bound with, and before `apic::init`, which has no LVT to arm. The same
//!   floor `crate::deadline` states, and the same sentinel design in
//!   `issues/hardware/the-t14-boots-toyos-unattended.md` is what would reach
//!   below it.
//! - **A CPU with `IF` set that no timer ever interrupts.** Nothing resets it,
//!   deliberately, and what keeps that from being a hole is
//!   `hw::KernelHw::idle_wait`: the `stop_timer` that pairs with the halt is
//!   undone on the way *out* of it, so a CPU executing at all has a one-shot
//!   armed and takes it — whether or not the wake that ended its halt was the
//!   timer's.

use core::fmt;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

use crate::arch::{apic, cpu, percpu, smp};
use crate::sched::MAX_CPUS;

/// The negative control, in a file of its own because it says what it staged
/// and nothing here may say anything.
#[cfg(feature = "boot-actuators")]
pub mod probe;

/// What a sealed record opens with, and so what the next boot's loader prints
/// under `Previous boot's panic:`. A constant because the harness judges the
/// line and nothing links the two crates (`src/bootlog.rs`).
pub const LOCKED_UP: &str = "a cpu locked up with interrupts off";

/// `RFLAGS.IF` in the frame the NMI pushed: whether the CPU this landed on
/// could have taken any other interrupt at that instant.
const RFLAGS_IF: u64 = 1 << 9;

/// How often an armed CPU samples itself, in nanoseconds of unhalted time.
///
/// A second: the bound is measured in tens of them, so a sample period this
/// long costs one NMI per CPU per second and puts the detection within one
/// period of the bound. It is also the period the report's ages are quoted at.
const SAMPLE_NS: u64 = 1_000_000_000;

/// The bound in TSC ticks, or 0 for a boot that armed none. Written on the BSP
/// before any AP exists.
static BOUND_TSC: AtomicU64 = AtomicU64::new(0);

/// The same bound in milliseconds, for the record to quote and the probe to
/// stage against.
static BOUND_MS: AtomicU64 = AtomicU64::new(0);

/// TSC ticks per millisecond, so the report can turn a span into a number
/// without reaching for the nanosecond clock's out-of-line divide.
static TICKS_PER_MS: AtomicU64 = AtomicU64::new(0);

/// The counter's reload period, in reference cycles — which are TSC ticks,
/// since fixed counter 2 counts the reference clock (SDM Vol. 3B §20.2.2).
static PERIOD: AtomicU64 = AtomicU64::new(0);

/// The counter's width as CPUID states it, as a mask: bits above it may not be
/// written back.
static WIDTH_MASK: AtomicU64 = AtomicU64::new(0);

/// Whether the panic path has taken this machine. A panicked kernel holds its
/// panel with `IF` clear for [`toyos_tco::PANIC_BOUND_MS`] and is not wedged —
/// somebody is reading it — so the detector stands down rather than resetting a
/// machine out from under its own report.
static STOOD_DOWN: AtomicBool = AtomicBool::new(false);

/// Per CPU, all of it written by that CPU from its own NMI and read by whichever
/// CPU seals: the progress count at its last sample, when that count last moved,
/// and where the last sample found it.
static PROGRESS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static STILL_SINCE: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static AT_TSC: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static AT_RIP: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static AT_RSP: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static AT_RFLAGS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static ARMED_PMU: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];

/// The lock a CPU is spinning on and the `#[track_caller]` site that asked for
/// it, written by `sync::Lock::lock` at the top of its spin and cleared when it
/// acquires. Zero is a CPU that is inside no contended acquisition.
static SPIN_LOCK: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static SPIN_AT: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

/// The architectural performance-monitoring MSRs this file writes, SDM Vol. 3B
/// §20.2.2 and Vol. 4 Table 2-2. Nothing else in this kernel programs the PMU,
/// which is why each control register below is declared whole and written
/// whole rather than read, modified and written back.
const IA32_FIXED_CTR2: u32 = 0x30B;
const IA32_FIXED_CTR_CTRL: u32 = 0x38D;
const IA32_PERF_GLOBAL_STATUS: u32 = 0x38E;
const IA32_PERF_GLOBAL_CTRL: u32 = 0x38F;
const IA32_PERF_GLOBAL_OVF_CTRL: u32 = 0x390;

/// Fixed counter 2's nibble of `IA32_FIXED_CTR_CTRL` is bits 11:8: enable in
/// ring 0 (bit 8) and ring 3 (bit 9), no AnyThread (bit 10), and PMI on
/// overflow (bit 11). Counting in both rings, because a CPU that stops taking
/// interrupts in either is the same defect.
const FIXED_CTR2_ARMED: u64 = 0b1011 << 8;

/// The fixed counters live in the high half of `IA32_PERF_GLOBAL_CTRL` and of
/// the status and overflow-clear registers beside it, so counter 2 is bit 34.
const GLOBAL_FIXED_CTR2: u64 = 1 << 34;

/// Turn the bound this boot named into a bound on one CPU, and arm the BSP.
///
/// Called from `deadline::start` with the deadline's own bound: one parameter
/// carries both, so a boot that shortens the one it gave itself shortens what a
/// deaf CPU gets with it and the two can never disagree.
///
/// **Says nothing itself, and hands its caller the words.** Every other path in
/// this file is reached from an NMI, where a log record would reenter the ring
/// the interrupted context may be mid-publish of, and `src/sourcegate.rs`'s
/// `nmi_does_not_log` holds the whole file to that. Its caller runs in ordinary
/// context and writes the record.
#[must_use]
pub fn start(deadline_ms: u64) -> Armed {
    let ms = toyos_tco::hard_lockup_bound_ms(deadline_ms);
    let ticks = crate::clock::tsc_ticks(ms.saturating_mul(1_000_000));
    let per_ms = crate::clock::tsc_ticks(1_000_000);
    if ms == 0 || ticks == 0 || per_ms == 0 {
        return Armed { ms: 0, sampled: false };
    }
    BOUND_MS.store(ms, Relaxed);
    BOUND_TSC.store(ticks, Relaxed);
    TICKS_PER_MS.store(per_ms, Relaxed);
    PERIOD.store(crate::clock::tsc_ticks(SAMPLE_NS), Relaxed);
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
                 after which a cpu that has taken no interrupt seals a WEDGED record and resets \
                 the machine",
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
    if BOUND_TSC.load(Relaxed) == 0 {
        return;
    }
    let me = percpu::cpu_id() as usize;
    if me >= MAX_CPUS {
        return;
    }
    // The baseline first and unconditionally: a CPU with no counter of its own
    // is still sampled by any NMI that reaches it, and a sample against an
    // unwritten baseline would read as a CPU that has been stuck since boot.
    PROGRESS[me].store(crate::irq_census::taken_here(), Relaxed);
    STILL_SINCE[me].store(cpu::rdtsc(), Relaxed);
    let Some(width) = architectural_pmu() else { return };
    WIDTH_MASK.store(mask_of(width), Relaxed);
    // Stopped, then set up, then started: a counter enabled while its control
    // register is half written can overflow into an LVT that is not armed yet.
    write_msr(IA32_PERF_GLOBAL_CTRL, 0);
    write_msr(IA32_FIXED_CTR_CTRL, FIXED_CTR2_ARMED);
    write_msr(IA32_PERF_GLOBAL_OVF_CTRL, GLOBAL_FIXED_CTR2);
    reload();
    apic::arm_perf_nmi();
    write_msr(IA32_PERF_GLOBAL_CTRL, GLOBAL_FIXED_CTR2);
    ARMED_PMU[me].store(true, Relaxed);
}

/// Stand the detector down for the rest of this machine's life.
///
/// Called from `apic::halt_all_cpus`, which is every fatal path's one funnel: a
/// panicked kernel pages its panel with `IF` clear, under a bound of its own,
/// and a reader holding the machine open is not a machine to reset. One relaxed
/// store, and the LVT then stays masked of its own accord — hardware masks it
/// on delivery and only [`sample`]'s re-arm clears it.
pub fn stand_down() {
    STOOD_DOWN.store(true, Relaxed);
}

/// The bound in milliseconds, or 0 where this boot armed none. The control's
/// question: it stages against the bound rather than a number of its own.
#[cfg(feature = "boot-actuators")]
pub fn bound_ms() -> u64 {
    BOUND_MS.load(Relaxed)
}

/// One sample of the CPU this NMI landed on: where it is, whether it has taken
/// anything since the last one, and whether that has gone on too long.
///
/// Called from `arch::idt::nmi`'s `note` and nowhere else. Returns on every NMI
/// that is not this CPU's own overflow, so the diagnostic senders — the blocked
/// task dump's probe, the syscall-window storm — cost one load and one compare.
pub fn sample(rip: u64, rsp: u64, rflags: u64) {
    if BOUND_TSC.load(Relaxed) == 0 || STOOD_DOWN.load(Relaxed) {
        return;
    }
    let me = percpu::cpu_id() as usize;
    if me >= MAX_CPUS {
        return;
    }
    let mine = ARMED_PMU[me].load(Relaxed) && overflowed();
    // A machine with no PMU has this bound only under the actuator that sends
    // the NMI the counter would have, which is how QEMU's guest reaches this
    // decision at all.
    if !mine && !crate::actuator::hard_lockup_probe() {
        return;
    }
    let now = cpu::rdtsc();
    AT_RIP[me].store(rip, Relaxed);
    AT_RSP[me].store(rsp, Relaxed);
    AT_RFLAGS[me].store(rflags, Relaxed);
    AT_TSC[me].store(now, Relaxed);

    let taken = crate::irq_census::taken_here();
    let moved = PROGRESS[me].swap(taken, Relaxed) != taken;
    // `IF` set is the whole difference between this bound and the deadline's: a
    // CPU that can still take an interrupt is one the timer entry's poll
    // reaches, and this mechanism is not about it.
    if moved || rflags & RFLAGS_IF != 0 {
        STILL_SINCE[me].store(now, Relaxed);
    } else if now.wrapping_sub(STILL_SINCE[me].load(Relaxed)) >= BOUND_TSC.load(Relaxed) {
        locked_up(me, rip, rsp, now)
    }
    // **After the decision and never before it.** Re-arming clears the mask
    // hardware set on delivery; leaving it set is what stops a second NMI
    // landing on the stack this one is still standing on while it seals, which
    // `arch::idt::nmi`'s `nested_nmi` would answer by stopping the machine
    // without resetting it.
    if mine {
        write_msr(IA32_PERF_GLOBAL_OVF_CTRL, GLOBAL_FIXED_CTR2);
        reload();
        apic::arm_perf_nmi();
    }
}

/// Seal where every CPU is and hand the machine back.
///
/// Runs on the stuck CPU itself, from its own NMI frame, which is the only
/// context that has its `rip`. No CPU is asked anything from here: an NMI sent
/// to a sibling already inside its own would enter `nested_nmi` and stop the
/// machine instead of resetting it, so the record is built out of what each CPU
/// last wrote about itself.
fn locked_up(me: usize, rip: u64, rsp: u64, now: u64) -> ! {
    if !crate::deadline::claim_the_reset() {
        // Another CPU is already sealing and resetting. This one has nothing to
        // add and must not race it into the page.
        cpu::halt();
    }
    crate::drivers::panic_console::seal_wedge(format_args!(
        "{}",
        Report { me, rip, rsp, now, cpus: (smp::cpu_count() as usize).min(MAX_CPUS) }
    ));
    // The seal first, because the USB stop `reset_now` makes before it writes
    // the register is bounded but not instant, and this record is the
    // diagnostic the whole mechanism exists for.
    crate::drivers::acpi::reset_now()
}

/// Note that this CPU is inside a contended acquisition, so a record sealed
/// while it is there can name what it is waiting for.
///
/// Called from `sync::Lock::lock`'s spin — the contended path only, so an
/// uncontended acquisition costs nothing — and by nothing else.
pub fn spinning_on(lock: u64, at: &'static core::panic::Location<'static>) {
    let me = percpu::cpu_id() as usize;
    if me >= MAX_CPUS {
        return;
    }
    SPIN_AT[me].store(at as *const _ as u64, Relaxed);
    // Last, and what the reader tests: a non-zero lock means the site beside it
    // was already stored.
    SPIN_LOCK[me].store(lock, Relaxed);
}

/// The acquisition succeeded; this CPU is waiting for nothing.
pub fn spinning_on_nothing() {
    let me = percpu::cpu_id() as usize;
    if me < MAX_CPUS {
        SPIN_LOCK[me].store(0, Relaxed);
    }
}

/// The counter's width, or `None` on a CPU with no architectural performance
/// monitoring — which is every QEMU TCG guest.
///
/// SDM Vol. 2A, CPUID leaf 0AH: EAX[7:0] is the version, and version 2 is where
/// the fixed-function counters and `IA32_PERF_GLOBAL_CTRL` appear; EDX[4:0] is
/// how many fixed counters there are and EDX[12:5] how wide they are. Fixed
/// counter 2 needs three of them.
fn architectural_pmu() -> Option<u32> {
    if cpu::cpuid(0, 0).0 < 0x0A {
        return None;
    }
    let (eax, _, _, edx) = cpu::cpuid(0x0A, 0);
    let version = eax & 0xff;
    let counters = edx & 0x1f;
    let width = (edx >> 5) & 0xff;
    if version < 2 || counters < 3 || width == 0 || width > 64 {
        return None;
    }
    Some(width)
}

const fn mask_of(width: u32) -> u64 {
    match width >= 64 {
        true => u64::MAX,
        false => (1u64 << width) - 1,
    }
}

/// Whether fixed counter 2 is the reason this NMI arrived, SDM Vol. 3B §20.2.2:
/// `IA32_PERF_GLOBAL_STATUS` bit 34 is its overflow.
fn overflowed() -> bool {
    cpu::rdmsr(IA32_PERF_GLOBAL_STATUS) & GLOBAL_FIXED_CTR2 != 0
}

/// Set the counter one period below its own overflow.
fn reload() {
    let period = PERIOD.load(Relaxed);
    let mask = WIDTH_MASK.load(Relaxed);
    // Masked to the width CPUID stated: a fixed counter refuses a write of the
    // bits above it, and the negative count is what makes the overflow land a
    // period from here.
    write_msr(IA32_FIXED_CTR2, 0u64.wrapping_sub(period) & mask);
}

fn write_msr(msr: u32, value: u64) {
    // SAFETY: every MSR here is an architectural performance-monitoring counter
    // or its control register, enumerated by CPUID leaf 0AH before this file
    // writes any of them, and each value is that register's own field encoding.
    unsafe { cpu::wrmsr(msr, value) };
}

/// Milliseconds, from a TSC span. One `div`, and only on the path that has
/// already decided to end the machine.
struct Ms(u64);

impl fmt::Display for Ms {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match TICKS_PER_MS.load(Relaxed) {
            0 => f.write_str("an unmeasured span"),
            per_ms => write!(f, "{} ms", self.0 / per_ms),
        }
    }
}

/// The lock one CPU is inside a contended acquisition of, as the record says it.
struct Waiting(usize);

impl fmt::Display for Waiting {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let lock = SPIN_LOCK[self.0].load(Relaxed);
        if lock == 0 {
            return Ok(());
        }
        write!(f, " spinning on the lock at {lock:#x}")?;
        let at = SPIN_AT[self.0].load(Relaxed);
        if at != 0 {
            // SAFETY: written by `spinning_on` from a `&'static Location`, which
            // `#[track_caller]` places in this image's read-only data and which
            // outlives every reader; the store of `lock` above is what publishes
            // that this one is set.
            let at: &'static core::panic::Location<'static> = unsafe { &*(at as *const _) };
            write!(f, ", taken at {at}")?;
        }
        Ok(())
    }
}

/// Where a `rip` is, spelled without saying a word: `symbols::resolve_kernel`
/// writes a log record, which is the one thing this path may not do.
struct At(u64);

impl fmt::Display for At {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#018x}", self.0)?;
        match crate::symbols::kernel_symbol(self.0) {
            None => Ok(()),
            Some((name, offset)) => write!(
                f,
                "  {}+{offset:#x}",
                toyos_symbols::symbol_text(rustc_demangle::demangle(name)),
            ),
        }
    }
}

/// What the machine looked like from the CPU that ended it.
struct Report {
    me: usize,
    rip: u64,
    rsp: u64,
    now: u64,
    cpus: usize,
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { me, rip, rsp, now, cpus } = *self;
        writeln!(
            f,
            "{LOCKED_UP}: cpu{me} has taken no interrupt for {}, with `IF` clear at every sample \
             in that span. Its bound is {} ms.",
            Ms(now.wrapping_sub(STILL_SINCE[me].load(Relaxed))),
            BOUND_MS.load(Relaxed),
        )?;
        writeln!(f, "  rip={}", At(rip))?;
        writeln!(f, "  rsp={rsp:#018x}{}", Waiting(me))?;
        // Every CPU, and each line is what that CPU last wrote about itself:
        // the holder of whatever the stuck one is waiting for is somewhere in
        // this list, and nothing else in a wedged machine can point at it.
        for cpu in 0..cpus {
            // The live count, not the sampled one: a CPU whose total has moved
            // since its own last sample is a CPU that is still running, which
            // is the first thing a reader of a wedged machine wants to know.
            let irqs = crate::irq_census::taken_by(cpu as u32).unwrap_or(0);
            let at = AT_TSC[cpu].load(Relaxed);
            if at == 0 {
                writeln!(
                    f,
                    "  cpu{cpu} irqs={irqs}, never sampled: halted through every period, or no \
                     counter of its own{}",
                    Waiting(cpu),
                )?;
                continue;
            }
            writeln!(
                f,
                "  cpu{cpu} irqs={irqs} (={} when sampled {} ago) if={} at {}{}",
                PROGRESS[cpu].load(Relaxed),
                Ms(now.wrapping_sub(at)),
                u8::from(AT_RFLAGS[cpu].load(Relaxed) & RFLAGS_IF != 0),
                At(AT_RIP[cpu].load(Relaxed)),
                Waiting(cpu),
            )?;
        }
        f.write_str("The tail of the log ring follows — which is what nothing was draining.\n")
    }
}


/// Whether `cpu` armed a counter of its own — what separates a boot the counter
/// samples from one only the control's sender reaches.
fn has_a_counter(cpu: usize) -> bool {
    cpu < MAX_CPUS && ARMED_PMU[cpu].load(Relaxed)
}
