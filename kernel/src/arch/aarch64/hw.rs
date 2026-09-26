//! `KernelHw` — the kernel's side of the scheduler-core hardware boundary:
//! the generic timer, SGIs, `WFI` and the context switch, the port's stage 4.

use toyos_sched::cpu::RunToken;
use toyos_sched::hw::{CpuId, Hw, Kicker, Machine, Nanos, TraceEvent};
use toyos_sched::task::{TaskAccounting, TaskKey};

use crate::sched::payload::KernelPayload;

/// The one instance; zero-sized, holds no per-CPU state.
pub static HW: KernelHw = KernelHw;

pub struct KernelHw;

/// The scheduler clock, in raw nanoseconds.
pub fn now_ns() -> u64 {
    HW.now().0
}

impl Kicker for KernelHw {
    fn kick(&self, _target: CpuId) {
        owed!("the interrupt controller", "stage 4")
    }
}

impl Machine for KernelHw {
    type IrqGuard = crate::arch::IrqGuard;

    fn now(&self) -> Nanos {
        Nanos(crate::clock::nanos_since_boot())
    }

    fn set_timer(&self, _deadline: Nanos) {
        owed!("the timer", "stage 4")
    }

    fn stop_timer(&self) {
        owed!("the timer", "stage 4")
    }

    fn irq_guard(&self) -> crate::arch::IrqGuard {
        crate::arch::IrqGuard::close()
    }

    fn halt(&self) {
        owed!("the interrupt controller", "stage 4")
    }

    fn need_resched(&self, _cpu: CpuId) {
        owed!("the interrupt controller", "stage 4")
    }

    fn trace(&self, ev: TraceEvent) {
        crate::trace::record(ev);
    }
}

impl Hw for KernelHw {
    type Payload = KernelPayload;

    unsafe fn switch(&self, _token: RunToken<KernelPayload>) {
        owed!("the context switch", "stage 4")
    }

    fn release(&self, _key: TaskKey, _payload: KernelPayload, _acct: TaskAccounting) {
        owed!("the context switch", "stage 4")
    }
}

/// What every kernel crash says about the machine's contexts: until stage 4
/// switches any, only the memory facts. Never owed: a crash report that
/// panicked would bury the crash.
pub fn report_contexts(sp: u64, _subject: Option<u64>) {
    crate::log!("  Contexts: the boot CPU crashed at sp={sp:#018x}; no context has been switched");
    crate::mm::report_on_crash();
}

/// AMD's `SYSRET` erratum has no AArch64 counterpart; the probe is x86-64's.
#[cfg(feature = "boot-actuators")]
pub fn sysret_ss_probe(_parkable: &crate::scheduler::Parkable) {
    crate::log!("sysret-ss: AArch64 has no SYSRET, so there is no SS to probe");
}
