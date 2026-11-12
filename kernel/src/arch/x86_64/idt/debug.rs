//! Vector 1, on IST4.
//!
//! A `#DB` from Ring 3 is a userland trap like any other fault: its frame
//! moves to the thread's kernel stack, where the CPU puts every other Ring 3
//! frame, and it goes on through `common_entry`.
//!
//! One at CPL 0 is a trap Ring 3 held back over the instruction after a `MOV
//! SS` or `POP SS`, which Intel delivers for a data breakpoint and AMD for
//! single-step too (SDM Vol. 3A §7.8.3; APM Vol. 2 §8.1.4). It lands on the
//! first instruction of the gate or `SYSCALL` entry that instruction reached:
//! before its `swapgs`, and for `SYSCALL` on the user's stack. So the entry
//! installs the kernel's GS base whatever it finds, as the NMI's does, and
//! resumes with the base it found. On its own first instruction there is no
//! entry to resume: the held trap followed an `INT1`, whose own `#DB` frame
//! this one's delivery overwrote on IST4, and the thread that ran it ends as a
//! trap. Nothing in the kernel sets `TF` or arms a debug register
//! (`control_regs`), so a CPL-0 `#DB` anywhere else ends the machine.

use core::arch::naked_asm;

use crate::arch::entry::install_kernel_gs;
use crate::arch::percpu::{MSR_GS_BASE, OFF_GDT, OFF_KERNEL_RSP, USER_CS, USER_DS};

/// Eleven pushes place the CPU frame's `rip` at 88.
const RIP_OFFSET: usize = 88;

#[unsafe(naked)]
pub(super) extern "sysv64" fn debug_entry() {
    naked_asm!(
        "cld",
        "clac",
        // Neither path addresses GS before it has made it the kernel's, so a
        // mispredicted branch here loads nothing through a user's base.
        "test byte ptr [rsp + 8], 3",
        "jnz 2f",
        "push rax",
        "push rcx",
        "push rdx",
        "mov ecx, {gs_base_msr}",
        "rdmsr",
        "shl rdx, 32",
        "or rax, rdx",
        "push rax",
        install_kernel_gs!(),
        "push rsi",
        "push rdi",
        "push r8",
        "push r9",
        "push r10",
        "push r11",
        "push rbp",
        "lea rdi, [rsp + {rip_offset}]",
        "mov rbp, rsp",
        "and rsp, -16",
        "call {deferred}",
        "mov rsp, rbp",
        "pop rbp",
        "pop r11",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rdi",
        "pop rsi",
        // The base this `#DB` found, back: nothing after it reads GS.
        "pop rax",
        "mov rdx, rax",
        "shr rdx, 32",
        "mov ecx, {gs_base_msr}",
        "wrmsr",
        "pop rdx",
        "pop rcx",
        "pop rax",
        // `deferred` makes the frame a Ring 3 one where its thread is to end.
        "test byte ptr [rsp + 8], 3",
        "jnz 2f",
        "iretq",
        // From Ring 3: the CPU's five words to the top of this thread's
        // kernel stack, the one `tss.rsp0` names too (`percpu::set_kernel_stack`),
        // read through `GDTR` rather than GS, then on as every other exception.
        "2:",
        "push rax",
        "push rcx",
        "sub rsp, 16",
        "sgdt [rsp + 6]",
        "mov rax, [rsp + 8]",
        "add rsp, 16",
        "sub rax, {gdt_in_percpu}",
        "mov rax, [rax + {kernel_rsp}]",
        "mov rcx, [rsp + 48]", "mov [rax - 8], rcx",
        "mov rcx, [rsp + 40]", "mov [rax - 16], rcx",
        "mov rcx, [rsp + 32]", "mov [rax - 24], rcx",
        "mov rcx, [rsp + 24]", "mov [rax - 32], rcx",
        "mov rcx, [rsp + 16]", "mov [rax - 40], rcx",
        "sub rax, 40",
        "pop rcx",
        "xchg rax, [rsp]",
        "pop rsp",
        "push 0",
        "push {vector}",
        "jmp {common}",
        gs_base_msr = const MSR_GS_BASE,
        gdt_in_percpu = const OFF_GDT,
        kernel_rsp = const OFF_KERNEL_RSP,
        rip_offset = const RIP_OFFSET,
        vector = const super::Vector::Debug as usize,
        deferred = sym deferred,
        common = sym super::common_entry,
    );
}

/// The CPU's frame of an interrupt or exception without an error code.
#[repr(C)]
struct Frame {
    rip: u64,
    cs: u64,
    rflags: u64,
    rsp: u64,
    ss: u64,
}

/// A `#DB` at CPL 0 on `frame`: resumed where its `rip` is an entry Ring 3
/// reaches, whose trap it is. On `debug_entry` itself the frame becomes a Ring
/// 3 one, its lost `rip` and `rsp` zero, so its thread ends as a trap.
extern "sysv64" fn deferred(frame: &mut Frame) {
    if frame.rip == debug_entry as *const () as u64 {
        *frame = Frame { rip: 0, cs: u64::from(USER_CS), rflags: 0x202, rsp: 0, ss: u64::from(USER_DS) };
        return;
    }
    if frame.rip != crate::arch::syscall::entry_address() && !super::is_gate_target(frame.rip) {
        panic!("#DB at {:#x} in Ring 0, which is no entry: nothing in this kernel arms a debug trap", frame.rip);
    }
}
