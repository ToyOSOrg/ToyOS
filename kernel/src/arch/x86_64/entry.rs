//! What every gate a Ring 3 thread can arrive through opens with, and how a
//! task first runs.
//!
//! No gate saves the user machine state: the kernel touches none of it, so it
//! stays in the registers across every entry, and `arch::fpu` moves it only
//! where a switch takes a thread off its CPU. A thread's trampoline loads the
//! declared state before anything that can switch, so every save of its area
//! follows a restore on that CPU (`arch::fpu`'s invariant).

/// `cld`, which every Ring 0 entry runs first: no gate clears `DF`, and this
/// kernel's forward-only `memmove`/`memcpy` write backward if a tick
/// interrupts a copy with `DF` set.
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

/// `naked_asm!` for an interrupt or trap gate a Ring 3 thread can arrive
/// through: prepends `cld` and `clac`. The body must end with a trailing comma.
///
/// Such a gate clears neither `DF` nor `AC` (Intel SDM Vol. 3A §6.12.1.3 names
/// `TF`, `VM`, `RF`, `NT`, and `IF` for an interrupt gate), and Ring 3 sets
/// `AC` with a `popf`, under which SMAP does not refuse a Ring 0 access to a
/// user page (§4.6.1). A gate that is not this macro owes both itself.
macro_rules! ring3_naked_asm {
    ($($body:tt)*) => {
        core::arch::naked_asm!(
            $crate::arch::entry::cld!(),
            "clac",
            $($body)*
        )
    };
}

/// `naked_asm!` for `SYSCALL`'s entry: prepends `cld`. `IA32_FMASK` clears
/// `AC` (`arch::syscall::init`), so the entry's first instructions are the
/// switch off the user's stack.
macro_rules! syscall_naked_asm {
    ($($body:tt)*) => {
        core::arch::naked_asm!(
            $crate::arch::entry::cld!(),
            $($body)*
        )
    };
}

pub(crate) use {cld, ring3_naked_asm, syscall_naked_asm};

/// A thread's first entry to Ring 3, at `r12` on the stack `r13`, `$align`
/// applied to it, with `rdi` = `r14`: a process's argument is zero, a thread's
/// is its own. Every other general register but `rsp` reaches the first
/// instruction zero, so nothing of the kernel's layout is left in one.
/// `trampoline_entry` and `load_initial` are sysv64, so `r12`–`r14` survive
/// both calls; `rsp` is the stack's top here, aligned for them.
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
