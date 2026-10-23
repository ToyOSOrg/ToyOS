//! Four 64-bit words. Every value below 2^256 is an element, so [`Loose`] is
//! [`Tight`] and a carry changes nothing; only [`Tight::bytes`] reduces below
//! p. Each operation folds what passes 2^256 back as 38 (2^256 = 38 mod p).

use crate::ct;

#[derive(Clone, Copy)]
pub(crate) struct Tight([u64; 4]);

/// The same words: a sum needs no carry before it is an operand again.
pub(crate) type Loose = Tight;

#[inline(always)]
const fn mac(acc: u64, a: u64, b: u64, carry: u64) -> (u64, u64) {
    let t = (a as u128).wrapping_mul(b as u128).wrapping_add(acc as u128).wrapping_add(carry as u128);
    (t as u64, (t >> 64) as u64)
}

#[inline(always)]
const fn adc(a: u64, b: u64, carry: u64) -> (u64, u64) {
    let t = (a as u128).wrapping_add(b as u128).wrapping_add(carry as u128);
    (t as u64, (t >> 64) as u64)
}

#[inline(always)]
const fn sbb(a: u64, b: u64, borrow: u64) -> (u64, u64) {
    let t = (a as u128).wrapping_sub(b as u128).wrapping_sub(borrow as u128);
    (t as u64, (t >> 127) as u64)
}

/// r + top 2^256 for top below 2^58: 38 top into the words, and 38 more for
/// a carry out, which leaves the words below 38 top, so word zero takes the
/// second 38 without a carry.
#[inline(always)]
const fn fold(r: [u64; 4], top: u64) -> Tight {
    let (r0, c) = adc(r[0], top.wrapping_mul(38), 0);
    let (r1, c) = adc(r[1], 0, c);
    let (r2, c) = adc(r[2], 0, c);
    let (r3, c) = adc(r[3], 0, c);
    Tight([r0.wrapping_add(c.wrapping_mul(38)), r1, r2, r3])
}

/// The eight-word t_lo + 2^256 t_hi as t_lo + 38 t_hi.
#[inline(always)]
const fn reduce(t: [u64; 8]) -> Tight {
    let (r0, c) = mac(t[0], t[4], 38, 0);
    let (r1, c) = mac(t[1], t[5], 38, c);
    let (r2, c) = mac(t[2], t[6], 38, c);
    let (r3, top) = mac(t[3], t[7], 38, c);
    fold([r0, r1, r2, r3], top)
}

impl Tight {
    pub(crate) const ZERO: Self = Self([0; 4]);
    pub(crate) const ONE: Self = Self([1, 0, 0, 0]);

    /// The low 255 bits of a little-endian encoding.
    pub(crate) const fn from_bytes(b: &[u8; 32]) -> Self {
        let mut w = [0u64; 4];
        let mut i = 0;
        while i < 32 {
            w[i / 8] |= (b[i] as u64) << (8 * (i % 8));
            i += 1;
        }
        Self([w[0], w[1], w[2], w[3] & (u64::MAX >> 1)])
    }

    pub(crate) const fn add(&self, b: &Tight) -> Tight {
        let (a, b) = (&self.0, &b.0);
        let (r0, c) = adc(a[0], b[0], 0);
        let (r1, c) = adc(a[1], b[1], c);
        let (r2, c) = adc(a[2], b[2], c);
        let (r3, c) = adc(a[3], b[3], c);
        fold([r0, r1, r2, r3], c)
    }

    /// self - b, and 38 less for a borrow out (-2^256 = -38 mod p), and 38
    /// less again for a second borrow, after which word zero is at least
    /// 2^64 - 38.
    pub(crate) const fn sub(&self, b: &Tight) -> Tight {
        let (a, b) = (&self.0, &b.0);
        let (r0, c) = sbb(a[0], b[0], 0);
        let (r1, c) = sbb(a[1], b[1], c);
        let (r2, c) = sbb(a[2], b[2], c);
        let (r3, c) = sbb(a[3], b[3], c);
        let (r0, c) = sbb(r0, c.wrapping_mul(38), 0);
        let (r1, c) = sbb(r1, 0, c);
        let (r2, c) = sbb(r2, 0, c);
        let (r3, c) = sbb(r3, 0, c);
        Self([r0.wrapping_sub(c.wrapping_mul(38)), r1, r2, r3])
    }

    pub(crate) const fn widen(&self) -> Tight {
        *self
    }

    pub(crate) const fn carry(&self) -> Tight {
        *self
    }

    pub(crate) const fn neg(&self) -> Tight {
        Self::ZERO.sub(self)
    }

    /// The `const` form of [`Multiply::mul`].
    pub(crate) const fn times(&self, b: &Tight) -> Tight {
        product(&self.0, &b.0)
    }

    pub(crate) const fn squared(&self) -> Tight {
        square(&self.0)
    }

