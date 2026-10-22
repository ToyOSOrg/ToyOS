//! Montgomery arithmetic modulo the NIST curves' primes and orders, N = 4 or 6
//! words. Each modulus is a type, so a residue modulo one cannot meet a residue
//! modulo another. Every operation on a residue has a fixed schedule and a
//! masked final subtraction; inversion's exponent is public. Residues are
//! `Copy` intermediates, not erased (`secret`'s contract).
#![allow(
    clippy::needless_range_loop,
    reason = "carry chains index parallel fixed-size arrays; iterator adaptors stay out of line in the shipped profile"
)]
use crate::ct;
use core::marker::PhantomData;

/// An odd modulus p with 2^(64N-1) < p < 2^(64N), and its Montgomery
/// constants for R = 2^(64N), evaluated at compile time.
pub(crate) trait Modulus<const N: usize>: 'static {
    const P: [u64; N];
    /// How a reduction step forms q p.
    const FORM: Form = Form::Generic;
    /// -p^-1 mod 2^64.
    const N0: u64 = n0(Self::P[0]);
    /// R mod p, the Montgomery form of one.
    const ONE: [u64; N] = shift_mod(&one(), &Self::P, 64 * N);
    /// R^2 mod p.
    const R2: [u64; N] = shift_mod(&one(), &Self::P, 128 * N);
}

/// The shape of a modulus that its reduction step exploits.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Form {
    Generic,
    /// p = 2^256 - 2^224 + 2^192 + 2^96 - 1. Its low word is 2^64 - 1, so
    /// -p^-1 = 1 mod 2^64 and a step's quotient digit is the low word q
    /// itself; t0 + q (2^64 - 1) + q (2^32 - 1) 2^64 leaves q 2^32 at word
    /// one, and only q p3 needs a multiplication (Gueron and Krasnov, "Fast
    /// prime field elliptic-curve cryptography with 256-bit primes", J.
    /// Cryptogr. Eng. 2015).
    P256,
}

/// x * R mod p, the Montgomery form of an integer x < p, for constants.
pub(crate) const fn encode<const N: usize>(x: &[u64; N], p: &[u64; N]) -> [u64; N] {
    shift_mod(x, p, 64 * N)
}

/// A residue modulo `M` in Montgomery form, below `M::P`.
pub(crate) struct Residue<M: Modulus<N>, const N: usize>([u64; N], PhantomData<M>);

impl<M: Modulus<N>, const N: usize> Clone for Residue<M, N> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<M: Modulus<N>, const N: usize> Copy for Residue<M, N> {}
// SAFETY: words and a zero-sized marker, and zero is a valid residue.
unsafe impl<M: Modulus<N>, const N: usize> crate::secret::Zeroable for Residue<M, N> {}

