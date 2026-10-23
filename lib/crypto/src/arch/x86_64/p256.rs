//! P-256 field multiplication and squaring on BMI2 and ADX (Intel SDM Vol.
//! 2A, ADCX, ADOX and MULX). No branch or address depends on the operands.

use core::arch::asm;

/// p's top word; its other words enter the reduction step as shifts and
/// immediates.
const P3: u64 = <crate::nist::P256Field as crate::montgomery::Modulus<4>>::P[3];

/// a b / 2^256 mod p for a, b < p, by the portable field's CIOS with P-256's
/// reduction step (`montgomery`'s `Form::P256`).
///
/// Row i adds a_i b to the five running words, carrying into a sixth: low
/// halves on CF through `adcx`, high halves on OF through `adox`. Its
/// reduction step, with q the low word (-p^-1 = 1 mod 2^64), adds q 2^96 as
/// q << 32 at word one and q >> 32 at word two, and q p3 at words three and
/// four, which leaves word zero zero and drops it. Each row's six words
/// rotate one register on. The sum is below 2p, and p is subtracted where
/// the subtraction does not borrow, by `cmov`.
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
            // Row 0 into (A B C D E), F = 0.
            "mov rdx, [{a}]",
            "mulx {B}, {A}, [{b}]",
            "mulx {C}, {lo}, [{b} + 8]",
            "add {B}, {lo}",
            "mulx {D}, {lo}, [{b} + 16]",
            "adc {C}, {lo}",
            "mulx {E}, {lo}, [{b} + 24]",
            "adc {D}, {lo}",
            "adc {E}, 0",
            "xor {F:e}, {F:e}",
            // Reduction: q = A; the sum moves to (B C D E F).
            "mov rdx, {A}",
            "mulx {hi}, {lo}, {p3}",
            "shl rdx, 32",
            "shr {A}, 32",
            "add {B}, rdx",
            "adc {C}, {A}",
            "adc {D}, {lo}",
            "adc {E}, {hi}",
            "adc {F}, 0",
            // Row 1 into (B C D E F), A = 0.
            "mov rdx, [{a} + 8]",
            "xor {A:e}, {A:e}",
            "mulx {hi}, {lo}, [{b}]",
            "adcx {B}, {lo}",
            "adox {C}, {hi}",
            "mulx {hi}, {lo}, [{b} + 8]",
            "adcx {C}, {lo}",
            "adox {D}, {hi}",
            "mulx {hi}, {lo}, [{b} + 16]",
            "adcx {D}, {lo}",
            "adox {E}, {hi}",
            "mulx {hi}, {lo}, [{b} + 24]",
            "adcx {E}, {lo}",
            "adox {F}, {hi}",
            "adcx {F}, {A}",
            "adox {A}, {A}",
            "adc {A}, 0",
            // Reduction: q = B; the sum moves to (C D E F A).
            "mov rdx, {B}",
            "mulx {hi}, {lo}, {p3}",
            "shl rdx, 32",
            "shr {B}, 32",
            "add {C}, rdx",
            "adc {D}, {B}",
            "adc {E}, {lo}",
            "adc {F}, {hi}",
            "adc {A}, 0",
            // Row 2 into (C D E F A), B = 0.
            "mov rdx, [{a} + 16]",
            "xor {B:e}, {B:e}",
            "mulx {hi}, {lo}, [{b}]",
            "adcx {C}, {lo}",
            "adox {D}, {hi}",
            "mulx {hi}, {lo}, [{b} + 8]",
            "adcx {D}, {lo}",
            "adox {E}, {hi}",
            "mulx {hi}, {lo}, [{b} + 16]",
            "adcx {E}, {lo}",
            "adox {F}, {hi}",
            "mulx {hi}, {lo}, [{b} + 24]",
            "adcx {F}, {lo}",
            "adox {A}, {hi}",
            "adcx {A}, {B}",
            "adox {B}, {B}",
            "adc {B}, 0",
            // Reduction: q = C; the sum moves to (D E F A B).
            "mov rdx, {C}",
            "mulx {hi}, {lo}, {p3}",
            "shl rdx, 32",
            "shr {C}, 32",
            "add {D}, rdx",
            "adc {E}, {C}",
            "adc {F}, {lo}",
            "adc {A}, {hi}",
            "adc {B}, 0",
            // Row 3 into (D E F A B), C = 0.
            "mov rdx, [{a} + 24]",
            "xor {C:e}, {C:e}",
            "mulx {hi}, {lo}, [{b}]",
            "adcx {D}, {lo}",
            "adox {E}, {hi}",
            "mulx {hi}, {lo}, [{b} + 8]",
            "adcx {E}, {lo}",
            "adox {F}, {hi}",
            "mulx {hi}, {lo}, [{b} + 16]",
            "adcx {F}, {lo}",
            "adox {A}, {hi}",
            "mulx {hi}, {lo}, [{b} + 24]",
            "adcx {A}, {lo}",
            "adox {B}, {hi}",
            "adcx {B}, {C}",
            "adox {C}, {C}",
            "adc {C}, 0",
            // Reduction: q = D; the sum moves to (E F A B C).
            "mov rdx, {D}",
            "mulx {hi}, {lo}, {p3}",
            "shl rdx, 32",
            "shr {D}, 32",
            "add {E}, rdx",
            "adc {F}, {D}",
            "adc {A}, {lo}",
            "adc {B}, {hi}",
            "adc {C}, 0",
            // (E F A B C) - p, p1 = 2^32 - 1 in b's register; no borrow out
            // means the sum was at least p.
            "mov {D}, {E}",
            "mov {lo}, {F}",
            "mov {hi}, {A}",
            "mov rdx, {B}",
            "mov {b:e}, 0xffffffff",
            "sub {D}, -1",
            "sbb {lo}, {b}",
            "sbb {hi}, 0",
            "sbb rdx, {p3}",
            "sbb {C}, 0",
            "cmovnc {E}, {D}",
            "cmovnc {F}, {lo}",
            "cmovnc {A}, {hi}",
            "cmovnc {B}, rdx",
            a = in(reg) a.as_ptr(),
            b = inout(reg) b.as_ptr() => _,
            p3 = in(reg) P3,
            A = out(reg) r2,
            B = out(reg) r3,
            C = out(reg) _,
            D = out(reg) _,
            E = out(reg) r0,
            F = out(reg) r1,
            lo = out(reg) _,
            hi = out(reg) _,
            out("rdx") _,
            options(pure, readonly, nostack),
        );
    }
    [r0, r1, r2, r3]
}

