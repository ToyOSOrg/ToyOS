//! What every gate a Ring 3 thread can arrive through opens with, and how a
//! task first runs.
//!
//! No gate saves the user machine state: the kernel touches none of it, so it
//! stays in the registers across every entry, and `arch::fpu` moves it only
//! where a switch takes a thread off its CPU. A thread's trampoline loads the
//! declared state before anything that can switch, so every save of its area
//! follows a restore on that CPU (`arch::fpu`'s invariant); a kernel thread's
//! loads none, having no user state.

/// `naked_asm!` for a gate a Ring 3 thread can arrive through: prepends
/// `cld`. The body must end with a trailing comma.
#[cfg(not(feature = "entry-df-unclean"))]
macro_rules! ring3_naked_asm {
    ($($body:tt)*) => {
        core::arch::naked_asm!(
            // An interrupt/trap gate does not clear `DF`, and this kernel's
            // forward-only `memmove`/`memcpy` write backward if a tick
            // interrupts a copy with `DF` set.
            "cld",
            $($body)*
        )
    };
}

/// Negative control (`entry-df-unclean`): the `cld` above removed;
/// `arch::syscall::init` matches it by taking `DF` back out of `IA32_FMASK`.
#[cfg(feature = "entry-df-unclean")]
macro_rules! ring3_naked_asm {
    ($($body:tt)*) => {
        core::arch::naked_asm!(
            $($body)*
        )
    };
}

pub(crate) use ring3_naked_asm;

/// Entry point for new processes, reached through `context_switch`'s `ret`. r12 = entry point, r13 = user stack pointer.
// `rsp` is the stack's top here, aligned for the first `call`.
#[unsafe(naked)]
pub(crate) extern "C" fn process_start() {
    core::arch::naked_asm!(
        "call {load_initial}",
        "push r12",
        "push r13",
        "call {unlock}",
        "pop r13",
        "pop r12",
        "push {user_ss}",
        "push r13",         // RSP: user stack
        "push 0x202",       // RFLAGS: IF=1
        "push {user_cs}",
        "push r12",         // RIP: entry point
        "iretq",
        load_initial = sym crate::arch::fpu::load_initial,
        unlock = sym crate::sched::driver::trampoline_entry,
        user_ss = const crate::arch::percpu::USER_DS,
        user_cs = const crate::arch::percpu::USER_CS,
    );
}

/// Entry point for new threads. r14 carries the argument, which lands in rdi.
// `rsp` is the stack's top here, aligned for the first `call`.
#[unsafe(naked)]
pub(crate) extern "C" fn thread_start() {
    core::arch::naked_asm!(
        "call {load_initial}",
        "push r12",
        "push r13",
        "push r14",
        "call {unlock}",
        "pop r14",
        "pop r13",
        "pop r12",
        "mov rdi, r14",
        "sub r13, 8",       // ABI: RSP must be 16n+8 at function entry
        "push {user_ss}",
        "push r13",
        "push 0x202",
        "push {user_cs}",
        "push r12",
        "iretq",
        load_initial = sym crate::arch::fpu::load_initial,
        unlock = sym crate::sched::driver::trampoline_entry,
        user_ss = const crate::arch::percpu::USER_DS,
        user_cs = const crate::arch::percpu::USER_CS,
    );
}

/// Entry point for a kernel thread: r12 = body, r14 = argument. Never reaches Ring 3.
// The `sti` is load-bearing: `alloc_kernel_stack` leaves `IF` clear, and `trampoline_entry` requires it clear on entry.
#[unsafe(naked)]
pub(crate) extern "C" fn kernel_start() {
    core::arch::naked_asm!(
        "call {unlock}",
        "sti",
        "mov rdi, r14",
        "call r12",
        "call {returned}",
        unlock = sym crate::sched::driver::trampoline_entry,
        returned = sym kernel_thread_returned,
    );
}

/// What [`kernel_start`] calls when a kernel thread's body returns: panics rather than halting silently.
extern "C" fn kernel_thread_returned() -> ! {
    panic!("a kernel thread's body returned; nothing runs on this stack now");
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
