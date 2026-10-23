//! A user thread's timing protection: Intel's DOITM, `IA32_UARCH_MISC_CTL[0]`
//! (`control_regs::doit`), which only Ring 0 can write. The bit is the running
//! thread's: [`request`] sets it on this CPU for the thread that asks, a
//! switch away from a user thread saves what the CPU holds into its
//! [`Thread`], and a switch onto one writes the register only when the CPU
//! holds the other value. So the state follows the thread across every switch
//! and migration, and a machine where no thread asks never writes it after
//! `control_regs::init`. A switch onto the idle loop or a kernel thread
//! leaves the register as it is.

use core::sync::atomic::{AtomicBool, Ordering::Relaxed};

use super::control_regs::{self, doit};
use super::cpu;

/// A user thread's DOITM, saved while it is not running. Off at its start.
#[derive(Clone, Copy)]
pub struct Thread {
    doitm: bool,
}

impl Thread {
    pub const START: Self = Self { doitm: false };
}

/// The DOITM bit this CPU holds, which `control_regs::init` leaves off.
pub struct Local(AtomicBool);

impl Local {
    pub const fn new() -> Self {
        Self(AtomicBool::new(false))
    }
}

fn local() -> &'static AtomicBool {
    &crate::cpu_local::here().arch.timing.0
}

fn write(on: bool) {
    // SAFETY: the register exists wherever a thread's state can differ from
    // the CPU's, since `request` sets none on a machine without it, and bits
    // 63:1 are reserved zero (`control_regs`'s declaration).
    unsafe { cpu::wrmsr(doit::UARCH_MISC_CTL, if on { doit::DOITM } else { 0 }) };
}

/// The outgoing user thread's state, read off this CPU. Called by the switch
/// under its preemption hold.
pub(crate) fn save(thread: &mut Thread) {
    thread.doitm = local().load(Relaxed);
}

/// Put the incoming user thread's state on this CPU. Called by the switch
/// under its preemption hold.
pub(crate) fn restore(thread: &Thread) {
    let held = local();
    if held.load(Relaxed) != thread.doitm {
        write(thread.doitm);
        held.store(thread.doitm, Relaxed);
    }
}

/// The calling thread's DOITM before this call, read from the register, and
/// with `set` that value put there: `None` on a machine without it. Runs in
/// the thread's syscall, whose raised preemption count keeps it on this CPU.
pub fn request(set: Option<bool>) -> Option<bool> {
    if !control_regs::doitm() {
        return None;
    }
    let held = local();
    let believed = held.load(Relaxed);
    let live = cpu::rdmsr(doit::UARCH_MISC_CTL) & doit::DOITM != 0;
    assert!(
        live == believed,
        "timing: cpu{} holds DOITM {live} and its switches believe {believed}",
        super::percpu::cpu_id(),
    );
    if let Some(on) = set.filter(|&on| on != live) {
        write(on);
        held.store(on, Relaxed);
    }
    Some(live)
}
