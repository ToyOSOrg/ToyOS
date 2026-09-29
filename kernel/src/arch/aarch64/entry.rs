//! Where a new context starts: the frame [`super::switch::context_switch`]
//! restores it from, and the trampolines its return lands in — to EL0 for the
//! first time, or into a kernel thread's body.
//!
//! A trampoline to EL0 leaves nothing of the kernel in a register: every
//! general register but the argument is zeroed, and the FP/SIMD state is the
//! zero the initial frame carried.

use super::switch::{DAIF_AT, FRAME_BYTES, RETURN_AT};

/// `DAIF` a new context starts with: every exception masked, as an entry from
/// EL0 is, for `trampoline_entry`'s contract.
const DAIF_MASKED: u64 = 0b1111 << 6;

/// Zero x1–x30, the registers a first entry to EL0 must not carry.
macro_rules! zero_registers {
    () => {
        concat!(
            "mov x1, xzr\n", "mov x2, xzr\n", "mov x3, xzr\n", "mov x4, xzr\n", "mov x5, xzr\n",
            "mov x6, xzr\n", "mov x7, xzr\n", "mov x8, xzr\n", "mov x9, xzr\n", "mov x10, xzr\n",
            "mov x11, xzr\n", "mov x12, xzr\n", "mov x13, xzr\n", "mov x14, xzr\n", "mov x15, xzr\n",
            "mov x16, xzr\n", "mov x17, xzr\n", "mov x18, xzr\n", "mov x19, xzr\n", "mov x20, xzr\n",
            "mov x21, xzr\n", "mov x22, xzr\n", "mov x23, xzr\n", "mov x24, xzr\n", "mov x25, xzr\n",
            "mov x26, xzr\n", "mov x27, xzr\n", "mov x28, xzr\n", "mov x29, xzr\n", "mov x30, xzr\n",
        )
    };
}

/// A thread's first entry to EL0 at `x19` on the stack `x20`, with `x0` =
/// `x21`: a process's argument is zero, a thread's is its own. `SPSR_EL1`
/// zero is EL0 with every exception unmasked, and `SP_EL1` is left at the
/// top of this thread's kernel stack, which is where its entries land.
#[unsafe(naked)]
pub(crate) extern "C" fn process_start() {
    core::arch::naked_asm!(
        "bl {unlock}",
        "msr elr_el1, x19",
        "msr sp_el0, x20",
        "msr spsr_el1, xzr",
        "mov x0, x21",
        zero_registers!(),
        "eret",
        unlock = sym crate::sched::driver::trampoline_entry,
    );
}

/// A thread's first entry is a process's, with its argument in `x0`.
pub(crate) use process_start as thread_start;

/// Entry point for a kernel thread: `x19` = body, `x21` = argument. Never
/// reaches EL0; unmasks interrupts once `trampoline_entry`, which requires
/// them masked, is done.
#[unsafe(naked)]
pub(crate) extern "C" fn kernel_start() {
    core::arch::naked_asm!(
        "bl {unlock}",
        "msr daifclr, #3",
        "mov x0, x21",
        "blr x19",
        "bl {returned}",
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
/// where its return lands, with the entry, the stack and the argument where
/// the trampolines read them (`x19`, `x20`, `x21`), and FP/SIMD state zero.
/// # Safety
/// `top` is the 16-byte-aligned end of a fresh kernel stack nothing else
/// references, at least [`FRAME_BYTES`] deep.
pub unsafe fn initial_frame(
    top: u64,
    trampoline: unsafe extern "C" fn(),
    user_entry: u64,
    user_sp: u64,
    arg: u64,
) -> u64 {
    let frame = top - FRAME_BYTES as u64;
    // SAFETY: `[frame, top)` is the top `FRAME_BYTES` of the stack the caller owns.
    unsafe {
        core::ptr::write_bytes(frame as *mut u8, 0, FRAME_BYTES);
        let word = |at: usize| (frame + at as u64) as *mut u64;
        *word(0) = user_entry;
        *word(8) = user_sp;
        *word(16) = arg;
        *word(DAIF_AT) = DAIF_MASKED;
        *word(RETURN_AT) = trampoline as usize as u64;
    }
    frame
}
