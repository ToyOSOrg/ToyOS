//! AArch64's half of `crate::hw::KernelHw`: the halt and the context switch.

use kernel::sched::cpu::RunToken;
use kernel::sched::hw::Hw;
use kernel::sched::task::{TaskAccounting, TaskKey};

use super::switch::{context_switch, RETURN_AT};
use super::{cpu, percpu};
use crate::hw::{report_contexts, KernelHw};
use crate::sched::payload::{KernelCtx, KernelPayload};

/// `WFI` with interrupts masked, then unmasked: a pending interrupt wakes
/// `WFI` whatever `DAIF` says (Arm ARM K.a, D1.6.2), so a wake that lands
/// between the decision and the wait is taken right after it, not slept through.
pub fn halt() {
    // Before the `WFI`: what it waits for is taken the moment it unmasks, so
    // the wait is no window.
    #[cfg(feature = "mask-windows")]
    if !cpu::interrupts_enabled() {
        crate::windows::irqs_unmasking();
    }
    // SAFETY: waits for an interrupt and unmasks `I` and `F`; touches no memory.
    unsafe { core::arch::asm!("wfi", "msr daifclr, #3", "isb", options(nomem, nostack)) };
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
        // Every context is saved masked, so `context_switch`'s `msr daif`
        // never unmasks; the resumed context's own guard puts back what it had.
        #[cfg(feature = "mask-windows")]
        let _masked = crate::arch::IrqGuard::close();
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
                    percpu::set_kernel_stack(incoming.kernel_stack_top);
                    incoming.root.activate();
                    cpu::write_thread_pointer(incoming.thread_pointer);
                }
                None => {
                    percpu::set_kernel_stack(percpu::idle_stack_top());
                    incoming.root.activate();
                }
            }
            crate::hw::note_running(restore);
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
