//! Each CPU's [`toyos_sched::windows::Windows`], fed where this CPU's
//! interrupts and preempt count change, and printed beside the IRQ census as
//! `windows: cpuN irqs_off_ns=… preempt_off_ns=…` (`mask-windows` builds only).
//!
//! Each architecture calls [`irqs_masked`] and [`irqs_unmasking`] from every
//! instruction that changes whether it takes a maskable interrupt: its
//! masking primitives, the halt, every entry (a gate, `SYSCALL` or an
//! exception masks) and every return (`scheduler::exit_to_user`'s end for a
//! return to user mode, the entry's own hook for a return to the kernel).
//! Every entry is also held to what the hardware says it interrupted: a
//! maskable interrupt is delivered only with interrupts open, and an
//! exception's frame carries the flag. A context switch changes nothing: this
//! build switches inside an `IrqGuard`, so every saved context holds
//! interrupts masked. An NMI is not a window: nothing masks it, and it runs no
//! hook. [`preempt_raised`] and [`preempt_lowering`] follow the preempt count's
//! accessors and the entries that move it inline.
//!
//! The hooks that change `IF` run with interrupts masked; a preempt hook may
//! run with them open, since an interrupt cannot move the count across zero
//! between it and the count it follows or precedes. A transition the record
//! refuses panics, after the CPU stops being tracked so the panic's own
//! masking finds nothing to check. A CPU is tracked from [`start_here`], where
//! it joins the scheduler, and a window is reported by the report after it
//! closes.

use core::sync::atomic::Ordering::Relaxed;

use toyos_sched::windows::{Unseen, Windows};

use crate::arch::{cpu, percpu};
use crate::sched::MAX_CPUS;

static CPUS: [Windows; MAX_CPUS] = [const { Windows::new() }; MAX_CPUS];

/// This CPU's record, once there is a per-CPU block to say which CPU this is.
fn on(transition: impl FnOnce(&Windows) -> Result<(), Unseen>) {
    if !crate::log::PERCPU_READY.load(Relaxed) {
        return;
    }
    if let Err(unseen) = transition(&CPUS[percpu::cpu_id() as usize]) {
        refuse(unseen);
    }
}

#[cold]
#[inline(never)]
fn refuse(unseen: Unseen) -> ! {
    let cpu = percpu::cpu_id();
    CPUS[cpu as usize].stop();
    panic!("mask-windows: cpu{cpu} {unseen}");
}

/// This CPU joins the scheduler, and is tracked from here.
pub fn start_here() {
    // Masked from here whatever it stood with; the guard's drop opens them
    // again, through its hook, if it found them open.
    let _masked = crate::arch::IrqGuard::close();
    CPUS[percpu::cpu_id() as usize].start(crate::preempt::count(), cpu::counter());
}

/// Interrupts were open and this CPU has just masked them.
pub fn irqs_masked() {
    on(|w| w.masked(cpu::counter));
}

/// An exception's frame says it interrupted this CPU with interrupts masked.
pub fn irqs_found_masked() {
    on(Windows::found_masked);
}

/// Interrupts are masked and this CPU is about to open them.
pub fn irqs_unmasking() {
    on(|w| w.unmasking(cpu::counter));
}

/// The preempt count has just been raised by one.
pub fn preempt_raised() {
    on(|w| w.raised(crate::preempt::count(), cpu::counter));
}

/// The preempt count is about to be lowered by one.
pub fn preempt_lowering() {
    on(|w| w.lowering(crate::preempt::count(), cpu::counter));
}

/// The preempt count was just set from `old` to `new` whole.
pub fn preempt_set(old: u32, new: u32) {
    on(|w| w.set(old, new, cpu::counter));
}

/// A scheduler pass has decided what runs here.
pub fn scheduled() {
    on(|w| w.scheduled(cpu::counter));
}

/// This CPU halts inside a pass.
pub fn halting() {
    on(|w| w.halting(cpu::counter));
}

/// The halt has ended, still inside its pass.
pub fn woken() {
    on(|w| w.woken(cpu::counter));
}

/// `cpu`'s line, taking its longest windows so the next report starts from none.
pub fn log_cpu(cpu: u32) {
    let Some(of) = CPUS.get(cpu as usize) else { return };
    let (irqs, preempt) = of.take();
    crate::log!(
        "windows: cpu{cpu} irqs_off_ns={} preempt_off_ns={}",
        crate::clock::nanos_of_ticks(irqs),
        crate::clock::nanos_of_ticks(preempt),
    );
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
