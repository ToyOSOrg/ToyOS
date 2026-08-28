use toyos_sched::hw::{CpuId, Machine, TraceEvent, TraceKind};

use crate::arch::entry::{restore_user_state, ring3_naked_asm, save_user_state};
use crate::hw::HW;

// Ring 0 re-arms the timer itself; without it, a fire in Ring 0 disables preemption for good, since the one-shot never refires on its own.
// Ring 3 exit runs `kernel_exit_to_user_check` like every other return to userland: skipping it here let a thread killed in Ring 3 be redispatched to userland with the kill unread.
#[unsafe(naked)]
pub(super) extern "sysv64" fn timer_entry() {
    ring3_naked_asm!(
        // No error code for interrupts. CS is at [rsp + 8].
        "test dword ptr [rsp + 8], 3",
        "jz 2f",

        "push 0", // dummy error code for stack layout consistency
        "push r15", "push r14", "push r13", "push r12",
        "push r11", "push r10", "push r9",  "push r8",
        "push rbp", "push rdi", "push rsi", "push rdx",
        "push rcx", "push rbx", "push rax",

        save_user_state!(),

        // Re-arm before Rust runs so the timer survives even if the handler path panics before scheduler::do_preempt → arm_one_shot.
        "mov ecx, 0x838",
        "mov eax, dword ptr gs:[{armed_ticks}]",
        "xor edx, edx",
        "wrmsr",

        "call {handler}",

        // IF is 0 here (interrupt gate, no sti), the epilogue's entry contract; user state stays parked on this kernel stack across it, as `device_irq` also parks it.
        "call {exit_to_user}",

        restore_user_state!(),

        "pop rax",  "pop rbx",  "pop rcx",  "pop rdx",
        "pop rsi",  "pop rdi",  "pop rbp",
        "pop r8",   "pop r9",   "pop r10",  "pop r11",
        "pop r12",  "pop r13",  "pop r14",  "pop r15",
        "add rsp, 8", // pop dummy error code
        "iretq",

        "2:",
        // No Rust half on this branch, so these two `add`s inline what `timer_handler` does for Ring 3; flags are dead after the `test` above, so none are saved.
        "add qword ptr gs:[{irq_total}], 1",
        "add qword ptr gs:[{irq_timer}], 1",
        "push rax",
        "push rcx",
        "push rdx",
        "mov ecx, 0x80B",       // X2APIC_EOI
        "xor eax, eax",
        "xor edx, edx",
        "wrmsr",
        "mov ecx, 0x838",       // X2APIC_TIMER_INIT — re-arm with last value;
        "mov eax, dword ptr gs:[{armed_ticks}]",  // 0 = disabled.
        "xor edx, edx",
        "wrmsr",
        "mov byte ptr gs:[{need_resched}], 1",
        // No lock on the fire count: single writer, IF=0.
        "inc dword ptr gs:[{ring0_fires}]",
        "pop rdx",
        "pop rcx",
        "pop rax",
        "iretq",
        handler = sym timer_handler,
        exit_to_user = sym crate::arch::idt::kernel_exit_to_user_check,
        armed_ticks = const crate::arch::percpu::OFF_LAST_ARMED_TICKS,
        need_resched = const crate::arch::percpu::OFF_NEED_RESCHED,
        ring0_fires = const crate::arch::percpu::OFF_RING0_TIMER_FIRES,
        irq_total = const crate::irq_census::slot_offset(crate::irq_census::TOTAL),
        irq_timer = const crate::irq_census::slot_offset(
            1 + crate::irq_census::Source::Timer as usize
        ),
    );
}

extern "sysv64" fn timer_handler() {
    crate::irq_census::irq_took!(Timer);
    // Only the Ring 3 tick reaches here, so the interrupted context is user code and holds no `Lock`; the assert below checks that gate.
    assert_eq!(
        crate::preempt::count(),
        0,
        "the timer handler ran in kernel context, where a lock may be held",
    );

    // Routed through `Machine` rather than the ring directly: this handler is the driver entry the boundary cutover builds on.
    HW.trace(TraceEvent {
        ts: HW.now(),
        cpu: CpuId(crate::arch::percpu::cpu_id()),
        kind: TraceKind::TimerFire,
    });
    crate::arch::apic::eoi();

    crate::scheduler::do_preempt();
}