    pub(crate) fn select(a: &Self, b: &Self, bit: u64) -> Self {
        let m = ct::mask(bit);
        let (a, b) = (&a.0, &b.0);
        Self([a[0] ^ ((a[0] ^ b[0]) & m), a[1] ^ ((a[1] ^ b[1]) & m), a[2] ^ ((a[2] ^ b[2]) & m), a[3] ^ ((a[3] ^ b[3]) & m)])
    }

    /// Exchanges a and b under a mask of all ones, and leaves them under zero.
    pub(crate) fn swap(a: &mut Self, b: &mut Self, mask: u64) {
        for i in 0..4 {
            let t = (a.0[i] ^ b.0[i]) & mask;
            a.0[i] ^= t;
            b.0[i] ^= t;
        }
    }

    /// The canonical little-endian encoding: bit 255 folded in as 19, which
    /// leaves v below 2^255 + 19, then v + 19 - 2^255 where that is not
    /// negative, which is v - p exactly when v is at least p.
    pub(crate) fn bytes(&self) -> [u8; 32] {
        let h = &self.0;
        let (v0, c) = adc(h[0], (h[3] >> 63).wrapping_mul(19), 0);
        let (v1, c) = adc(h[1], 0, c);
        let (v2, c) = adc(h[2], 0, c);
        let v3 = (h[3] & (u64::MAX >> 1)).wrapping_add(c);
        let (w0, c) = adc(v0, 19, 0);
        let (w1, c) = adc(v1, 0, c);
        let (w2, c) = adc(v2, 0, c);
        let w3 = v3.wrapping_add(c);
        let reduced = Self([w0, w1, w2, w3 & (u64::MAX >> 1)]);
        let words = Self::select(&Self([v0, v1, v2, v3]), &reduced, w3 >> 63).0;
        let mut out = [0; 32];
        for (i, word) in words.iter().enumerate() {
            out[8 * i..8 * i + 8].copy_from_slice(&word.to_le_bytes());
        }
        out
    }
}

/// Multiplications, by MULX, ADCX and ADOX where the CPU has them
/// (`arch::f25519_mul`, `arch::f25519_square`).
pub(crate) trait Multiply {
    fn mul(&self, b: &Tight) -> Tight;
    fn square(&self) -> Tight;
    /// self * k for k < 2^17.
    fn mul_small(&self, k: u32) -> Tight;
}

impl Multiply for Tight {
    #[inline(always)]
    fn mul(&self, b: &Tight) -> Tight {
        match crate::arch::f25519_mul(&self.0, &b.0) {
            Some(v) => Tight(v),
            None => mul_portable(&self.0, &b.0),
        }
    }

    #[inline(always)]
    fn square(&self) -> Tight {
        match crate::arch::f25519_square(&self.0) {
            Some(v) => Tight(v),
            None => square_portable(&self.0),
        }
    }

    #[inline(always)]
    fn mul_small(&self, k: u32) -> Tight {
        let (a, k) = (&self.0, k as u64);
        let (r0, c) = mac(0, a[0], k, 0);
        let (r1, c) = mac(0, a[1], k, c);
        let (r2, c) = mac(0, a[2], k, c);
        let (r3, top) = mac(0, a[3], k, c);
        fold([r0, r1, r2, r3], top)
    }
}

#[inline(never)]
fn mul_portable(a: &[u64; 4], b: &[u64; 4]) -> Tight {
    product(a, b)
}

#[inline(never)]
fn square_portable(a: &[u64; 4]) -> Tight {
    square(a)
}

/// The sixteen products, row by row, then [`reduce`].
#[inline(always)]
const fn product(a: &[u64; 4], b: &[u64; 4]) -> Tight {
    let (t0, c) = mac(0, a[0], b[0], 0);
    let (t1, c) = mac(0, a[0], b[1], c);
    let (t2, c) = mac(0, a[0], b[2], c);
    let (t3, t4) = mac(0, a[0], b[3], c);
    let (t1, c) = mac(t1, a[1], b[0], 0);
    let (t2, c) = mac(t2, a[1], b[1], c);
    let (t3, c) = mac(t3, a[1], b[2], c);
    let (t4, t5) = mac(t4, a[1], b[3], c);
    let (t2, c) = mac(t2, a[2], b[0], 0);
    let (t3, c) = mac(t3, a[2], b[1], c);
    let (t4, c) = mac(t4, a[2], b[2], c);
    let (t5, t6) = mac(t5, a[2], b[3], c);
    let (t3, c) = mac(t3, a[3], b[0], 0);
    let (t4, c) = mac(t4, a[3], b[1], c);
    let (t5, c) = mac(t5, a[3], b[2], c);
    let (t6, t7) = mac(t6, a[3], b[3], c);
    reduce([t0, t1, t2, t3, t4, t5, t6, t7])
}

