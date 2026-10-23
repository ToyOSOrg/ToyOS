//! 2^255 - 19 multiplication and squaring on BMI2 and ADX (Intel SDM Vol.
//! 2A, ADCX, ADOX and MULX), in `field25519`'s four-word form: any value
//! below 2^256 in, such a value out. No branch or address depends on the
//! operands.

use core::arch::asm;

/// a b mod p, below 2^256 but not necessarily below p.
///
/// Row i adds a_i b to the running words, low halves on CF through `adcx`
/// and high halves on OF through `adox`; the eight-word product is then
/// folded as t_lo + 38 t_hi (2^256 = 38 mod p), and the fold's top word
/// times 38 once more. A carry out of that last addition leaves the low
/// word below 2^11, so its 38 is added without a carry.
///
/// # Safety
/// The CPU has BMI2 and ADX.
#[inline]
pub(super) unsafe fn mul(a: &[u64; 4], b: &[u64; 4]) -> [u64; 4] {
    let (r0, r1, r2, r3): (u64, u64, u64, u64);
    // SAFETY: the template reads the four words of `a` and of `b`, writes
    // only its declared registers and leaves the stack alone.
    unsafe {
        asm!(
            // Row 0 into (T0 .. T4).
            "mov rdx, [{a}]",
            "mulx {T1}, {T0}, [{b}]",
            "mulx {T2}, {lo}, [{b} + 8]",
            "add {T1}, {lo}",
            "mulx {T3}, {lo}, [{b} + 16]",
            "adc {T2}, {lo}",
            "mulx {T4}, {lo}, [{b} + 24]",
            "adc {T3}, {lo}",
            "adc {T4}, 0",
            // Row 1 into (T1 .. T5).
            "mov rdx, [{a} + 8]",
            "xor {lo:e}, {lo:e}",
            "mulx {hi}, {lo}, [{b}]",
            "adcx {T1}, {lo}",
            "adox {T2}, {hi}",
            "mulx {hi}, {lo}, [{b} + 8]",
            "adcx {T2}, {lo}",
            "adox {T3}, {hi}",
            "mulx {hi}, {lo}, [{b} + 16]",
            "adcx {T3}, {lo}",
            "adox {T4}, {hi}",
            "mulx {T5}, {lo}, [{b} + 24]",
            "adcx {T4}, {lo}",
            "mov {lo:e}, 0",
            "adox {T5}, {lo}",
            "adcx {T5}, {lo}",
            // Row 2 into (T2 .. T6).
            "mov rdx, [{a} + 16]",
            "xor {lo:e}, {lo:e}",
            "mulx {hi}, {lo}, [{b}]",
            "adcx {T2}, {lo}",
            "adox {T3}, {hi}",
            "mulx {hi}, {lo}, [{b} + 8]",
            "adcx {T3}, {lo}",
            "adox {T4}, {hi}",
            "mulx {hi}, {lo}, [{b} + 16]",
            "adcx {T4}, {lo}",
            "adox {T5}, {hi}",
            "mulx {T6}, {lo}, [{b} + 24]",
            "adcx {T5}, {lo}",
            "mov {lo:e}, 0",
            "adox {T6}, {lo}",
            "adcx {T6}, {lo}",
            // Row 3 into (T3 .. T7).
            "mov rdx, [{a} + 24]",
            "xor {lo:e}, {lo:e}",
            "mulx {hi}, {lo}, [{b}]",
            "adcx {T3}, {lo}",
            "adox {T4}, {hi}",
            "mulx {hi}, {lo}, [{b} + 8]",
            "adcx {T4}, {lo}",
            "adox {T5}, {hi}",
            "mulx {hi}, {lo}, [{b} + 16]",
            "adcx {T5}, {lo}",
            "adox {T6}, {hi}",
            "mulx {T7}, {lo}, [{b} + 24]",
            "adcx {T6}, {lo}",
            "mov {lo:e}, 0",
            "adox {T7}, {lo}",
            "adcx {T7}, {lo}",
            // (T0 .. T3) + 38 (T4 .. T7), the top word into T4.
            "mov edx, 38",
            "xor {lo:e}, {lo:e}",
            "mulx {hi}, {lo}, {T4}",
            "adcx {T0}, {lo}",
            "adox {T1}, {hi}",
            "mulx {hi}, {lo}, {T5}",
            "adcx {T1}, {lo}",
            "adox {T2}, {hi}",
            "mulx {hi}, {lo}, {T6}",
            "adcx {T2}, {lo}",
            "adox {T3}, {hi}",
            "mulx {T4}, {lo}, {T7}",
            "adcx {T3}, {lo}",
            "mov {lo:e}, 0",
            "adox {T4}, {lo}",
            "adcx {T4}, {lo}",
            // + 38 T4, then 38 for a carry out.
            "imul {T4}, {T4}, 38",
            "add {T0}, {T4}",
            "adc {T1}, 0",
            "adc {T2}, 0",
            "adc {T3}, 0",
            "sbb {lo}, {lo}",
            "and {lo}, 38",
            "add {T0}, {lo}",
            a = in(reg) a.as_ptr(),
            b = in(reg) b.as_ptr(),
            T0 = out(reg) r0,
            T1 = out(reg) r1,
            T2 = out(reg) r2,
            T3 = out(reg) r3,
            T4 = out(reg) _,
            T5 = out(reg) _,
            T6 = out(reg) _,
            T7 = out(reg) _,
            lo = out(reg) _,
            hi = out(reg) _,
            out("rdx") _,
            options(pure, readonly, nostack),
        );
    }
    [r0, r1, r2, r3]
}