impl<M: Modulus<N>, const N: usize> Residue<M, N> {
    /// Words already in Montgomery form and below p: a compile-time constant
    /// from [`encode`].
    pub(crate) const fn from_montgomery(words: [u64; N]) -> Self {
        Self(words, PhantomData)
    }
    pub(crate) const fn zero() -> Self {
        Self([0; N], PhantomData)
    }
    pub(crate) const fn one() -> Self {
        Self(M::ONE, PhantomData)
    }
    /// The canonical integer `x`, or `None` when x >= p. Only the answer
    /// depends on x.
    pub(crate) fn from_integer(x: &[u64; N]) -> Option<Self> {
        if below(x, &M::P) != 1 {
            return None;
        }
        Some(Self::from_reduced(x))
    }
    /// An integer x < 2^(64N) < 2p, reduced once.
    pub(crate) fn from_wide(x: &[u64; N]) -> Self {
        Self(reduce(x, 0, &M::P), PhantomData).mul(&Self(M::R2, PhantomData))
    }
    fn from_reduced(x: &[u64; N]) -> Self {
        Self(*x, PhantomData).mul(&Self(M::R2, PhantomData))
    }
    /// The canonical integer this residue represents.
    pub(crate) fn integer(&self) -> [u64; N] {
        let one = Self(one(), PhantomData);
        self.mul(&one).0
    }
    pub(crate) fn add(&self, b: &Self) -> Self {
        let mut v = [0; N];
        let mut carry = false;
        for i in 0..N {
            (v[i], carry) = carrying_add(self.0[i], b.0[i], carry);
        }
        Self(reduce(&v, u64::from(carry), &M::P), PhantomData)
    }
    pub(crate) fn double(&self) -> Self {
        self.add(self)
    }
    pub(crate) fn sub(&self, b: &Self) -> Self {
        let mut v = [0; N];
        let mut borrow = false;
        for i in 0..N {
            (v[i], borrow) = borrowing_sub(self.0[i], b.0[i], borrow);
        }
        let mask = ct::mask(u64::from(borrow));
        let mut carry = false;
        for i in 0..N {
            (v[i], carry) = carrying_add(v[i], M::P[i] & mask, carry);
        }
        Self(v, PhantomData)
    }
    /// self * b / R mod p, word by word with each product row followed by its
    /// reduction step (CIOS; Koc, Acar and Kaliski, "Analyzing and comparing
    /// Montgomery multiplication algorithms", IEEE Micro 1996).
    pub(crate) fn mul(&self, b: &Self) -> Self {
        let (t, high) = product::<M, N>(&self.0, &b.0);
        Self(reduce(&t, high, &M::P), PhantomData)
    }
    pub(crate) fn square(&self) -> Self {
        self.mul(self)
    }
    /// One when self is zero, otherwise zero.
    pub(crate) fn is_zero(&self) -> u64 {
        let mut d = 0;
        for word in self.0 {
            d |= word
        }
        1 ^ ct::nonzero(d)
    }
    pub(crate) fn eq(&self, b: &Self) -> u64 {
        let mut d = 0;
        for i in 0..N {
            d |= self.0[i] ^ b.0[i]
        }
        1 ^ ct::nonzero(d)
    }
    pub(crate) fn select(a: &Self, b: &Self, bit: u64) -> Self {
        let m = ct::mask(bit);
        let mut v = [0; N];
        for i in 0..N {
            v[i] = (a.0[i] & !m) | (b.0[i] & m);
        }
        Self(v, PhantomData)
    }
    /// self^(p-2), the inverse of a nonzero residue, by square-and-multiply
    /// over the public exponent.
    pub(crate) fn invert(&self) -> Self {
        let mut e = M::P;
        let mut borrow = false;
        (e[0], borrow) = borrowing_sub(e[0], 2, borrow);
        for word in &mut e[1..] {
            (*word, borrow) = borrowing_sub(*word, 0, borrow);
        }
        let mut r = Self::one();
        for i in (0..N * 64).rev() {
            r = r.square();
            if (e[i / 64] >> (i % 64)) & 1 == 1 {
                r = r.mul(self);
            }
        }
        r
    }
}

/// One when x < p, otherwise zero; the time does not depend on x.
pub(crate) fn below<const N: usize>(x: &[u64; N], p: &[u64; N]) -> u64 {
    let mut borrow = false;
    for i in 0..N {
        (_, borrow) = borrowing_sub(x[i], p[i], borrow);
    }
    u64::from(borrow)
}

/// Big-endian bytes, exactly 8N of them, as little-endian words.
pub(crate) fn from_be<const N: usize>(bytes: &[u8]) -> [u64; N] {
    assert_eq!(bytes.len(), N * 8);
    let mut words = [0; N];
    for (i, word) in words.iter_mut().enumerate() {
        let end = bytes.len() - 8 * i;
        *word = u64::from_be_bytes(bytes[end - 8..end].try_into().unwrap());
    }
    words
}

pub(crate) fn to_be<const N: usize>(words: &[u64; N], bytes: &mut [u8]) {
    assert_eq!(bytes.len(), N * 8);
    for (i, word) in words.iter().enumerate() {
        let end = bytes.len() - 8 * i;
        bytes[end - 8..end].copy_from_slice(&word.to_be_bytes())
    }
}

