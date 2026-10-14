//! Arithmetic modulo 2^255-19. Five radix-2^51 limbs; products fit u128.
//! Every operation has a fixed schedule. Encodings and equality are public at
//! verification boundaries. Arithmetic storage is erased on drop.

use crate::{ct, secret::wipe64};
const M: u64 = (1 << 51) - 1;
pub(crate) struct F([u64; 5]);
impl Drop for F {
    fn drop(&mut self) {
        wipe64(&mut self.0)
    }
}
impl F {
    pub(crate) fn zero() -> Self {
        Self([0; 5])
    }
    pub(crate) fn one() -> Self {
        Self([1, 0, 0, 0, 0])
    }
    pub(crate) fn small(x: u32) -> Self {
        Self([u64::from(x), 0, 0, 0, 0])
    }
    pub(crate) fn from_bytes(b: &[u8; 32]) -> Self {
        let mut v = [0u64; 5];
        for i in 0..255 {
            v[i / 51] |= u64::from((b[i / 8] >> (i % 8)) & 1) << (i % 51)
        }
        Self(v)
    }
    fn carry(v: &mut [u128; 5]) -> Self {
        // Products start at <=77*(2^51-1)^2; three passes leave every limb <2^51.
        for _ in 0..3 {
            for i in 0..4 {
                let c = v[i] >> 51;
                v[i] &= u128::from(M);
                v[i + 1] += c;
            }
            let c = v[4] >> 51;
            v[4] &= u128::from(M);
            v[0] += c * 19;
        }
        let out = Self(core::array::from_fn(|i| v[i] as u64));
        // Volatile erasure covers the explicit wide product workspace.
        for x in v {
            unsafe { core::ptr::write_volatile(x, 0) }
        }
        core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
        out
    }
    pub(crate) fn add(&self, b: &Self) -> Self {
        Self::carry(&mut core::array::from_fn(|i| {
            u128::from(self.0[i]) + u128::from(b.0[i])
        }))
    }
    pub(crate) fn sub(&self, b: &Self) -> Self {
        Self::carry(&mut core::array::from_fn(|i| {
            u128::from(self.0[i]) + 2 * u128::from(if i == 0 { M - 18 } else { M })
                - u128::from(b.0[i])
        }))
    }
    pub(crate) fn neg(&self) -> Self {
        Self::zero().sub(self)
    }
    pub(crate) fn mul(&self, b: &Self) -> Self {
        let mut v = [0u128; 5];
        for i in 0..5 {
            for j in 0..5 {
                let k = i + j;
                v[k % 5] +=
                    u128::from(self.0[i]) * u128::from(b.0[j]) * if k >= 5 { 19 } else { 1 };
            }
        }
        Self::carry(&mut v)
    }
    pub(crate) fn square(&self) -> Self {
        self.mul(self)
    }
    pub(crate) fn select(a: &Self, b: &Self, bit: u64) -> Self {
        Self(core::array::from_fn(|i| ct::select(a.0[i], b.0[i], bit)))
    }
    pub(crate) fn pow(&self, e: &[u8; 32], bits: usize) -> Self {
        let mut r = Self::one();
        for i in (0..bits).rev() {
            let square = r.square();
            let product = square.mul(self);
            r = Self::select(&square, &product, u64::from((e[i / 8] >> (i % 8)) & 1));
        }
        r
    }
    pub(crate) fn invert(&self) -> Self {
        let mut e = [0xff; 32];
        e[0] = 0xeb;
        e[31] = 0x7f;
        self.pow(&e, 255)
    }
    pub(crate) fn bytes(&self) -> [u8; 32] {
        let n = Self::carry(&mut self.0.map(u128::from));
        let mut t = [0u64; 5];
        let mut borrow = 0;
        for (i, word) in t.iter_mut().enumerate() {
            let p = if i == 0 { M - 18 } else { M };
            let d = (1u128 << 51) + u128::from(n.0[i]) - u128::from(p) - u128::from(borrow);
            *word = d as u64 & M;
            borrow = 1 - ((d >> 51) as u64);
        }
        let reduced = Self(t);
        let value = Self::select(&reduced, &n, borrow);
        let mut out = [0; 32];
        for i in 0..255 {
            out[i / 8] |= ((value.0[i / 51] >> (i % 51)) as u8 & 1) << (i % 8)
        }
        out
    }
    pub(crate) fn eq(&self, b: &Self) -> u64 {
        let mut a = self.bytes();
        let mut bb = b.bytes();
        let result = u64::from(ct::equal(&a, &bb));
        crate::secret::wipe(&mut a);
        crate::secret::wipe(&mut bb);
        result
    }
}
