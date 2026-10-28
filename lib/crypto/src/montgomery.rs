//! Montgomery arithmetic modulo the NIST curves' primes and orders, N = 4 or 6
//! words. Each modulus is a type, so a residue modulo one cannot meet a residue
//! modulo another, and a computation names a second type of the same p only
//! to run on its arithmetic (`Modulus::dispatch`). Every operation on a
//! residue has a fixed schedule: masked final subtractions, and inversion by
//! a fixed count of divsteps; only `Residue::inverse_of_public`, for public
//! integers, does not. Residues are `Copy` intermediates, not erased
//! (`secret`'s contract).
#![allow(
    clippy::needless_range_loop,
    reason = "carry chains index parallel fixed-size arrays; iterator adaptors stay out of line in the shipped profile"
)]
use crate::ct;
use core::marker::PhantomData;

mod divsteps;

/// An odd modulus p with 2^(64N-1) < p < 2^(64N), its Montgomery constants
/// for R = 2^(64N), evaluated at compile time, and the arithmetic on its
/// residues. Each operation takes and returns words below p, on a fixed
/// schedule; a modulus with a faster form of one overrides it.
pub(crate) trait Modulus<const N: usize>: Sized + 'static {
    const P: [u64; N];
    /// How a reduction step forms q p.
    const FORM: Form = Form::Generic;
    /// -p^-1 mod 2^64.
    const N0: u64 = n0(Self::P[0]);
    /// R mod p, the Montgomery form of one.
    const ONE: [u64; N] = shift_mod(&one(), &Self::P, 64 * N);
    /// R^2 mod p.
    const R2: [u64; N] = shift_mod(&one(), &Self::P, 128 * N);

    fn add(a: &[u64; N], b: &[u64; N]) -> [u64; N] {
        let mut v = [0; N];
        let mut carry = false;
        for i in 0..N {
            (v[i], carry) = carrying_add(a[i], b[i], carry);
        }
        reduce(&v, u64::from(carry), &Self::P)
    }
    #[inline(always)]
    fn double(a: &[u64; N]) -> [u64; N] {
        Self::add(a, a)
    }
    fn sub(a: &[u64; N], b: &[u64; N]) -> [u64; N] {
        let mut v = [0; N];
        let mut borrow = false;
        for i in 0..N {
            (v[i], borrow) = borrowing_sub(a[i], b[i], borrow);
        }
        let mask = ct::mask(u64::from(borrow));
        let mut carry = false;
        for i in 0..N {
            (v[i], carry) = carrying_add(v[i], Self::P[i] & mask, carry);
        }
        v
    }
    /// a b / R mod p, word by word with each product row followed by its
    /// reduction step (CIOS; Koc, Acar and Kaliski, "Analyzing and comparing
    /// Montgomery multiplication algorithms", IEEE Micro 1996).
    fn mul(a: &[u64; N], b: &[u64; N]) -> [u64; N] {
        let (t, high) = product::<Self, N>(a, b);
        reduce(&t, high, &Self::P)
    }
    /// a^2 / R mod p: the N(N - 1)/2 cross products once, doubled, the N
    /// squares added, then N reduction steps over the 2N-word square.
    fn square(a: &[u64; N]) -> [u64; N] {
        let (t, high) = redc::<Self, N>(&mut square_wide(a));
        reduce(&t, high, &Self::P)
    }
    /// The words of table[index] for a secret index, or table[0] for an
    /// index past 15, reading every entry under masks: the table's order
    /// and length are public.
    #[inline(always)]
    fn select<T, const W: usize>(table: &[T; 16], words: impl Fn(&T) -> [&[u64; N]; W], index: u64) -> [[u64; N]; W] {
        select(table, words, index)
    }
    /// `computation` over the fastest arithmetic modulo p that the CPU
    /// admits: this modulus's own unless it says otherwise.
    #[inline(always)]
    fn dispatch<C: Computation<N>>(computation: C) -> C::Output {
        computation.run::<Self>()
    }
}

