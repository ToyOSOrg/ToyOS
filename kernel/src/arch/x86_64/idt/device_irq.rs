//! Shared device interrupt entry: saves scratch GPRs, brackets the handler
//! with the percpu preempt count, and on Ring 3 return runs the
//! deferred-preempt epilogue with the user machine state parked on this
//! kernel stack across the call.
//!
//! IF stays 0 through the entry (interrupt gate; handlers never sti), satisfying
//! `kernel_exit_to_user_check`'s IF=0-on-entry contract without an explicit cli.

/// Defines a naked device-interrupt entry point running `$handler` then the deferred-preempt epilogue.
macro_rules! device_irq_entry {
    ($(#[$meta:meta])* $vis:vis fn $name:ident => $handler:path) => {
        $(#[$meta])*
        #[unsafe(naked)]
        $vis extern "sysv64" fn $name() {
            $crate::arch::entry::ring3_naked_asm!(
                // Handler may clobber any scratch GPR; one left unsaved leaks kernel state into a user register on iretq.
                "push rax",
                "push rcx",
                "push rdx",
                "push rsi",
                "push rdi",
                "push r8",
                "push r9",
                "push r10",
                "push r11",
                "push rbp",
                "lock add dword ptr gs:[{preempt_count}], 1",
                // Ring 0 entry has unknown rsp alignment; align via the rbp save.
                "mov rbp, rsp",
                "and rsp, -16",
                #[cfg(feature = "mask-windows")]
                "call {windows_entered}",
                // No user-state save around this call: kernel code is soft-float and never touches it.
                "call {handler}",
                #[cfg(feature = "mask-windows")]
                "mov rdi, [rbp + 88]",
                #[cfg(feature = "mask-windows")]
                "call {windows_leaving}",
                "mov rsp, rbp",
                "lock sub dword ptr gs:[{preempt_count}], 1",
                "test dword ptr [rsp + 88], 3", // CS = 10 GPRs + RIP above
                "jz 1f",
                // Bracket leaves rsp aligned for the call.
                $crate::arch::entry::save_user_state!(),
                "call {exit_to_user}",
                $crate::arch::entry::restore_user_state!(),
                "1:",
                "pop rbp",
                "pop r11",
                "pop r10",
                "pop r9",
                "pop r8",
                "pop rdi",
                "pop rsi",
                "pop rdx",
                "pop rcx",
                "pop rax",
                "iretq",
                handler = sym $handler,
                exit_to_user = sym crate::arch::idt::kernel_exit_to_user_check,
                preempt_count = const $crate::arch::percpu::OFF_PREEMPT_COUNT,
                #[cfg(feature = "mask-windows")]
                windows_entered = sym super::device_irq::windows_entered,
                #[cfg(feature = "mask-windows")]
                windows_leaving = sym super::device_irq::windows_leaving,
            );
        }
    };
}

pub(crate) use device_irq_entry;

/// The gate masked interrupts, which a maskable vector finds open (SDM Vol. 3A
/// §6.8.1), and the stub raised the preempt count by one.
#[cfg(feature = "mask-windows")]
pub(super) extern "sysv64" fn windows_entered() {
    crate::windows::irqs_masked();
    crate::windows::preempt_raised();
    crate::windows::handler_entered();
}

/// The stub lowers the count next, with the frame's `cs`; its `iretq` opens
/// interrupts on a return to Ring 0, and `exit_to_user` on one to Ring 3.
#[cfg(feature = "mask-windows")]
pub(super) extern "sysv64" fn windows_leaving(cs: u64) {
    crate::windows::handler_leaving();
    crate::windows::preempt_lowering();
    if !toyos_userbound::Ring::of_cs(cs).is_user() {
        crate::windows::irqs_unmasking();
    }
}
