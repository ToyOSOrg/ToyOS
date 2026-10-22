//! M2 probe kernels: one multiplication each for 2^255-19 in 4x64, P-256
//! Montgomery and 2048-bit Montgomery, as (a) u128, (b) mulx + addcarry
//! intrinsics, (c) asm! with dual adcx/adox carry chains. Exploratory only;
//! never lands.
#![allow(unused_unsafe)]

use core::arch::x86_64::{_addcarry_u64, _mulx_u64};

#[inline(always)]
fn mac(t: u64, a: u64, b: u64, c: u64) -> (u64, u64) {
    let v = u128::from(a)
        .wrapping_mul(u128::from(b))
        .wrapping_add(u128::from(t))
        .wrapping_add(u128::from(c));
    (v as u64, (v >> 64) as u64)
}

// ---- 2^255 - 19 in four saturated limbs, result in [0, 2^256) ----

#[inline(always)]
pub fn f25519_mul_u128(a: &[u64; 4], b: &[u64; 4]) -> [u64; 4] {
    let mut t = [0u64; 8];
    for i in 0..4 {
        let mut c = 0;
        for j in 0..4 {
            (t[i + j], c) = mac(t[i + j], a[i], b[j], c);
        }
        t[i + 4] = c;
    }
    let mut r = [0u64; 4];
    let mut c = 0;
    for i in 0..4 {
        (r[i], c) = mac(t[i], t[i + 4], 38, c);
    }
    let (r0, carry) = mac(r[0], c, 38, 0);
    r[0] = r0;
    let mut carry = carry;
    for i in 1..4 {
        let (s, o) = r[i].overflowing_add(carry);
        r[i] = s;
        carry = o as u64;
    }
    r[0] = r[0].wrapping_add(carry.wrapping_mul(38));
    r
}

#[target_feature(enable = "bmi2,adx")]
#[inline]
pub unsafe fn f25519_mul_mulx(a: &[u64; 4], b: &[u64; 4]) -> [u64; 4] {
    unsafe {
        let mut t = [0u64; 8];
        for i in 0..4 {
            let mut lo = [0u64; 4];
            let mut hi = [0u64; 4];
            for j in 0..4 {
                lo[j] = _mulx_u64(a[j], b[i], &mut hi[j]);
            }
            let mut c = 0u8;
            for j in 0..4 {
                c = _addcarry_u64(c, t[i + j], lo[j], &mut t[i + j]);
            }
            let mut top = 0;
            _addcarry_u64(c, 0, 0, &mut top);
            t[i + 4] = top;
            let mut c = 0u8;
            for j in 0..4 {
                c = _addcarry_u64(c, t[i + j + 1], hi[j], &mut t[i + j + 1]);
            }
        }
        let mut lo = [0u64; 4];
        let mut hi = [0u64; 4];
        for j in 0..4 {
            lo[j] = _mulx_u64(t[j + 4], 38, &mut hi[j]);
        }
        let mut r = [0u64; 4];
        let mut c = 0u8;
        for j in 0..4 {
            c = _addcarry_u64(c, t[j], lo[j], &mut r[j]);
        }
        let mut top = hi[3];
        _addcarry_u64(c, top, 0, &mut top);
        let mut c = 0u8;
        for j in 1..4 {
            c = _addcarry_u64(c, r[j], hi[j - 1], &mut r[j]);
        }
        _addcarry_u64(c, top, 0, &mut top);
        let mut c = _addcarry_u64(0, r[0], top.wrapping_mul(38), &mut r[0]);
        for j in 1..4 {
            c = _addcarry_u64(c, r[j], 0, &mut r[j]);
        }
        r[0] = r[0].wrapping_add(0u64.wrapping_sub(u64::from(c)) & 38);
        r
    }
}