/// A computation over residues modulo one p, generic over the modulus type
/// whose arithmetic computes them; [`Modulus::dispatch`] chooses it, and
/// every type it passes has the same p.
pub(crate) trait Computation<const N: usize> {
    type Output;
    fn run<F: Modulus<N>>(self) -> Self::Output;
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
    /// from [`encode`], or a residue's own [`Self::words`].
    pub(crate) const fn from_montgomery(words: [u64; N]) -> Self {
        Self(words, PhantomData)
    }
    /// The Montgomery-form words, for [`Modulus::select`].
    pub(crate) const fn words(&self) -> &[u64; N] {
        &self.0
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
    #[inline(always)]
    pub(crate) fn add(&self, b: &Self) -> Self {
        Self(M::add(&self.0, &b.0), PhantomData)
    }
    #[inline(always)]
    pub(crate) fn double(&self) -> Self {
        Self(M::double(&self.0), PhantomData)
    }
    #[inline(always)]
    pub(crate) fn sub(&self, b: &Self) -> Self {
        Self(M::sub(&self.0, &b.0), PhantomData)
    }
    #[inline(always)]
    pub(crate) fn mul(&self, b: &Self) -> Self {
        Self(M::mul(&self.0, &b.0), PhantomData)
    }
    #[inline(always)]
    pub(crate) fn square(&self) -> Self {
        Self(M::square(&self.0), PhantomData)
    }
    /// The same residue under another modulus type with the same p: the
    /// type whose arithmetic [`Modulus::dispatch`] chose.
    #[inline(always)]
    pub(crate) fn cast<F: Modulus<N>>(&self) -> Residue<F, N> {
        const { assert!(same(&M::P, &F::P)) };
        Residue(self.0, PhantomData)
    }
    /// The `const` forms of [`Self::mul`], [`Self::add`] and [`Self::sub`],
    /// for tables built at compile time and for tests: not for secrets. They
    /// lack the register barrier, and `minus` branches on its borrow.
    pub(crate) const fn times(&self, b: &Self) -> Self {
        let (t, high) = product::<M, N>(&self.0, &b.0);
        Self(reduce_const(&t, high, &M::P), PhantomData)
    }
    pub(crate) const fn plus(&self, b: &Self) -> Self {
        let mut v = [0; N];
        let mut carry = false;
        let mut i = 0;
        while i < N {
            (v[i], carry) = carrying_add(self.0[i], b.0[i], carry);
            i += 1;
        }
        Self(reduce_const(&v, carry as u64, &M::P), PhantomData)
    }
    pub(crate) const fn minus(&self, b: &Self) -> Self {
        let mut v = [0; N];
        let mut borrow = false;
        let mut i = 0;
        while i < N {
            (v[i], borrow) = borrowing_sub(self.0[i], b.0[i], borrow);
            i += 1;
        }
        if borrow {
            let mut carry = false;
            let mut i = 0;
            while i < N {
                (v[i], carry) = carrying_add(v[i], M::P[i], carry);
                i += 1;
            }
        }
        Self(v, PhantomData)
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
    /// self^(p - 2) by 4-bit windows, the inverse of a nonzero residue: the
    /// `const` form of [`Self::invert`], for compile time and tests, as
    /// [`Self::times`].
    pub(crate) const fn inverted(&self) -> Self {
        let e = exponent::<M, N>();
        let mut powers = [Self::one(); 16];
        let mut j = 1;
        while j < 16 {
            powers[j] = powers[j - 1].times(self);
            j += 1;
        }
        let mut r = Self::one();
        let mut nibble = N * 16;
        while nibble > 0 {
            nibble -= 1;
            let mut k = 0;
            while k < 4 {
                r = r.times(&r);
                k += 1;
            }
            let digit = (e[nibble / 16] >> (4 * (nibble % 16))) & 15;
            if digit != 0 {
                r = r.times(&powers[digit as usize]);
            }
        }
        r
    }
    /// The inverse of a nonzero residue, and zero for zero, by divsteps; the
    /// time does not depend on the residue.
    pub(crate) fn invert(&self) -> Self {
        // (x R)^-1 R^2 = x^-1 R.
        Self(divsteps::invert::<M, N>(&self.0, &M::R2), PhantomData)
    }
    /// The residue x^-1 for a public integer 0 < x < p, in variable time.
    pub(crate) fn inverse_of_public(x: &[u64; N]) -> Self {
        // x^-1 R is x^-1's Montgomery form.
        Self(divsteps::invert_public::<M, N>(x, &M::ONE), PhantomData)
    }
}

/// p - 2 for M's p.
const fn exponent<M: Modulus<N>, const N: usize>() -> [u64; N] {
    let mut e = M::P;
    let mut borrow;
    (e[0], borrow) = borrowing_sub(e[0], 2, false);
    let mut i = 1;
    while i < N {
        (e[i], borrow) = borrowing_sub(e[i], 0, borrow);
        i += 1;
    }
    e
}

/// P-256's field arithmetic written out word by word, `P256Field`'s: the
/// shipped profile does not unroll the generic loops. Every step is the
/// generic one's (`product`, `square_wide`, `redc`, `reduce`).
pub(crate) mod p256 {
    use super::{Form, Modulus, ct, mac};
    use crate::nist::P256Field;

    const P: [u64; 4] = <P256Field as Modulus<4>>::P;
    const _: () = assert!(matches!(<P256Field as Modulus<4>>::FORM, Form::P256));

    #[inline(always)]
    fn adc(a: u64, b: u64, carry: u64) -> (u64, u64) {
        let (s, c1) = a.overflowing_add(b);
        let (s, c2) = s.overflowing_add(carry);
        (s, (c1 | c2) as u64)
    }

    #[inline(always)]
    fn sbb(a: u64, b: u64, borrow: u64) -> (u64, u64) {
        let (d, b1) = a.overflowing_sub(b);
        let (d, b2) = d.overflowing_sub(borrow);
        (d, (b1 | b2) as u64)
    }

    /// v mod p for v + high 2^256 < 2p, p subtracted under a mask.
    #[inline(always)]
    fn reduce(v: [u64; 4], high: u64) -> [u64; 4] {
        let (d0, b) = sbb(v[0], P[0], 0);
        let (d1, b) = sbb(v[1], P[1], b);
        let (d2, b) = sbb(v[2], P[2], b);
        let (d3, b) = sbb(v[3], P[3], b);
        let m = ct::mask(ct::nonzero(high) | (1 ^ b));
        [(v[0] & !m) | (d0 & m), (v[1] & !m) | (d1 & m), (v[2] & !m) | (d2 & m), (v[3] & !m) | (d3 & m)]
    }

    #[inline(always)]
    pub(crate) fn add(a: &[u64; 4], b: &[u64; 4]) -> [u64; 4] {
        let (v0, c) = adc(a[0], b[0], 0);
        let (v1, c) = adc(a[1], b[1], c);
        let (v2, c) = adc(a[2], b[2], c);
        let (v3, c) = adc(a[3], b[3], c);
        reduce([v0, v1, v2, v3], c)
    }

    #[inline(always)]
    pub(crate) fn sub(a: &[u64; 4], b: &[u64; 4]) -> [u64; 4] {
        let (v0, w) = sbb(a[0], b[0], 0);
        let (v1, w) = sbb(a[1], b[1], w);
        let (v2, w) = sbb(a[2], b[2], w);
        let (v3, w) = sbb(a[3], b[3], w);
        let m = ct::mask(w);
        let (r0, c) = adc(v0, P[0] & m, 0);
        let (r1, c) = adc(v1, P[1] & m, c);
        let (r2, c) = adc(v2, P[2] & m, c);
        let (r3, _) = adc(v3, P[3] & m, c);
        [r0, r1, r2, r3]
    }

    /// CIOS with P-256's reduction step, as `product`; out of line in the
    /// point formulas.
    #[inline(never)]
    pub(crate) fn mul(a: &[u64; 4], b: &[u64; 4]) -> [u64; 4] {
        let (mut t0, mut t1, mut t2, mut t3, mut high) = (0u64, 0u64, 0u64, 0u64, 0u64);
        macro_rules! row {
            ($ai:expr) => {{
                let ai = $ai;
                let (x0, c) = mac(t0, ai, b[0], 0);
                let (x1, c) = mac(t1, ai, b[1], c);
                let (x2, c) = mac(t2, ai, b[2], c);
                let (x3, c) = mac(t3, ai, b[3], c);
                let (top, overflow) = adc(high, c, 0);
                // q = x0; x0 + q (2^64 - 1) + q (2^32 - 1) 2^64 leaves q 2^32
                // at word one, and q p3 lands at word three.
                let (y0, c) = adc(x1, x0 << 32, 0);
                let (y1, c) = adc(x2, x0 >> 32, c);
                let (y2, c) = mac(x3, x0, P[3], c);
                let (y3, carry) = adc(top, c, 0);
                (t0, t1, t2, t3, high) = (y0, y1, y2, y3, overflow | carry);
            }};
        }
        row!(a[0]);
        row!(a[1]);
        row!(a[2]);
        row!(a[3]);
        reduce([t0, t1, t2, t3], high)
    }

    /// The cross products once, doubled, the squares added, then four
    /// reduction steps, as `square_wide` and `redc`; out of line.
    #[inline(never)]
    pub(crate) fn square(a: &[u64; 4]) -> [u64; 4] {
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
        let mut t = [t0, t1, t2, t3, t4, t5, t6, t7];
        let mut extra = 0u64;
        macro_rules! step {
            ($i:literal) => {{
                let q = t[$i];
                let (v, c) = adc(t[$i + 1], q << 32, 0);
                t[$i + 1] = v;
                let (v, c) = adc(t[$i + 2], q >> 32, c);
                t[$i + 2] = v;
                let (lo, hi) = mac(0, q, P[3], 0);
                let (v, c) = adc(t[$i + 3], lo, c);
                t[$i + 3] = v;
                let (v, mut c) = adc(t[$i + 4], hi, c);
                t[$i + 4] = v;
                let mut k = $i + 5;
                while k < 8 {
                    (t[k], c) = adc(t[k], c, 0);
                    k += 1;
                }
                extra |= c;
            }};
        }
        step!(0);
        step!(1);
        step!(2);
        step!(3);
        reduce([t[4], t[5], t[6], t[7]], extra)
    }
}

/// [`Modulus::select`]'s portable form, by `ct` masks.
#[inline(always)]
pub(crate) fn select<T, const N: usize, const W: usize>(
    table: &[T; 16],
    words: impl Fn(&T) -> [&[u64; N]; W],
    index: u64,
) -> [[u64; N]; W] {
    let mut chosen = words(&table[0]).map(|w| *w);
    // Indexing, not an iterator adaptor, which the shipped profile keeps out
    // of line.
    for j in 1..16 {
        let m = ct::mask(1 ^ ct::nonzero(index ^ j as u64));
        let entry = words(&table[j]);
        for w in 0..W {
            for i in 0..N {
                chosen[w][i] = (chosen[w][i] & !m) | (entry[w][i] & m);
            }
        }
    }
    chosen
}

/// Whether two moduli are one p, at compile time.
const fn same<const N: usize>(a: &[u64; N], b: &[u64; N]) -> bool {
    let mut i = 0;
    while i < N {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
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

/// The 2N-word square of a, in a buffer sized for N <= 6.
#[inline(always)]
const fn square_wide<const N: usize>(a: &[u64; N]) -> [u64; 12] {
    let mut t = [0u64; 12];
    let mut i = 0;
    while i < N {
        let mut c = 0;
        let mut j = i + 1;
        while j < N {
            (t[i + j], c) = mac(t[i + j], a[i], a[j], c);
            j += 1;
        }
        t[i + N] = c;
        i += 1;
    }
    // The cross products sum below 2^(128N - 1), so doubling loses no bit.
    let mut top = 0;
    let mut i = 0;
    while i < 2 * N {
        let next = t[i] >> 63;
        t[i] = t[i] << 1 | top;
        top = next;
        i += 1;
    }
    let mut c = 0u64;
    let mut i = 0;
    while i < N {
        let (lo, hi) = mac(0, a[i], a[i], 0);
        let v = (t[2 * i] as u128).wrapping_add(lo as u128).wrapping_add(c as u128);
        t[2 * i] = v as u64;
        let v = (t[2 * i + 1] as u128).wrapping_add(hi as u128).wrapping_add(v >> 64);
        t[2 * i + 1] = v as u64;
        c = (v >> 64) as u64;
        i += 1;
    }
    t
}

/// t / R mod 2p for a 2N-word t < p^2: N reduction steps, each adding the
/// multiple of p that clears the lowest remaining word, then the top N words
/// and a top bit.
#[inline(always)]
const fn redc<M: Modulus<N>, const N: usize>(t: &mut [u64; 12]) -> ([u64; N], u64) {
    // The total stays below 2^(128N + 1), so at most one step carries out of
    // the top word.
    let mut extra = 0u64;
    let mut i = 0;
    while i < N {
        let mut c: u64;
        let mut k;
        match M::FORM {
            Form::Generic => {
                let q = t[i].wrapping_mul(M::N0);
                c = 0;
                let mut j = 0;
                while j < N {
                    (t[i + j], c) = mac(t[i + j], q, M::P[j], c);
                    j += 1;
                }
                k = i + N;
            }
            Form::P256 => {
                // t_i + q (2^64 - 1) = q 2^64 with q = t_i, and with q (2^32 - 1)
                // at the next word that is q 2^32 there; q p3 lands two words up.
                let q = t[i];
                t[i] = 0;
                let v = (t[i + 1] as u128).wrapping_add((q << 32) as u128);
                t[i + 1] = v as u64;
                let v = (t[i + 2] as u128).wrapping_add((q >> 32) as u128).wrapping_add(v >> 64);
                t[i + 2] = v as u64;
                let (lo, hi) = mac(0, q, M::P[3], 0);
                let v = (t[i + 3] as u128).wrapping_add(lo as u128).wrapping_add(v >> 64);
                t[i + 3] = v as u64;
                let v = (t[i + 4] as u128).wrapping_add(hi as u128).wrapping_add(v >> 64);
                t[i + 4] = v as u64;
                c = (v >> 64) as u64;
                k = i + 5;
            }
        }
        while k < 2 * N {
            let (sum, carry) = t[k].overflowing_add(c);
            t[k] = sum;
            c = carry as u64;
            k += 1;
        }
        extra |= c;
        i += 1;
    }
    let mut out = [0; N];
    let mut j = 0;
    while j < N {
        out[j] = t[N + j];
        j += 1;
    }
    (out, extra)
}

/// [`reduce`] for constants: the same arithmetic, without the register
/// barrier.
const fn reduce_const<const N: usize>(v: &[u64; N], high: u64, p: &[u64; N]) -> [u64; N] {
    let mut d = [0; N];
    let mut borrow = false;
    let mut i = 0;
    while i < N {
        (d[i], borrow) = borrowing_sub(v[i], p[i], borrow);
        i += 1;
    }
    let take = 0u64.wrapping_sub((high != 0 || !borrow) as u64);
    let mut out = [0; N];
    let mut i = 0;
    while i < N {
        out[i] = (v[i] & !take) | (d[i] & take);
        i += 1;
    }
    out
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
const fn carrying_add(a: u64, b: u64, carry: bool) -> (u64, bool) {
    let (s, c1) = a.overflowing_add(b);
    let (s, c2) = s.overflowing_add(carry as u64);
    (s, c1 | c2)
}

#[inline(always)]
const fn borrowing_sub(a: u64, b: u64, borrow: bool) -> (u64, bool) {
    let (d, b1) = a.overflowing_sub(b);
    let (d, b2) = d.overflowing_sub(borrow as u64);
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

