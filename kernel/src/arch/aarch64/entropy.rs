//! The CPU's own random source: `RNDR`, where `ID_AA64ISAR0_EL1.RNDR` says
//! there is one. A machine without it (QEMU under HVF is one) draws from
//! virtio-rng instead, which the port's stage 6 brings up.

/// How often a caller may ask before taking "no data" as the answer: `RNDR`
/// reports a transient failure through `NZCV`, as `RDRAND` does through `CF`.
pub const ATTEMPTS: u32 = 10;

fn has_rndr() -> bool {
    let isar0: u64;
    // SAFETY: reads an ID register; touches nothing.
    unsafe {
        core::arch::asm!("mrs {}, id_aa64isar0_el1", out(reg) isar0, options(nomem, nostack, preserves_flags))
    };
    (isar0 >> 60) & 0xF != 0
}

/// Whether this CPU can draw at all, or why not.
pub fn available() -> Result<(), &'static str> {
    if has_rndr() {
        Ok(())
    } else {
        Err("ID_AA64ISAR0_EL1.RNDR is zero, so this CPU has no RNDR, and virtio-rng is the port's stage 6")
    }
}

/// One drawn `u64`, or `None` when the source had nothing to give; never waits.
pub fn draw() -> Option<u64> {
    let value: u64;
    let failed: u64;
    // SAFETY: `RNDR` (`S3_3_C2_C4_0`) reads a random number, setting `Z` on
    // failure; `available` said the register exists before any caller draws.
    unsafe {
        core::arch::asm!(
            "mrs {value}, s3_3_c2_c4_0",
            "cset {failed}, eq",
            value = out(reg) value,
            failed = out(reg) failed,
            options(nomem, nostack),
        )
    };
    (failed == 0).then_some(value)
}