/// Dual-chain row accumulation; the fold by 38 on two chains as well.
#[inline(always)]
pub unsafe fn f25519_mul_asm(a: &[u64; 4], b: &[u64; 4]) -> [u64; 4] {
    let mut r = *a;
    unsafe {
        core::arch::asm!(
            // row 0: b0 * a -> t0..t4 (r8..r12)
            "mov rdx, [{b}]",
            "mulx r9, r8, [{a}]",
            "mulx r10, rax, [{a} + 8]",
            "add r9, rax",
            "mulx r11, rax, [{a} + 16]",
            "adc r10, rax",
            "mulx r12, rax, [{a} + 24]",
            "adc r11, rax",
            "adc r12, 0",
            // row 1: t1..t5 (r9..r13)
            "mov rdx, [{b} + 8]",
            "xor r13d, r13d",
            "mulx rcx, rax, [{a}]",
            "adcx r9, rax",
            "adox r10, rcx",
            "mulx rcx, rax, [{a} + 8]",
            "adcx r10, rax",
            "adox r11, rcx",
            "mulx rcx, rax, [{a} + 16]",
            "adcx r11, rax",
            "adox r12, rcx",
            "mulx rcx, rax, [{a} + 24]",
            "adcx r12, rax",
            "adox r13, rcx",
            "mov rax, 0",
            "adcx r13, rax",
            // row 2: t2..t6 (r10..r14)
            "mov rdx, [{b} + 16]",
            "xor r14d, r14d",
            "mulx rcx, rax, [{a}]",
            "adcx r10, rax",
            "adox r11, rcx",
            "mulx rcx, rax, [{a} + 8]",
            "adcx r11, rax",
            "adox r12, rcx",
            "mulx rcx, rax, [{a} + 16]",
            "adcx r12, rax",
            "adox r13, rcx",
            "mulx rcx, rax, [{a} + 24]",
            "adcx r13, rax",
            "adox r14, rcx",
            "mov rax, 0",
            "adcx r14, rax",
            // row 3: t3..t7 (r11..r15)
            "mov rdx, [{b} + 24]",
            "xor r15d, r15d",
            "mulx rcx, rax, [{a}]",
            "adcx r11, rax",
            "adox r12, rcx",
            "mulx rcx, rax, [{a} + 8]",
            "adcx r12, rax",
            "adox r13, rcx",
            "mulx rcx, rax, [{a} + 16]",
            "adcx r13, rax",
            "adox r14, rcx",
            "mulx rcx, rax, [{a} + 24]",
            "adcx r14, rax",
            "adox r15, rcx",
            "mov rax, 0",
            "adcx r15, rax",
            // fold: t0..t3 += 38 * t4..t7, lows on CF, highs on OF
            "mov edx, 38",
            "xor eax, eax",
            "mulx rcx, rax, r12",
            "adcx r8, rax",
            "adox r9, rcx",
            "mulx rcx, rax, r13",
            "adcx r9, rax",
            "adox r10, rcx",
            "mulx rcx, rax, r14",
            "adcx r10, rax",
            "adox r11, rcx",
            "mulx r12, rax, r15",
            "adcx r11, rax",
            "mov eax, 0",
            "adox r12, rax",
            "adcx r12, rax",
            // second fold of the top word, then a carry of at most one
            "imul r12, r12, 38",
            "add r8, r12",
            "adc r9, 0",
            "adc r10, 0",
            "adc r11, 0",
            "sbb rax, rax",
            "and eax, 38",
            "add r8, rax",
            "mov [{a}], r8",
            "mov [{a} + 8], r9",
            "mov [{a} + 16], r10",
            "mov [{a} + 24], r11",
            a = in(reg) r.as_mut_ptr(),
            b = in(reg) b.as_ptr(),
            out("rax") _, out("rcx") _, out("rdx") _,
            out("r8") _, out("r9") _, out("r10") _, out("r11") _,
            out("r12") _, out("r13") _, out("r14") _, out("r15") _,
            options(nostack),
        );
    }
    r
}

// ---- P-256 Montgomery, R = 2^256 ----

pub const P256: [u64; 4] = [0xffffffffffffffff, 0x00000000ffffffff, 0, 0xffffffff00000001];

/// Generic CIOS, as `montgomery::Residue::mul` does it.
#[inline(always)]
pub fn p256_mul_u128(a: &[u64; 4], b: &[u64; 4]) -> [u64; 4] {
    let mut t = [0u64; 4];
    let mut high = 0u64;
    for i in 0..4 {
        let mut c = 0;
        for j in 0..4 {
            (t[j], c) = mac(t[j], a[i], b[j], c);
        }
        let (top, overflow) = high.overflowing_add(c);
        // p = -1 mod 2^64, so the quotient digit is t0.
        let q = t[0];
        let (_, mut c) = mac(t[0], q, P256[0], 0);
        for j in 1..4 {
            (t[j - 1], c) = mac(t[j], q, P256[j], c);
        }
        let (top, carry) = top.overflowing_add(c);
        t[3] = top;
        high = u64::from(overflow) | u64::from(carry);
    }
    sub_p256(t, high)
}

#[inline(always)]
fn sub_p256(t: [u64; 4], high: u64) -> [u64; 4] {
    let mut d = [0u64; 4];
    let mut borrow = false;
    for i in 0..4 {
        let (x, b1) = t[i].overflowing_sub(P256[i]);
        let (x, b2) = x.overflowing_sub(u64::from(borrow));
        d[i] = x;
        borrow = b1 | b2;
    }
    let keep = 0u64.wrapping_sub(u64::from(borrow) & (1 ^ (high & 1)));
    core::array::from_fn(|i| (t[i] & keep) | (d[i] & !keep))
}

