//! Where Ring 3 enters, and what the CPU is told to do when it does.
//!
//! Three MSRs and one naked function. `STAR` names the selectors, `LSTAR` is
//! the one address `syscall` can reach, and `FMASK` is the four `RFLAGS` bits a
//! Ring 3 thread may not hand the kernel. Nothing here knows what a syscall
//! number means — [`super::dispatch`] is the first code that does, and the
//! handler this file calls is the only thing it knows about it.

use crate::arch::cpu;
use crate::arch::entry::{restore_user_state, ring3_naked_asm, save_user_state, Ring3Entry};
use crate::arch::percpu;

use super::dispatch::syscall_dispatch;

// MSR addresses. `IA32_EFER` is not among them: `SCE` is one bit of a register
// `arch::control_regs` declares whole, and reading it back to OR a bit in here
// was the second place deciding what one register held.
const MSR_STAR: u32 = 0xC000_0081;
const MSR_LSTAR: u32 = 0xC000_0082;
const MSR_FMASK: u32 = 0xC000_0084;

/// Point `SYSCALL` at [`syscall_entry`] on this CPU.
///
/// `EFER.SCE` — the bit that makes the instruction exist at all — is not set
/// here: it is `arch::control_regs`'s, applied and asserted on this CPU before
/// this call on both the BSP's path and an AP's.
pub fn init() {
    let star = ((percpu::STAR_SYSRET_BASE as u64) << 48) | ((percpu::KERNEL_CS as u64) << 32);
    // SAFETY: `cpu::wrmsr` asks its caller to own the MSR it names and the value
    // it writes, and this function is that owner for all three of `SYSCALL`'s.
    // `STAR` is built from `percpu`'s own selector constants; `LSTAR` is a
    // [`Ring3Entry`] — a kernel text address this module classified, and the one
    // register whose wrong value would aim Ring 3 at somewhere else in the
    // kernel; `FMASK` is the literal below. None can `#GP` for being
    // unimplemented: `control_regs::declaration` asserts
    // `CPUID.80000001H:EDX[11]` on every CPU before this runs, which is what
    // makes `SYSCALL` exist at all.
    //
    // **One block, because the three are one declaration.** A CPU holding two of
    // them is a CPU whose `SYSCALL` gate is aimed by something this file did not
    // decide, and there is no point between these writes where that is a state
    // the machine may be left in.
    unsafe {
        cpu::wrmsr(MSR_STAR, star);
        // `LSTAR` is an IDT slot by another name: the one thing `syscall` can
        // reach.
        cpu::wrmsr(MSR_LSTAR, Ring3Entry::new(syscall_entry).addr());
        // The four `RFLAGS` bits a Ring 3 thread may not hand the kernel.
        //
        // **`SYSCALL` clears exactly what this word names and nothing else**, so
        // every bit left out of it is a Ring 3 thread's flag running Ring 0 code.
        // A thread's own copy survives either way: the CPU puts the pre-mask
        // `RFLAGS` in `r11` and `sysretq` restores it, so what this decides is
        // only what the *kernel* runs with.
        //
        // - `DF` — a kernel that inherits a set direction flag runs every
        //   `rep movs`/`rep stos` backwards, writing the `n` bytes *below* a
        //   destination instead of at it. `arch::entry::ring3_naked_asm`'s `cld`
        //   carries the whole argument; this is the same fix on the one entry
        //   where the hardware lets a mask word make it.
        // - `TF` — **without it, three Ring 3 instructions halt the machine.**
        //   The single-step trap after a `popfq` that set `TF` is deferred by
        //   exactly one instruction, and if that instruction is `syscall` the
        //   `#DB` is taken at `LSTAR` with CPL already 0 and `rsp` still the
        //   *user* stack, because the entry has not reached its stack switch.
        //   The `#DB` gate has no IST, so the CPU builds its frame there — a
        //   supervisor write to a user page, which SMAP refuses — and the `#PF`
        //   lands on the same stack and escalates. Measured on this tree before
        //   the bit was added: `DOUBLE FAULT on CPU 1`, `rip=syscall_entry+0x0`,
        //   `cr2 = rsp - 8` on a `P=1 W=1 U=1` page, every CPU halted.
        //   `debug_trap`'s `tf-syscall` arm is the gate.
        // - `IF` and `AC` — interrupts stay masked for the whole of a syscall,
        //   and `RFLAGS.AC` clear is what makes SMAP bind at all.
        //
        // `entry-df-unclean` is `arch::entry`'s negative control and this is its
        // other half: it takes `DF` back out, so the arm stages the whole defect
        // rather than the gates' share of it. It takes nothing else out — a
        // control that removed two bits would be measuring two things.
        const TF: u64 = 1 << 8;
        const IF: u64 = 1 << 9;
        const DF: u64 = 1 << 10;
        const AC: u64 = 1 << 18;
        let df = if cfg!(feature = "entry-df-unclean") { 0 } else { DF };
        cpu::wrmsr(MSR_FMASK, TF | IF | AC | df);
    }
}

// Syscall entry: GS permanently points to kernel per-CPU data (no swapgs needed).
//
// The bracket spans the handler *and* the exit-to-user epilogue, because both
// can context-switch. The epilogue used to run with the user state already put
// back, so a switch there returned to Ring 3 carrying whatever the task that
// ran in between had left in the registers.
//
// **`SYSCALL` switches no stack, so the instructions before `mov rsp,
// gs:[{kernel_rsp}]` run at CPL 0 on the user's stack — and that is the whole of the window an
// exception may not land in.** It was six instructions and it is three: the
// three diagnostic stores below it — `syscall_rip`, `syscall_num`,
// `syscall_rbp` — are reads of `rcx`, `rdi` and `rbp`, which the stack switch
// does not touch, so they are the same stores done one instruction later on a
// stack the CPU may write. What is left cannot be shortened: `cld` is at offset
// 0 by `arch::entry`'s rule and every Ring 0 entry owes it, `rsp` must be parked
// before it is overwritten, and overwriting it is the fix. The exit has a
// one-instruction window of the same kind, between `pop rsp` and `sysretq`.
//
// Which is why the vectors that can arrive there have an IST (`arch::idt`'s
// table): the window is a floor, not a bug to be closed.
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
        // cli before exit_to_user and pop rsp / sysretq: an interrupt after
        // pop rsp would land on the user RSP as a kernel stack. Helper
        // preserves IF=0 across its return.
        "cli",
        // exit_to_user runs BEFORE restoring user GPRs — the sysv64 call
        // would otherwise clobber rcx/r11 (sysretq RIP/RFLAGS) and the
        // restored arg regs. The 16 bytes park the syscall return value and
        // keep rsp aligned for the call, which the bracket left it.
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

extern "sysv64" fn syscall_handler(num: u64, a1: u64, a2: u64, _: u64, a3: u64, a4: u64) -> u64 {
    #[cfg(feature = "df-witness")]
    cpu::df_witness("syscall_handler");
    syscall_dispatch(num, a1, a2, a3, a4)
}
