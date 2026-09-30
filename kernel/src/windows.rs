//! The longest interrupts-off and the longest preemption-off window each CPU
//! closed since the last report, printed beside the IRQ census as
//! `windows: cpuN irqs_off_ns=… preempt_off_ns=…` (`mask-windows` builds only).
//!
//! **An interrupts-off window** opens where this CPU stops taking maskable
//! interrupts and closes where it takes them again. Each architecture feeds
//! [`irqs_masked`] and [`irqs_unmasking`] from every instruction that changes
//! that: its masking primitives, the halt, every entry (a gate, `SYSCALL` or an
//! exception masks) and every return (`scheduler::exit_to_user`'s end for a
//! return to user mode, the entry's own hook for a return to the kernel). A
//! context switch changes nothing: this build switches inside an
//! `IrqGuard`, so every saved context holds interrupts masked. An NMI is not
//! a window: nothing masks it, and it runs no hook.
//!
//! **A preemption-off window** opens where this CPU's preempt count leaves
//! zero and closes where it returns there. A scheduler pass ends it and starts
//! the next, because a pass is where a waiting thread gets the CPU, and the
//! idle halt inside a pass is no window, since a wake ends it.
//! [`preempt_raised`] follows every raise by one and [`preempt_lowering`]
//! precedes every lowering by one; each acts only on a crossing of zero.
//!
//! **Every hook checks the state it finds against the transition it
//! reports**, so a transition no hook saw panics at the next hook on that CPU,
//! and every entry is held to what the CPU says it interrupted: a maskable
//! interrupt is delivered only with interrupts open, and an exception's frame
//! carries the flag. The hooks that change `IF` run with
//! interrupts masked; a preempt hook may run with them open, since an
//! interrupt cannot move the count across zero between it and the count it
//! follows or precedes. A CPU is tracked from [`start_here`], where it joins
//! the scheduler.
//!
//! A window is reported by the report after it closes, so one still open when
//! a report is made belongs to the next.

use core::sync::atomic::{AtomicU64, Ordering::Relaxed};

use crate::arch::{cpu, percpu};
use crate::sched::MAX_CPUS;

/// A CPU that has not yet joined the scheduler: every hook is a no-op on it.
const UNTRACKED: u64 = u64::MAX;

/// One CPU's windows, written by that CPU alone; the report takes the longest
/// ones from another. The counter is never zero once firmware has run, so a
/// stamp of zero is no open window.
#[repr(align(64))]
struct Cpu {
    /// [`UNTRACKED`], 0 while interrupts are open, else the counter at which they were masked.
    irqs_off: AtomicU64,
    /// 0 while the preempt count is zero, else the counter at which it left zero or a pass last ran.
    preempt_off: AtomicU64,
    irqs_longest: AtomicU64,
    preempt_longest: AtomicU64,
}

static CPUS: [Cpu; MAX_CPUS] = [const {
    Cpu {
        irqs_off: AtomicU64::new(UNTRACKED),
        preempt_off: AtomicU64::new(0),
        irqs_longest: AtomicU64::new(0),
        preempt_longest: AtomicU64::new(0),
    }
}; MAX_CPUS];

/// This CPU's windows once it is tracked; before the per-CPU block exists
/// there is no CPU to ask.
fn tracked() -> Option<&'static Cpu> {
    if !crate::log::PERCPU_READY.load(Relaxed) {
        return None;
    }
    let here = &CPUS[percpu::cpu_id() as usize];
    (here.irqs_off.load(Relaxed) != UNTRACKED).then_some(here)
}

fn record(longest: &AtomicU64, since: u64, now: u64) {
    let span = now.saturating_sub(since);
    if span > longest.load(Relaxed) {
        longest.fetch_max(span, Relaxed);
    }
}

/// A transition no hook saw. This CPU stops being tracked first, so the panic's
/// own masking finds nothing to check.
#[cold]
#[inline(never)]
fn unseen(what: &str) -> ! {
    let cpu = percpu::cpu_id();
    CPUS[cpu as usize].irqs_off.store(UNTRACKED, Relaxed);
    panic!("mask-windows: cpu{cpu} {what}");
}

/// Interrupts were open and this CPU has just masked them.
pub fn irqs_masked() {
    let Some(here) = tracked() else { return };
    if here.irqs_off.load(Relaxed) != 0 {
        unseen("masked interrupts with a window open: an unmask reached no hook");
    }
    here.irqs_off.store(cpu::counter(), Relaxed);
}

/// This CPU joins the scheduler, and its windows are tracked from here, open
/// as it stands.
pub fn start_here() {
    let here = &CPUS[percpu::cpu_id() as usize];
    // Masked from here whatever it stood with; the guard's drop opens them
    // again, through its hook, if it found them open.
    let _masked = crate::arch::IrqGuard::close();
    let now = cpu::counter();
    here.preempt_off.store(if crate::preempt::count() == 0 { 0 } else { now }, Relaxed);
    here.irqs_off.store(now, Relaxed);
}

/// An exception's frame says it interrupted this CPU with interrupts masked.
pub fn irqs_found_masked() {
    let Some(here) = tracked() else { return };
    if here.irqs_off.load(Relaxed) == 0 {
        unseen("took an exception with interrupts masked and no window open: a mask reached no hook");
    }
}

