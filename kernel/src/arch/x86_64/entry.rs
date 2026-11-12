//! What every gate a Ring 3 thread can arrive through opens with, and how a
//! task first runs.
//!
//! No gate saves the user machine state: the kernel touches none of it, so it
//! stays in the registers across every entry, and `arch::fpu` moves it only
//! where a switch takes a thread off its CPU. A thread's trampoline loads the
//! declared state before anything that can switch, so every save of its area
//! follows a restore on that CPU (`arch::fpu`'s invariant).
//!
//! **GS.** While Ring 3 runs, `IA32_GS_BASE` is its own and
//! `IA32_KERNEL_GS_BASE` this CPU's `PerCpu`; while the kernel runs they are
//! swapped. `SYSCALL` comes only from Ring 3 and swaps unconditionally; every
//! other gate swaps when its frame's `CS` is Ring 3's. That decision is sound
//! because the kernel's CPL-0 spans with a user's base live — before
//! `SYSCALL`'s `swapgs`, and from an exit's `swapgs` to its `SYSRET` or
//! `IRETQ` — run with `IF` clear and no instruction that can fault. What can
//! still land there takes the kernel's base from `GDTR` whatever GS holds
//! ([`install_kernel_gs`]): NMI, `#MC`, `#DF`, and `#DB`, which a `MOV SS`
//! holds over the next instruction into the first one of whatever entry that
//! instruction reaches — for a data breakpoint on Intel (SDM Vol. 3A §7.8.3),
//! for single-step too on AMD (APM Vol. 2 §8.1.4). `#DB` relies on neither.

/// `cld`, which every Ring 0 entry runs before it calls anything: no gate
/// clears `DF`, and this kernel's forward-only `memmove`/`memcpy` write
/// backward if a tick interrupts a copy with `DF` set.
#[cfg(not(feature = "entry-df-unclean"))]
macro_rules! cld {
    () => {
        "cld"
    };
}

/// Negative control (`entry-df-unclean`): the `cld` above removed;
/// `arch::syscall::init` matches it by taking `DF` back out of `IA32_FMASK`.
#[cfg(feature = "entry-df-unclean")]
macro_rules! cld {
    () => {
        ""
    };
}

/// The [`GATE_PREFIX`] bytes every gate the IDT names opens with: where the
/// frame's `CS`, `{gate_cs}` bytes above `rsp`, is Ring 3's, a call of
/// `{clear_branch_history}` (`arch::mitigation::clear_branch_history`). A CPU
/// whose decision names the clear has its IDT enter each gate here; every
/// other CPU's enters past it and runs none of it (`arch::idt`). A `jz`
/// mispredicted past the clear is followed by no load through GS and no
/// indirect branch before the gate's own `lfence` or serializing `wrmsr`.
macro_rules! gate_prefix {
    () => {
        "test byte ptr [rsp + {gate_cs}], 3\njz 78f\ncall {clear_branch_history}\n78:"
    };
}

/// [`gate_prefix`]'s length: `test` with an 8-bit displacement and
/// immediate, a short `jz` and a `call` with a 32-bit displacement. `arch::idt`
/// reads it back off each gate before it points a table past it.
pub(crate) const GATE_PREFIX: usize = 12;

/// `naked_asm!` for an interrupt or trap gate a Ring 3 thread can arrive
/// through, `cs = N;` bytes below its frame's `CS` on entry: prepends `cld`,
/// `clac`, and a `swapgs` when that `CS` is Ring 3's. The body must end with a
/// trailing comma, and swap back before every `iretq` that can return to
/// Ring 3: [`gate_iretq`] where the frame decides, a bare `swapgs` on a path
/// only a Ring 3 frame takes. `gate cs = N;` makes it a gate the IDT names,
/// opening with [`gate_prefix`]; `cs = N;` one a stub jumps to.
///
/// Such a gate clears neither `DF` nor `AC` (Intel SDM Vol. 3A §6.12.1.3 names
/// `TF`, `VM`, `RF`, `NT`, and `IF` for an interrupt gate), and Ring 3 sets
/// `AC` with a `popf`, under which SMAP does not refuse a Ring 0 access to a
/// user page (§4.6.1). A gate that is not this macro owes all three itself.
///
/// The `lfence` after the join is Intel's for a conditional `swapgs`: no load
/// through GS issues until the branch has resolved, so neither a skipped nor
/// an extra `swapgs` is followed speculatively (Intel, "Speculative Behavior
/// of SWAPGS and Segment Registers", ID 758371). On AMD it holds where
/// `LFENCE` is dispatch-serializing (AMD 70586 G-2).
macro_rules! ring3_naked_asm {
    (gate cs = $cs:expr; $($body:tt)*) => {
        core::arch::naked_asm!(
            $crate::arch::entry::gate_prefix!(),
            $crate::arch::entry::cld!(),
            "clac",
            "test byte ptr [rsp + {gate_cs}], 3",
            "jz 70f",
            "swapgs",
            "70:",
            "lfence",
            $($body)*
            gate_cs = const $cs,
            clear_branch_history = sym $crate::arch::mitigation::clear_branch_history,
        )
    };
    (cs = $cs:expr; $($body:tt)*) => {
        core::arch::naked_asm!(
            $crate::arch::entry::cld!(),
            "clac",
            "test byte ptr [rsp + {gate_cs}], 3",
            "jz 70f",
            "swapgs",
            "70:",
            "lfence",
            $($body)*
            gate_cs = const $cs,
        )
    };
}

