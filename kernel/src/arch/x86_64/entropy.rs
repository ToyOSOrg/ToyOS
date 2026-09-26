//! The CPU's own random source: `RDRAND`.

use super::cpu;

/// How often a caller may ask before taking "no data" as the answer.
pub const ATTEMPTS: u32 = cpu::RDRAND_ATTEMPTS;

/// Whether this CPU can draw at all, or why not.
pub fn available() -> Result<(), &'static str> {
    if cpu::has_rdrand() {
        Ok(())
    } else {
        Err("CPUID.01H:ECX[30] is clear, so this CPU has no RDRAND")
    }
}

/// One drawn `u64`, or `None` when the source had nothing to give; never waits.
pub fn draw() -> Option<u64> {
    cpu::rdrand()
}
