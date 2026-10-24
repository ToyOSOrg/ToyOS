//! P-256 field arithmetic in general registers: multiplication and squaring
//! on BMI2 and ADX (Intel SDM Vol. 2A, ADCX, ADOX and MULX), and addition and
//! subtraction on the base instruction set. Operands below p arrive in
//! registers and results below p leave in them, so a point formula's values
//! need not pass through memory between operations. No branch or address
//! depends on the operands; every conditional subtraction is a `cmov` or a
//! mask taken from a borrow.
//!
//! Each Montgomery reduction step adds q p for q the lowest word, which
//! clears it as -p^-1 = 1 mod 2^64, and drops it: q 2^32 at the next word
//! (from q (2^64 - 1) + q (2^32 - 1) 2^64), and q p3 two words above that,
//! where q p3 = q 2^64 - q 2^32 + q is formed by shifts and subtraction
//! (Gueron and Krasnov, "Fast prime field elliptic-curve cryptography with
//! 256-bit primes", J. Cryptogr. Eng. 2015). Every register a template may
//! take is in use, so p3 has none to sit in for a `mulx`.

use core::arch::asm;

/// a b / 2^256 mod p for a, b < p, by the portable field's CIOS with P-256's
/// reduction step (`montgomery`'s `Form::P256`): a in registers, b0 in RDX
/// and b's other words read where they lie.
///
/// Row i adds a b_i to the running words, carrying into a sixth: low halves
/// on CF through `adcx`, high halves on OF through `adox`. Each row's
/// reduction step leaves the low word zero and drops it, so the six words
/// rotate one register on. The sum is below 2p, and p is subtracted where
/// the subtraction does not borrow.
///
/// # Safety
/// The CPU has BMI2 and ADX.
#[inline(always)]
pub(super) unsafe fn mul(a: &[u64; 4], b: &[u64; 4]) -> [u64; 4] {
    let [mut r0, mut r1, mut r2, mut r3] = *a;
    // SAFETY: the template reads b's words 1 to 3, writes only its declared
    // registers and leaves the stack alone.
    unsafe {
        asm!(
            // Row 0 into (A B C D E), F = 0.
            "mulx {B}, {A}, {a0}",
            "mulx {C}, {lo}, {a1}",
            "add {B}, {lo}",
            "mulx {D}, {lo}, {a2}",
            "adc {C}, {lo}",
            "mulx {E}, {lo}, {a3}",
            "adc {D}, {lo}",
            "adc {E}, 0",
            "xor {F:e}, {F:e}",
            // Reduction: q = A; q p3 into (A, rdx); the sum moves to (B C D E F).
            "mov {lo}, {A}",
            "shl {lo}, 32",
            "mov {hi}, {A}",
            "shr {hi}, 32",
            "mov rdx, {A}",
            "sub {A}, {lo}",
            "sbb rdx, {hi}",
            "add {B}, {lo}",
            "adc {C}, {hi}",
            "adc {D}, {A}",
            "adc {E}, rdx",
            "adc {F}, 0",
            // Row 1 into (B C D E F), A = 0.
            "mov rdx, [{b} + 8]",
            "xor {A:e}, {A:e}",
            "mulx {hi}, {lo}, {a0}",
            "adcx {B}, {lo}",
            "adox {C}, {hi}",
            "mulx {hi}, {lo}, {a1}",
            "adcx {C}, {lo}",
            "adox {D}, {hi}",
            "mulx {hi}, {lo}, {a2}",
            "adcx {D}, {lo}",
            "adox {E}, {hi}",
            "mulx {hi}, {lo}, {a3}",
            "adcx {E}, {lo}",
            "adox {F}, {hi}",
            "adcx {F}, {A}",
            "adox {A}, {A}",
            "adc {A}, 0",
            // Reduction: q = B; the sum moves to (C D E F A).
            "mov {lo}, {B}",
            "shl {lo}, 32",
            "mov {hi}, {B}",
            "shr {hi}, 32",
            "mov rdx, {B}",
            "sub {B}, {lo}",
            "sbb rdx, {hi}",
            "add {C}, {lo}",
            "adc {D}, {hi}",
            "adc {E}, {B}",
            "adc {F}, rdx",
            "adc {A}, 0",
            // Row 2 into (C D E F A), B = 0.
            "mov rdx, [{b} + 16]",
            "xor {B:e}, {B:e}",
            "mulx {hi}, {lo}, {a0}",
            "adcx {C}, {lo}",
            "adox {D}, {hi}",
            "mulx {hi}, {lo}, {a1}",
            "adcx {D}, {lo}",
            "adox {E}, {hi}",
            "mulx {hi}, {lo}, {a2}",
            "adcx {E}, {lo}",
            "adox {F}, {hi}",
            "mulx {hi}, {lo}, {a3}",
            "adcx {F}, {lo}",
            "adox {A}, {hi}",
            "adcx {A}, {B}",
            "adox {B}, {B}",
            "adc {B}, 0",
            // Reduction: q = C; the sum moves to (D E F A B).
            "mov {lo}, {C}",
            "shl {lo}, 32",
            "mov {hi}, {C}",
            "shr {hi}, 32",
            "mov rdx, {C}",
            "sub {C}, {lo}",
            "sbb rdx, {hi}",
            "add {D}, {lo}",
            "adc {E}, {hi}",
            "adc {F}, {C}",
            "adc {A}, rdx",
            "adc {B}, 0",
            // Row 3 into (D E F A B), C = 0.
            "mov rdx, [{b} + 24]",
            "xor {C:e}, {C:e}",
            "mulx {hi}, {lo}, {a0}",
            "adcx {D}, {lo}",
            "adox {E}, {hi}",
            "mulx {hi}, {lo}, {a1}",
            "adcx {E}, {lo}",
            "adox {F}, {hi}",
            "mulx {hi}, {lo}, {a2}",
            "adcx {F}, {lo}",
            "adox {A}, {hi}",
            "mulx {hi}, {lo}, {a3}",
            "adcx {A}, {lo}",
            "adox {B}, {hi}",
            "adcx {B}, {C}",
            "adox {C}, {C}",
            "adc {C}, 0",
            // Reduction: q = D; the sum moves to (E F A B C).
            "mov {lo}, {D}",
            "shl {lo}, 32",
            "mov {hi}, {D}",
            "shr {hi}, 32",
            "mov rdx, {D}",
            "sub {D}, {lo}",
            "sbb rdx, {hi}",
            "add {E}, {lo}",
            "adc {F}, {hi}",
            "adc {A}, {D}",
            "adc {B}, rdx",
            "adc {C}, 0",
            // (E F A B C) - p into a's registers; a borrow means the sum
            // was below p, and it stands.
            "mov {a0}, {E}",
            "mov {a1}, {F}",
            "mov {a2}, {A}",
            "mov {a3}, {B}",
            "mov {lo:e}, 0xffffffff",
            "mov {hi}, 0xffffffff00000001",
            "sub {a0}, -1",
            "sbb {a1}, {lo}",
            "sbb {a2}, 0",
            "sbb {a3}, {hi}",
            "sbb {C}, 0",
            "cmovc {a0}, {E}",
            "cmovc {a1}, {F}",
            "cmovc {a2}, {A}",
            "cmovc {a3}, {B}",
            a0 = inout(reg) r0,
            a1 = inout(reg) r1,
            a2 = inout(reg) r2,
            a3 = inout(reg) r3,
            b = in(reg) b.as_ptr(),
            A = out(reg) _,
            B = out(reg) _,
            C = out(reg) _,
            D = out(reg) _,
            E = out(reg) _,
            F = out(reg) _,
            lo = out(reg) _,
            hi = out(reg) _,
            inout("rdx") b[0] => _,
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
#[inline(always)]
pub(super) unsafe fn square(a: &[u64; 4]) -> [u64; 4] {
    let [a0, a1, a2, a3] = *a;
    let (r0, r1, r2, r3): (u64, u64, u64, u64);
    // SAFETY: the template writes only its declared registers and leaves
    // memory and the stack alone.
    unsafe {
        asm!(
            // Cross products into (T1 .. T6).
            "mov rdx, {a0}",
            "mulx {T2}, {T1}, {a1}",
            "mulx {T3}, {lo}, {a2}",
            "add {T2}, {lo}",
            "mulx {T4}, {lo}, {a3}",
            "adc {T3}, {lo}",
            "adc {T4}, 0",
            "mov rdx, {a1}",
            "mulx {hi}, {lo}, {a2}",
            "add {T3}, {lo}",
            "adc {T4}, {hi}",
            "mulx {T5}, {lo}, {a3}",
            "adc {T5}, 0",
            "add {T4}, {lo}",
            "adc {T5}, 0",
            "mov rdx, {a2}",
            "mulx {T6}, {lo}, {a3}",
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
            // The squares added into (T0 .. T7), T0 in a0's register.
            "mov rdx, {a0}",
            "mulx {hi}, {a0}, rdx",
            "add {T1}, {hi}",
            "mov rdx, {a1}",
            "mulx {hi}, {lo}, rdx",
            "adc {T2}, {lo}",
            "adc {T3}, {hi}",
            "mov rdx, {a2}",
            "mulx {hi}, {lo}, rdx",
            "adc {T4}, {lo}",
            "adc {T5}, {hi}",
            "mov rdx, {a3}",
            "mulx {hi}, {lo}, rdx",
            "adc {T6}, {lo}",
            "adc {T7}, {hi}",
            // Reduction steps; T0 then gathers the carry out of T7.
            "mov {lo}, {a0}",
            "shl {lo}, 32",
            "mov {hi}, {a0}",
            "shr {hi}, 32",
            "mov rdx, {a0}",
            "sub {a0}, {lo}",
            "sbb rdx, {hi}",
            "add {T1}, {lo}",
            "adc {T2}, {hi}",
            "adc {T3}, {a0}",
            "adc {T4}, rdx",
            "adc {T5}, 0",
            "adc {T6}, 0",
            "adc {T7}, 0",
            "mov {a0:e}, 0",
            "adc {a0}, 0",
            "mov {lo}, {T1}",
            "shl {lo}, 32",
            "mov {hi}, {T1}",
            "shr {hi}, 32",
            "mov rdx, {T1}",
            "sub {T1}, {lo}",
            "sbb rdx, {hi}",
            "add {T2}, {lo}",
            "adc {T3}, {hi}",
            "adc {T4}, {T1}",
            "adc {T5}, rdx",
            "adc {T6}, 0",
            "adc {T7}, 0",
            "adc {a0}, 0",
            "mov {lo}, {T2}",
            "shl {lo}, 32",
            "mov {hi}, {T2}",
            "shr {hi}, 32",
            "mov rdx, {T2}",
            "sub {T2}, {lo}",
            "sbb rdx, {hi}",
            "add {T3}, {lo}",
            "adc {T4}, {hi}",
            "adc {T5}, {T2}",
            "adc {T6}, rdx",
            "adc {T7}, 0",
            "adc {a0}, 0",
            "mov {lo}, {T3}",
            "shl {lo}, 32",
            "mov {hi}, {T3}",
            "shr {hi}, 32",
            "mov rdx, {T3}",
            "sub {T3}, {lo}",
            "sbb rdx, {hi}",
            "add {T4}, {lo}",
            "adc {T5}, {hi}",
            "adc {T6}, {T3}",
            "adc {T7}, rdx",
            "adc {a0}, 0",
            // (T4 .. T7 a0) - p, as in `mul`, into a1, a2, a3 and rdx.
            "mov {a1}, {T4}",
            "mov {a2}, {T5}",
            "mov {a3}, {T6}",
            "mov rdx, {T7}",
            "mov {lo:e}, 0xffffffff",
            "mov {hi}, 0xffffffff00000001",
            "sub {a1}, -1",
            "sbb {a2}, {lo}",
            "sbb {a3}, 0",
            "sbb rdx, {hi}",
            "sbb {a0}, 0",
            "cmovnc {T4}, {a1}",
            "cmovnc {T5}, {a2}",
            "cmovnc {T6}, {a3}",
            "cmovnc {T7}, rdx",
            a0 = inout(reg) a0 => _,
            a1 = inout(reg) a1 => _,
            a2 = inout(reg) a2 => _,
            a3 = inout(reg) a3 => _,
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
            options(pure, nomem, nostack),
        );
    }
    [r0, r1, r2, r3]
}

/// (a + b) mod p for a, b < p: the five-word sum, then p subtracted where
/// that does not borrow.
#[inline(always)]
pub(super) fn add(a: &[u64; 4], b: &[u64; 4]) -> [u64; 4] {
    let [mut r0, mut r1, mut r2, mut r3] = *a;
    let [b0, b1, b2, b3] = *b;
    // SAFETY: the template writes only its declared registers and leaves
    // memory and the stack alone.
    unsafe {
        asm!(
            "xor {c:e}, {c:e}",
            "add {r0}, {b0}",
            "adc {r1}, {b1}",
            "adc {r2}, {b2}",
            "adc {r3}, {b3}",
            "adc {c}, 0",
            "mov {b0}, {r0}",
            "mov {b1}, {r1}",
            "mov {b2}, {r2}",
            "mov {b3}, {r3}",
            "mov {p1:e}, 0xffffffff",
            "mov {p3}, 0xffffffff00000001",
            "sub {b0}, -1",
            "sbb {b1}, {p1}",
            "sbb {b2}, 0",
            "sbb {b3}, {p3}",
            "sbb {c}, 0",
            "cmovnc {r0}, {b0}",
            "cmovnc {r1}, {b1}",
            "cmovnc {r2}, {b2}",
            "cmovnc {r3}, {b3}",
            r0 = inout(reg) r0,
            r1 = inout(reg) r1,
            r2 = inout(reg) r2,
            r3 = inout(reg) r3,
            b0 = inout(reg) b0 => _,
            b1 = inout(reg) b1 => _,
            b2 = inout(reg) b2 => _,
            b3 = inout(reg) b3 => _,
            c = out(reg) _,
            p1 = out(reg) _,
            p3 = out(reg) _,
            options(pure, nomem, nostack),
        );
    }
    [r0, r1, r2, r3]
}

/// 2a mod p for a < p, as [`add`].
#[inline(always)]
pub(super) fn double(a: &[u64; 4]) -> [u64; 4] {
    let [mut r0, mut r1, mut r2, mut r3] = *a;
    // SAFETY: the template writes only its declared registers and leaves
    // memory and the stack alone.
    unsafe {
        asm!(
            "xor {c:e}, {c:e}",
            "add {r0}, {r0}",
            "adc {r1}, {r1}",
            "adc {r2}, {r2}",
            "adc {r3}, {r3}",
            "adc {c}, 0",
            "mov {t0}, {r0}",
            "mov {t1}, {r1}",
            "mov {t2}, {r2}",
            "mov {t3}, {r3}",
            "mov {p1:e}, 0xffffffff",
            "mov {p3}, 0xffffffff00000001",
            "sub {t0}, -1",
            "sbb {t1}, {p1}",
            "sbb {t2}, 0",
            "sbb {t3}, {p3}",
            "sbb {c}, 0",
            "cmovnc {r0}, {t0}",
            "cmovnc {r1}, {t1}",
            "cmovnc {r2}, {t2}",
            "cmovnc {r3}, {t3}",
            r0 = inout(reg) r0,
            r1 = inout(reg) r1,
            r2 = inout(reg) r2,
            r3 = inout(reg) r3,
            t0 = out(reg) _,
            t1 = out(reg) _,
            t2 = out(reg) _,
            t3 = out(reg) _,
            c = out(reg) _,
            p1 = out(reg) _,
            p3 = out(reg) _,
            options(pure, nomem, nostack),
        );
    }
    [r0, r1, r2, r3]
}

/// (a - b) mod p for a, b < p: the difference, then p added under the mask
/// of its borrow. p's words under a mask m are m, m >> 32, 0 and
/// (m << 32) - m.
#[inline(always)]
pub(super) fn sub(a: &[u64; 4], b: &[u64; 4]) -> [u64; 4] {
    let [mut r0, mut r1, mut r2, mut r3] = *a;
    let [b0, b1, b2, b3] = *b;
    // SAFETY: the template writes only its declared registers and leaves
    // memory and the stack alone.
    unsafe {
        asm!(
            "sub {r0}, {b0}",
            "sbb {r1}, {b1}",
            "sbb {r2}, {b2}",
            "sbb {r3}, {b3}",
            "sbb {b0}, {b0}",
            "mov {b1}, {b0}",
            "shr {b1}, 32",
            "mov {b3}, {b0}",
            "shl {b3}, 32",
            "sub {b3}, {b0}",
            "add {r0}, {b0}",
            "adc {r1}, {b1}",
            "adc {r2}, 0",
            "adc {r3}, {b3}",
            r0 = inout(reg) r0,
            r1 = inout(reg) r1,
            r2 = inout(reg) r2,
            r3 = inout(reg) r3,
            b0 = inout(reg) b0 => _,
            b1 = inout(reg) b1 => _,
            b2 = in(reg) b2,
            b3 = inout(reg) b3 => _,
            options(pure, nomem, nostack),
        );
    }
    [r0, r1, r2, r3]
}
