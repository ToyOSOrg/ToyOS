//! Five radix-2^51 limbs with u128 products (Bernstein, "Curve25519: new
//! Diffie-Hellman speed records", PKC 2006). The limb bounds are the types.

use crate::ct;

const MASK: u64 = (1 << 51) - 1;

/// Limbs 0, 2, 3 and 4 below 2^51, limb 1 below 2^51 + 2^19.
#[derive(Clone, Copy)]
pub(crate) struct Tight([u64; 5]);

/// Every limb below 2^53.
#[derive(Clone, Copy)]
pub(crate) struct Loose([u64; 5]);

/// A multiplication operand: either bound.
pub(crate) trait Operand {
    fn limbs(&self) -> &[u64; 5];
}

impl Operand for Tight {
    #[inline(always)]
    fn limbs(&self) -> &[u64; 5] {
        &self.0
    }
}

impl Operand for Loose {
    #[inline(always)]
    fn limbs(&self) -> &[u64; 5] {
        &self.0
    }
}

/// 2p, limb by limb: above every tight limb, so a subtraction never borrows.
const TWO_P: [u64; 5] = [2 * (MASK - 18), 2 * MASK, 2 * MASK, 2 * MASK, 2 * MASK];

#[inline(always)]
const fn wide(a: u64, b: u64) -> u128 {
    (a as u128).wrapping_mul(b as u128)
}

/// A column of products. The limb bounds keep every sum below 2^115, so the
/// wrapping additions are exact and carry no overflow check.
macro_rules! column {
    ($first:expr $(, $rest:expr)*) => { $first $(.wrapping_add($rest))* };
}

/// Columns below 2^115 into tight limbs: one carry pass, the top carry times
/// 19 into limb 0 (2^255 = 19 mod p), and one more carry into limb 1.
#[inline(always)]
const fn carry(v: [u128; 5]) -> Tight {
    let [v0, v1, v2, v3, v4] = v;
    let v1 = v1.wrapping_add(v0 >> 51);
    let v2 = v2.wrapping_add(v1 >> 51);
    let v3 = v3.wrapping_add(v2 >> 51);
    let v4 = v4.wrapping_add(v3 >> 51);
    let r0 = ((v0 as u64 & MASK) as u128).wrapping_add((v4 >> 51).wrapping_mul(19));
    let r1 = (v1 as u64 & MASK).wrapping_add((r0 >> 51) as u64);
    Tight([
        r0 as u64 & MASK,
        r1,
        v2 as u64 & MASK,
        v3 as u64 & MASK,
        v4 as u64 & MASK,
    ])
}

impl Tight {
    pub(crate) const ZERO: Self = Self([0; 5]);
    pub(crate) const ONE: Self = Self([1, 0, 0, 0, 0]);

    /// The low 255 bits of a little-endian encoding.
    pub(crate) const fn from_bytes(b: &[u8; 32]) -> Self {
        let mut w = [0u64; 4];
        let mut i = 0;
        while i < 32 {
            w[i / 8] |= (b[i] as u64) << (8 * (i % 8));
            i += 1;
        }
        Self([
            w[0] & MASK,
            (w[0] >> 51 | w[1] << 13) & MASK,
            (w[1] >> 38 | w[2] << 26) & MASK,
            (w[2] >> 25 | w[3] << 39) & MASK,
            (w[3] >> 12) & MASK,
        ])
    }

    #[cfg(test)]
    pub(crate) fn from_limbs(limbs: [u64; 5]) -> Self {
        Self(limbs)
    }

    pub(crate) const fn add(&self, b: &Tight) -> Loose {
        let (a, b) = (&self.0, &b.0);
        Loose([
            a[0].wrapping_add(b[0]),
            a[1].wrapping_add(b[1]),
            a[2].wrapping_add(b[2]),
            a[3].wrapping_add(b[3]),
            a[4].wrapping_add(b[4]),
        ])
    }

    /// self + 2p - b, limb by limb.
    pub(crate) const fn sub(&self, b: &Tight) -> Loose {
        let (a, b) = (&self.0, &b.0);
        Loose([
            a[0].wrapping_add(TWO_P[0]).wrapping_sub(b[0]),
            a[1].wrapping_add(TWO_P[1]).wrapping_sub(b[1]),
            a[2].wrapping_add(TWO_P[2]).wrapping_sub(b[2]),
            a[3].wrapping_add(TWO_P[3]).wrapping_sub(b[3]),
            a[4].wrapping_add(TWO_P[4]).wrapping_sub(b[4]),
        ])
    }