/// a^2 mod p as [`mul`]: the six cross products once, doubled, the four
/// squares added, then `mul`'s fold.
///
/// # Safety
/// The CPU has BMI2 and ADX.
#[inline]
pub(super) unsafe fn square(a: &[u64; 4]) -> [u64; 4] {
    let (r0, r1, r2, r3): (u64, u64, u64, u64);
    // SAFETY: the template reads the four words of `a`, writes only its
    // declared registers and leaves the stack alone.
    unsafe {
        asm!(
            // Cross products into (T1 .. T6).
            "mov rdx, [{a}]",
            "mulx {T2}, {T1}, [{a} + 8]",
            "mulx {T3}, {lo}, [{a} + 16]",
            "add {T2}, {lo}",
            "mulx {T4}, {lo}, [{a} + 24]",
            "adc {T3}, {lo}",
            "adc {T4}, 0",
            "mov rdx, [{a} + 8]",
            "mulx {hi}, {lo}, [{a} + 16]",
            "add {T3}, {lo}",
            "adc {T4}, {hi}",
            "mulx {T5}, {lo}, [{a} + 24]",
            "adc {T5}, 0",
            "add {T4}, {lo}",
            "adc {T5}, 0",
            "mov rdx, [{a} + 16]",
            "mulx {T6}, {lo}, [{a} + 24]",
            "add {T5}, {lo}",
            "adc {T6}, 0",
            // Doubled into (T1 .. T7).
            "xor {T7:e}, {T7:e}",
            "add {T1}, {T1}",
            "adc {T2}, {T2}",
            "adc {T3}, {T3}",
            "adc {T4}, {T4}",
            "adc {T5}, {T5}",
            "adc {T6}, {T6}",
            "adc {T7}, 0",
            // The squares added into (T0 .. T7).
            "mov rdx, [{a}]",
            "mulx {hi}, {T0}, rdx",
            "add {T1}, {hi}",
            "mov rdx, [{a} + 8]",
            "mulx {hi}, {lo}, rdx",
            "adc {T2}, {lo}",
            "adc {T3}, {hi}",
            "mov rdx, [{a} + 16]",
            "mulx {hi}, {lo}, rdx",
            "adc {T4}, {lo}",
            "adc {T5}, {hi}",
            "mov rdx, [{a} + 24]",
            "mulx {hi}, {lo}, rdx",
            "adc {T6}, {lo}",
            "adc {T7}, {hi}",
            // The fold, as `mul`'s.
            "mov edx, 38",
            "xor {lo:e}, {lo:e}",
            "mulx {hi}, {lo}, {T4}",
            "adcx {T0}, {lo}",
            "adox {T1}, {hi}",
            "mulx {hi}, {lo}, {T5}",
            "adcx {T1}, {lo}",
            "adox {T2}, {hi}",
            "mulx {hi}, {lo}, {T6}",
            "adcx {T2}, {lo}",
            "adox {T3}, {hi}",
            "mulx {T4}, {lo}, {T7}",
            "adcx {T3}, {lo}",
            "mov {lo:e}, 0",
            "adox {T4}, {lo}",
            "adcx {T4}, {lo}",
            "imul {T4}, {T4}, 38",
            "add {T0}, {T4}",
            "adc {T1}, 0",
            "adc {T2}, 0",
            "adc {T3}, 0",
            "sbb {lo}, {lo}",
            "and {lo}, 38",
            "add {T0}, {lo}",
            a = in(reg) a.as_ptr(),
            T0 = out(reg) r0,
            T1 = out(reg) r1,
            T2 = out(reg) r2,
            T3 = out(reg) r3,
            T4 = out(reg) _,
            T5 = out(reg) _,
            T6 = out(reg) _,
            T7 = out(reg) _,
            lo = out(reg) _,
            hi = out(reg) _,
            out("rdx") _,
            options(pure, readonly, nostack),
        );
    }
    [r0, r1, r2, r3]
}
