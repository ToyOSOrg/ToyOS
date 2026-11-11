/// The counter the clock page's line describes: the time-stamp counter, read
/// once every earlier instruction has executed and every earlier load is
/// globally visible (SDM Vol. 2B, RDTSCP; AMD APM Vol. 3, RDTSCP, "forces all
/// older instructions to retire before reading the time-stamp counter"), so a
/// reading is never older than one this thread took before it, or than one
/// whose writer's record this thread has loaded. Not `LFENCE; RDTSC`: on AMD
/// `LFENCE` holds later instructions back only where DE_CFG bit 1 or CPUID
/// Fn8000_0021 EAX bit 2 makes it dispatch serializing. Nothing holds a later
/// instruction back ([`crate::clock::Latch::read_with`]).
#[inline]
pub fn counter() -> u64 {
    let lo: u32;
    let hi: u32;
    // SAFETY: reads the time-stamp counter and `IA32_TSC_AUX` into the declared
    // outputs; unprivileged while CR4.TSD is clear, which the kernel's
    // control-register declaration makes true on every CPU, and the kernel's
    // clock refuses to start on a CPU without RDTSCP. Not `nomem`, which would
    // let the compiler move the clock page's loads across the read.
    unsafe { core::arch::asm!("rdtscp", out("eax") lo, out("edx") hi, out("ecx") _, options(nostack, preserves_flags)) };
    (hi as u64) << 32 | lo as u64
}

/// The processor's CPUID identity.
pub fn processor_id() -> Option<crate::ProcessorId> {
    let leaf_0 = core::arch::x86_64::__cpuid(0);
    let vendor = core::array::from_fn(|i| [leaf_0.ebx, leaf_0.edx, leaf_0.ecx][i / 4].to_le_bytes()[i % 4]);
    Some(crate::ProcessorId { vendor, signature: core::arch::x86_64::__cpuid(1).eax })
}

/// `in` and `out` from Ring 3: a port this process holds no grant for, which
/// its CPU's I/O permission bitmap refuses, faults it.
pub mod ioport {
    pub fn in8(port: u16) -> u8 {
        let value: u8;
        // SAFETY: an `in` has no memory effect.
        unsafe { core::arch::asm!("in al, dx", in("dx") port, out("al") value, options(nomem, nostack, preserves_flags)) };
        value
    }

    pub fn in16(port: u16) -> u16 {
        let value: u16;
        // SAFETY: as `in8`'s.
        unsafe { core::arch::asm!("in ax, dx", in("dx") port, out("ax") value, options(nomem, nostack, preserves_flags)) };
        value
    }

    /// What the device does with the byte is the caller's.
    pub fn out8(port: u16, value: u8) {
        // SAFETY: an `out` has no memory effect.
        unsafe { core::arch::asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags)) };
    }

    pub fn out16(port: u16, value: u16) {
        // SAFETY: as `out8`'s.
        unsafe { core::arch::asm!("out dx, ax", in("dx") port, in("ax") value, options(nomem, nostack, preserves_flags)) };
    }
}