    /// The same element as a loose operand.
    pub(crate) const fn widen(&self) -> Loose {
        Loose(self.0)
    }

    pub(crate) const fn neg(&self) -> Loose {
        Self::ZERO.sub(self)
    }

    /// The `const` form of [`Multiply::mul`], tight by tight.
    pub(crate) const fn times(&self, b: &Tight) -> Tight {
        product(&self.0, &b.0)
    }

    pub(crate) const fn squared(&self) -> Tight {
        square(&self.0)
    }

    pub(crate) fn select(a: &Self, b: &Self, bit: u64) -> Self {
        let m = ct::mask(bit);
        let (a, b) = (&a.0, &b.0);
        Self([
            a[0] ^ ((a[0] ^ b[0]) & m),
            a[1] ^ ((a[1] ^ b[1]) & m),
            a[2] ^ ((a[2] ^ b[2]) & m),
            a[3] ^ ((a[3] ^ b[3]) & m),
            a[4] ^ ((a[4] ^ b[4]) & m),
        ])
    }

    /// Exchanges a and b under a mask of all ones, and leaves them under zero.
    pub(crate) fn swap(a: &mut Self, b: &mut Self, mask: u64) {
        for i in 0..5 {
            let t = (a.0[i] ^ b.0[i]) & mask;
            a.0[i] ^= t;
            b.0[i] ^= t;
        }
    }

    /// The canonical little-endian encoding.
    pub(crate) fn bytes(&self) -> [u8; 32] {
        let h = &self.0;
        // q = floor((h + 19) / 2^255), which is 1 exactly when h >= p for a
        // tight h < 2p.
        let mut q = h[0].wrapping_add(19) >> 51;
        q = h[1].wrapping_add(q) >> 51;
        q = h[2].wrapping_add(q) >> 51;
        q = h[3].wrapping_add(q) >> 51;
        q = h[4].wrapping_add(q) >> 51;
        // h - q p = h + 19 q - q 2^255, with bit 255 dropped.
        let h0 = h[0].wrapping_add(q.wrapping_mul(19));
        let h1 = h[1].wrapping_add(h0 >> 51);
        let h2 = h[2].wrapping_add(h1 >> 51);
        let h3 = h[3].wrapping_add(h2 >> 51);
        let h4 = h[4].wrapping_add(h3 >> 51);
        let l = [h0 & MASK, h1 & MASK, h2 & MASK, h3 & MASK, h4 & MASK];
        let words = [
            l[0] | l[1] << 51,
            l[1] >> 13 | l[2] << 38,
            l[2] >> 26 | l[3] << 25,
            l[3] >> 39 | l[4] << 12,
        ];
        let mut out = [0; 32];
        for (i, word) in words.iter().enumerate() {
            out[8 * i..8 * i + 8].copy_from_slice(&word.to_le_bytes());
        }
        out
    }
}

impl Loose {
    /// The same element, carried tight.
    pub(crate) const fn carry(&self) -> Tight {
        let v = &self.0;
        carry([v[0] as u128, v[1] as u128, v[2] as u128, v[3] as u128, v[4] as u128])
    }
}

/// Multiplications, each leaving a tight element.
pub(crate) trait Multiply: Operand {
    #[inline(always)]
    fn mul(&self, b: &impl Operand) -> Tight {
        product(self.limbs(), b.limbs())
    }

    #[inline(always)]
    fn square(&self) -> Tight {
        square(self.limbs())
    }

    /// self * k for k < 2^17.
    #[inline(always)]
    fn mul_small(&self, k: u32) -> Tight {
        let a = self.limbs();
        let k = k as u64;
        let v = [wide(a[0], k), wide(a[1], k), wide(a[2], k), wide(a[3], k), wide(a[4], k)];
        carry(v)
    }
}

impl Multiply for Tight {}
impl Multiply for Loose {}