/// The special form: q * p = q 2^256 - q 2^224 + q 2^192 + q 2^96 - q, with
/// the multiplication by the top limb by mulx.
#[target_feature(enable = "bmi2,adx")]
#[inline]
pub unsafe fn p256_mul_mulx(a: &[u64; 4], b: &[u64; 4]) -> [u64; 4] {
    unsafe {
        let mut t = [0u64; 6];
        for i in 0..4 {
            let mut lo = [0u64; 4];
            let mut hi = [0u64; 4];
            for j in 0..4 {
                lo[j] = _mulx_u64(b[j], a[i], &mut hi[j]);
            }
            let mut c = 0u8;
            for j in 0..4 {
                c = _addcarry_u64(c, t[j], lo[j], &mut t[j]);
            }
            c = _addcarry_u64(c, t[4], 0, &mut t[4]);
            t[5] = u64::from(c);
            let mut c = 0u8;
            for j in 0..4 {
                c = _addcarry_u64(c, t[j + 1], hi[j], &mut t[j + 1]);
            }
            _addcarry_u64(c, t[5], 0, &mut t[5]);
            // Reduction step: t += q * p, then shift by one word. With
            // q = t0: q * p = q (2^64 - 1) + q 2^96 - ... formed as
            // q*p[1] = q * (2^32 - 1) at word 1 and q * p[3] at word 3.
            let q = t[0];
            let mut h1 = 0;
            let l1 = _mulx_u64(q, P256[1], &mut h1);
            let mut h3 = 0;
            let l3 = _mulx_u64(q, P256[3], &mut h3);
            // q * p[0] = q * 2^64 - q: t0 + q*p0 = q*2^64, low word 0, carry q.
            let mut c = _addcarry_u64(0, t[1], q, &mut t[1]);
            let mut c2 = _addcarry_u64(0, t[1], l1, &mut t[1]);
            c = _addcarry_u64(c, t[2], h1, &mut t[2]);
            c2 = _addcarry_u64(c2, t[2], 0, &mut t[2]);
            c = _addcarry_u64(c, t[3], l3, &mut t[3]);
            c2 = _addcarry_u64(c2, t[3], 0, &mut t[3]);
            c = _addcarry_u64(c, t[4], h3, &mut t[4]);
            c2 = _addcarry_u64(c2, t[4], 0, &mut t[4]);
            let mut top = t[5];
            _addcarry_u64(c, top, u64::from(c2), &mut top);
            t = [t[1], t[2], t[3], t[4], top, 0];
        }
        sub_p256([t[0], t[1], t[2], t[3]], t[4])
    }
}

/// Dual chains for the product row; the special reduction on a single chain.
#[inline(always)]
pub unsafe fn p256_mul_asm(a: &[u64; 4], b: &[u64; 4]) -> [u64; 4] {
    let mut r = *a;
    unsafe {
        core::arch::asm!(
            "xor r8d, r8d",
            "xor r9d, r9d",
            "xor r10d, r10d",
            "xor r11d, r11d",
            "xor r12d, r12d",
            "mov rdx, [{b} + 0]",
            "xor r13d, r13d",
            "mulx rcx, rax, [{a} + 0]",
            "adcx r8, rax",
            "adox r9, rcx",
            "mulx rcx, rax, [{a} + 8]",
            "adcx r9, rax",
            "adox r10, rcx",
            "mulx rcx, rax, [{a} + 16]",
            "adcx r10, rax",
            "adox r11, rcx",
            "mulx rcx, rax, [{a} + 24]",
            "adcx r11, rax",
            "adox r12, rcx",
            "mov eax, 0",
            "adcx r12, rax",
            "adox r13, rax",
            "adcx r13, rax",
            "mov rdx, r8",
            "mov r14, r8",
            "shl r14, 32",
            "mov r15, r8",
            "shr r15, 32",
            "mov rax, 0xffffffff00000001",
            "mulx rcx, rax, rax",
            "add r9, r14",
            "adc r10, r15",
            "adc r11, rax",
            "adc r12, rcx",
            "adc r13, 0",
            "mov rdx, [{b} + 8]",
            "xor r8d, r8d",
            "mulx rcx, rax, [{a} + 0]",
            "adcx r9, rax",
            "adox r10, rcx",
            "mulx rcx, rax, [{a} + 8]",
            "adcx r10, rax",
            "adox r11, rcx",
            "mulx rcx, rax, [{a} + 16]",
            "adcx r11, rax",
            "adox r12, rcx",
            "mulx rcx, rax, [{a} + 24]",
            "adcx r12, rax",
            "adox r13, rcx",
            "mov eax, 0",
            "adcx r13, rax",
            "adox r8, rax",
            "adcx r8, rax",
            "mov rdx, r9",
            "mov r14, r9",
            "shl r14, 32",
            "mov r15, r9",
            "shr r15, 32",
            "mov rax, 0xffffffff00000001",
            "mulx rcx, rax, rax",
            "add r10, r14",
            "adc r11, r15",
            "adc r12, rax",
            "adc r13, rcx",
            "adc r8, 0",
            "mov rdx, [{b} + 16]",
            "xor r9d, r9d",
            "mulx rcx, rax, [{a} + 0]",
            "adcx r10, rax",
            "adox r11, rcx",
            "mulx rcx, rax, [{a} + 8]",
            "adcx r11, rax",
            "adox r12, rcx",
            "mulx rcx, rax, [{a} + 16]",
            "adcx r12, rax",
            "adox r13, rcx",
            "mulx rcx, rax, [{a} + 24]",
            "adcx r13, rax",
            "adox r8, rcx",
            "mov eax, 0",
            "adcx r8, rax",
            "adox r9, rax",
            "adcx r9, rax",
            "mov rdx, r10",
            "mov r14, r10",
            "shl r14, 32",
            "mov r15, r10",
            "shr r15, 32",
            "mov rax, 0xffffffff00000001",
            "mulx rcx, rax, rax",
            "add r11, r14",
            "adc r12, r15",
            "adc r13, rax",
            "adc r8, rcx",
            "adc r9, 0",
            "mov rdx, [{b} + 24]",
            "xor r10d, r10d",
            "mulx rcx, rax, [{a} + 0]",
            "adcx r11, rax",
            "adox r12, rcx",
            "mulx rcx, rax, [{a} + 8]",
            "adcx r12, rax",
            "adox r13, rcx",
            "mulx rcx, rax, [{a} + 16]",
            "adcx r13, rax",
            "adox r8, rcx",
            "mulx rcx, rax, [{a} + 24]",
            "adcx r8, rax",
            "adox r9, rcx",
            "mov eax, 0",
            "adcx r9, rax",
            "adox r10, rax",
            "adcx r10, rax",
            "mov rdx, r11",
            "mov r14, r11",
            "shl r14, 32",
            "mov r15, r11",
            "shr r15, 32",
            "mov rax, 0xffffffff00000001",
            "mulx rcx, rax, rax",
            "add r12, r14",
            "adc r13, r15",
            "adc r8, rax",
            "adc r9, rcx",
            "adc r10, 0",
            "mov rax, r12",
            "mov rcx, r13",
            "mov rdx, r8",
            "mov r14, r9",
            "sub rax, -1",
            "mov r15, 0x00000000ffffffff",
            "sbb rcx, r15",
            "sbb rdx, 0",
            "mov r15, 0xffffffff00000001",
            "sbb r14, r15",
            "sbb r10, 0",
            "cmovc rax, r12",
            "cmovc rcx, r13",
            "cmovc rdx, r8",
            "cmovc r14, r9",
            "mov [{a}], rax",
            "mov [{a} + 8], rcx",
            "mov [{a} + 16], rdx",
            "mov [{a} + 24], r14",

            a = in(reg) r.as_mut_ptr(),
            b = in(reg) b.as_ptr(),
            out("rax") _, out("rcx") _, out("rdx") _,
            out("r8") _, out("r9") _, out("r10") _, out("r11") _,
            out("r12") _, out("r13") _, out("r14") _, out("r15") _,
            options(nostack),
        );
    }
    r
}

