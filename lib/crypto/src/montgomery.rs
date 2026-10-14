//! Bounded Montgomery arithmetic. N is four or six for the NIST curves.
//! Every secret operation uses fixed loop bounds, carry propagation and masked
//! reductions. Moduli and exponents for inversion are public. No general bignum
//! or allocator crosses the primitive boundary.
use crate::{ct, secret::wipe64};
pub(crate) struct V<const N: usize>(pub(crate) [u64; N]);
impl<const N: usize> Drop for V<N> {
    fn drop(&mut self) {
        wipe64(&mut self.0)
    }
}
pub(crate) struct Mod<const N: usize> {
    pub(crate) p: [u64; N],
    inverse: u64,
    r2: V<N>,
    one: V<N>,
}
impl<const N: usize> V<N> {
    pub(crate) fn zero() -> Self {
        Self([0; N])
    }
    pub(crate) fn select(a: &Self, b: &Self, bit: u64) -> Self {
        Self(core::array::from_fn(|i| ct::select(a.0[i], b.0[i], bit)))
    }
    pub(crate) fn eq(&self, b: &Self) -> u64 {
        let mut d = 0;
        for i in 0..N {
            d |= self.0[i] ^ b.0[i]
        }
        1 ^ ct::nonzero(d)
    }
}
impl<const N: usize> Mod<N> {
    pub(crate) fn new(p: [u64; N]) -> Self {
        // Raw N-limb digests reduce with one subtraction only when 2*p > 2^(64*N).
        assert!(matches!(N, 4 | 6) && p[0] & 1 == 1 && p[N - 1] >> 63 == 1);
        let mut inverse = 1u64;
        for _ in 0..6 {
            inverse = inverse.wrapping_mul(2u64.wrapping_sub(p[0].wrapping_mul(inverse)))
        }
        let mut m = Self {
            p,
            inverse: inverse.wrapping_neg(),
            r2: V::zero(),
            one: V::zero(),
        };
        let mut x = V::zero();
        x.0[0] = 1;
        for _ in 0..2 * N * 64 {
            x = m.add(&x, &x)
        }
        m.r2 = x;
        let mut raw = V::zero();
        raw.0[0] = 1;
        m.one = m.mul(&raw, &m.r2);
        m
    }
    pub(crate) fn one(&self) -> V<N> {
        V(self.one.0)
    }
    // The incoming value is below 2*p, including its carry limb.
    fn reduce(&self, v: &[u64; N], high: u64) -> V<N> {
        let mut d = [0; N];
        let mut borrow = 0u128;
        for i in 0..N {
            let t = (1u128 << 64) + u128::from(v[i]) - u128::from(self.p[i]) - borrow;
            d[i] = t as u64;
            borrow = 1 - (t >> 64)
        }
        let bit = ct::nonzero(high) | (1 - borrow as u64);
        let result = V(core::array::from_fn(|i| ct::select(v[i], d[i], bit)));
        wipe64(&mut d);
        result
    }
    pub(crate) fn add(&self, a: &V<N>, b: &V<N>) -> V<N> {
        let mut v = [0; N];
        let mut c = 0u128;
        for (i, word) in v.iter_mut().enumerate() {
            let t = u128::from(a.0[i]) + u128::from(b.0[i]) + c;
            *word = t as u64;
            c = t >> 64
        }
        let result = self.reduce(&v, c as u64);
        wipe64(&mut v);
        result
    }
    pub(crate) fn sub(&self, a: &V<N>, b: &V<N>) -> V<N> {
        let mut v = [0; N];
        let mut borrow = 0u128;
        for (i, word) in v.iter_mut().enumerate() {
            let t = (1u128 << 64) + u128::from(a.0[i]) - u128::from(b.0[i]) - borrow;
            *word = t as u64;
            borrow = 1 - (t >> 64)
        }
        let mask = ct::mask(borrow as u64);
        let mut c = 0u128;
        for (i, word) in v.iter_mut().enumerate() {
            let t = u128::from(*word) + u128::from(self.p[i] & mask) + c;
            *word = t as u64;
            c = t >> 64
        }
        V(v)
    }
    pub(crate) fn mul(&self, a: &V<N>, b: &V<N>) -> V<N> {
        let mut t = [0u64; 14];
        for i in 0..N {
            let mut c = 0u128;
            for j in 0..N {
                let value = u128::from(a.0[i]) * u128::from(b.0[j]) + u128::from(t[i + j]) + c;
                t[i + j] = value as u64;
                c = value >> 64
            }
            t[i + N] = c as u64;
        }
        for i in 0..N {
            let q = t[i].wrapping_mul(self.inverse);
            let mut c = 0u128;
            for j in 0..N {
                let value = u128::from(q) * u128::from(self.p[j]) + u128::from(t[i + j]) + c;
                t[i + j] = value as u64;
                c = value >> 64
            }
            for word in t.iter_mut().take(2 * N + 1).skip(i + N) {
                let value = u128::from(*word) + c;
                *word = value as u64;
                c = value >> 64
            }
            assert_eq!(c, 0);
        }
        let result = self.reduce(t[N..2 * N].try_into().unwrap(), t[2 * N]);
        wipe64(&mut t);
        result
    }
    pub(crate) fn square(&self, a: &V<N>) -> V<N> {
        self.mul(a, a)
    }
    pub(crate) fn small(&self, n: u64) -> V<N> {
        let mut raw = V::zero();
        raw.0[0] = n;
        self.mul(&raw, &self.r2)
    }
    pub(crate) fn encode_raw(&self, raw: &V<N>) -> V<N> {
        self.mul(raw, &self.r2)
    }
    pub(crate) fn raw(&self, v: &V<N>) -> V<N> {
        let mut one = V::zero();
        one.0[0] = 1;
        self.mul(v, &one)
    }
    pub(crate) fn valid(&self, v: &V<N>) -> u64 {
        let mut borrow = 0u128;
        for i in 0..N {
            let t = (1u128 << 64) + u128::from(v.0[i]) - u128::from(self.p[i]) - borrow;
            borrow = 1 - (t >> 64)
        }
        borrow as u64
    }
    pub(crate) fn invert(&self, a: &V<N>) -> V<N> {
        let mut e = self.p;
        let mut borrow = 2u128;
        for word in &mut e {
            let t = (1u128 << 64) + u128::from(*word) - borrow;
            *word = t as u64;
            borrow = 1 - (t >> 64)
        }
        let mut r = self.one();
        for i in (0..N * 64).rev() {
            let square = self.square(&r);
            let product = self.mul(&square, a);
            r = V::select(&square, &product, (e[i / 64] >> (i % 64)) & 1)
        }
        r
    }
    pub(crate) fn from_be(bytes: &[u8]) -> V<N> {
        assert_eq!(bytes.len(), N * 8);
        V(core::array::from_fn(|i| {
            u64::from_be_bytes(
                bytes[bytes.len() - 8 * (i + 1)..bytes.len() - 8 * i]
                    .try_into()
                    .unwrap(),
            )
        }))
    }
    pub(crate) fn to_be(v: &V<N>, bytes: &mut [u8]) {
        assert_eq!(bytes.len(), N * 8);
        for i in 0..N {
            let end = bytes.len() - 8 * i;
            bytes[end - 8..end].copy_from_slice(&v.0[i].to_be_bytes())
        }
    }
}
