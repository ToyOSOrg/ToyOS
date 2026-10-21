//! The CPU's own counter, read in program order, for timing a few hundred
//! instructions where a clock read would be the larger cost.

/// The time-stamp counter, read after every earlier instruction and before
/// any later one begins: an `LFENCE` on each side (Intel SDM vol. 2B,
/// "RDTSC").
#[cfg(target_arch = "x86_64")]
#[inline(always)]
pub fn ticks() -> u64 {
    use core::arch::x86_64::{_mm_lfence, _rdtsc};
    // SAFETY: `lfence` has no operands; `rdtsc` is unprivileged while CR4.TSD
    // is clear, which the kernel's control-register declaration makes true on
    // every CPU.
    unsafe {
        _mm_lfence();
        let now = _rdtsc();
        _mm_lfence();
        now
    }
}

/// The virtual counter, between two `ISB`s, which keep it from being read
/// out of program order.
#[cfg(target_arch = "aarch64")]
#[inline(always)]
pub fn ticks() -> u64 {
    let now: u64;
    // SAFETY: an `isb` and a read of `CNTVCT_EL0`, which EL0 may read; neither
    // touches memory.
    unsafe { core::arch::asm!("isb", "mrs {now}, cntvct_el0", "isb", now = out(reg) now, options(nomem, nostack)) };
    now
}
