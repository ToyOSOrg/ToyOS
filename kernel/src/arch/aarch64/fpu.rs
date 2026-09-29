//! User FP/SIMD state: v0–v31, `FPCR` and `FPSR`. The kernel is built
//! soft-float and never touches them, so a thread's own stay in the registers
//! across every entry from EL0; they are saved and restored only where a
//! thread stops running, in [`super::switch`]'s frame, and a thread that has
//! never run starts from zero — `FPCR` zero is round-to-nearest with every
//! trap disabled (Arm ARM K.a, D24.2.62).

/// The state's bytes in a switch frame: 32 128-bit registers, then `FPCR` and `FPSR`.
pub const STATE_BYTES: usize = 32 * 16 + 16;