// ---- 2048-bit Montgomery (32 words), CIOS ----

pub const W: usize = 32;

#[inline(always)]
fn finish(t: &[u64; W + 2], m: &[u64; W]) -> [u64; W] {
    let mut out = [0u64; W];
    out.copy_from_slice(&t[..W]);
    let mut d = [0u64; W];
    let mut borrow = false;
    for i in 0..W {
        let (x, b1) = out[i].overflowing_sub(m[i]);
        let (x, b2) = x.overflowing_sub(u64::from(borrow));
        d[i] = x;
        borrow = b1 | b2;
    }
    if t[W] != 0 || !borrow { d } else { out }
}

#[inline]
pub fn mont_u128(a: &[u64; W], b: &[u64; W], m: &[u64; W], n0: u64) -> [u64; W] {
    let mut t = [0u64; W + 2];
    for &bi in b {
        let mut c = 0;
        for j in 0..W {
            (t[j], c) = mac(t[j], a[j], bi, c);
        }
        let (s, o) = t[W].overflowing_add(c);
        t[W] = s;
        t[W + 1] = u64::from(o);
        let q = t[0].wrapping_mul(n0);
        let (_, mut c) = mac(t[0], q, m[0], 0);
        for j in 1..W {
            (t[j - 1], c) = mac(t[j], q, m[j], c);
        }
        let (s, o) = t[W].overflowing_add(c);
        t[W - 1] = s;
        t[W] = t[W + 1].wrapping_add(u64::from(o));
    }
    finish(&t, m)
}

