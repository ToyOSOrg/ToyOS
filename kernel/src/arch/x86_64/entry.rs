//! The one bracket every transition out of Ring 3 uses.
//!
//! A transition out of Ring 3 that can reach another task must save and
//! restore the whole user machine state, as the last act before returning to
//! Ring 3, after any point the task could have switched. [`save_user_state`]
//! leaves `rsp` aligned to [`UserFpState`]; [`restore_user_state`] restores it
//! exactly, and `r11` is scratch between them.

use core::mem::{align_of, size_of};

use super::fpu::UserFpState;

/// A gate whose handler can reach another task before returning to Ring 3,
/// and therefore brackets the user machine state.
#[derive(Clone, Copy)]
pub struct Ring3Entry(unsafe extern "sysv64" fn());

/// A gate whose handler cannot reach another task, so it saves nothing.
#[derive(Clone, Copy)]
pub struct Ring0Entry(unsafe extern "sysv64" fn());

impl Ring3Entry {
    pub const fn new(handler: unsafe extern "sysv64" fn()) -> Self {
        Self(handler)
    }

    pub fn addr(self) -> u64 {
        self.0 as *const () as u64
    }
}

impl Ring0Entry {
    pub const fn declare(handler: unsafe extern "sysv64" fn()) -> Self {
        Self(handler)
    }

    pub fn addr(self) -> u64 {
        self.0 as *const () as u64
    }
}

// `fp_bytes + fp_align`, aligned down, fits any incoming `rsp` alignment,
// with room in the slack above it to stash the caller's `rsp`.
const _: () = assert!(size_of::<UserFpState>().is_multiple_of(align_of::<UserFpState>()));
const _: () = assert!(align_of::<UserFpState>() >= 8);

/// `naked_asm!` for an entry that can reach another task: supplies the save
/// area's size and alignment from [`UserFpState`], and prepends `cld`. The
/// body must end with a trailing comma.
#[cfg(not(feature = "entry-df-unclean"))]
macro_rules! ring3_naked_asm {
    ($($body:tt)*) => {
        core::arch::naked_asm!(
            // An interrupt/trap gate does not clear `DF`, and this kernel's
            // forward-only `memmove`/`memcpy` write backward if a tick
            // interrupts a copy with `DF` set.
            "cld",
            $($body)*
            fp_bytes = const core::mem::size_of::<$crate::arch::fpu::UserFpState>(),
            fp_align = const core::mem::align_of::<$crate::arch::fpu::UserFpState>(),
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
            fp_bytes = const core::mem::size_of::<$crate::arch::fpu::UserFpState>(),
            fp_align = const core::mem::align_of::<$crate::arch::fpu::UserFpState>(),
        )
    };
}

/// `naked_asm!` for a trampoline into Ring 3 for the first time: supplies
/// the declared state's address from `fpu::INITIAL_IMAGE`.
#[cfg(not(feature = "fpu-save-nothing"))]
macro_rules! ring3_trampoline_asm {
    ($($body:tt)*) => {
        core::arch::naked_asm!(
            $($body)*
            fp_initial = sym $crate::arch::fpu::INITIAL_IMAGE,
        )
    };
}

/// The state a task that has never been in Ring 3 starts from.
#[cfg(not(feature = "fpu-save-nothing"))]
macro_rules! initial_user_state {
    () => {
        "fxrstor64 [rip + {fp_initial}]\n"
    };
}

/// Park the user machine state on this kernel stack, leaving `rsp` aligned
/// for the System V `call` that follows.
#[cfg(not(feature = "fpu-save-nothing"))]
macro_rules! save_user_state {
    () => {
        concat!(
            "mov r11, rsp\n",
            "sub rsp, {fp_bytes}\n",
            "sub rsp, {fp_align}\n",
            "and rsp, -{fp_align}\n",
            "mov [rsp + {fp_bytes}], r11\n",
            // fxsave64, not fsave/fxsave: non-waiting (won't trap a pending x87
            // exception) and REX.W-wide (fxsave truncates FIP/FDP to 32 bits).
            "fxsave64 [rsp]\n",
        )
    };
}

/// Put it back, and `rsp` with it.
#[cfg(not(feature = "fpu-save-nothing"))]
macro_rules! restore_user_state {
    () => {
        concat!(
            "fxrstor64 [rsp]\n",
            "mov rsp, [rsp + {fp_bytes}]\n",
        )
    };
}

// Negative control (`fpu-save-nothing`): same reservation, alignment and
// `rsp` bookkeeping as above, without moving the state.

#[cfg(feature = "fpu-save-nothing")]
macro_rules! ring3_trampoline_asm {
    ($($body:tt)*) => { core::arch::naked_asm!($($body)*) };
}

#[cfg(feature = "fpu-save-nothing")]
macro_rules! initial_user_state {
    () => {
        ""
    };
}

#[cfg(feature = "fpu-save-nothing")]
macro_rules! save_user_state {
    () => {
        concat!(
            "mov r11, rsp\n",
            "sub rsp, {fp_bytes}\n",
            "sub rsp, {fp_align}\n",
            "and rsp, -{fp_align}\n",
            "mov [rsp + {fp_bytes}], r11\n",
        )
    };
}

#[cfg(feature = "fpu-save-nothing")]
macro_rules! restore_user_state {
    () => {
        "mov rsp, [rsp + {fp_bytes}]\n"
    };
}

pub(crate) use {restore_user_state, ring3_naked_asm, save_user_state};

/// Entry point for new processes, reached through `context_switch`'s `ret`. r12 = entry point, r13 = user stack pointer.
// State loads after `unlock`, not before: earlier, registers hold the previous tenant's kernel context.
#[unsafe(naked)]
pub(crate) extern "C" fn process_start() {
    ring3_trampoline_asm!(
        "push r12",
        "push r13",
        "call {unlock}",
        "pop r13",
        "pop r12",
        initial_user_state!(),
        "push {user_ss}",
        "push r13",         // RSP: user stack
        "push 0x202",       // RFLAGS: IF=1
        "push {user_cs}",
        "push r12",         // RIP: entry point
        "iretq",
        unlock = sym crate::sched::driver::trampoline_entry,
        user_ss = const crate::arch::percpu::USER_DS,
        user_cs = const crate::arch::percpu::USER_CS,
    );
}

/// Entry point for new threads. r14 carries the argument, which lands in rdi.
#[unsafe(naked)]
pub(crate) extern "C" fn thread_start() {
    ring3_trampoline_asm!(
        "push r12",
        "push r13",
        "push r14",
        "call {unlock}",
        "pop r14",
        "pop r13",
        "pop r12",
        initial_user_state!(),
        "mov rdi, r14",
        "sub r13, 8",       // ABI: RSP must be 16n+8 at function entry
        "push {user_ss}",
        "push r13",
        "push 0x202",
        "push {user_cs}",
        "push r12",
        "iretq",
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
