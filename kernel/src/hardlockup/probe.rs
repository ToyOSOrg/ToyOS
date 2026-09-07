//! Wedge one CPU the way the T14 did, and sample it the way a machine with no
//! counter would have to: the negative control on [`super`].
//!
//! **Staged from inside `deadline`'s wedge and not from the idle loop.** The
//! detector's bound is tens of seconds and a boot's job list is over in one, so
//! a control that let this machine reach its shutdown would be a boot that ended
//! for the ordinary reason. `wedge-before-reset` is what holds it open — the
//! actuator table implies it behind this one — and that wedge takes every CPU
//! out of the scheduler, the idle loop included, so this is staged where those
//! CPUs land rather than where they used to be.
//!
//! **The rest of the staging is precise on purpose.** A CPU spinning on a ticket
//! lock is already bounded by `sync::Lock::lock`'s own 500M-spin panic — about
//! half a minute at a Tiger Lake `pause` — so a control that began spinning at
//! the top of the bound would be a race between two mechanisms rather than a
//! test of one. The staged CPU goes deaf first and spends all but
//! [`REACH_THE_LOCK_NS`] of the bound there, so the sample that finds it stuck
//! finds it inside the spin, with the lock's address and the site that asked for
//! it in the record.
//!
//! Its own file, and not a module inside [`super`]: everything there is reached
//! from an NMI and may write no log record, which `src/sourcegate.rs`'s
//! `nmi_does_not_log` holds it to. This runs in ordinary context and says what
//! it staged, which is what makes the sealed record readable as a control's.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::arch::{apic, cpu, percpu, smp};

/// What the staged CPU says before it stops answering, and the witness a sealed
/// record carries in its tail: a `WEDGED` page whose text does not hold this
/// line is a machine ended for some other reason, which is what makes the
/// control a control. Judged by the harness, so it is a constant
/// (`src/bootlog.rs`).
pub const PROBE_STAGED: &str = "hard-lockup: staged, and only the lockup detector ends this cpu";

/// How much of the bound is left when the staged CPU stops waiting and starts
/// spinning on a lock it will never get. Longer than one sample period, so the
/// sample that fires always lands inside the spin; far short of the 500M spins
/// that would panic it there instead.
const REACH_THE_LOCK_NS: u64 = 3_000_000_000;

/// How often the sender re-sends, on a machine whose counter cannot.
const SEND_EVERY_NS: u64 = 100_000_000;

const IDLE: u32 = 0;
const HELD: u32 = 1;
const DEAF: u32 = 2;

static STAGE: AtomicU32 = AtomicU32::new(IDLE);
static SAID: AtomicBool = AtomicBool::new(false);

/// The lock the staged CPU never gets. Its own, so nothing else on this machine
/// waits behind the control.
static PROBE_LOCK: crate::sync::Lock<()> = crate::sync::Lock::new(());

/// Called from `deadline`'s staged wedge as each CPU arrives there, and from
/// nowhere else. Two CPUs have a part; the rest go back and spin out the wedge.
pub fn stage() {
    let cpus = (smp::cpu_count() as usize).min(crate::sched::MAX_CPUS);
    let bound_ms = super::bound_ms();
    // Refused by name: two CPUs, one to hold the lock and one to go deaf
    // wanting it, and a bound for the deaf one to outlast. A control that
    // staged half of itself would be a boot that ended for some other reason.
    if cpus < 2 || bound_ms == 0 {
        if !SAID.swap(true, Ordering::AcqRel) {
            log!(
                "hard-lockup-probe: {cpus} cpu(s) and a bound of {bound_ms} ms — this control \
                 needs two cpus and a bound, so it stages nothing"
            );
        }
        return;
    }
    match percpu::cpu_id() as usize {
        me if me == cpus - 1 => go_deaf(me, bound_ms),
        0 => hold_and_sample(cpus - 1),
        _ => {}
    }
}

/// Take the lock, never give it back, and — on a machine whose CPUID states no
/// counter — send the deaf CPU the NMI its counter would have.
///
/// One CPU for both, because the wedge this is staged inside has left no CPU
/// with anything else to do, and because the holder is the one CPU that is
/// certain to still be running when the deaf one wants the lock.
fn hold_and_sample(victim: usize) -> ! {
    let _held = PROBE_LOCK.lock();
    log!(
        "hard-lockup-probe: cpu{} holds the lock and does not give it back",
        percpu::cpu_id(),
    );
    STAGE.store(HELD, Ordering::Release);
    while STAGE.load(Ordering::Acquire) < DEAF {
        core::hint::spin_loop();
    }
    // What separates a run the counter proves from one only this sender
    // reaches, said once so a reader of either knows which they have.
    if super::has_a_counter(victim) {
        log!(
            "hard-lockup-probe: cpu{victim} has a performance counter of its own, so its own NMI \
             is the sample and nothing is sent to it"
        );
        loop {
            core::hint::spin_loop();
        }
    }
    log!(
        "hard-lockup-probe: CPUID states no counter on cpu{victim}, so cpu{} sends it the NMI the \
         counter would have, every {} ms",
        percpu::cpu_id(),
        SEND_EVERY_NS / 1_000_000,
    );
    loop {
        apic::send_nmi(victim as u32);
        let until = crate::clock::tsc_deadline(SEND_EVERY_NS);
        while cpu::rdtsc() < until {
            core::hint::spin_loop();
        }
    }
}

/// Stop taking interrupts, wait out all but the last of the bound, then spin on
/// the lock the CPU above is sitting on.
fn go_deaf(me: usize, bound_ms: u64) -> ! {
    while STAGE.load(Ordering::Acquire) < HELD {
        core::hint::spin_loop();
    }
    let deaf_ns = bound_ms.saturating_mul(1_000_000).saturating_sub(REACH_THE_LOCK_NS);
    log!(
        "{PROBE_STAGED}: cpu{me} stops taking interrupts now and reaches the lock {} ms from here",
        deaf_ns / 1_000_000,
    );
    // Read before `IF` goes, because the record above has to be out first.
    let until = crate::clock::tsc_deadline(deaf_ns);
    STAGE.store(DEAF, Ordering::Release);
    // Not an `IrqGuard`: nothing here re-enables them, which is the point.
    cpu::disable_interrupts();
    while cpu::rdtsc() < until {
        core::hint::spin_loop();
    }
    // Never acquires. The detector's NMI is what ends this CPU, and its `rip`
    // is inside `Lock::lock`'s spin when it does.
    let _never = PROBE_LOCK.lock();
    loop {
        core::hint::spin_loop();
    }
}
