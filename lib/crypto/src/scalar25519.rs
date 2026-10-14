//! Scalars modulo the Ed25519 prime subgroup order. Fixed 512-bit division
//! and fixed 256-step multiplication avoid data-dependent quotient estimates.
use crate::{ct, secret::wipe64};
const L: [u64; 4] = [
    0x5812631a5cf5d3ed,
    0x14def9dea2f79cd6,
    0,
    0x1000000000000000,
];
pub(crate) struct S([u64; 4]);
impl Drop for S {
    fn drop(&mut self) {
        wipe64(&mut self.0)
    }
}
impl S {
    fn reduce_once(v: &[u64; 4]) -> Self {
        let mut d = [0; 4];
        let mut borrow = 0;
        for i in 0..4 {
            let t = (1u128 << 64) + u128::from(v[i]) - u128::from(L[i]) - borrow;
            d[i] = t as u64;
            borrow = 1 - (t >> 64)
        }
        let result = Self(core::array::from_fn(|i| {
            ct::select(d[i], v[i], borrow as u64)
        }));
        wipe64(&mut d);
        result
    }
    pub(crate) fn reduce(input: &[u8]) -> Self {
        let mut r = Self([0; 4]);
        for i in (0..input.len() * 8).rev() {
            let mut v = [0; 4];
            let mut c = u64::from((input[i / 8] >> (i % 8)) & 1);
            for (j, word) in v.iter_mut().enumerate() {
                *word = (r.0[j] << 1) | c;
                c = r.0[j] >> 63;
            }
            r = Self::reduce_once(&v);
            wipe64(&mut v);
        }
        r
    }
    pub(crate) fn canonical(input: &[u8; 32]) -> Option<Self> {
        let mut v = core::array::from_fn(|i| {
            u64::from_le_bytes(input[8 * i..8 * i + 8].try_into().unwrap())
        });
        let r = Self::reduce_once(&v);
        wipe64(&mut v);
        if ct::equal(input, &r.bytes()) {
            Some(r)
        } else {
            None
        }
    }
    pub(crate) fn bytes(&self) -> [u8; 32] {
        let mut out = [0; 32];
        for i in 0..4 {
            out[8 * i..8 * i + 8].copy_from_slice(&self.0[i].to_le_bytes())
        }
        out
    }
    #[cfg(feature = "sign")]
    pub(crate) fn add(&self, b: &Self) -> Self {
        let mut v = [0; 4];
        let mut c = 0;
        for (i, word) in v.iter_mut().enumerate() {
            let t = u128::from(self.0[i]) + u128::from(b.0[i]) + c;
            *word = t as u64;
            c = t >> 64;
        }
        let result = Self::reduce_once(&v);
        wipe64(&mut v);
        result
    }
    #[cfg(feature = "sign")]
    pub(crate) fn mul(&self, b: &Self) -> Self {
        let mut r = Self([0; 4]);
        for i in (0..256).rev() {
            let double = r.add(&r);
            let add = double.add(self);
            r = Self(core::array::from_fn(|j| {
                ct::select(double.0[j], add.0[j], (b.0[i / 64] >> (i % 64)) & 1)
            }));
        }
        r
    }
}
