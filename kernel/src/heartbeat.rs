//! One line every quarter second saying the machine is still running, and
//! which CPUs are still alive.
//!
//! A live idle desktop and a dead machine have the same last thing to say, so
//! an ordinary log records a freeze's existence and never its time. A frozen
//! boot's log ends at the last heartbeat before it died, which is the **time**
//! of death rather than only the fact of it.
//!
//! # What each field is evidence of
//!
//! A set bit in `alive=`/`mask=` says: that CPU took an interrupt, returned
//! from `hlt`, and reached the top of a scheduler pass. The next tick is armed
//! by the timer stub in assembly before any Rust runs, so one fire arms the
//! next and the chain breaks only where a CPU stops taking interrupts — which
//! is the thing being measured, not a step in measuring it.
//!
//! **A clear bit means that CPU stopped only because `diag-tick` is under it**:
//! `diag-tick` caps how long a CPU may sleep, so every CPU reaches a pass two
//! or three times per line whether or not the machine has work. Without it a
//! CPU that found no work halts, does not run the idle loop, and reads clear
//! for having nothing to do. The shape of the end then discriminates a local
//! cause spreading (the mask thins CPU by CPU) from a global one (full to
//! nothing between two lines).
//!
//! `ran=` counts **tasks switched onto a CPU** since the previous line,
//! machine-wide, and separates two signatures `alive=` alone cannot:
//!
//! - the line stops → nothing is scheduling; the machine stopped.
//! - the line continues with `ran=0` → the machine is scheduling and running
//!   nothing. That is a lost wakeup, or a userland that has stopped asking.
//!
//! **The signal is a rate, so the instrument is a counter and not a sampled
//! queue depth**: a woken task is dispatched within microseconds, so `ready=`
//! sampled four times a second reads 0 on a healthy machine and 0 on a dead
//! one. What `ran=0` does not mean on its own is that nothing *could* run — a
//! machine with genuinely nothing to do also runs nothing; cross-check it
//! against the i8042 counter line, which says whether input was arriving
//! meanwhile.
//!
//! The hole none of it closes: if every CPU's LAPIC timer stopped at once — a
//! C-state that parks the APIC timer, an SMI storm — the log would go silent
//! and read as death. The kernel only ever executes `hlt`, never `MWAIT`, and
//! programs no C-state MSR, so the timers should keep counting; but the
//! laptop's firmware is not ours. So a log that stops means *the machine
//! stopped taking timer interrupts*, which is weaker than *the machine died*
//! and is written down as the weaker claim. `gap=` tells the two apart after
//! the fact: a machine that went quiet and came back says so on the line that
//! resumes.
//!
//! # Where it runs
//!
//! [`poll`] is called from `sched::driver::idle_loop`. **Nothing here waits on
//! a lock**: a heartbeat that could block would be a diagnostic that stops for
//! the reason it exists to report. The one lock in reach is the I/O APIC
//! topology, behind `i8042::report_line`'s `try_lock`, which prints `rte=busy`
//! rather than waiting.
//!
//! # The line beside it
//!
//! `alive=` and `ran=` cannot reach a desktop that is alive, has nothing to do
//! and never will again because the one channel that could give it something
//! has stopped. So [`crate::drivers::i8042::report_line`] prints the state of
//! the pin beside every heartbeat: the controller's status byte and both
//! redirection entries, raw. `alive=8/8 ran=0` above `status=0x15` is a
//! controller holding a byte nobody will ever read; above a masked entry it is
//! a line that got switched off; above a clean one with the counters flat it is
//! a machine whose EC has stopped talking, and this kernel is out of it.
//!
//! # What is deliberately not here
//!
//! A heartbeat that summons the dump by itself when a CPU has been missing from
//! the mask for several periods, though `dump::request` is reachable from here
//! as written, at preempt count 0 holding nothing. It cannot be gated:
//! `dump-deaf-cpu` stages a 400 ms window and calls `request()` itself, so it
//! can neither reach a multi-period threshold nor let a test attribute the
//! resulting dump.
//!
//! # Cost
//!
//! Four lines a second, about 60 bytes each, each of which `/bin/logd` writes
//! and `fsync`s — a device cache flush of whatever `/log` sits on. Against its
//! 1 MiB rotation that is a new part roughly every 70 minutes and sixteen kept.
//! **That is a diagnostic budget and not a shipping one**, which is why the
//! shipping kernel does not carry this module at all — and with `diag-tick`
//! under it, the instrument is no longer a passive observer of the boot it is
//! watching.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::arch::percpu;
use crate::sched::MAX_CPUS;

/// How often a line is emitted. Short enough that the time of death is
/// localised to a quarter second, long enough that the log is readable and the
/// flush path is not the load.
const PERIOD_NS: u64 = 250_000_000;

/// When the last line was emitted, and the reference point every CPU's stamp is
/// compared against. 0 until the first [`poll`], which only starts it.
static LAST_AT: AtomicU64 = AtomicU64::new(0);

/// Per-CPU: when this CPU last reached a scheduler pass. Never reset — the
/// comparison is against [`LAST_AT`], so a CPU that stops updating simply stops
/// appearing in the mask, which is the signal. 0 means it has never reached
/// one, which is a different report from having stopped reaching them.
static TICKED: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