/// Interrupts are masked and this CPU is about to open them.
pub fn irqs_unmasking() {
    let Some(here) = tracked() else { return };
    let since = here.irqs_off.load(Relaxed);
    if since == 0 {
        unseen("opened interrupts with no window open: a mask reached no hook");
    }
    record(&here.irqs_longest, since, cpu::counter());
    here.irqs_off.store(0, Relaxed);
}

/// The preempt count has just been raised by one.
pub fn preempt_raised() {
    let Some(here) = tracked() else { return };
    if crate::preempt::count() != 1 {
        return;
    }
    if here.preempt_off.load(Relaxed) != 0 {
        unseen("raised the preempt count off zero with a window open: a lowering reached no hook");
    }
    here.preempt_off.store(cpu::counter(), Relaxed);
}

/// The preempt count is about to be lowered by one.
pub fn preempt_lowering() {
    let Some(here) = tracked() else { return };
    if crate::preempt::count() != 1 {
        return;
    }
    let since = here.preempt_off.load(Relaxed);
    if since == 0 {
        unseen("lowered the preempt count to zero with no window open: a raise reached no hook");
    }
    record(&here.preempt_longest, since, cpu::counter());
    here.preempt_off.store(0, Relaxed);
}

/// The preempt count was just set from `old` to `new` whole.
pub fn preempt_set(old: u32, new: u32) {
    let Some(here) = tracked() else { return };
    let since = here.preempt_off.load(Relaxed);
    match (old, new) {
        (0, 0) => {}
        (0, _) => {
            if since != 0 {
                unseen("set the preempt count off zero with a window open: a lowering reached no hook");
            }
            here.preempt_off.store(cpu::counter(), Relaxed);
        }
        (_, 0) => {
            if since == 0 {
                unseen("set the preempt count to zero with no window open: a raise reached no hook");
            }
            record(&here.preempt_longest, since, cpu::counter());
            here.preempt_off.store(0, Relaxed);
        }
        _ => {}
    }
}

/// A scheduler pass has decided what runs here: the window it ran in ends,
/// and the next starts, since the count is still raised.
pub fn scheduled() {
    let Some(here) = tracked() else { return };
    let since = here.preempt_off.load(Relaxed);
    if since == 0 {
        unseen("ran a pass with no preemption-off window open: a raise reached no hook");
    }
    let now = cpu::counter();
    record(&here.preempt_longest, since, now);
    here.preempt_off.store(now, Relaxed);
}

/// This CPU halts inside a pass, waiting for whatever runs next. The wait is
/// no window, so the one open ends here and the next opens at [`woken`]; an
/// interrupt taken meanwhile raises the count past one, which acts on nothing.
pub fn halting() {
    let Some(here) = tracked() else { return };
    let since = here.preempt_off.load(Relaxed);
    if since == 0 {
        unseen("halted inside a pass with no preemption-off window open: a raise reached no hook");
    }
    record(&here.preempt_longest, since, cpu::counter());
    here.preempt_off.store(0, Relaxed);
}

/// The halt [`halting`] began has ended, still inside its pass.
pub fn woken() {
    let Some(here) = tracked() else { return };
    if here.preempt_off.load(Relaxed) != 0 {
        unseen("woke from a halt with a preemption-off window open: a raise reached no hook");
    }
    here.preempt_off.store(cpu::counter(), Relaxed);
}

/// `cpu`'s line, taking its longest windows so the next report starts from none.
pub fn log_cpu(cpu: u32) {
    let Some(of) = CPUS.get(cpu as usize) else { return };
    let irqs = crate::clock::nanos_of_ticks(of.irqs_longest.swap(0, Relaxed));
    let preempt = crate::clock::nanos_of_ticks(of.preempt_longest.swap(0, Relaxed));
    crate::log!("windows: cpu{cpu} irqs_off_ns={irqs} preempt_off_ns={preempt}");
}

/// What `windows-staged` spins for: past any window an emulated guest closes
/// on its own, so a report carrying it carries the spin, and far inside
/// `time::DEAF_CPU`.
#[cfg(feature = "boot-actuators")]
const STAGED_NS: u64 = 250_000_000;

/// `windows-staged`: at the first process exit, spin for [`STAGED_NS`] inside
/// the exit syscall, where both windows are open, and say how long, so a later
/// report of this CPU must carry both windows at least that long.
#[cfg(feature = "boot-actuators")]
pub fn stage_once() {
    use core::sync::atomic::AtomicBool;
    static STAGED: AtomicBool = AtomicBool::new(false);
    if STAGED.swap(true, Relaxed) {
        return;
    }
    let ticks = crate::clock::counter_ticks(STAGED_NS);
    assert!(ticks > 0, "windows-staged: the clock has no period to spin by");
    let from = cpu::counter();
    while cpu::counter().saturating_sub(from) < ticks {
        core::hint::spin_loop();
    }
    crate::log!(
        "windows: staged cpu{} {}ns",
        percpu::cpu_id(),
        crate::clock::nanos_of_ticks(ticks)
    );
}