#[target_feature(enable = "bmi2,adx")]
#[inline]
pub unsafe fn mont_mulx(a: &[u64; W], b: &[u64; W], m: &[u64; W], n0: u64) -> [u64; W] {
    let mut t = [0u64; W + 2];
    for &bi in b {
        let mut c = 0u8;
        let mut hi_prev = 0u64;
        let mut c2 = 0u8;
        for j in 0..W {
            let mut hi = 0;
            let lo = _mulx_u64(a[j], bi, &mut hi);
            c = _addcarry_u64(c, t[j], lo, &mut t[j]);
            c2 = _addcarry_u64(c2, t[j], hi_prev, &mut t[j]);
            hi_prev = hi;
        }
        let mut top = 0;
        let c3 = _addcarry_u64(c, t[W], hi_prev, &mut top);
        let c4 = _addcarry_u64(c2, top, 0, &mut t[W]);
        t[W + 1] = u64::from(c3).wrapping_add(u64::from(c4));
        let q = t[0].wrapping_mul(n0);
        let mut hi_prev = 0u64;
        let mut c = 0u8;
        let mut c2 = 0u8;
        for j in 0..W {
            let mut hi = 0;
            let lo = _mulx_u64(m[j], q, &mut hi);
            let mut word = 0;
            c = _addcarry_u64(c, t[j], lo, &mut word);
            c2 = _addcarry_u64(c2, word, hi_prev, &mut word);
            if j > 0 {
                t[j - 1] = word;
            }
            hi_prev = hi;
        }
        let mut top = 0;
        let c3 = _addcarry_u64(c, t[W], hi_prev, &mut top);
        let c4 = _addcarry_u64(c2, top, 0, &mut t[W - 1]);
        t[W] = t[W + 1].wrapping_add(u64::from(c3)).wrapping_add(u64::from(c4));
    }
    finish(&t, m)
}