/// The `iretq` of a [`ring3_naked_asm`] gate, with `rsp` at the CPU's frame:
/// the user's GS base back first when the frame returns to Ring 3. Nothing
/// after the `swapgs` loads through GS, and `IRETQ` serializes, so this branch
/// needs no fence.
macro_rules! gate_iretq {
    () => {
        "test byte ptr [rsp + 8], 3\njz 71f\nswapgs\n71:\niretq"
    };
}

/// `naked_asm!` for `SYSCALL`'s entries: prepends `swapgs`, since only Ring 3
/// executes `SYSCALL`, then `cld`. `IA32_FMASK` clears `AC`
/// (`arch::syscall::init`), so the entry's next instructions are the switch
/// off the user's stack.
macro_rules! syscall_naked_asm {
    ($($body:tt)*) => {
        core::arch::naked_asm!(
            "swapgs",
            $crate::arch::entry::cld!(),
            $($body)*
        )
    };
}

/// Puts this CPU's `PerCpu` in `IA32_GS_BASE` whatever GS held, for a gate
/// that can arrive in the kernel's CPL-0 stretches with a user's base live.
/// `GDTR`'s base is the block's own `gdt` (`percpu::load_gdt`), so it names
/// the CPU without reading GS. Clobbers `rax`, `rcx` and `rdx`; the operands
/// are `gs_base_msr` and `gdt_in_percpu`. No branch precedes the `wrmsr`, so
/// no load through GS after it is speculated with another base.
macro_rules! install_kernel_gs {
    () => {
        "mov ecx, {gs_base_msr}\n\
         sub rsp, 16\n\
         sgdt [rsp + 6]\n\
         mov rax, [rsp + 8]\n\
         add rsp, 16\n\
         sub rax, {gdt_in_percpu}\n\
         mov rdx, rax\n\
         shr rdx, 32\n\
         wrmsr"
    };
}

pub(crate) use {cld, gate_iretq, gate_prefix, install_kernel_gs, ring3_naked_asm, syscall_naked_asm};

/// A thread's first entry to Ring 3, at `r12` on the stack `r13`, `$align`
/// applied to it, with `rdi` = `r14`: a process's argument is zero, a thread's
/// is its own. Every other general register but `rsp` reaches the first
/// instruction zero, so nothing of the kernel's layout is left in one.
/// `trampoline_entry` and `load_initial` are sysv64, so `r12`–`r14` survive
/// both calls; `rsp` is the stack's top here, aligned for them. `IF` is clear
/// from the switch's frame through `exit_to_user`, so the `swapgs` before the
/// `iretq` is in no gate's way.
macro_rules! first_entry {
    ($($align:literal)?) => {
        core::arch::naked_asm!(
            "call {load_initial}",
            "call {unlock}",
            $($align,)?
            "mov rdi, r14",
            "push {user_ss}",
            "push r13",         // RSP
            "push 0x202",       // RFLAGS: IF=1
            "push {user_cs}",
            "push r12",         // RIP
            "xor eax, eax", "xor ebx, ebx", "xor ecx, ecx", "xor edx, edx",
            "xor esi, esi", "xor ebp, ebp",
            "xor r8d, r8d", "xor r9d, r9d", "xor r10d, r10d", "xor r11d, r11d",
            "xor r12d, r12d", "xor r13d, r13d", "xor r14d, r14d", "xor r15d, r15d",
            "swapgs",
            "iretq",
            load_initial = sym crate::arch::fpu::load_initial,
            unlock = sym crate::sched::driver::trampoline_entry,
            user_ss = const crate::arch::percpu::USER_DS,
            user_cs = const crate::arch::percpu::USER_CS,
        )
    };
}

/// A process's first entry: `_start` with the stack as given.
#[unsafe(naked)]
pub(crate) extern "C" fn process_start() {
    first_entry!()
}

/// A thread's first entry: a function, so `rsp` is `16n + 8` there, as after a
/// `call`.
#[unsafe(naked)]
pub(crate) extern "C" fn thread_start() {
    first_entry!("sub r13, 8")
}


/// Lay out, just below `top`, the frame `context_switch` restores a new
/// context from, and answer the stack pointer that names it: `trampoline` is
/// where its `ret` lands, with the entry, the stack and the argument where the
/// trampolines read them (`r12`, `r13`, `r14`).
/// # Safety
/// `top` is the end of a fresh kernel stack nothing else references, at least
/// 64 bytes deep.
pub unsafe fn initial_frame(
    top: u64,
    trampoline: unsafe extern "C" fn(),
    user_entry: u64,
    user_sp: u64,
    arg: u64,
) -> u64 {
    // Layout must match context_switch's pop sequence: pushfq, rbp..r15, return address.
    let frame = (top - 8 * 8) as *mut u64;
    // SAFETY: the eight writes cover `[frame, frame + 64)`, the top 64 bytes of
    // the stack the caller owns.
    unsafe {
        *frame.add(0) = 0; // r15
        *frame.add(1) = arg; // r14
        *frame.add(2) = user_sp; // r13
        *frame.add(3) = user_entry; // r12
        *frame.add(4) = 0; // rbx
        *frame.add(5) = 0; // rbp
        *frame.add(6) = 0x002; // RFLAGS (IF=0, AC=0)
        *frame.add(7) = trampoline as usize as u64; // return address
    }
    frame as u64
}
