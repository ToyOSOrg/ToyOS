//! The one bracket every transition out of Ring 3 uses.
//!
//! A transition out of Ring 3 that can reach another task must save and
//! restore the whole user machine state, as the last act before returning to
//! Ring 3, after any point the task could have switched. [`save_user_state`]
//! parks it in [`fpu::AREA_BYTES`](super::fpu::AREA_BYTES) on this kernel
//! stack, leaving `rsp` 64-byte aligned on the slot that holds the `rsp` it
//! found; [`restore_user_state`] restores both. Each clobbers `rax` and `rdx`,
//! the components' mask, and the save leaves `r11` the `rsp` it found.
//!
//! **`XSAVEOPT`'s invariant.** `XSAVEOPT` skips a component unmodified since
//! the last `XRSTOR` from the same address, so between the `XRSTOR` that
//! returns a thread to Ring 3 and that thread's next save on the same CPU
//! nothing may write the area. It holds because every restore is followed
//! only by pops and `iretq`/`sysretq` with `IF` clear; the area lies on the
//! thread's own kernel stack below the frame every Ring 3 entry builds; and
//! the IST stacks that save (`#DF`, `#MC`) never return to Ring 3. A save and
//! restore nested in Ring 0 is undone by the outer restore before Ring 3 runs.

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

/// `naked_asm!` for an entry that can reach another task: supplies the save
/// area's size, and prepends `cld`. The body must end with a trailing comma.
#[cfg(not(feature = "entry-df-unclean"))]
macro_rules! ring3_naked_asm {
    ($($body:tt)*) => {
        core::arch::naked_asm!(
            // An interrupt/trap gate does not clear `DF`, and this kernel's
            // forward-only `memmove`/`memcpy` write backward if a tick
            // interrupts a copy with `DF` set.
            "cld",
            $($body)*
            fp_area = sym $crate::arch::fpu::AREA_BYTES,
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
            fp_area = sym $crate::arch::fpu::AREA_BYTES,
        )
    };
}

/// `naked_asm!` for a trampoline into Ring 3 for the first time: supplies
/// the declared state's address from `fpu::INITIAL_IMAGE`.
macro_rules! ring3_trampoline_asm {
    ($($body:tt)*) => {
        core::arch::naked_asm!(
            $($body)*
            fp_initial = sym $crate::arch::fpu::INITIAL_IMAGE,
        )
    };
}

/// The state a task that has never been in Ring 3 starts from: every
/// component `XCR0` enables, so no upper half of a register is left from the
/// CPU's last tenant. Clobbers `rax` and `rdx`.
macro_rules! initial_user_state {
    () => {
        concat!(
            "mov eax, -1\n",
            "mov edx, -1\n",
            "xrstor64 [rip + {fp_initial}]\n",
        )
    };
}

/// Park the user machine state on this kernel stack, leaving `rsp` aligned
/// for the System V `call` that follows.
macro_rules! save_user_state {
    () => {
        concat!(
            "mov r11, rsp\n",
            "sub rsp, qword ptr [rip + {fp_area}]\n",
            "and rsp, -64\n",
            "sub rsp, 64\n",
            "mov [rsp], r11\n",
            // `XSAVEOPT` writes only `XCR0`'s bits of `XSTATE_BV`, and the
            // standard form's `XRSTOR` is `#GP` on any other bit set or on a
            // nonzero `XCOMP_BV` or reserved word: a stack holds whatever was
            // last there (SDM Vol. 1 §13.8.1).
            "mov qword ptr [rsp + 64 + 512], 0\n",
            "mov qword ptr [rsp + 64 + 520], 0\n",
            "mov qword ptr [rsp + 64 + 528], 0\n",
            "mov eax, -1\n",
            "mov edx, -1\n",
            "xsaveopt64 [rsp + 64]\n",
        )
    };
}

/// Put it back, and `rsp` with it.
macro_rules! restore_user_state {
    () => {
        concat!(
            "mov eax, -1\n",
            "mov edx, -1\n",
            "xrstor64 [rsp + 64]\n",
            "mov rsp, [rsp]\n",
        )
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
