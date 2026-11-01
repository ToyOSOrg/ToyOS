//! Montgomery multiplication for RSA moduli on BMI2 and ADX: `mulx` with two
//! carry chains, CF through `adcx` and OF through `adox`, so each product
//! row's low and high halves add without waiting on one flag (Intel SDM
//! Vol. 2A, ADCX, ADOX and MULX). The data is public, as all RSA data here.

use core::arch::asm;

/// For a, b < m and n a multiple of four up to 64: out and the returned top
/// word are a b / 2^(64n) mod m plus at most one m, below 2m and unreduced.
/// For each word of b, one product row and one reduction row, each four
/// words of a or m per loop step; `t` holds the n + 2 running words.
///
/// # Safety
/// The CPU has BMI2 and ADX; n is a multiple of four up to 64, and `a`, `b`,
/// `m` and `out` hold at least n words.
#[target_feature(enable = "bmi2,adx")]
pub(super) unsafe fn mul(a: &[u64], b: &[u64], m: &[u64], n0: u64, n: usize, out: &mut [u64]) -> u64 {
    let mut t = [0u64; 66];
    for &bi in &b[..n] {
        // SAFETY: the rows read n words of `a` and `m` and write n + 2 of `t`;
        // they write no other memory and leave the stack alone.
        unsafe { row(a.as_ptr(), bi, m.as_ptr(), n0, n, t.as_mut_ptr()) };
    }
    out[..n].copy_from_slice(&t[..n]);
    t[n]
}

/// t = (t + a bi + q m) / 2^64 for q = (t + a bi)_0 n0 mod 2^64.
#[target_feature(enable = "bmi2,adx")]
unsafe fn row(a: *const u64, bi: u64, m: *const u64, n0: u64, n: usize, t: *mut u64) {
    unsafe {
        asm!(
            // Product row: t[j] += lo(a_j bi) on CF and hi(a_{j-1} bi) on OF.
            "mov {a_ptr}, {a}",
            "mov {t_ptr}, {t}",
            "mov rcx, {quads}",
            "xor {hi:e}, {hi:e}",
            "2:",
            "mulx {h0}, {lo}, [{a_ptr}]",
            "adcx {lo}, [{t_ptr}]",
            "adox {lo}, {hi}",
            "mov [{t_ptr}], {lo}",
            "mulx {hi}, {lo}, [{a_ptr} + 8]",
            "adcx {lo}, [{t_ptr} + 8]",
            "adox {lo}, {h0}",
            "mov [{t_ptr} + 8], {lo}",
            "mulx {h0}, {lo}, [{a_ptr} + 16]",
            "adcx {lo}, [{t_ptr} + 16]",
            "adox {lo}, {hi}",
            "mov [{t_ptr} + 16], {lo}",
            "mulx {hi}, {lo}, [{a_ptr} + 24]",
            "adcx {lo}, [{t_ptr} + 24]",
            "adox {lo}, {h0}",
            "mov [{t_ptr} + 24], {lo}",
            "lea {a_ptr}, [{a_ptr} + 32]",
            "lea {t_ptr}, [{t_ptr} + 32]",
            "lea rcx, [rcx - 1]",
            "jrcxz 3f",
            "jmp 2b",
            "3:",
            // t[n] += hi + CF + OF; t[n + 1] = the carries out of that.
            "mov {lo}, 0",
            "mov {h0}, [{t_ptr}]",
            "adcx {h0}, {lo}",
            "adox {h0}, {hi}",
            "mov [{t_ptr}], {h0}",
            "mov {h0}, 0",
            "adcx {h0}, {lo}",
            "adox {h0}, {lo}",
            "mov [{t_ptr} + 8], {h0}",
            // Reduction row: q = t_0 n0; t[j - 1] = t[j] + lo(q m_j) + hi(q m_{j-1}).
            "mov rdx, [{t}]",
            "imul rdx, {n0}",
            "mov {a_ptr}, {m}",
            "mov {t_ptr}, {t}",
            "mov rcx, {quads}",
            "xor {hi:e}, {hi:e}",
            // Word 0 of the sum is zero by the choice of q; only its carries
            // remain, so it is computed and not stored.
            "mulx {hi}, {lo}, [{a_ptr}]",
            "adcx {lo}, [{t_ptr}]",
            "mulx {h0}, {lo}, [{a_ptr} + 8]",
            "adcx {lo}, [{t_ptr} + 8]",
            "adox {lo}, {hi}",
            "mov [{t_ptr}], {lo}",
            "mulx {hi}, {lo}, [{a_ptr} + 16]",
            "adcx {lo}, [{t_ptr} + 16]",
            "adox {lo}, {h0}",
            "mov [{t_ptr} + 8], {lo}",
            "mulx {h0}, {lo}, [{a_ptr} + 24]",
            "adcx {lo}, [{t_ptr} + 24]",
            "adox {lo}, {hi}",
            "mov [{t_ptr} + 16], {lo}",
            "lea {a_ptr}, [{a_ptr} + 32]",
            "lea {t_ptr}, [{t_ptr} + 32]",
            "lea rcx, [rcx - 1]",
            "jrcxz 5f",
            "4:",
            "mulx {hi}, {lo}, [{a_ptr}]",
            "adcx {lo}, [{t_ptr}]",
            "adox {lo}, {h0}",
            "mov [{t_ptr} - 8], {lo}",
            "mulx {h0}, {lo}, [{a_ptr} + 8]",
            "adcx {lo}, [{t_ptr} + 8]",
            "adox {lo}, {hi}",
            "mov [{t_ptr}], {lo}",
            "mulx {hi}, {lo}, [{a_ptr} + 16]",
            "adcx {lo}, [{t_ptr} + 16]",
            "adox {lo}, {h0}",
            "mov [{t_ptr} + 8], {lo}",
            "mulx {h0}, {lo}, [{a_ptr} + 24]",
            "adcx {lo}, [{t_ptr} + 24]",
            "adox {lo}, {hi}",
            "mov [{t_ptr} + 16], {lo}",
            "lea {a_ptr}, [{a_ptr} + 32]",
            "lea {t_ptr}, [{t_ptr} + 32]",
            "lea rcx, [rcx - 1]",
            "jrcxz 5f",
            "jmp 4b",
            "5:",
            // t[n - 1] = t[n] + hi + CF + OF; t[n] = t[n + 1] + the carries.
            "mov {lo}, [{t_ptr}]",
            "adcx {lo}, {zero}",
            "adox {lo}, {h0}",
            "mov [{t_ptr} - 8], {lo}",
            "mov {lo}, [{t_ptr} + 8]",
            "adcx {lo}, {zero}",
            "adox {lo}, {zero}",
            "mov [{t_ptr}], {lo}",
            a = in(reg) a,
            m = in(reg) m,
            t = in(reg) t,
            n0 = in(reg) n0,
            quads = in(reg) n / 4,
            zero = in(reg) 0u64,
            a_ptr = out(reg) _,
            t_ptr = out(reg) _,
            lo = out(reg) _,
            hi = out(reg) _,
            h0 = out(reg) _,
            inout("rdx") bi => _,
            out("rcx") _,
            options(nostack),
        );
    }
}