/// a b / R as N words and a top bit, below 2p: each product row followed by
/// its reduction step (CIOS).
#[inline(always)]
const fn product<M: Modulus<N>, const N: usize>(a: &[u64; N], b: &[u64; N]) -> ([u64; N], u64) {
    let mut t = [0u64; N];
    let mut high = 0u64;
    let mut i = 0;
    while i < N {
        let mut c = 0;
        let mut j = 0;
        while j < N {
            (t[j], c) = mac(t[j], a[i], b[j], c);
            j += 1;
        }
        let (top, overflow) = high.overflowing_add(c);
        let q = t[0].wrapping_mul(M::N0);
        let mut c;
        match M::FORM {
            Form::Generic => {
                (_, c) = mac(t[0], q, M::P[0], 0);
                let mut j = 1;
                while j < N {
                    (t[j - 1], c) = mac(t[j], q, M::P[j], c);
                    j += 1;
                }
            }
            Form::P256 => {
                let carry;
                (t[0], carry) = t[1].overflowing_add(q << 32);
                (t[1], c) = mac(t[2], q >> 32, 1, carry as u64);
                (t[2], c) = mac(t[3], q, M::P[3], c);
            }
        }
        let (top, carry) = top.overflowing_add(c);
        t[N - 1] = top;
        // t < 2p after each step, so at most one of the two is set.
        high = (overflow | carry) as u64;
        i += 1;
    }
    (t, high)
}

/// v mod p for v + high * 2^(64N) < 2p: p subtracted under a mask.
fn reduce<const N: usize>(v: &[u64; N], high: u64, p: &[u64; N]) -> [u64; N] {
    let mut d = [0; N];
    let mut borrow = false;
    for i in 0..N {
        (d[i], borrow) = borrowing_sub(v[i], p[i], borrow);
    }
    let m = ct::mask(ct::nonzero(high) | (1 ^ u64::from(borrow)));
    let mut out = [0; N];
    for i in 0..N {
        out[i] = (v[i] & !m) | (d[i] & m);
    }
    out
}

/// (lo, hi) of t + a * b + c, which cannot exceed 2^128 - 1.
#[inline(always)]
pub(crate) const fn mac(t: u64, a: u64, b: u64, c: u64) -> (u64, u64) {
    let v = (a as u128)
        .wrapping_mul(b as u128)
        .wrapping_add(t as u128)
        .wrapping_add(c as u128);
    (v as u64, (v >> 64) as u64)
}

#[inline(always)]
fn carrying_add(a: u64, b: u64, carry: bool) -> (u64, bool) {
    let (s, c1) = a.overflowing_add(b);
    let (s, c2) = s.overflowing_add(u64::from(carry));
    (s, c1 | c2)
}

#[inline(always)]
fn borrowing_sub(a: u64, b: u64, borrow: bool) -> (u64, bool) {
    let (d, b1) = a.overflowing_sub(b);
    let (d, b2) = d.overflowing_sub(u64::from(borrow));
    (d, b1 | b2)
}

const fn one<const N: usize>() -> [u64; N] {
    let mut x = [0; N];
    x[0] = 1;
    x
}

/// -p0^-1 mod 2^64 for an odd p0.
pub(crate) const fn n0(p0: u64) -> u64 {
    // Newton's iteration doubles the correct low bits of p0^-1 from 1 to 64.
    let mut inverse = 1u64;
    let mut i = 0;
    while i < 6 {
        inverse = inverse.wrapping_mul(2u64.wrapping_sub(p0.wrapping_mul(inverse)));
        i += 1;
    }
    inverse.wrapping_neg()
}

/// x * 2^k mod p for x < p, by k modular doublings, at compile time.
const fn shift_mod<const N: usize>(x: &[u64; N], p: &[u64; N], k: usize) -> [u64; N] {
    assert!(p[0] & 1 == 1 && p[N - 1] >> 63 == 1);
    let mut x = *x;
    let mut step = 0;
    while step < k {
        let mut carry = 0;
        let mut i = 0;
        while i < N {
            let top = x[i] >> 63;
            x[i] = (x[i] << 1) | carry;
            carry = top;
            i += 1;
        }
        let mut d = [0; N];
        let mut borrow = 0u64;
        let mut i = 0;
        while i < N {
            let (t, b1) = x[i].overflowing_sub(p[i]);
            let (t, b2) = t.overflowing_sub(borrow);
            d[i] = t;
            borrow = (b1 | b2) as u64;
            i += 1;
        }
        if carry == 1 || borrow == 0 {
            x = d;
        }
        step += 1;
    }
    x
}
