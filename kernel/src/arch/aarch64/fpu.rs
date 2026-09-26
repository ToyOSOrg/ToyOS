//! User FP/SIMD state: `CPACR_EL1.FPEN` traps it at EL1 and EL0 until the
//! port's stage 7 saves and restores it. The kernel itself never uses it.
