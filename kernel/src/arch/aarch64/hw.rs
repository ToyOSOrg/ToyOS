//! `KernelHw` — the kernel's side of the scheduler-core hardware boundary:
//! the generic timer, SGIs, `WFI` and the context switch. Nothing here makes
//! a scheduling decision.

use toyos_sched::cpu::{RunToken, SleepToken};
use toyos_sched::fair::QUANTUM_NS;
use toyos_sched::hw::{CpuId, Hw, Kicker, Machine, Nanos, TraceEvent};
use toyos_sched::task::{TaskAccounting, TaskKey};

use super::switch::{context_switch, RETURN_AT};
use super::{cpu, irqchip, percpu};
use crate::sched::payload::{KernelCtx, KernelPayload};

/// The one instance; zero-sized, holds no per-CPU state.
pub static HW: KernelHw = KernelHw;

pub struct KernelHw;

/// The scheduler clock, in raw nanoseconds.
pub fn now_ns() -> u64 {
    HW.now().0
}

impl Kicker for KernelHw {
    fn kick(&self, target: CpuId) {
        irqchip::kick_cpu(target.0);
    }
}

impl Machine for KernelHw {
    fn now(&self) -> Nanos {
        Nanos(crate::clock::nanos_since_boot())
    }

    /// The absolute deadline as the one-shot's span from now; one already past
    /// becomes the floor rather than an interrupt at once.
    fn set_timer(&self, deadline: Nanos) {
        irqchip::arm_one_shot(deadline.0.saturating_sub(self.now().0));
    }

    fn stop_timer(&self) {
        irqchip::stop_timer();
    }

    /// `WFI` with interrupts masked, then unmasked: a pending interrupt wakes
    /// `WFI` whatever `DAIF` says (Arm ARM K.a, D1.6.2), so a wake that lands
    /// between the decision and the wait is taken right after it, not slept through.
    fn halt(&self) {
        // SAFETY: waits for an interrupt and unmasks `I` and `F`; touches no memory.
        unsafe { core::arch::asm!("wfi", "msr daifclr, #3", "isb", options(nomem, nostack)) };
    }

    /// An SGI is how a remote CPU's `need_resched` gets set — there is no way to write it directly.
    fn need_resched(&self, cpu: CpuId) {
        if cpu.0 == percpu::cpu_id() {
            crate::preempt::set_need_resched();
        } else {
            self.kick(cpu);
        }
    }

    fn trace(&self, ev: TraceEvent) {
        crate::trace::record(ev);
    }

    /// Diagnostic builds arm a periodic wake before halting so a quiescent CPU
    /// still reports; and a boot under a deadline re-arms after the wake, as
    /// x86-64's does, because the poll rests on some CPU taking a timer interrupt.
    fn idle_wait(&self, token: SleepToken) {
        let _consumed = token;
        #[cfg(feature = "boot-actuators")]
        if crate::actuator::diag_tick() {
            irqchip::arm_within(DIAG_TICK_NS);
        }
        self.halt();
        if crate::deadline::armed() {
            irqchip::arm_within(QUANTUM_NS);
        }
    }
}

/// Longest sleep on a `diag-tick` build; kept under `heartbeat`'s reporting period so a healthy CPU reports on every line.
#[cfg(feature = "boot-actuators")]
const DIAG_TICK_NS: u64 = 100_000_000;

/// Which context each CPU last switched onto; read by [`report_contexts`] on crash.
static RUNNING_CTX: [core::sync::atomic::AtomicU64; crate::sched::MAX_CPUS] =
    [const { core::sync::atomic::AtomicU64::new(0) }; crate::sched::MAX_CPUS];

/// Prints which CPU is standing on which context and stack, on every kernel crash.
///
/// Allocates, locks or formats nothing but integers, since a crash may already
/// hold any lock this could try to take.
pub fn report_contexts(sp: u64, subject: Option<u64>) {
    let me = percpu::cpu_id() as usize;
    let count = (super::smp::cpu_count() as usize).min(crate::sched::MAX_CPUS);
    let mine = RUNNING_CTX.get(me).map_or(0, |slot| slot.load(core::sync::atomic::Ordering::Relaxed));
    let subject = subject.unwrap_or(mine);
    crate::log!("  Contexts: cpu{me} crashed at sp={sp:#018x}, asking about ctx {subject:#x}");
    for (cpu, slot) in RUNNING_CTX.iter().enumerate().take(count) {
        let held = slot.load(core::sync::atomic::Ordering::Relaxed);
        if !crate::mm::is_kernel_addr(held) || !held.is_multiple_of(8) {
            crate::log!("  cpu{cpu} is on ctx {held:#x} (never switched, or not a context)");
            continue;
        }
        // SAFETY: a pointer this kernel's own `Hw::switch` stored, into the boxed, always-mapped direct map.
        let ctx = unsafe { &*(held as *const KernelCtx) };
        let top = ctx.kernel_stack_top;
        match ctx.id {
            None => crate::log!("  cpu{cpu} is on ctx {held:#x} (its idle context) saved_sp={:#018x}", ctx.sp),
            Some(id) => crate::log!(
                "  cpu{cpu} is on ctx {held:#x} pid={} tid={} stack_top={top:#018x} saved_sp={:#018x}{}",
                id.0.raw(),
                id.1.raw(),
                ctx.sp,
                if cpu != me && sp <= top && sp > top.wrapping_sub(crate::process::KERNEL_STACK_SIZE as u64) {
                    "  <== AND THIS CRASH IS ON THAT STACK"
                } else {
                    ""
                },
            ),
        }
    }
    crate::mm::report_on_crash();
}

