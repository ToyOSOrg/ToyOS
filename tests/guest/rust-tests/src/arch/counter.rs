//! The CPU's own counter around a few hundred instructions, where a clock
//! read would be the larger cost: [`start`] and [`stop`] each read it once
//! every earlier instruction has finished.
//!
//! On x86-64 both ends are `CPUID` then `RDTSC`, the first of Intel's
//! sequences (Paoloni, "How to Benchmark Code Execution Times on Intel IA-32
//! and IA-64 Instruction Set Architectures", 2010). `CPUID` is a serializing
//! instruction on Intel (SDM vol. 3A, "Serializing Instructions") and on AMD
//! (APM vol. 2, "Serializing Instructions"): every instruction before it has
//! finished, and none after it has begun, when it completes. Its own
//! cost is in every measurement of either class alike. `LFENCE` is not used:
//! on AMD it holds back later instructions only where DE_CFG bit 1 or CPUID
//! Fn8000_0021 EAX bit 2 says so, which this kernel neither sets nor checks.
//! `RDTSCP` is not used either: QEMU's default CPU, which a smoke run takes,
//! has none.

#[cfg(target_arch = "x86_64")]
mod x86_64 {
    use core::arch::x86_64::{__cpuid, _rdtsc};

    #[inline(always)]
    pub fn start() -> u64 {
        let _ = __cpuid(0);
        // SAFETY: RDTSC is unprivileged while CR4.TSD is clear, which the
        // kernel's control-register declaration makes true on every CPU.
        unsafe { _rdtsc() }
    }
    pub use start as stop;
}
#[cfg(target_arch = "x86_64")]
pub use x86_64::{start, stop};

/// The virtual counter between two `ISB`s, which keep its read in program
/// order, at both ends.
#[cfg(target_arch = "aarch64")]
mod aarch64 {
    #[inline(always)]
    pub fn start() -> u64 {
        let now: u64;
        // SAFETY: an `isb` and a read of `CNTVCT_EL0`, which EL0 may read;
        // neither touches memory.
        unsafe { core::arch::asm!("isb", "mrs {now}, cntvct_el0", "isb", now = out(reg) now, options(nomem, nostack)) };
        now
    }
    pub use start as stop;
}
#[cfg(target_arch = "aarch64")]
pub use aarch64::{start, stop};
