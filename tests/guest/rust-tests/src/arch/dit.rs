//! The calling thread's `PSTATE.DIT`, read through the `DIT` register's view
//! (`S3_3_C4_C2_5`, bit 24), where the kernel answers that every CPU has
//! FEAT_DIT: `None` otherwise, and on x86-64, which has no such state.

#[cfg(target_arch = "aarch64")]
pub fn dit() -> Option<bool> {
    if !toyos_abi::syscall::timing_control() {
        return None;
    }
    let value: u64;
    // SAFETY: `DIT` is readable at EL0 on a CPU with FEAT_DIT, which the
    // kernel answered every CPU has.
    unsafe { core::arch::asm!("mrs {}, s3_3_c4_c2_5", out(reg) value, options(nomem, nostack, preserves_flags)) };
    Some(value & 1 << 24 != 0)
}

#[cfg(target_arch = "x86_64")]
pub fn dit() -> Option<bool> {
    None
}