/// The 25 products, with b's limbs that wrap past 2^255 premultiplied by 19.
#[inline(always)]
const fn product(a: &[u64; 5], b: &[u64; 5]) -> Tight {
    let b1 = b[1].wrapping_mul(19);
    let b2 = b[2].wrapping_mul(19);
    let b3 = b[3].wrapping_mul(19);
    let b4 = b[4].wrapping_mul(19);
    let v = [
        column!(wide(a[0], b[0]), wide(a[1], b4), wide(a[2], b3), wide(a[3], b2), wide(a[4], b1)),
        column!(wide(a[0], b[1]), wide(a[1], b[0]), wide(a[2], b4), wide(a[3], b3), wide(a[4], b2)),
        column!(wide(a[0], b[2]), wide(a[1], b[1]), wide(a[2], b[0]), wide(a[3], b4), wide(a[4], b3)),
        column!(wide(a[0], b[3]), wide(a[1], b[2]), wide(a[2], b[1]), wide(a[3], b[0]), wide(a[4], b4)),
        column!(wide(a[0], b[4]), wide(a[1], b[3]), wide(a[2], b[2]), wide(a[3], b[1]), wide(a[4], b[0])),
    ];
    carry(v)
}

/// The 15 distinct products of a square.
#[inline(always)]
const fn square(a: &[u64; 5]) -> Tight {
    let d0 = a[0].wrapping_mul(2);
    let d1 = a[1].wrapping_mul(2);
    let d2 = a[2].wrapping_mul(2);
    let d3 = a[3].wrapping_mul(2);
    let a3_19 = a[3].wrapping_mul(19);
    let a4_19 = a[4].wrapping_mul(19);
    let v = [
        column!(wide(a[0], a[0]), wide(d1, a4_19), wide(d2, a3_19)),
        column!(wide(d0, a[1]), wide(a[3], a3_19), wide(d2, a4_19)),
        column!(wide(d0, a[2]), wide(a[1], a[1]), wide(d3, a4_19)),
        column!(wide(d0, a[3]), wide(d1, a[2]), wide(a[4], a4_19)),
        column!(wide(d0, a[4]), wide(d1, a[3]), wide(a[2], a[2])),
    ];
    carry(v)
}

// SAFETY: five words, and zero is a valid element.
unsafe impl crate::secret::Zeroable for Tight {}

#[cfg(test)]
mod tests {
    use super::*;
    use num_bigint::BigUint;

    /// The largest limbs the operations reach, through every operation: the
    /// bounds the types state are enough for the carries.
    #[test]
    fn largest_reachable_limbs_stay_within_their_bounds() {
        let p = (BigUint::from(1u8) << 255) - 19u8;
        let tight = Tight::from_limbs([(1 << 51) - 1, (1 << 51) + (1 << 19) - 1, (1 << 51) - 1, (1 << 51) - 1, (1 << 51) - 1]);
        let value = |l: &[u64; 5]| l.iter().enumerate().fold(BigUint::from(0u8), |v, (i, &x)| v + (BigUint::from(x) << (51 * i)));
        let t = value(Operand::limbs(&tight)) % &p;
        let loose = tight.add(&tight);
        let l = value(Operand::limbs(&loose)) % &p;
        assert_eq!(BigUint::from_bytes_le(&tight.bytes()), t);
        assert_eq!(BigUint::from_bytes_le(&loose.mul(&loose).bytes()), (&l * &l) % &p);
        assert_eq!(BigUint::from_bytes_le(&loose.square().bytes()), (&l * &l) % &p);
        assert_eq!(BigUint::from_bytes_le(&tight.sub(&tight).carry().bytes()), BigUint::from(0u8));
        assert_eq!(BigUint::from_bytes_le(&loose.mul_small(121665).bytes()), (&l * 121665u32) % &p);
        let widest = tight.sub(&Tight::ZERO);
        let w = value(Operand::limbs(&widest)) % &p;
        assert_eq!(BigUint::from_bytes_le(&widest.mul(&loose).bytes()), (&w * &l) % &p);
        assert_eq!(BigUint::from_bytes_le(&widest.square().bytes()), (&w * &w) % &p);
        for limb in Operand::limbs(&loose.mul(&loose)) {
            assert!(*limb < (1 << 51) + (1 << 19));
        }
    }
}