#[inline]
pub unsafe fn mont_asm(a: &[u64; W], b: &[u64; W], m: &[u64; W], n0: u64) -> [u64; W] {
    let mut t = [0u64; W + 2];
    for &bi in b {
        unsafe {
            core::arch::asm!(
                "xor eax, eax",
                "mulx r8, rcx, [{a} + 0]",
                "mov r10, [{t} + 0]",
                "adcx r10, rcx",
                "mov [{t} + 0], r10",
                "mulx r9, rcx, [{a} + 8]",
                "mov r10, [{t} + 8]",
                "adcx r10, rcx",
                "adox r10, r8",
                "mov [{t} + 8], r10",
                "mulx r8, rcx, [{a} + 16]",
                "mov r10, [{t} + 16]",
                "adcx r10, rcx",
                "adox r10, r9",
                "mov [{t} + 16], r10",
                "mulx r9, rcx, [{a} + 24]",
                "mov r10, [{t} + 24]",
                "adcx r10, rcx",
                "adox r10, r8",
                "mov [{t} + 24], r10",
                "mulx r8, rcx, [{a} + 32]",
                "mov r10, [{t} + 32]",
                "adcx r10, rcx",
                "adox r10, r9",
                "mov [{t} + 32], r10",
                "mulx r9, rcx, [{a} + 40]",
                "mov r10, [{t} + 40]",
                "adcx r10, rcx",
                "adox r10, r8",
                "mov [{t} + 40], r10",
                "mulx r8, rcx, [{a} + 48]",
                "mov r10, [{t} + 48]",
                "adcx r10, rcx",
                "adox r10, r9",
                "mov [{t} + 48], r10",
                "mulx r9, rcx, [{a} + 56]",
                "mov r10, [{t} + 56]",
                "adcx r10, rcx",
                "adox r10, r8",
                "mov [{t} + 56], r10",
                "mulx r8, rcx, [{a} + 64]",
                "mov r10, [{t} + 64]",
                "adcx r10, rcx",
                "adox r10, r9",
                "mov [{t} + 64], r10",
                "mulx r9, rcx, [{a} + 72]",
                "mov r10, [{t} + 72]",
                "adcx r10, rcx",
                "adox r10, r8",
                "mov [{t} + 72], r10",
                "mulx r8, rcx, [{a} + 80]",
                "mov r10, [{t} + 80]",
                "adcx r10, rcx",
                "adox r10, r9",
                "mov [{t} + 80], r10",
                "mulx r9, rcx, [{a} + 88]",
                "mov r10, [{t} + 88]",
                "adcx r10, rcx",
                "adox r10, r8",
                "mov [{t} + 88], r10",
                "mulx r8, rcx, [{a} + 96]",
                "mov r10, [{t} + 96]",
                "adcx r10, rcx",
                "adox r10, r9",
                "mov [{t} + 96], r10",
                "mulx r9, rcx, [{a} + 104]",
                "mov r10, [{t} + 104]",
                "adcx r10, rcx",
                "adox r10, r8",
                "mov [{t} + 104], r10",
                "mulx r8, rcx, [{a} + 112]",
                "mov r10, [{t} + 112]",
                "adcx r10, rcx",
                "adox r10, r9",
                "mov [{t} + 112], r10",
                "mulx r9, rcx, [{a} + 120]",
                "mov r10, [{t} + 120]",
                "adcx r10, rcx",
                "adox r10, r8",
                "mov [{t} + 120], r10",
                "mulx r8, rcx, [{a} + 128]",
                "mov r10, [{t} + 128]",
                "adcx r10, rcx",
                "adox r10, r9",
                "mov [{t} + 128], r10",
                "mulx r9, rcx, [{a} + 136]",
                "mov r10, [{t} + 136]",
                "adcx r10, rcx",
                "adox r10, r8",
                "mov [{t} + 136], r10",
                "mulx r8, rcx, [{a} + 144]",
                "mov r10, [{t} + 144]",
                "adcx r10, rcx",
                "adox r10, r9",
                "mov [{t} + 144], r10",
                "mulx r9, rcx, [{a} + 152]",
                "mov r10, [{t} + 152]",
                "adcx r10, rcx",
                "adox r10, r8",
                "mov [{t} + 152], r10",
                "mulx r8, rcx, [{a} + 160]",
                "mov r10, [{t} + 160]",
                "adcx r10, rcx",
                "adox r10, r9",
                "mov [{t} + 160], r10",
                "mulx r9, rcx, [{a} + 168]",
                "mov r10, [{t} + 168]",
                "adcx r10, rcx",
                "adox r10, r8",
                "mov [{t} + 168], r10",
                "mulx r8, rcx, [{a} + 176]",
                "mov r10, [{t} + 176]",
                "adcx r10, rcx",
                "adox r10, r9",
                "mov [{t} + 176], r10",
                "mulx r9, rcx, [{a} + 184]",
                "mov r10, [{t} + 184]",
                "adcx r10, rcx",
                "adox r10, r8",
                "mov [{t} + 184], r10",
                "mulx r8, rcx, [{a} + 192]",
                "mov r10, [{t} + 192]",
                "adcx r10, rcx",
                "adox r10, r9",
                "mov [{t} + 192], r10",
                "mulx r9, rcx, [{a} + 200]",
                "mov r10, [{t} + 200]",
                "adcx r10, rcx",
                "adox r10, r8",
                "mov [{t} + 200], r10",
                "mulx r8, rcx, [{a} + 208]",
                "mov r10, [{t} + 208]",
                "adcx r10, rcx",
                "adox r10, r9",
                "mov [{t} + 208], r10",
                "mulx r9, rcx, [{a} + 216]",
                "mov r10, [{t} + 216]",
                "adcx r10, rcx",
                "adox r10, r8",
                "mov [{t} + 216], r10",
                "mulx r8, rcx, [{a} + 224]",
                "mov r10, [{t} + 224]",
                "adcx r10, rcx",
                "adox r10, r9",
                "mov [{t} + 224], r10",
                "mulx r9, rcx, [{a} + 232]",
                "mov r10, [{t} + 232]",
                "adcx r10, rcx",
                "adox r10, r8",
                "mov [{t} + 232], r10",
                "mulx r8, rcx, [{a} + 240]",
                "mov r10, [{t} + 240]",
                "adcx r10, rcx",
                "adox r10, r9",
                "mov [{t} + 240], r10",
                "mulx r9, rcx, [{a} + 248]",
                "mov r10, [{t} + 248]",
                "adcx r10, rcx",
                "adox r10, r8",
                "mov [{t} + 248], r10",
                "mov r10, [{t} + 256]",
                "mov ecx, 0",
                "adcx r10, rcx",
                "adox r10, r9",
                "mov [{t} + 256], r10",
                "mov r10, 0",
                "adcx r10, rcx",
                "adox r10, rcx",
                "mov [{t} + 264], r10",
                "mov rdx, [{t}]",
                "imul rdx, {n0}",
                "xor eax, eax",
                "mulx r8, rcx, [{m} + 0]",
                "mov r10, [{t} + 0]",
                "adcx r10, rcx",
                "mulx r9, rcx, [{m} + 8]",
                "mov r10, [{t} + 8]",
                "adcx r10, rcx",
                "adox r10, r8",
                "mov [{t} + 0], r10",
                "mulx r8, rcx, [{m} + 16]",
                "mov r10, [{t} + 16]",
                "adcx r10, rcx",
                "adox r10, r9",
                "mov [{t} + 8], r10",
                "mulx r9, rcx, [{m} + 24]",
                "mov r10, [{t} + 24]",
                "adcx r10, rcx",
                "adox r10, r8",
                "mov [{t} + 16], r10",
                "mulx r8, rcx, [{m} + 32]",
                "mov r10, [{t} + 32]",
                "adcx r10, rcx",
                "adox r10, r9",
                "mov [{t} + 24], r10",
                "mulx r9, rcx, [{m} + 40]",
                "mov r10, [{t} + 40]",
                "adcx r10, rcx",
                "adox r10, r8",
                "mov [{t} + 32], r10",
                "mulx r8, rcx, [{m} + 48]",
                "mov r10, [{t} + 48]",
                "adcx r10, rcx",
                "adox r10, r9",
                "mov [{t} + 40], r10",
                "mulx r9, rcx, [{m} + 56]",
                "mov r10, [{t} + 56]",
                "adcx r10, rcx",
                "adox r10, r8",
                "mov [{t} + 48], r10",
                "mulx r8, rcx, [{m} + 64]",
                "mov r10, [{t} + 64]",
                "adcx r10, rcx",
                "adox r10, r9",
                "mov [{t} + 56], r10",
                "mulx r9, rcx, [{m} + 72]",
                "mov r10, [{t} + 72]",
                "adcx r10, rcx",
                "adox r10, r8",
                "mov [{t} + 64], r10",
                "mulx r8, rcx, [{m} + 80]",
                "mov r10, [{t} + 80]",
                "adcx r10, rcx",
                "adox r10, r9",
                "mov [{t} + 72], r10",
                "mulx r9, rcx, [{m} + 88]",
                "mov r10, [{t} + 88]",
                "adcx r10, rcx",
                "adox r10, r8",
                "mov [{t} + 80], r10",
                "mulx r8, rcx, [{m} + 96]",
                "mov r10, [{t} + 96]",
                "adcx r10, rcx",
                "adox r10, r9",
                "mov [{t} + 88], r10",
                "mulx r9, rcx, [{m} + 104]",
                "mov r10, [{t} + 104]",
                "adcx r10, rcx",
                "adox r10, r8",
                "mov [{t} + 96], r10",
                "mulx r8, rcx, [{m} + 112]",
                "mov r10, [{t} + 112]",
                "adcx r10, rcx",
                "adox r10, r9",
                "mov [{t} + 104], r10",
                "mulx r9, rcx, [{m} + 120]",
                "mov r10, [{t} + 120]",
                "adcx r10, rcx",
                "adox r10, r8",
                "mov [{t} + 112], r10",
                "mulx r8, rcx, [{m} + 128]",
                "mov r10, [{t} + 128]",
                "adcx r10, rcx",
                "adox r10, r9",
                "mov [{t} + 120], r10",
                "mulx r9, rcx, [{m} + 136]",
                "mov r10, [{t} + 136]",
                "adcx r10, rcx",
                "adox r10, r8",
                "mov [{t} + 128], r10",
                "mulx r8, rcx, [{m} + 144]",
                "mov r10, [{t} + 144]",
                "adcx r10, rcx",
                "adox r10, r9",
                "mov [{t} + 136], r10",
                "mulx r9, rcx, [{m} + 152]",
                "mov r10, [{t} + 152]",
                "adcx r10, rcx",
                "adox r10, r8",
                "mov [{t} + 144], r10",
                "mulx r8, rcx, [{m} + 160]",
                "mov r10, [{t} + 160]",
                "adcx r10, rcx",
                "adox r10, r9",
                "mov [{t} + 152], r10",
                "mulx r9, rcx, [{m} + 168]",
                "mov r10, [{t} + 168]",
                "adcx r10, rcx",
                "adox r10, r8",
                "mov [{t} + 160], r10",
                "mulx r8, rcx, [{m} + 176]",
                "mov r10, [{t} + 176]",
                "adcx r10, rcx",
                "adox r10, r9",
                "mov [{t} + 168], r10",
                "mulx r9, rcx, [{m} + 184]",
                "mov r10, [{t} + 184]",
                "adcx r10, rcx",
                "adox r10, r8",
                "mov [{t} + 176], r10",
                "mulx r8, rcx, [{m} + 192]",
                "mov r10, [{t} + 192]",
                "adcx r10, rcx",
                "adox r10, r9",
                "mov [{t} + 184], r10",
                "mulx r9, rcx, [{m} + 200]",
                "mov r10, [{t} + 200]",
                "adcx r10, rcx",
                "adox r10, r8",
                "mov [{t} + 192], r10",
                "mulx r8, rcx, [{m} + 208]",
                "mov r10, [{t} + 208]",
                "adcx r10, rcx",
                "adox r10, r9",
                "mov [{t} + 200], r10",
                "mulx r9, rcx, [{m} + 216]",
                "mov r10, [{t} + 216]",
                "adcx r10, rcx",
                "adox r10, r8",
                "mov [{t} + 208], r10",
                "mulx r8, rcx, [{m} + 224]",
                "mov r10, [{t} + 224]",
                "adcx r10, rcx",
                "adox r10, r9",
                "mov [{t} + 216], r10",
                "mulx r9, rcx, [{m} + 232]",
                "mov r10, [{t} + 232]",
                "adcx r10, rcx",
                "adox r10, r8",
                "mov [{t} + 224], r10",
                "mulx r8, rcx, [{m} + 240]",
                "mov r10, [{t} + 240]",
                "adcx r10, rcx",
                "adox r10, r9",
                "mov [{t} + 232], r10",
                "mulx r9, rcx, [{m} + 248]",
                "mov r10, [{t} + 248]",
                "adcx r10, rcx",
                "adox r10, r8",
                "mov [{t} + 240], r10",
                "mov r10, [{t} + 256]",
                "mov ecx, 0",
                "adcx r10, rcx",
                "adox r10, r9",
                "mov [{t} + 248], r10",
                "mov r10, [{t} + 264]",
                "adcx r10, rcx",
                "adox r10, rcx",
                "mov [{t} + 256], r10",

                a = in(reg) a.as_ptr(),
                m = in(reg) m.as_ptr(),
                t = in(reg) t.as_mut_ptr(),
                n0 = in(reg) n0,
                inout("rdx") bi => _,
                out("rax") _, out("rcx") _, out("r8") _, out("r9") _, out("r10") _,
                options(nostack),
            );
        }
    }
    finish(&t, m)
}

