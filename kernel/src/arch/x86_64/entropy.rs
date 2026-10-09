//! The CPU's own random sources, which `crate::random` mixes into the
//! generator's key: `RDSEED`, the conditioned entropy source itself, and
//! `RDRAND`, the DRBG it seeds (SDM Vol. 1, "Random Number Generator
//! Instructions").

use core::arch::asm;

use super::cpu;

pub use crate::random::Source;

pub const SOURCES: &[Source] = &[
    Source { name: "RDSEED", available: has_rdseed, draw: rdseed },
    Source { name: "RDRAND", available: has_rdrand, draw: cpu::rdrand },
];

fn has_rdrand() -> Result<(), &'static str> {
    if cpu::has_rdrand() {
        Ok(())
    } else {
        Err("CPUID.01H:ECX[30] is clear, so this CPU has no RDRAND")
    }
}

/// `CPUID.(EAX=07H,ECX=0):EBX[18]`; without it `RDSEED` is `#UD`.
fn has_rdseed() -> Result<(), &'static str> {
    if cpu::cpuid(7, 0).1 & (1 << 18) != 0 {
        Ok(())
    } else {
        Err("CPUID.07H:EBX[18] is clear, so this CPU has no RDSEED")
    }
}

/// One drawn `u64`, or `None` after [`cpu::RDRAND_ATTEMPTS`] reported no data:
/// `RDSEED` clears CF while the entropy source refills, and never spins here.
fn rdseed() -> Option<u64> {
    for _ in 0..cpu::RDRAND_ATTEMPTS {
        let val: u64;
        let ok: u8;
        // SAFETY: writes one register and CF, which `setc` reads back into the
        // second declared output; `has_rdseed` answered before any draw.
        unsafe {
            asm!(
                "rdseed {val}",
                "setc {ok}",
                val = out(reg) val,
                ok = out(reg_byte) ok,
                options(nomem, nostack),
            );
        }
        if ok != 0 {
            return Some(val);
        }
        core::hint::spin_loop();
    }
    None
}
