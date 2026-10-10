//! A fault taken with the stack registers aimed where the process chose.

#[cfg(target_arch = "x86_64")]
pub use x86_64::*;

#[cfg(target_arch = "x86_64")]
mod x86_64 {
    /// #UD with `rsp` and `rbp` set first, so the fault's report meets both.
    pub fn undefined_with_stack_at(rsp: u64, rbp: u64) -> ! {
        // SAFETY: none — the fault is the point, and it ends this process.
        unsafe {
            core::arch::asm!(
                "mov rsp, {s}",
                "mov rbp, {f}",
                "ud2",
                s = in(reg) rsp,
                f = in(reg) rbp,
                options(noreturn),
            );
        }
    }
}

#[cfg(target_arch = "aarch64")]
pub use aarch64::*;

#[cfg(target_arch = "aarch64")]
mod aarch64 {
    /// An undefined instruction with `sp` and the frame pointer `x29` set
    /// first, so the fault's report meets both.
    pub fn undefined_with_stack_at(sp: u64, fp: u64) -> ! {
        // SAFETY: none — the fault is the point, and it ends this process.
        unsafe {
            core::arch::asm!(
                "mov sp, {s}",
                "mov x29, {f}",
                "udf #0",
                s = in(reg) sp,
                f = in(reg) fp,
                options(noreturn),
            );
        }
    }
}