// ---- timed loops: each multiplication depends on the last, as a ladder's do ----

macro_rules! chain {
    ($name:ident, $kernel:path, $n:expr, plain) => {
        #[inline(never)]
        pub fn $name(ops: u64, x: &mut [u64; $n], y: &[u64; $n]) {
            for _ in 0..ops {
                *x = $kernel(x, y);
            }
        }
    };
    ($name:ident, $kernel:path, $n:expr, unsafe) => {
        /// # Safety
        /// The CPU has BMI2 and ADX.
        #[target_feature(enable = "bmi2,adx")]
        #[inline(never)]
        pub unsafe fn $name(ops: u64, x: &mut [u64; $n], y: &[u64; $n]) {
            for _ in 0..ops {
                *x = unsafe { $kernel(x, y) };
            }
        }
    };
}
chain!(f25519_chain_u128, f25519_mul_u128, 4, plain);
chain!(f25519_chain_mulx, f25519_mul_mulx, 4, unsafe);
chain!(f25519_chain_asm, f25519_mul_asm, 4, unsafe);
chain!(p256_chain_u128, p256_mul_u128, 4, plain);
chain!(p256_chain_mulx, p256_mul_mulx, 4, unsafe);
chain!(p256_chain_asm, p256_mul_asm, 4, unsafe);