/// Per-CPU: how many times a task has been switched onto this CPU. Monotonic
/// and never reset; the line prints the machine-wide delta.
static DISPATCHED: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

/// The dispatch total at the previous line, so `ran=` is a delta.
static LAST_DISPATCHED: AtomicU64 = AtomicU64::new(0);

/// This CPU reached a scheduler pass. Called from `drain_irqs`, which is the
/// top of every pass on every CPU.
///
/// One relaxed store of a clock read the caller's own path is about to take
/// anyway. It is deliberately *not* in the timer ISR: what the freeze violates
/// is reaching a pass, and an interrupt taken by a CPU that then fails to
/// schedule would report that CPU as healthy.
pub fn note_pass() {
    if !crate::actuator::heartbeat() {
        return;
    }
    let cpu = percpu::cpu_id() as usize;
    if cpu < MAX_CPUS {
        TICKED[cpu].store(crate::clock::nanos_since_boot().max(1), Ordering::Relaxed);
    }
}

/// A task — not the idle context — is being switched onto this CPU. Called from
/// `KernelHw::switch`.
///
/// Load and store rather than `fetch_add`: only this CPU writes this slot, so
/// the locked RMW would buy nothing on the one path in the kernel that runs at
/// context-switch rate.
pub fn note_dispatch() {
    if !crate::actuator::heartbeat() {
        return;
    }
    let cpu = percpu::cpu_id() as usize;
    if cpu < MAX_CPUS {
        let n = DISPATCHED[cpu].load(Ordering::Relaxed);
        DISPATCHED[cpu].store(n + 1, Ordering::Relaxed);
    }
}

/// Emit a heartbeat if one is due. Called from the idle loop.
pub fn poll() {
    if !crate::actuator::heartbeat() {
        return;
    }
    let now = crate::clock::nanos_since_boot();
    let last = LAST_AT.load(Ordering::Relaxed);
    if last != 0 && now.saturating_sub(last) < PERIOD_NS {
        return;
    }
    // One CPU per period. A CAS rather than a store, because on eight CPUs
    // several reach this in the same microsecond and eight identical lines a
    // period would bury the field they are printed for.
    if LAST_AT
        .compare_exchange(last, now, Ordering::AcqRel, Ordering::Relaxed)
        .is_err()
    {
        return;
    }
    let cpus = (crate::arch::smp::cpu_count() as usize).min(MAX_CPUS);
    let dispatched: u64 = (0..cpus).map(|c| DISPATCHED[c].load(Ordering::Relaxed)).sum();
    // Saturating because a diagnostic in the idle loop may not be the thing
    // that panics; the counters are monotonic, so this can only ever be a
    // subtraction that already worked.
    let ran = dispatched.saturating_sub(LAST_DISPATCHED.swap(dispatched, Ordering::Relaxed));
    // The first call starts both clocks and says nothing. Its window would open
    // at boot, so it would report every CPU that had not yet reached its first
    // pass as silent — true, useless, and the first line a reader sees.
    if last == 0 {
        return;
    }

    // Sampled once and kept, because the summary and the lines below it are one
    // reading: a second read of a CPU's stamp can put a mask that names it
    // silent above a line saying it had just run.
    let mut stamps = [0u64; MAX_CPUS];
    let mut mask = 0u64;
    let mut alive = 0u32;
    for cpu in 0..cpus {
        stamps[cpu] = TICKED[cpu].load(Ordering::Relaxed);
        // `last` is past zero by here, so this excludes a CPU that has never
        // reached a pass without a second test for it.
        if stamps[cpu] >= last {
            mask |= 1 << cpu;
            alive += 1;
        }
    }
    let (gs, gms) = split(now - last);
    let (ts, tms) = split(now);
    log!(
        "heartbeat: t={ts}.{tms:03}s alive={alive}/{cpus} mask={mask:#04x} ran={ran} \
         gap={gs}.{gms:03}s"
    );

    // Beside the heartbeat rather than on a cadence of its own, so a reader can
    // pair the two by eye. `alive=8/8 ran=0` says the machine is running and
    // running nothing; whether that is a machine with nothing to do or a machine
    // whose input died is what the next line answers, and on the laptop there is no
    // third channel to ask it on.
    crate::drivers::i8042::report_line();

    // A CPU that is missing gets a line naming it, because the summary says
    // only how many. Bounded by the CPU count, and silent on a healthy machine.
    for (cpu, &stamp) in stamps.iter().enumerate().take(cpus) {
        if mask & (1 << cpu) != 0 {
            continue;
        }
        match stamp {
            0 => log!("heartbeat: cpu{cpu} has never reached a scheduler pass"),
            stamp => {
                let (s, ms) = split(now.saturating_sub(stamp));
                log!("heartbeat: cpu{cpu} last reached one {s}.{ms:03}s ago");
            }
        }
    }
}

/// Whole seconds and milliseconds, the units the log's own timestamps carry, so
/// a reader comparing a duration against them has no conversion to do.
fn split(nanos: u64) -> (u64, u64) {
    (nanos / 1_000_000_000, (nanos % 1_000_000_000) / 1_000_000)
}
