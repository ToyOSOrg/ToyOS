//! Which CPU a thread is on, as that CPU answers.

#[cfg(target_arch = "x86_64")]
pub use x86_64::*;

#[cfg(target_arch = "x86_64")]
mod x86_64 {
    use core::arch::x86_64::{__cpuid, __cpuid_count};

    /// The x2APIC id of the CPU that ran this (CPUID leaf 0BH, `EDX`: "x2APIC
    /// ID of the current logical processor", Intel SDM vol. 2A). The caller's
    /// CPU only until the scheduler moves the thread.
    pub fn x2apic_id() -> u32 {
        let leaves = __cpuid(0).eax;
        assert!(leaves >= 0xb, "this CPU's highest basic CPUID leaf is {leaves:#x}, below 0BH: it names no x2APIC id");
        __cpuid_count(0xb, 0).edx
    }
}