/// a^2 / 2^256 mod p for a < p: the cross products once, doubled, the
/// squares added, then [`mul`]'s reduction steps in place, the word each
/// step clears gathering the carry out of the top.
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
            // Reduction steps; T0 then gathers the carry out of T7.
            "mov {a}, {p3}",
            "mov rdx, {T0}",
            "mulx {hi}, {lo}, {a}",
            "shl rdx, 32",
            "shr {T0}, 32",
            "add {T1}, rdx",
            "adc {T2}, {T0}",
            "adc {T3}, {lo}",
            "adc {T4}, {hi}",
            "adc {T5}, 0",
            "adc {T6}, 0",
            "adc {T7}, 0",
            "mov {T0:e}, 0",
            "adc {T0}, 0",
            "mov rdx, {T1}",
            "mulx {hi}, {lo}, {a}",
            "shl rdx, 32",
            "shr {T1}, 32",
            "add {T2}, rdx",
            "adc {T3}, {T1}",
            "adc {T4}, {lo}",
            "adc {T5}, {hi}",
            "adc {T6}, 0",
            "adc {T7}, 0",
            "adc {T0}, 0",
            "mov rdx, {T2}",
            "mulx {hi}, {lo}, {a}",
            "shl rdx, 32",
            "shr {T2}, 32",
            "add {T3}, rdx",
            "adc {T4}, {T2}",
            "adc {T5}, {lo}",
            "adc {T6}, {hi}",
            "adc {T7}, 0",
            "adc {T0}, 0",
            "mov rdx, {T3}",
            "mulx {hi}, {lo}, {a}",
            "shl rdx, 32",
            "shr {T3}, 32",
            "add {T4}, rdx",
            "adc {T5}, {T3}",
            "adc {T6}, {lo}",
            "adc {T7}, {hi}",
            "adc {T0}, 0",
            // (T4 .. T7 T0) - p, as in `mul`.
            "mov {T1}, {T4}",
            "mov {T2}, {T5}",
            "mov {T3}, {T6}",
            "mov rdx, {T7}",
            "mov {lo:e}, 0xffffffff",
            "sub {T1}, -1",
            "sbb {T2}, {lo}",
            "sbb {T3}, 0",
            "sbb rdx, {a}",
            "sbb {T0}, 0",
            "cmovnc {T4}, {T1}",
            "cmovnc {T5}, {T2}",
            "cmovnc {T6}, {T3}",
            "cmovnc {T7}, rdx",
            a = inout(reg) a.as_ptr() => _,
            p3 = const P3,
            T0 = out(reg) _,
            T1 = out(reg) _,
            T2 = out(reg) _,
            T3 = out(reg) _,
            T4 = out(reg) r0,
            T5 = out(reg) r1,
            T6 = out(reg) r2,
            T7 = out(reg) r3,
            lo = out(reg) _,
            hi = out(reg) _,
            out("rdx") _,
            options(pure, readonly, nostack),
        );
    }
    [r0, r1, r2, r3]
}
