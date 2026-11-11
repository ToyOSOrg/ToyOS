/// The counter the clock page's line describes: the generic timer's
/// virtual count, which EL0 may read, read after every earlier instruction, as
/// the x86-64 one is.
#[inline]
pub fn counter() -> u64 {
    let now: u64;
    // SAFETY: an `isb` and a read of `CNTVCT_EL0` into a register; neither
    // touches memory. Not `nomem`, which would let the compiler move the clock
    // page's loads across the read.
    unsafe { core::arch::asm!("isb", "mrs {now}, cntvct_el0", now = out(reg) now, options(nostack, preserves_flags)) };
    now
}

/// AArch64 has no CPUID identity.
pub fn processor_id() -> Option<crate::ProcessorId> {
    None
}

/// AArch64 has no port space, and no grant names a port on it: each of these
/// dies naming the port it was asked for.
pub mod ioport {
    #[cold]
    fn none(port: u16) -> ! {
        panic!("ioport: port {port:#x} asked for on AArch64, which has no port space")
    }

    pub fn in8(port: u16) -> u8 {
        none(port)
    }

    pub fn in16(port: u16) -> u16 {
        none(port)
    }

    pub fn out8(port: u16, _value: u8) {
        none(port)
    }

    pub fn out16(port: u16, _value: u16) {
        none(port)
    }
}
