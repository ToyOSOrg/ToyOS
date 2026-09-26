use crate::arch::entry::{restore_user_state, ring3_naked_asm, save_user_state};

/// IDT entry point for the TLB-shootdown IPI vector.
#[unsafe(naked)]
pub(super) extern "sysv64" fn tlb_flush_entry() {
    ring3_naked_asm!(
        // Every pushed register must be popped before `iretq`: `flush` may clobber any System V scratch register.
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
        "mov rbp, rsp",
        "and rsp, -16",
        "call {flush}",
        "mov rsp, rbp",
        "mov ecx, 0x80B",
        "xor eax, eax",
        "xor edx, edx",
        "wrmsr",
        "lock sub dword ptr gs:[{preempt_count}], 1",
        // Only this branch can context-switch; user state is saved and restored around it alone.
        "test dword ptr [rsp + 88], 3",
        "jz 1f",
        "cli",
        save_user_state!(),
        "call {exit_to_user}",
        restore_user_state!(),
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
        flush = sym flush,
        exit_to_user = sym crate::arch::idt::kernel_exit_to_user_check,
        preempt_count = const crate::arch::percpu::OFF_PREEMPT_COUNT,
    );
}

fn flush() {
    crate::irq_census::irq_took!(Tlb);
    crate::arch::tlb::serve_ipi();
}