/// Panics before the switch's `ret` would land somewhere that makes the failure unnameable.
#[cold]
#[inline(never)]
fn switch_frame_is_wrong(ctx: &KernelCtx, sp: u64) -> ! {
    report_contexts(sp, Some(ctx as *const KernelCtx as u64));
    panic!(
        "context_switch: the frame about to be restored is not one — its sp {sp:#018x} is not a \
         16-byte-aligned kernel address, or its return slot is not kernel text (stack top {:#018x})",
        ctx.kernel_stack_top,
    );
}

/// The incoming context's saved stack pointer, checked; the only load of it.
#[inline]
#[must_use]
fn check_switch_frame(ctx: &KernelCtx) -> u64 {
    let sp = ctx.sp;
    if !crate::mm::is_kernel_addr(sp) || !sp.is_multiple_of(16) {
        switch_frame_is_wrong(ctx, sp);
    }
    #[cfg(feature = "stack-witness")]
    {
        let top = match ctx.id {
            Some(_) => ctx.kernel_stack_top,
            None => percpu::idle_stack_top(),
        };
        if sp > top || sp <= top - crate::process::KERNEL_STACK_SIZE as u64 {
            switch_frame_is_wrong(ctx, sp);
        }
    }
    // SAFETY: `sp` is aligned inside the incoming stack, so its frame's return slot is mapped.
    let ret = unsafe { core::ptr::read_volatile((sp + RETURN_AT as u64) as *const u64) };
    if !crate::mm::is_kernel_addr(ret) {
        switch_frame_is_wrong(ctx, sp);
    }
    sp
}

impl Hw for KernelHw {
    type Payload = KernelPayload;

    /// Outgoing per-CPU state is captured, and the incoming root and thread
    /// pointer installed, before the stack pointer moves — after that this
    /// frame no longer exists.
    unsafe fn switch(&self, token: RunToken<KernelPayload>) {
        let save = token.save_ptr();
        let restore = token.restore_ptr();
        // SAFETY: `save`/`restore` are live Box-backed contexts from
        // `SchedPass::finish`, freed only by a later pass.
        unsafe {
            (*save).thread_pointer = cpu::thread_pointer();
            (*save).preempt = crate::preempt::count();
            let incoming: &KernelCtx = &*restore;
            let sp = check_switch_frame(incoming);
            crate::preempt::set_count(incoming.preempt);
            percpu::set_current_tid(incoming.id.map(|id| id.1));
            percpu::set_current_pid(incoming.id.map(|id| id.0));
            match incoming.id {
                Some(_) => {
                    #[cfg(feature = "boot-actuators")]
                    crate::heartbeat::note_dispatch();
                    percpu::set_kernel_stack(incoming.kernel_stack_top);
                    incoming.root.activate();
                    cpu::write_thread_pointer(incoming.thread_pointer);
                }
                None => {
                    percpu::set_kernel_stack(percpu::idle_stack_top());
                    incoming.root.activate();
                }
            }
            RUNNING_CTX[percpu::cpu_id() as usize].store(restore as u64, core::sync::atomic::Ordering::Relaxed);
            context_switch(&raw mut (*save).sp, sp);
        }
    }

    /// Reached once per task, from a later pass running on another stack, so
    /// dropping `payload` here never frees the stack this call stands on.
    fn release(&self, _key: TaskKey, payload: KernelPayload, acct: TaskAccounting) {
        payload.handle.finalize(acct);
    }
}

/// AMD's `SYSRET` erratum has no AArch64 counterpart; the probe is x86-64's.
#[cfg(feature = "boot-actuators")]
pub fn sysret_ss_probe(_parkable: &crate::scheduler::Parkable) {
    crate::log!("sysret-ss: AArch64 has no SYSRET, so there is no SS to probe");
}
