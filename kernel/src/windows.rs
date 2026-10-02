//! Each CPU's [`toyos_sched::windows::Windows`], fed where this CPU's
//! interrupts and preempt count change, and printed beside the IRQ census as
//! `windows: cpuN irqs_off_ns=… preempt_off_ns=…` (`mask-windows` builds only).
//!
//! Each architecture calls [`irqs_masked`] and [`irqs_unmasking`] from every
//! instruction that changes whether it takes a maskable interrupt: its
//! masking primitives, the halt, every entry (a gate, `SYSCALL` or an
//! exception masks) and every return (`scheduler::exit_to_user`'s end for a
//! return to user mode, the entry's own hook for a return to the kernel).
//! Every maskable interrupt is also held to what the hardware says it
//! interrupted: one is delivered only with interrupts open. A context switch
//! changes nothing: this build switches inside an `IrqGuard`, so every saved
//! context holds interrupts masked. An NMI is not a window: nothing masks it,
//! and it runs no hook. [`preempt_raised`] and [`preempt_lowering`] follow the
//! preempt count's accessors and the entries that move it inline.
//!
//! The hooks that change `IF` run with interrupts masked; a preempt hook may
//! run with them open, since an interrupt cannot move the count across zero
//! between it and the count it follows or precedes. A transition the record
//! refuses panics, after the CPU stops being tracked so the panic's own
//! masking finds nothing to check; an exception the kernel itself took stops
//! it too ([`stop_here`]), so the fault's report is the one that is read. A CPU
//! is tracked from [`start_here`], where it joins the scheduler, and a window
//! is reported by the report after it closes.
//!
//! Once a boot, [`hold_once`] keeps both windows open for a span it reads off
//! the counter and prints, which a later report of that CPU reads back.

use core::sync::atomic::AtomicBool;
use core::sync::atomic::Ordering::Relaxed;

use toyos_sched::windows::{Unseen, Windows, HELD_NS};

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

/// This CPU took an exception in the kernel, which ends the machine: nothing
/// more of it is tracked or checked.
pub fn stop_here() {
    on(|w| {
        w.stop();
        Ok(())
    });
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
    let (irqs, preempt) = CPUS[cpu as usize].take();
    crate::log!(
        "windows: cpu{cpu} irqs_off_ns={} preempt_off_ns={}",
        crate::clock::nanos_of_ticks(irqs),
        crate::clock::nanos_of_ticks(preempt),
    );
}

/// The boot's first `SYS_EXIT`, which its entry left with interrupts masked
/// and the preempt count raised, stays there for [`HELD_NS`] by this CPU's
/// counter and says how long that was: `windows: held cpuN ns=…`.
pub fn hold_once() {
    static HELD: AtomicBool = AtomicBool::new(false);
    if HELD.swap(true, Relaxed) {
        return;
    }
    let from = cpu::counter();
    let owed = crate::clock::counter_ticks(HELD_NS);
    let mut held = 0;
    while held < owed {
        core::hint::spin_loop();
        held = cpu::counter() - from;
    }
    crate::log!("windows: held cpu{} ns={}", percpu::cpu_id(), crate::clock::nanos_of_ticks(held));
}
