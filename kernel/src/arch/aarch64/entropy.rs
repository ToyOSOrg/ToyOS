//! The CPU's own random source, which `crate::random` mixes into the
//! generator's key: `RNDR`, where `ID_AA64ISAR0_EL1.RNDR` says there is one.
//! A guest under HVF has none, and its generator is keyed from the loader's
//! seed alone.

pub use crate::random::Source;

pub const SOURCES: &[Source] = &[Source { name: "RNDR", available, draw }];

/// How often [`draw`] asks before taking "no data" as the answer: `RNDR`
/// reports a transient failure through `NZCV`, as `RDRAND` does through `CF`.
const ATTEMPTS: u32 = 10;

fn available() -> Result<(), &'static str> {
    let isar0: u64;
    // SAFETY: reads an ID register; touches nothing.
    unsafe {
        core::arch::asm!("mrs {}, id_aa64isar0_el1", out(reg) isar0, options(nomem, nostack, preserves_flags))
    };
    if (isar0 >> 60) & 0xF != 0 {
        Ok(())
    } else {
        Err("ID_AA64ISAR0_EL1.RNDR is zero, so this CPU has no RNDR")
    }
}

/// One drawn `u64`, or `None` after [`ATTEMPTS`] reported no data; never waits.
fn draw() -> Option<u64> {
    for _ in 0..ATTEMPTS {
        let value: u64;
        let failed: u64;
        // SAFETY: `RNDR` (`S3_3_C2_C4_0`) reads a random number, setting `Z` on
        // failure; `available` said the register exists before any draw.
        unsafe {
            core::arch::asm!(
                "mrs {value}, s3_3_c2_c4_0",
                "cset {failed}, eq",
                value = out(reg) value,
                failed = out(reg) failed,
                options(nomem, nostack),
            )
        };
        if failed == 0 {
            return Some(value);
        }
    }
    None
}