/// One 2048-bit Montgomery multiplication per operation, each on the last's
/// result.
pub fn mont_chain(variant: u8, ops: u64, x: &mut [u64; W], y: &[u64; W], m: &[u64; W], n0: u64) {
    for _ in 0..ops {
        *x = match variant {
            0 => mont_u128(x, y, m, n0),
            // The caller checked BMI2 and ADX.
            1 => unsafe { mont_mulx(x, y, m, n0) },
            _ => unsafe { mont_asm(x, y, m, n0) },
        };
    }
}

// ---- the crate's 5 x 51 multiplication (lib/crypto/src/field25519.rs's
// `product` and `carry`), copied for timing beside the 4 x 64 forms ----

const MASK51: u64 = (1 << 51) - 1;

#[inline(always)]
fn wide(a: u64, b: u64) -> u128 {
    (a as u128).wrapping_mul(b as u128)
}

#[inline(always)]
pub fn f25519_mul_5x51(a: &[u64; 5], b: &[u64; 5]) -> [u64; 5] {
    let b1 = b[1].wrapping_mul(19);
    let b2 = b[2].wrapping_mul(19);
    let b3 = b[3].wrapping_mul(19);
    let b4 = b[4].wrapping_mul(19);
    let v0 = wide(a[0], b[0]).wrapping_add(wide(a[1], b4)).wrapping_add(wide(a[2], b3)).wrapping_add(wide(a[3], b2)).wrapping_add(wide(a[4], b1));
    let v1 = wide(a[0], b[1]).wrapping_add(wide(a[1], b[0])).wrapping_add(wide(a[2], b4)).wrapping_add(wide(a[3], b3)).wrapping_add(wide(a[4], b2));
    let v2 = wide(a[0], b[2]).wrapping_add(wide(a[1], b[1])).wrapping_add(wide(a[2], b[0])).wrapping_add(wide(a[3], b4)).wrapping_add(wide(a[4], b3));
    let v3 = wide(a[0], b[3]).wrapping_add(wide(a[1], b[2])).wrapping_add(wide(a[2], b[1])).wrapping_add(wide(a[3], b[0])).wrapping_add(wide(a[4], b4));
    let v4 = wide(a[0], b[4]).wrapping_add(wide(a[1], b[3])).wrapping_add(wide(a[2], b[2])).wrapping_add(wide(a[3], b[1])).wrapping_add(wide(a[4], b[0]));
    let v1 = v1.wrapping_add(v0 >> 51);
    let v2 = v2.wrapping_add(v1 >> 51);
    let v3 = v3.wrapping_add(v2 >> 51);
    let v4 = v4.wrapping_add(v3 >> 51);
    let r0 = ((v0 as u64 & MASK51) as u128).wrapping_add((v4 >> 51).wrapping_mul(19));
    let r1 = (v1 as u64 & MASK51).wrapping_add((r0 >> 51) as u64);
    [r0 as u64 & MASK51, r1, v2 as u64 & MASK51, v3 as u64 & MASK51, v4 as u64 & MASK51]
}

#[inline(never)]
pub fn f25519_chain_5x51(ops: u64, x: &mut [u64; 5], y: &[u64; 5]) {
    for _ in 0..ops {
        *x = f25519_mul_5x51(x, y);
    }
}
