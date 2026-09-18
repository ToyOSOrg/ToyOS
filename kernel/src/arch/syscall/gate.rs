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

}

/// The hold `nmi_gate::hold_one` asks for, spun inside the window.
///
/// Every instruction here runs at CPL 0 on the user's stack with every
/// register the thread's, so each takes an immediate and per-CPU memory and
/// nothing else: a push is the SMAP fault the window is about, and a register
/// is state the thread gets back. `ASKED` is the storm's to set and to clear,
/// `HELD` the entry's acknowledgement, and the spin ends on `ASKED` clearing
/// and on nothing else: the entry has no register to count in, so the asker's
/// bound — `nmi_gate::hold_one`'s two budgets — is the only one it has.
///
/// The two labels bound the instructions an NMI can interrupt with both bits
/// set, for [`hold_spin`]; `src/build.rs` refuses a shipping kernel that names
/// either, so no hold is compiled into an entry without saying so.
#[cfg(feature = "boot-actuators")]
macro_rules! window_hold {
    () => {
        concat!(
            "test qword ptr gs:[{nmi_hold}], {asked}\n",
            "jz syscall_entry_hold_end\n",
            "lock or qword ptr gs:[{nmi_hold}], {held}\n",
            ".globl syscall_entry_hold_spin\n",
            "syscall_entry_hold_spin:\n",
            "pause\n",
            "test qword ptr gs:[{nmi_hold}], {asked}\n",
            "jnz syscall_entry_hold_spin\n",
            ".globl syscall_entry_hold_end\n",
            "syscall_entry_hold_end:\n",
        )
    };
}

/// Where [`window_hold`] spins, as addresses: an NMI taken under an acknowledged hold has its `rip` in this range.
#[cfg(feature = "boot-actuators")]
pub(crate) fn hold_spin() -> core::ops::Range<u64> {
    extern "C" {
        static syscall_entry_hold_spin: u8;
        static syscall_entry_hold_end: u8;
    }
    let (start, end): (u64, u64);
    // A `rip`-relative `lea` and not the statics' addresses, for `arch::smp::asm_label_addr`'s reason.
    // SAFETY: `lea` with `nomem` reads and writes no memory.
    unsafe {
        core::arch::asm!(
            "lea {start}, [rip + {spin}]",
            "lea {end}, [rip + {stop}]",
            start = out(reg) start,
            end = out(reg) end,
            spin = sym syscall_entry_hold_spin,
            stop = sym syscall_entry_hold_end,
            options(nostack, nomem),
        );
    }
    start..end
}

// GS permanently points to kernel per-CPU data here; no swapgs.
// `SYSCALL` switches no stack: before the `rsp` switch below runs, the CPU is at CPL 0 on the user's stack, so nothing that can fault may execute there.
#[unsafe(naked)]
extern "sysv64" fn syscall_entry() {
    ring3_naked_asm!(
        "mov gs:[{user_rsp}], rsp",
        #[cfg(feature = "boot-actuators")]
        window_hold!(),
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
        #[cfg(feature = "boot-actuators")]
        nmi_hold = const percpu::OFF_NMI_HOLD,
        #[cfg(feature = "boot-actuators")]
        asked = const crate::nmi_gate::hold::ASKED,
        #[cfg(feature = "boot-actuators")]
        held = const crate::nmi_gate::hold::HELD,
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
