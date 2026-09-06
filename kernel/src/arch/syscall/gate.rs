//! Where Ring 3 enters, and what the CPU is told to do when it does.
//!
//! `STAR` names the selectors, `LSTAR` is the one address `syscall` can reach, and `FMASK` masks the `RFLAGS` bits a Ring 3 thread may not hand the kernel; [`super::dispatch`] is the first code that interprets the syscall number.

use crate::arch::cpu;
use crate::arch::entry::{restore_user_state, ring3_naked_asm, save_user_state, Ring3Entry};
use crate::arch::percpu;

use super::dispatch::syscall_dispatch;

// `IA32_EFER.SCE` is `arch::control_regs`'s bit, decided in one place, not read back here.
const MSR_STAR: u32 = 0xC000_0081;
const MSR_LSTAR: u32 = 0xC000_0082;
const MSR_FMASK: u32 = 0xC000_0084;

/// `EFER.SCE` is applied and asserted on this CPU before `init` is called, on both the BSP's path and an AP's.
pub fn init() {
    let star = ((percpu::STAR_SYSRET_BASE as u64) << 48) | ((percpu::KERNEL_CS as u64) << 32);
    // SAFETY: this function owns all three `SYSCALL` MSRs.
    // All three `wrmsr` writes stay in one `unsafe` block because they are one declaration: a CPU holding only some of them has its `SYSCALL` gate aimed by something this file did not decide, and no point between the writes is a state the machine may be left in.
    unsafe {
        cpu::wrmsr(MSR_STAR, star);
        cpu::wrmsr(MSR_LSTAR, Ring3Entry::new(syscall_entry).addr());
        // `SYSCALL` clears exactly the bits named here; every bit left out carries a Ring 3 thread's flag into Ring 0.
        // The pre-mask `RFLAGS` survives in `r11` and `sysretq` restores it to the thread regardless — `FMASK` only decides what the kernel itself runs with.
        const TF: u64 = 1 << 8;
        const IF: u64 = 1 << 9;
        const DF: u64 = 1 << 10;
        const AC: u64 = 1 << 18;
        // TF must stay masked: an unmasked single-step trap taken between entry and the stack switch takes `#DB` on the user stack, which SMAP refuses and which escalates to a double fault.
        // `debug_trap`'s `tf-syscall` arm is the check that catches `TF` being left unmasked.
        // `IF` stays masked so interrupts are off for the whole syscall, and `RFLAGS.AC` clear is what makes SMAP bind at all.
        // `DF` is cleared to match `arch::entry::ring3_naked_asm`'s `cld`.
        // `entry-df-unclean` takes out only `DF`, never another bit — a control that removed two bits would be measuring two things.
        let df = if cfg!(feature = "entry-df-unclean") { 0 } else { DF };
        cpu::wrmsr(MSR_FMASK, TF | IF | AC | df);
    }

    // The three MSRs as the CPU holds them, not as they were written: a gate
    // aimed anywhere but `LSTAR` here is a Ring 3 entry into the wrong address,
    // and `control_regs` reports its own registers the same way for the same
    // reason.
    crate::log!(
        "syscall: cpu{} star={:#018x} lstar={:#018x} fmask={:#x}",
        percpu::cpu_id(),
        cpu::rdmsr(MSR_STAR),
        cpu::rdmsr(MSR_LSTAR),
        cpu::rdmsr(MSR_FMASK),
    );
}

// GS permanently points to kernel per-CPU data here; no swapgs.
// `SYSCALL` switches no stack: before the `rsp` switch below runs, the CPU is at CPL 0 on the user's stack, so nothing that can fault may execute there.
#[unsafe(naked)]
extern "sysv64" fn syscall_entry() {
    ring3_naked_asm!(
        "mov gs:[{user_rsp}], rsp",
        "mov rsp, gs:[{kernel_rsp}]",
        "mov gs:[{syscall_rip}], rcx",
        "mov gs:[{syscall_num}], rdi",
        "mov gs:[{syscall_rbp}], rbp",
        "push gs:[{user_rsp}]",  // user RSP on kernel stack
        "push rcx",             // return RIP
        "push r11",             // return RFLAGS
        "push rdi",
        "push rsi",
        "push rdx",
        "push r8",
        "push r9",
        "push r10",

        save_user_state!(),

        "lock add dword ptr gs:[{preempt_count}], 1",

        "call {handler}",

        "lock sub dword ptr gs:[{preempt_count}], 1",
        // `cli` here: an interrupt after `pop rsp` would run on the user RSP as a kernel stack.
        "cli",
        // The helper called before `pop rsp`/`sysretq` (`exit_to_user`) preserves `IF=0` across its return.
        // Runs before GPR restore: the sysv64 call would otherwise clobber rcx/r11 (sysretq's RIP/RFLAGS) and the restored args.
        // The 16 bytes both park the syscall return value and keep `rsp` aligned for the `call`.
        "sub rsp, 16",
        "mov [rsp], rax",
        "call {exit_to_user}",
        "mov rax, [rsp]",
        "add rsp, 16",

        restore_user_state!(),

        "pop r10",
        "pop r9",
        "pop r8",
        "pop rdx",
        "pop rsi",
        "pop rdi",
        "pop r11",
        "pop rcx",
        "pop rsp",              // restore user RSP from kernel stack
        "sysretq",
        handler = sym syscall_handler,
        exit_to_user = sym crate::arch::idt::kernel_exit_to_user_check,
        kernel_rsp = const percpu::OFF_KERNEL_RSP,
        user_rsp = const percpu::OFF_USER_RSP,
        syscall_rip = const percpu::OFF_SYSCALL_RIP,
        syscall_num = const percpu::OFF_SYSCALL_NUM,
        syscall_rbp = const percpu::OFF_SYSCALL_RBP,
        preempt_count = const percpu::OFF_PREEMPT_COUNT,
    );
}

/// The syscall bracket: the entry's diagnostic stores stay readable only while [`percpu::in_syscall`] is true.
///
/// Not a guard type: a panic here does not unwind, so the panic handler must find the bracket still open to decide whether to kill the process.
extern "sysv64" fn syscall_handler(num: u64, a1: u64, a2: u64, _: u64, a3: u64, a4: u64) -> u64 {
    #[cfg(feature = "df-witness")]
    cpu::df_witness("syscall_handler");
    percpu::enter_syscall();
    let out = syscall_dispatch(num, a1, a2, a3, a4);
    percpu::leave_syscall();
    out
}