/// The six cross products once, doubled, the four squares added, then
/// [`reduce`].
#[inline(always)]
const fn square(a: &[u64; 4]) -> Tight {
    let (t1, c) = mac(0, a[0], a[1], 0);
    let (t2, c) = mac(0, a[0], a[2], c);
    let (t3, t4) = mac(0, a[0], a[3], c);
    let (t3, c) = mac(t3, a[1], a[2], 0);
    let (t4, t5) = mac(t4, a[1], a[3], c);
    let (t5, t6) = mac(t5, a[2], a[3], 0);
    let t7 = t6 >> 63;
    let (t6, t5, t4, t3, t2, t1) =
        (t6 << 1 | t5 >> 63, t5 << 1 | t4 >> 63, t4 << 1 | t3 >> 63, t3 << 1 | t2 >> 63, t2 << 1 | t1 >> 63, t1 << 1);
    let (t0, hi) = mac(0, a[0], a[0], 0);
    let (t1, c) = adc(t1, hi, 0);
    let (lo, hi) = mac(0, a[1], a[1], 0);
    let (t2, c) = adc(t2, lo, c);
    let (t3, c) = adc(t3, hi, c);
    let (lo, hi) = mac(0, a[2], a[2], 0);
    let (t4, c) = adc(t4, lo, c);
    let (t5, c) = adc(t5, hi, c);
    let (lo, hi) = mac(0, a[3], a[3], 0);
    let (t6, c) = adc(t6, lo, c);
    let (t7, _) = adc(t7, hi, c);
    reduce([t0, t1, t2, t3, t4, t5, t6, t7])
}

// SAFETY: four words, and zero is a valid element.
unsafe impl crate::secret::Zeroable for Tight {}

#[cfg(test)]
mod tests {
    use super::*;
    use num_bigint::BigUint;
    use std::vec::Vec;

    fn value(t: &Tight) -> BigUint {
        t.0.iter().rev().fold(BigUint::from(0u8), |v, &w| (v << 64) + w)
    }

    fn words(v: &BigUint) -> [u64; 4] {
        let mut w = [0; 4];
        for (i, digit) in v.iter_u64_digits().enumerate() {
            w[i] = digit;
        }
        w
    }

    /// Words at each value a fold or a borrow turns on, up to 2^256 - 1,
    /// which no decoding reaches but sums and products do, and seeded words
    /// with their top bits set: every operation stays congruent and
    /// canonical encoding reduces. Where this CPU has the instruction path,
    /// its words are the portable path's exactly.
    #[test]
    fn words_up_to_2_256_match_integers_and_the_portable_path() {
        let p: BigUint = (BigUint::from(1u8) << 255) - 19u8;
        let top: BigUint = BigUint::from(1u8) << 256;
        let mut values: Vec<BigUint> = [0u64, 1, 18, 19, 37, 38, 39].map(BigUint::from).to_vec();
        for v in [&p - 1u8, p.clone(), &p + 1u8, &p + 18u8, &p + 19u8, BigUint::from(1u8) << 255, (BigUint::from(1u8) << 255) - 1u8] {
            values.push(v);
        }
        for k in [1u8, 37, 38, 39, 76] {
            values.push(&top - k);
        }
        for k in [0u8, 1, 37] {
            values.push((&p << 1) + k);
        }
        for i in 0..4 {
            values.push((BigUint::from(1u8) << (64 * i + 64)) - 1u8);
            values.push(&top - (BigUint::from(1u8) << (64 * i)));
        }
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        for _ in 0..200 {
            let mut w = [0u64; 4];
            for word in &mut w {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                *word = seed;
            }
            w[3] |= 0xffff << 48;
            values.push(value(&Tight(w)));
        }
        let kernel = crate::arch::f25519_mul(&[0; 4], &[0; 4]).is_some();
        std::println!("f25519 path: {}", if kernel { "the BMI2/ADX kernels" } else { "portable" });
        for a in &values {
            let x = Tight(words(a));
            let canonical = BigUint::from_bytes_le(&x.bytes());
            assert_eq!(canonical, a % &p, "encoding {a}");
            let square = x.square();
            assert_eq!(square.0, x.squared().0, "{a}^2 by both paths");
            assert!(value(&square) < top);
            assert_eq!(value(&square) % &p, a * a % &p, "{a}^2");
            assert_eq!(value(&x.mul_small(121665)) % &p, a * 121665u32 % &p, "121665 {a}");
            assert_eq!(value(&x.neg()) % &p, (&p - a % &p) % &p, "-{a}");
            for b in &values {
                let y = Tight(words(b));
                let product = x.mul(&y);
                assert_eq!(product.0, x.times(&y).0, "{a} {b} by both paths");
                assert_eq!(value(&product) % &p, a * b % &p, "{a} {b}");
                assert_eq!(value(&x.add(&y)) % &p, (a + b) % &p, "{a} + {b}");
                assert_eq!(value(&x.sub(&y)) % &p, (a + &p * 4u8 - b) % &p, "{a} - {b}");
            }
        }
    }
}
