//! `context_switch`: what a context stands on, saved onto the outgoing stack
//! and restored off the incoming one — the callee-saved registers, `DAIF`,
//! and the thread's FP/SIMD state ([`super::fpu`]), which travels with the
//! stack of the thread it belongs to.
//!
//! The frame, from the saved stack pointer up: x19–x28, x29 and `DAIF`, x30
//! and a pad word, then [`super::fpu::STATE_BYTES`] of FP/SIMD state — the
//! general registers first, because a pair's store reaches only 504 bytes.

use core::arch::naked_asm;

use super::fpu::STATE_BYTES;

/// Where `DAIF` is saved, beside x29.
pub const DAIF_AT: usize = 88;
/// Where x30 is saved: the address the frame returns to.
pub const RETURN_AT: usize = 96;
/// Where the FP/SIMD state starts.
pub const FP_AT: usize = 112;
/// One saved context's bytes.
pub const FRAME_BYTES: usize = FP_AT + STATE_BYTES;

const _: () = assert!(FRAME_BYTES.is_multiple_of(16));

/// Save this context's frame below the stack pointer, store that pointer at
/// `old_sp`, and return into the frame at `new_sp`.
/// # Safety
/// `old_sp` is the outgoing context's save slot and `new_sp` a frame this
/// function or `super::entry::initial_frame` wrote, on a stack that is live.
#[unsafe(naked)]
pub(crate) unsafe extern "C" fn context_switch(old_sp: *mut u64, new_sp: u64) {
    naked_asm!(
        // The kernel is soft-float: the assembler is told these are here for this block alone.
        ".arch_extension fp",
        ".arch_extension simd",
        "sub sp, sp, #{frame}",
        "stp x19, x20, [sp, #0]",
        "stp x21, x22, [sp, #16]",
        "stp x23, x24, [sp, #32]",
        "stp x25, x26, [sp, #48]",
        "stp x27, x28, [sp, #64]",
        "mrs x9, daif",
        "stp x29, x9, [sp, #80]",
        "str x30, [sp, #{ret}]",
        "add x9, sp, #{fp}",
        "stp q0, q1, [x9, #0]",
        "stp q2, q3, [x9, #32]",
        "stp q4, q5, [x9, #64]",
        "stp q6, q7, [x9, #96]",
        "stp q8, q9, [x9, #128]",
        "stp q10, q11, [x9, #160]",
        "stp q12, q13, [x9, #192]",
        "stp q14, q15, [x9, #224]",
        "stp q16, q17, [x9, #256]",
        "stp q18, q19, [x9, #288]",
        "stp q20, q21, [x9, #320]",
        "stp q22, q23, [x9, #352]",
        "stp q24, q25, [x9, #384]",
        "stp q26, q27, [x9, #416]",
        "stp q28, q29, [x9, #448]",
        "stp q30, q31, [x9, #480]",
        "mrs x10, fpcr",
        "str x10, [x9, #512]",
        "mrs x10, fpsr",
        "str x10, [x9, #520]",
        "mov x9, sp",
        "str x9, [x0]",
        "mov sp, x1",
        "add x9, sp, #{fp}",
        "ldp q0, q1, [x9, #0]",
        "ldp q2, q3, [x9, #32]",
        "ldp q4, q5, [x9, #64]",
        "ldp q6, q7, [x9, #96]",
        "ldp q8, q9, [x9, #128]",
        "ldp q10, q11, [x9, #160]",
        "ldp q12, q13, [x9, #192]",
        "ldp q14, q15, [x9, #224]",
        "ldp q16, q17, [x9, #256]",
        "ldp q18, q19, [x9, #288]",
        "ldp q20, q21, [x9, #320]",
        "ldp q22, q23, [x9, #352]",
        "ldp q24, q25, [x9, #384]",
        "ldp q26, q27, [x9, #416]",
        "ldp q28, q29, [x9, #448]",
        "ldp q30, q31, [x9, #480]",
        "ldr x10, [x9, #512]",
        "msr fpcr, x10",
        "ldr x10, [x9, #520]",
        "msr fpsr, x10",
        "ldp x19, x20, [sp, #0]",
        "ldp x21, x22, [sp, #16]",
        "ldp x23, x24, [sp, #32]",
        "ldp x25, x26, [sp, #48]",
        "ldp x27, x28, [sp, #64]",
        "ldp x29, x9, [sp, #80]",
        "ldr x30, [sp, #{ret}]",
        "add sp, sp, #{frame}",
        "msr daif, x9",
        "ret",
        ".arch_extension nosimd",
        ".arch_extension nofp",
        frame = const FRAME_BYTES,
        fp = const FP_AT,
        ret = const RETURN_AT,
    );
}
