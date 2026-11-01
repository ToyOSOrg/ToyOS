//! SEC 1 NIST P-256/P-384 arithmetic. Secret scalar multiplication (ECDH and
//! key generation) runs Booth's signed 5-bit windows on a fixed schedule:
//! P-256's base point through a compile-time comb, other points through a
//! per-call table and doublings. Table reads scan every entry under masks,
//! and the scalar is first moved to an odd representative k + j n, so the
//! running point is never the identity. SEC 1 decoding, DER decoding, ECDSA
//! verification and key-validity refusals depend on public data;
//! verification multiplies in variable time, through `PublicScalar` only.
//! Only uncompressed SEC 1 points are accepted.
#![allow(
    clippy::needless_range_loop,
    reason = "masked selects index parallel fixed-size arrays, as `montgomery` does"
)]

use crate::{
    Error,
    montgomery::{self, Form, Modulus, Residue, below, from_be, to_be},
    secret::{Workspace, wipe64},
};

/// A short Weierstrass curve y^2 = x^3 - 3x + b of prime order and cofactor
/// one, with its constants in Montgomery form at compile time.
pub(crate) trait Curve<const N: usize>: Sized + 'static {
    type Field: Modulus<N>;
    type Order: Modulus<N>;
    const B: [u64; N];
    const GX: [u64; N];
    const GY: [u64; N];
    /// m of the representative k + j n, j the one of m and m + 1 that makes
    /// it odd, that secret multiplication runs on: the smallest m for which
    /// every such representative sets a bit of the top Booth window, so the
    /// first window's digit is nonzero.
    const OFFSET: u64;
    /// G, 3G, ..., 63G, for verification's width-7 recoding of u1.
    fn odd_multiples_of_g() -> &'static [Affine<Self, N>; 32];
    /// scalar G, on a fixed schedule.
    fn base_mul(scalar: &Scalar<Self, N>) -> Point<Self, N> {
        Point::base().mul(scalar)
    }
}
type Coord<C, const N: usize> = Residue<<C as Curve<N>>::Field, N>;
type ModN<C, const N: usize> = Residue<<C as Curve<N>>::Order, N>;

/// A scalar of curve `C` below n, erased on drop. A secret one is never zero.
pub(crate) struct Scalar<C: Curve<N>, const N: usize>([u64; N], core::marker::PhantomData<C>);
impl<C: Curve<N>, const N: usize> Drop for Scalar<C, N> {
    fn drop(&mut self) {
        wipe64(&mut self.0)
    }
}
impl<C: Curve<N>, const N: usize> Scalar<C, N> {
    /// One when 0 < x < n, otherwise zero; the time does not depend on x.
    fn valid(x: &[u64; N]) -> u64 {
        let mut d = 0;
        for word in x {
            d |= word
        }
        below(x, &C::Order::P) & crate::ct::nonzero(d)
    }
    /// Big-endian bytes, 8N of them. Only validity depends on their value.
    pub(crate) fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() != N * 8 {
            return Err(Error::Length);
        }
        let s = Self(from_be(bytes), core::marker::PhantomData);
        if Self::valid(&s.0) != 1 {
            return Err(Error::InvalidKey);
        }
        Ok(s)
    }
    /// The first valid candidate of `candidates`, 8N big-endian bytes each, on
    /// a schedule that discloses only whether any is valid.
    pub(crate) fn first_valid(candidates: &[u8]) -> Result<Self, Error> {
        assert_eq!(candidates.len() % (N * 8), 0);
        let mut scalar = Self([0; N], core::marker::PhantomData);
        let mut selected = 0;
        for i in 0..candidates.len() / (N * 8) {
            let bytes = &candidates[i * N * 8..(i + 1) * N * 8];
            let mut candidate = from_be::<N>(bytes);
            let take = crate::ct::mask(Self::valid(&candidate) & (1 ^ selected));
            for j in 0..N {
                scalar.0[j] = (scalar.0[j] & !take) | (candidate[j] & take);
            }
            selected |= take & 1;
            wipe64(&mut candidate);
        }
        if selected != 1 {
            return Err(Error::Entropy);
        }
        Ok(scalar)
    }
    /// k + j n for the one j of m = `C::OFFSET` and m + 1 that makes it odd:
    /// an integer with the same multiple of every point, below 2^(64N + 2), in
    /// N + 1 of the buffer's words. Erased when dropped.
    fn representative(&self) -> Workspace<[u64; 7]> {
        let multiplier = C::OFFSET.wrapping_add((C::OFFSET ^ self.0[0] ^ 1) & 1);
        let mut k = Workspace([0u64; 7]);
        let mut carry = 0u64;
        for i in 0..N {
            let (lo, hi) = montgomery::mac(self.0[i], C::Order::P[i], multiplier, carry);
            k.0[i] = lo;
            carry = hi;
        }
        k.0[N] = carry;
        k
    }
}

/// Booth digit i of a W-word integer, in [-16, 16]: with u the six bits
/// 5i - 1 ..= 5i + 4 (bit -1 and bits past the top are zero),
/// d = (u + 1) / 2 - 32 u_5, so that the integer is the sum of d_i 2^(5i)
/// while its bit 5i + 4 of the top window is clear. The index is public; the
/// arithmetic does not branch.
fn booth<const W: usize>(words: &[u64; W], i: usize) -> i8 {
    let bits = |from: usize| -> u64 {
        let (word, shift) = (from / 64, from % 64);
        let low = if word < W { words[word] >> shift } else { 0 };
        let high = if shift > 58 && word + 1 < W { words[word + 1] << (64 - shift) } else { 0 };
        (low | high) & 63
    };
    let u = if i == 0 { bits(0) << 1 & 63 } else { bits(5 * i - 1) };
    ((u.wrapping_add(1) >> 1) as i8).wrapping_sub(((u >> 5) << 5) as i8)
}

/// |d| and the sign bit of a Booth digit, by arithmetic.
fn magnitude(d: i8) -> (u64, u64) {
    let negative = u64::from((d as u8) >> 7);
    let magnitude = ((d ^ 0i8.wrapping_sub(negative as i8)).wrapping_add(negative as i8)) as u64;
    (magnitude, negative)
}

/// A public scalar below n, for variable-time verification only; a secret
/// [`Scalar`] does not convert to it.
struct PublicScalar<const N: usize>([u64; N]);

impl<const N: usize> PublicScalar<N> {
    /// The width-w NAF; k < n < 2^(64N) - 2^7, and 385 digits hold N = 6.
    fn naf(&self, w: u32) -> [i8; 385] {
        let mut digits = [0i8; 385];
        crate::naf::naf(&self.0, w, &mut digits);
        digits
    }
}

/// The 5-bit Booth windows of a representative below 2^(64N + 2), whose top
/// window keeps its bit 5i + 4 clear.
const fn windows(n: usize) -> usize {
    (64 * n + 2) / 5 + 1
}

/// An affine point (x, y), a table entry.
pub(crate) struct Affine<C: Curve<N>, const N: usize> {
    x: Coord<C, N>,
    y: Coord<C, N>,
}
impl<C: Curve<N>, const N: usize> Clone for Affine<C, N> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<C: Curve<N>, const N: usize> Copy for Affine<C, N> {}

impl<C: Curve<N>, const N: usize> Affine<C, N> {
    /// row[|d| - 1], with y negated by mask when d < 0, reading every entry.
    /// For d = 0 it is row[0], which the caller discards by mask: no entry
    /// is the identity, so no arithmetic runs on it.
    fn lookup(row: &[Self; 16], d: i8) -> Self {
        let (magnitude, negative) = magnitude(d);
        let mut chosen = row[0];
        // Indexing, not an iterator adaptor, which the shipped profile keeps
        // out of line.
        for j in 1..16 {
            let take = 1 ^ crate::ct::nonzero(magnitude ^ (j as u64 + 1));
            chosen.x = Residue::select(&chosen.x, &row[j].x, take);
            chosen.y = Residue::select(&chosen.y, &row[j].y, take);
        }
        chosen.y = Residue::select(&chosen.y, &Residue::zero().sub(&chosen.y), negative);
        chosen
    }
    fn point(&self) -> Point<C, N> {
        Point {
            x: self.x,
            y: self.y,
            z: Residue::one(),
        }
    }
}

/// Compile-time point arithmetic, in Jacobian (X, Y, Z), for building
/// tables. None of its inputs is the identity, and no addition's operands
/// are equal or opposite.
struct Build<C: Curve<N>, const N: usize>(core::marker::PhantomData<C>);

impl<C: Curve<N>, const N: usize> Build<C, N> {
    const G: [Coord<C, N>; 3] = [
        Residue::from_montgomery(C::GX),
        Residue::from_montgomery(C::GY),
        Residue::one(),
    ];
    /// dbl-2001-b for a = -3, as [`Point::double`].
    const fn double(p: &[Coord<C, N>; 3]) -> [Coord<C, N>; 3] {
        let [x, y, z] = p;
        let delta = z.times(z);
        let gamma = y.times(y);
        let beta = x.times(&gamma);
        let t = x.minus(&delta).times(&x.plus(&delta));
        let alpha = t.plus(&t).plus(&t);
        let beta4 = beta.plus(&beta).plus(&beta.plus(&beta));
        let x3 = alpha.times(&alpha).minus(&beta4.plus(&beta4));
        let z3 = y.plus(z).times(&y.plus(z)).minus(&gamma).minus(&delta);
        let gamma2 = gamma.times(&gamma);
        let gamma8 = gamma2.plus(&gamma2).plus(&gamma2.plus(&gamma2));
        let y3 = alpha.times(&beta4.minus(&x3)).minus(&gamma8.plus(&gamma8));
        [x3, y3, z3]
    }
    /// The Jacobian addition of [`Point::add_incomplete`].
    const fn add(p: &[Coord<C, N>; 3], q: &[Coord<C, N>; 3]) -> [Coord<C, N>; 3] {
        let (z1, z2) = (p[2].times(&p[2]), q[2].times(&q[2]));
        let (u1, u2) = (p[0].times(&z2), q[0].times(&z1));
        let s1 = p[1].times(&q[2].times(&z2));
        let s2 = q[1].times(&p[2].times(&z1));
        let h = u2.minus(&u1);
        let r = s2.minus(&s1);
        let h2 = h.times(&h);
        let h3 = h.times(&h2);
        let u = u1.times(&h2);
        let x = r.times(&r).minus(&h3).minus(&u.plus(&u));
        let y = r.times(&u.minus(&x)).minus(&s1.times(&h3));
        let z = h.times(&p[2].times(&q[2]));
        [x, y, z]
    }
    /// Jacobian points in affine form, with one inversion for all of them
    /// (Montgomery's simultaneous-inversion trick).
    const fn normalize<const K: usize>(points: &[[Coord<C, N>; 3]; K]) -> [Affine<C, N>; K] {
        let mut prefix = [Residue::one(); K];
        prefix[0] = points[0][2];
        let mut i = 1;
        while i < K {
            prefix[i] = prefix[i - 1].times(&points[i][2]);
            i += 1;
        }
        let mut inverse = prefix[K - 1].inverted();
        let mut out = [Affine {
            x: Residue::zero(),
            y: Residue::zero(),
        }; K];
        let mut i = K;
        while i > 0 {
            i -= 1;
            let z_inverse = if i == 0 { inverse } else { inverse.times(&prefix[i - 1]) };
            inverse = inverse.times(&points[i][2]);
            let z2 = z_inverse.times(&z_inverse);
            out[i] = Affine {
                x: points[i][0].times(&z2),
                y: points[i][1].times(&z2.times(&z_inverse)),
            };
        }
        out
    }
    /// G, 3G, ..., (2K - 1)G.
    const fn odd_multiples<const K: usize>() -> [Affine<C, N>; K] {
        let twice = Self::double(&Self::G);
        let mut points = [Self::G; K];
        let mut i = 1;
        while i < K {
            points[i] = Self::add(&points[i - 1], &twice);
            i += 1;
        }
        Self::normalize(&points)
    }
    /// Row i holds j 2^(5i) G for j = 1..=16.
    const fn comb<const R: usize>() -> [[Affine<C, N>; 16]; R] {
        let mut points = [[Self::G; 16]; R];
        let mut base = Self::G;
        let mut i = 0;
        while i < R {
            points[i][0] = base;
            points[i][1] = Self::double(&base);
            let mut j = 2;
            while j < 16 {
                points[i][j] = Self::add(&points[i][j - 1], &base);
                j += 1;
            }
            let mut k = 0;
            while k < 5 {
                base = Self::double(&base);
                k += 1;
            }
            i += 1;
        }
        // One simultaneous inversion for every entry, in row order.
        let mut prefix = [[Residue::one(); 16]; R];
        let mut running = Residue::one();
        let mut i = 0;
        while i < 16 * R {
            running = running.times(&points[i / 16][i % 16][2]);
            prefix[i / 16][i % 16] = running;
            i += 1;
        }
        let mut inverse = running.inverted();
        let mut out = [[Affine {
            x: Residue::zero(),
            y: Residue::zero(),
        }; 16]; R];
        let mut i = 16 * R;
        while i > 0 {
            i -= 1;
            let p = &points[i / 16][i % 16];
            let z_inverse = if i == 0 { inverse } else { inverse.times(&prefix[(i - 1) / 16][(i - 1) % 16]) };
            inverse = inverse.times(&p[2]);
            let z2 = z_inverse.times(&z_inverse);
            out[i / 16][i % 16] = Affine {
                x: p[0].times(&z2),
                y: p[1].times(&z2.times(&z_inverse)),
            };
        }
        out
    }
}

/// P-256's comb: j 2^(5i) G for j = 1..=16 in 52 rows (52 KiB).
// Building it runs some ten thousand field multiplications at compile time.
#[allow(long_running_const_eval)]
static P256_COMB: [[Affine<P256, 4>; 16]; windows(4)] = Build::<P256, 4>::comb();
static P256_G_ODD: [Affine<P256, 4>; 32] = Build::<P256, 4>::odd_multiples();
static P384_G_ODD: [Affine<P384, 6>; 32] = Build::<P384, 6>::odd_multiples();

/// The odd multiples P, 3P, ..., 15P of a public point, for verification.
type OddMultiples<C, const N: usize> = [Point<C, N>; 8];

/// Jacobian coordinates (X : Y : Z) for x = X/Z^2, y = Y/Z^3; Z = 0 is the
/// point at infinity.
pub(crate) struct Point<C: Curve<N>, const N: usize> {
    x: Coord<C, N>,
    y: Coord<C, N>,
    z: Coord<C, N>,
}
impl<C: Curve<N>, const N: usize> Clone for Point<C, N> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<C: Curve<N>, const N: usize> Copy for Point<C, N> {}
// SAFETY: three residues, and zero is a valid point representation.
unsafe impl<C: Curve<N>, const N: usize> crate::secret::Zeroable for Point<C, N> {}
impl<C: Curve<N>, const N: usize> Point<C, N> {
    fn identity() -> Self {
        Self {
            x: Residue::zero(),
            y: Residue::one(),
            z: Residue::zero(),
        }
    }
    pub(crate) fn base() -> Self {
        Self {
            x: Residue::from_montgomery(C::GX),
            y: Residue::from_montgomery(C::GY),
            z: Residue::one(),
        }
    }
    fn select(a: &Self, b: &Self, bit: u64) -> Self {
        Self {
            x: Residue::select(&a.x, &b.x, bit),
            y: Residue::select(&a.y, &b.y, bit),
            z: Residue::select(&a.z, &b.z, bit),
        }
    }
    /// "dbl-2001-b" for a = -3 (Bernstein and Lange, Explicit-Formulas
    /// Database), 3M + 5S. Infinity doubles to Z = 0, and no point of a
    /// prime-order curve has y = 0.
    fn double(&self) -> Self {
        let delta = self.z.square();
        let gamma = self.y.square();
        let beta = self.x.mul(&gamma);
        let t = self.x.sub(&delta).mul(&self.x.add(&delta));
        let alpha = t.double().add(&t);
        let beta4 = beta.double().double();
        let x = alpha.square().sub(&beta4.double());
        let z = self.y.add(&self.z).square().sub(&gamma).sub(&delta);
        let gamma2 = gamma.square().double();
        let y = alpha.mul(&beta4.sub(&x)).sub(&gamma2.double().double());
        Self { x, y, z }
    }
    /// scalar * self on a fixed schedule, for a point of prime order n:
    /// Booth's signed 5-bit windows of the odd representative k' over the
    /// public multiples P, 2P, ..., 16P, each window five doublings and one
    /// addition of a masked table read. self is public; neither branches nor
    /// addresses depend on the scalar.
    ///
    /// Exceptional additions. Before the addition of window i the running
    /// point is 32 T P, for T the value of the digits above i, and
    /// k' = 2^(5i + 5) T + S with |S| <= 2^(5i + 4). As k' > 2^(5 top - 1)
    /// (`Curve::OFFSET`), T >= 1 for every i < top; and as k' < 4n, for i >= 1
    /// 32 T <= k' / 2^(5i) + 16 < 4n / 32 + 16. So 0 < 32 T -+ d < n for
    /// every |d| <= 16: the running point is not the identity and neither
    /// equals nor opposes dP, the addend or the discarded table[0]. At i = 0,
    /// 32 T = k' - d0 is a nonzero multiple of 32 below 4n + 16, and n is odd,
    /// so it is not a multiple of n: the running point is not the identity.
    /// That last addition can double, and does so by mask; it cannot cancel,
    /// since k' is not 0 mod n.
    pub(crate) fn mul(&self, scalar: &Scalar<C, N>) -> Self {
        let mut table = [*self; 16];
        table[1] = self.double();
        for j in 2..16 {
            table[j] = table[j - 1].add_public(self);
        }
        let k = scalar.representative();
        let top = windows(N) - 1;
        let mut r = Self::lookup(&table, booth(&k.0, top));
        for i in (0..top).rev() {
            for _ in 0..5 {
                r = r.double();
            }
            let d = booth(&k.0, i);
            let (mut sum, equal) = r.add_incomplete(&Self::lookup(&table, d));
            if i == 0 {
                sum = Self::select(&sum, &r.double(), equal);
            }
            r = Self::select(&r, &sum, crate::ct::nonzero(d as u8 as u64));
        }
        r
    }
    /// self + q for points that are not the identity, 12M + 4S, and one when
    /// they are equal (h = r = 0), where the formula fails.
    fn add_incomplete(&self, q: &Self) -> (Self, u64) {
        let z1 = self.z.square();
        let z2 = q.z.square();
        let u1 = self.x.mul(&z2);
        let u2 = q.x.mul(&z1);
        let s1 = self.y.mul(&q.z.mul(&z2));
        let s2 = q.y.mul(&self.z.mul(&z1));
        let h = u2.sub(&u1);
        let r = s2.sub(&s1);
        let h2 = h.square();
        let h3 = h.mul(&h2);
        let u = u1.mul(&h2);
        let x = r.square().sub(&h3).sub(&u.double());
        let y = r.mul(&u.sub(&x)).sub(&s1.mul(&h3));
        let z = h.mul(&self.z.mul(&q.z));
        (Self { x, y, z }, h.is_zero() & r.is_zero())
    }
    /// table[|d| - 1] for table[j] = (j + 1) P, with Y negated by mask for
    /// d < 0, reading every entry. For d = 0 it is table[0], which the caller
    /// discards by mask.
    fn lookup(table: &[Self; 16], d: i8) -> Self {
        let (magnitude, negative) = magnitude(d);
        let mut chosen = table[0];
        for j in 1..16 {
            chosen = Self::select(&chosen, &table[j], 1 ^ crate::ct::nonzero(magnitude ^ (j as u64 + 1)));
        }
        let negated = Residue::zero().sub(&chosen.y);
        chosen.y = Residue::select(&chosen.y, &negated, negative);
        chosen
    }
    /// self + q for an affine q, 8M + 3S, and one when the operands are
    /// equal (h = r = 0), where the formula fails. self is not the identity.
    fn add_affine(&self, q: &Affine<C, N>) -> (Self, u64) {
        let z1 = self.z.square();
        let u2 = q.x.mul(&z1);
        let s2 = q.y.mul(&self.z.mul(&z1));
        let h = u2.sub(&self.x);
        let r = s2.sub(&self.y);
        let h2 = h.square();
        let h3 = h.mul(&h2);
        let u = self.x.mul(&h2);
        let x = r.square().sub(&h3).sub(&u.double());
        let y = r.mul(&u.sub(&x)).sub(&self.y.mul(&h3));
        let z = h.mul(&self.z);
        (Self { x, y, z }, h.is_zero() & r.is_zero())
    }
    /// scalar G by a comb, with no doublings: the odd representative's Booth
    /// digit i selects from row i, j 2^(5i) G, and one mixed addition adds
    /// it.
    ///
    /// Exceptional additions. Before window i >= 1 the running point is
    /// S_i G for the odd S_i = k' mod 2^(5i) - b 2^(5i), b = bit 5i - 1 of
    /// the representative k', so 0 < |S_i| <= 2^(5i - 1): never the identity.
    /// It equals or opposes the addend d 2^(5i) G only if
    /// S_i -+ d 2^(5i) = 0 mod n, while S_i is odd and d 2^(5i) even, so
    /// 0 < |S_i -+ d 2^(5i)| <= 16.5 2^(5i), which is below n for i < R - 1. At the top window S + d 2^(5i) = k' is
    /// not 0 mod n, but S = d 2^(5i) can hold, for k = 2^256 d mod n; that one
    /// addition doubles by mask instead. A zero digit's addition is
    /// discarded by mask.
    fn comb_mul<const R: usize>(table: &[[Affine<C, N>; 16]; R], scalar: &Scalar<C, N>) -> Self {
        let k = scalar.representative();
        // k' is odd, so digit 0 is nonzero.
        let mut r = Affine::lookup(&table[0], booth(&k.0, 0)).point();
        for i in 1..R {
            let d = booth(&k.0, i);
            let (mut sum, equal) = r.add_affine(&Affine::lookup(&table[i], d));
            if i == R - 1 {
                sum = Self::select(&sum, &r.double(), equal);
            }
            r = Self::select(&r, &sum, crate::ct::nonzero(d as u8 as u64));
        }
        r
    }
    /// self + q for a public affine q, branching on the values.
    fn add_affine_public(&self, q: &Affine<C, N>) -> Self {
        if self.z.is_zero() == 1 {
            return q.point();
        }
        let (sum, equal) = self.add_affine(q);
        if sum.z.is_zero() == 1 {
            return if equal == 1 { self.double() } else { Self::identity() };
        }
        sum
    }
    /// self + q for public points, branching on their values: no masked
    /// doubling or selects.
    fn add_public(&self, q: &Self) -> Self {
        if self.z.is_zero() == 1 {
            return *q;
        }
        if q.z.is_zero() == 1 {
            return *self;
        }
        let (sum, equal) = self.add_incomplete(q);
        if sum.z.is_zero() == 1 {
            return if equal == 1 { self.double() } else { Self::identity() };
        }
        sum
    }
    fn neg(&self) -> Self {
        Self {
            x: self.x,
            y: Residue::zero().sub(&self.y),
            z: self.z,
        }
    }
    fn odd_multiples(&self) -> OddMultiples<C, N> {
        let twice = self.double();
        let mut odd = [*self; 8];
        for j in 1..8 {
            odd[j] = odd[j - 1].add_public(&twice);
        }
        odd
    }
    /// u1 G + u2 self in one doubling chain over both scalars' width-5 NAFs
    /// (Straus's method), in variable time: every input is public.
    fn joint(&self, u1: &PublicScalar<N>, u2: &PublicScalar<N>) -> Self {
        let (g, q) = (C::odd_multiples_of_g(), self.odd_multiples());
        let (d1, d2) = (u1.naf(7), u2.naf(5));
        let Some(top) = (0..64 * N + 1).rev().find(|&i| d1[i] != 0 || d2[i] != 0) else {
            return Self::identity();
        };
        let mut r = Self::identity();
        for i in (0..=top).rev() {
            r = r.double();
            let d = d1[i];
            if d > 0 {
                r = r.add_affine_public(&g[d as usize / 2]);
            } else if d < 0 {
                let entry = g[(-d) as usize / 2];
                r = r.add_affine_public(&Affine {
                    x: entry.x,
                    y: Residue::zero().sub(&entry.y),
                });
            }
            let d = d2[i];
            if d > 0 {
                r = r.add_public(&q[d as usize / 2]);
            } else if d < 0 {
                r = r.add_public(&q[(-d) as usize / 2].neg());
            }
        }
        r
    }
    /// The affine coordinates as integers, into the caller's arrays.
    fn affine(&self, x: &mut [u64; N], y: &mut [u64; N]) -> Result<(), Error> {
        if self.z.is_zero() == 1 {
            return Err(Error::InvalidPoint);
        }
        let inverse = self.z.invert();
        let inv2 = inverse.square();
        *x = self.x.mul(&inv2).integer();
        *y = self.y.mul(&inv2.mul(&inverse)).integer();
        Ok(())
    }
    /// The uncompressed SEC 1 encoding, 1 + 16N bytes.
    pub(crate) fn encode(&self, out: &mut [u8]) -> Result<(), Error> {
        if out.len() != 1 + N * 16 {
            return Err(Error::Length);
        }
        let (mut x, mut y) = ([0; N], [0; N]);
        self.affine(&mut x, &mut y)?;
        out[0] = 4;
        to_be(&x, &mut out[1..1 + N * 8]);
        to_be(&y, &mut out[1 + N * 8..]);
        Ok(())
    }
    /// An uncompressed SEC 1 point on the curve.
    pub(crate) fn decode(encoded: &[u8]) -> Result<Self, Error> {
        if encoded.len() != 1 + N * 16 {
            return Err(Error::Length);
        }
        if encoded[0] != 4 {
            return Err(Error::InvalidEncoding);
        }
        let x = Coord::<C, N>::from_integer(&from_be(&encoded[1..1 + N * 8]));
        let y = Coord::<C, N>::from_integer(&from_be(&encoded[1 + N * 8..]));
        let (Some(x), Some(y)) = (x, y) else {
            return Err(Error::InvalidPoint);
        };
        let rhs = x
            .square()
            .mul(&x)
            .sub(&x.double().add(&x))
            .add(&Residue::from_montgomery(C::B));
        if y.square().eq(&rhs) != 1 {
            return Err(Error::InvalidPoint);
        }
        Ok(Point {
            x,
            y,
            z: Residue::one(),
        })
    }
    /// The x-coordinate of scalar * self into `output`, 8N bytes. The product
    /// and its affine coordinates are the shared secret's, erased here once
    /// `output` is written.
    pub(crate) fn agree(&self, scalar: &Scalar<C, N>, output: &mut [u8]) {
        let product = Workspace(self.mul(scalar));
        let mut coordinates = Workspace([[0; N]; 2]);
        let [x, y] = &mut coordinates.0;
        // Both curves have prime order and cofactor one; validated nonzero
        // scalars and finite peers cannot multiply to infinity.
        product
            .0
            .affine(x, y)
            .expect("validated prime-order point and nonzero scalar");
        to_be(x, output);
    }
    /// ECDSA verification of a DER signature over an 8N-byte digest. All
    /// inputs are public.
    pub(crate) fn verify(&self, digest: &[u8], signature: &[u8]) -> Result<(), Error> {
        if digest.len() != N * 8 {
            return Err(Error::Length);
        }
        let (r, s) = der::<N>(signature)?;
        let r = Scalar::<C, N>::from_bytes(&r[..N * 8]).map_err(|_| Error::InvalidSignature)?;
        let s = Scalar::<C, N>::from_bytes(&s[..N * 8]).map_err(|_| Error::InvalidSignature)?;
        let z = ModN::<C, N>::from_wide(&from_be(digest));
        let inverse = ModN::<C, N>::from_wide(&s.0).invert();
        let u1 = PublicScalar(z.mul(&inverse).integer());
        let u2 = PublicScalar(ModN::<C, N>::from_wide(&r.0).mul(&inverse).integer());
        let result = self.joint(&u1, &u2);
        if result.z.is_zero() == 1 {
            return Err(Error::InvalidSignature);
        }
        // x(R) = X / Z^2 is r mod n exactly when X = r' Z^2 for r' = r, or
        // r' = r + n where that is still below p: no inversion.
        let z2 = result.z.square();
        let r_field = Coord::<C, N>::from_integer(&r.0).expect("r < n < p");
        if result.x.eq(&r_field.mul(&z2)) == 1 {
            return Ok(());
        }
        let mut r_plus_n = [0; N];
        let mut carry = false;
        for i in 0..N {
            let (sum, c1) = r.0[i].overflowing_add(C::Order::P[i]);
            let (sum, c2) = sum.overflowing_add(u64::from(carry));
            r_plus_n[i] = sum;
            carry = c1 | c2;
        }
        match (carry, Coord::<C, N>::from_integer(&r_plus_n)) {
            (false, Some(r_field)) if result.x.eq(&r_field.mul(&z2)) == 1 => Ok(()),
            _ => Err(Error::InvalidSignature),
        }
    }
}
fn der<const N: usize>(input: &[u8]) -> Result<([u8; 48], [u8; 48]), Error> {
    // A two-integer P-384 signature is below 128 bytes: long lengths are invalid.
    if input.len() < 6 || input[0] != 0x30 || input[1] as usize != input.len() - 2 {
        return Err(Error::InvalidEncoding);
    }
    let mut cursor = 2;
    let mut integer = || -> Result<[u8; 48], Error> {
        if input.get(cursor) != Some(&2) {
            return Err(Error::InvalidEncoding);
        }
        let len = *input.get(cursor + 1).ok_or(Error::InvalidEncoding)? as usize;
        cursor += 2;
        let end = cursor.checked_add(len).ok_or(Error::InvalidEncoding)?;
        let mut bytes = input.get(cursor..end).ok_or(Error::InvalidEncoding)?;
        cursor = end;
        if bytes.is_empty() || bytes[0] & 128 != 0 {
            return Err(Error::InvalidEncoding);
        }
        if bytes[0] == 0 {
            if bytes.len() > 1 && bytes[1] & 128 == 0 {
                return Err(Error::InvalidEncoding);
            }
            bytes = &bytes[1..]
        }
        if bytes.len() > N * 8 {
            return Err(Error::InvalidSignature);
        }
        let mut result = [0; 48];
        result[N * 8 - bytes.len()..N * 8].copy_from_slice(bytes);
        Ok(result)
    };
    let r = integer()?;
    let s = integer()?;
    if cursor != input.len() {
        return Err(Error::InvalidEncoding);
    }
    Ok((r, s))
}
macro_rules! modulus {
    ($name:ident, $n:literal, $p:expr) => {
        pub(crate) struct $name;
        impl Modulus<$n> for $name {
            const P: [u64; $n] = $p;
        }
    };
}
/// P-256's prime, with its special reduction step.
pub(crate) struct P256Field;
impl Modulus<4> for P256Field {
    const P: [u64; 4] = [0xffffffffffffffff, 0x00000000ffffffff, 0, 0xffffffff00000001];
    const FORM: Form = Form::P256;
}
modulus!(P256Order, 4, [0xf3b9cac2fc632551, 0xbce6faada7179e84, 0xffffffffffffffff, 0xffffffff00000000]);
modulus!(
    P384Field,
    6,
    [
        0x00000000ffffffff,
        0xffffffff00000000,
        0xfffffffffffffffe,
        0xffffffffffffffff,
        0xffffffffffffffff,
        0xffffffffffffffff,
    ]
);
modulus!(
    P384Order,
    6,
    [
        0xecec196accc52973,
        0x581a0db248b0a77a,
        0xc7634d81f4372ddf,
        0xffffffffffffffff,
        0xffffffffffffffff,
        0xffffffffffffffff,
    ]
);

/// P-256 (FIPS 186-5, SP 800-186 section 3.2.1.3).
pub(crate) struct P256;
impl Curve<4> for P256 {
    type Field = P256Field;
    type Order = P256Order;
    // k + n >= n > 2^255 sets bit 254 or 255 of window 51 (bits 254..=259).
    const OFFSET: u64 = 1;
    fn odd_multiples_of_g() -> &'static [Affine<Self, 4>; 32] {
        &P256_G_ODD
    }
    fn base_mul(scalar: &Scalar<Self, 4>) -> Point<Self, 4> {
        Point::comb_mul(&P256_COMB, scalar)
    }
    const B: [u64; 4] = montgomery::encode(
        &[0x3bce3c3e27d2604b, 0x651d06b0cc53b0f6, 0xb3ebbd55769886bc, 0x5ac635d8aa3a93e7],
        &P256Field::P,
    );
    const GX: [u64; 4] = montgomery::encode(
        &[0xf4a13945d898c296, 0x77037d812deb33a0, 0xf8bce6e563a440f2, 0x6b17d1f2e12c4247],
        &P256Field::P,
    );
    const GY: [u64; 4] = montgomery::encode(
        &[0xcbb6406837bf51f5, 0x2bce33576b315ece, 0x8ee7eb4a7c0f9e16, 0x4fe342e2fe1a7f9b],
        &P256Field::P,
    );
}

/// P-384 (FIPS 186-5, SP 800-186 section 3.2.1.4).
pub(crate) struct P384;
impl Curve<6> for P384 {
    type Field = P384Field;
    type Order = P384Order;
    // k + 2n >= 2n > 2^384 sets bit 384 or 385 of window 77 (bits 384..=389);
    // k + n need not.
    const OFFSET: u64 = 2;
    fn odd_multiples_of_g() -> &'static [Affine<Self, 6>; 32] {
        &P384_G_ODD
    }
    const B: [u64; 6] = montgomery::encode(
        &[
            0x2a85c8edd3ec2aef,
            0xc656398d8a2ed19d,
            0x0314088f5013875a,
            0x181d9c6efe814112,
            0x988e056be3f82d19,
            0xb3312fa7e23ee7e4,
        ],
        &P384Field::P,
    );
    const GX: [u64; 6] = montgomery::encode(
        &[
            0x3a545e3872760ab7,
            0x5502f25dbf55296c,
            0x59f741e082542a38,
            0x6e1d3b628ba79b98,
            0x8eb1c71ef320ad74,
            0xaa87ca22be8b0537,
        ],
        &P384Field::P,
    );
    const GY: [u64; 6] = montgomery::encode(
        &[
            0x7a431d7c90ea0e5f,
            0x0a60b1ce1d7e819d,
            0xe9da3113b5f0b8c0,
            0xf8f41dbd289a147c,
            0x5d9e98bf9292dc29,
            0x3617de4a96262c6f,
        ],
        &P384Field::P,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first valid of four candidates is taken, and four invalid ones are
    /// refused.
    #[test]
    fn generation_takes_the_first_valid_candidate() {
        let mut n = [0u8; 32];
        to_be(&P256Order::P, &mut n);
        let (k1, k2) = ([0x11; 32], [0x22; 32]);
        let candidates = [n, [0; 32], k1, k2].concat();
        assert_eq!(Scalar::<P256, 4>::first_valid(&candidates).unwrap().0, from_be::<4>(&k1));
        let invalid = [n, [0; 32], [0xff; 32], n].concat();
        assert!(matches!(Scalar::<P256, 4>::first_valid(&invalid), Err(Error::Entropy)));
    }

    fn scalars<const N: usize>(order: &[u64; N]) -> std::vec::Vec<[u64; N]> {
        let mut out = std::vec![[0; N], [u64::MAX; N]];
        let mut n_minus_1 = *order;
        n_minus_1[0] -= 1;
        out.push(n_minus_1);
        let mut one = [0; N];
        one[0] = 1;
        out.push(one);
        // Every window's six bits 0b011111 or 0b100000: digits +16 and -16.
        out.push([0x7bdef7bdef7bdef7; N]);
        out.push([0x8421084210842108; N]);
        let mut seed = 0x0123456789abcdefu64;
        for _ in 0..40 {
            out.push(core::array::from_fn(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                seed
            }));
        }
        out
    }

    fn integer<const N: usize>(words: &[u64; N]) -> num_bigint::BigInt {
        num_bigint::BigInt::from_bytes_le(
            num_bigint::Sign::Plus,
            &words.iter().flat_map(|w| w.to_le_bytes()).collect::<std::vec::Vec<_>>(),
        )
    }

    /// The representative is odd, congruent to k and sets the top window;
    /// its Booth digits lie in [-16, 16] and recompose it, with +16 and -16
    /// both reached.
    fn check_booth<C: Curve<N>, const N: usize>() {
        use num_bigint::BigInt;
        let n = integer(&C::Order::P);
        let mut extremes = [false; 2];
        for k in scalars(&C::Order::P) {
            let k = ModN::<C, N>::from_wide(&k).integer();
            let scalar = Scalar::<C, N>(k, core::marker::PhantomData);
            let words = scalar.representative();
            let representative = integer(&words.0);
            assert_eq!(&representative % 2u8, BigInt::from(1));
            assert_eq!(&representative % &n, integer(&k));
            assert!(representative < BigInt::from(1) << (64 * N + 2));
            let mut sum = BigInt::from(0);
            for i in 0..windows(N) {
                let d = booth(&words.0, i);
                assert!((-16..=16).contains(&d));
                extremes[0] |= d == 16;
                extremes[1] |= d == -16;
                sum += BigInt::from(d) << (5 * i);
            }
            assert_eq!(sum, representative);
            assert_ne!(booth(&words.0, windows(N) - 1), 0);
        }
        assert_eq!(extremes, [true, true]);
    }

    #[test]
    fn booth_digits_recompose_the_scalar() {
        check_booth::<P256, 4>();
        check_booth::<P384, 6>();
    }

    /// The width-w NAF of 384-bit scalars recomposes them.
    #[test]
    fn naf_digits_recompose_384_bit_scalars() {
        use num_bigint::BigInt;
        for k in scalars(&P384Order::P) {
            let k = Coord::<P384, 6>::from_wide(&k).integer();
            for w in [2u32, 5, 8] {
                let digits = PublicScalar(k).naf(w);
                let sum = digits.iter().enumerate().fold(BigInt::from(0), |s, (i, &d)| s + (BigInt::from(d) << i));
                assert_eq!(sum, integer(&k));
            }
        }
    }

    /// The fixed-schedule Booth multiplication equals the public joint
    /// multiplication, a different recoding and addition, for scalars below
    /// n including those with extreme digits.
    fn check_mul<C: Curve<N>, const N: usize>() {
        let zero = PublicScalar([0; N]);
        for k in scalars(&C::Order::P) {
            let k = ModN::<C, N>::from_wide(&k).integer();
            let ours = Point::<C, N>::base().mul(&Scalar(k, core::marker::PhantomData));
            let theirs = Point::<C, N>::identity().joint(&PublicScalar(k), &zero);
            let (mut a, mut b) = ([0; N], [0; N]);
            let (mut c, mut d) = ([0; N], [0; N]);
            match (ours.affine(&mut a, &mut b), theirs.affine(&mut c, &mut d)) {
                (Ok(()), Ok(())) => assert_eq!((a, b), (c, d)),
                (Err(_), Err(_)) => assert_eq!(k, [0; N]),
                _ => panic!("one side is the identity"),
            }
        }
    }

    /// The public path's exceptional sums: equal summands double, and
    /// opposite ones cancel to the identity.
    fn check_exceptional_sums<C: Curve<N>, const N: usize>() {
        let mut one = [0; N];
        one[0] = 1;
        let (mut a, mut b) = ([0; N], [0; N]);
        let (mut c, mut d) = ([0; N], [0; N]);
        let g = Point::<C, N>::base();
        g.joint(&PublicScalar(one), &PublicScalar(one)).affine(&mut a, &mut b).unwrap();
        g.double().affine(&mut c, &mut d).unwrap();
        assert_eq!((a, b), (c, d));
        assert_eq!(g.add_public(&g.neg()).z.is_zero(), 1);
    }

    #[test]
    fn public_sums_of_equal_and_opposite_points() {
        check_exceptional_sums::<P256, 4>();
        check_exceptional_sums::<P384, 6>();
    }

    fn affine_of<C: Curve<N>, const N: usize>(p: &Point<C, N>) -> ([u64; N], [u64; N]) {
        let (mut x, mut y) = ([0; N], [0; N]);
        p.affine(&mut x, &mut y).unwrap();
        (x, y)
    }

    /// k G by the public path, for k below n.
    fn reference<C: Curve<N>, const N: usize>(k: &num_bigint::BigInt) -> ([u64; N], [u64; N]) {
        let (_, bytes) = k.to_bytes_le();
        let mut words = [0u64; N];
        for (i, chunk) in bytes.chunks(8).enumerate() {
            let mut b = [0u8; 8];
            b[..chunk.len()].copy_from_slice(chunk);
            words[i] = u64::from_le_bytes(b);
        }
        affine_of(&Point::<C, N>::identity().joint(&PublicScalar(words), &PublicScalar([0; N])))
    }

    /// Every comb entry is j 2^(5i) G, and the odd-multiple tables (2j + 1) G.
    #[test]
    fn static_tables_hold_their_multiples() {
        use num_bigint::BigInt;
        let n = integer(&P256Order::P);
        for (i, row) in P256_COMB.iter().enumerate() {
            for (j, entry) in row.iter().enumerate() {
                let k = (BigInt::from(j + 1) << (5 * i)) % &n;
                assert_eq!(affine_of(&entry.point()), reference::<P256, 4>(&k), "row {i}, entry {j}");
            }
        }
        for (j, entry) in P256_G_ODD.iter().enumerate() {
            assert_eq!(affine_of(&entry.point()), reference::<P256, 4>(&BigInt::from(2 * j + 1)));
        }
        for (j, entry) in P384_G_ODD.iter().enumerate() {
            assert_eq!(affine_of(&entry.point()), reference::<P384, 6>(&BigInt::from(2 * j + 1)));
        }
    }

    /// The comb equals the public path for every test scalar and for the
    /// ones k = 2^256 d mod n for d = 1..=4, among them those whose top
    /// addition doubles.
    #[test]
    fn the_comb_matches_the_public_path() {
        use num_bigint::BigInt;
        let n = integer(&P256Order::P);
        let mut ks: std::vec::Vec<BigInt> =
            scalars(&P256Order::P).iter().map(|k| integer(k) % &n).filter(|k| *k != BigInt::from(0)).collect();
        for d in 1..=4u8 {
            ks.push((BigInt::from(d) << 256) % &n);
        }
        for k in ks {
            let (_, bytes) = k.to_bytes_le();
            let mut words = [0u64; 4];
            for (i, chunk) in bytes.chunks(8).enumerate() {
                let mut b = [0u8; 8];
                b[..chunk.len()].copy_from_slice(chunk);
                words[i] = u64::from_le_bytes(b);
            }
            let comb = P256::base_mul(&Scalar(words, core::marker::PhantomData));
            assert_eq!(affine_of(&comb), reference::<P256, 4>(&k), "k = {k:x}");
        }
    }

    /// The scalars k = 2d mod n whose representative's first digit is d,
    /// for which the last addition of [`Point::mul`] doubles: at least one
    /// exists per curve, and each multiplies as the public path does.
    fn check_last_addition_doubles<C: Curve<N>, const N: usize>() {
        use num_bigint::BigInt;
        let n = integer(&C::Order::P);
        let mut found = 0;
        for d in (-16i32..=16).filter(|&d| d != 0) {
            let k = ((BigInt::from(2 * d) % &n) + &n) % &n;
            let (_, bytes) = k.to_bytes_le();
            let mut words = [0u64; N];
            for (i, chunk) in bytes.chunks(8).enumerate() {
                let mut b = [0u8; 8];
                b[..chunk.len()].copy_from_slice(chunk);
                words[i] = u64::from_le_bytes(b);
            }
            let scalar = Scalar::<C, N>(words, core::marker::PhantomData);
            if i32::from(booth(&scalar.representative().0, 0)) != d {
                continue;
            }
            found += 1;
            assert_eq!(affine_of(&Point::<C, N>::base().mul(&scalar)), reference::<C, N>(&k), "d = {d}");
        }
        assert!(found > 0);
    }

    #[test]
    fn the_last_addition_doubles_where_it_must() {
        check_last_addition_doubles::<P256, 4>();
        check_last_addition_doubles::<P384, 6>();
    }

    #[test]
    fn booth_multiplication_matches_the_public_path() {
        check_mul::<P256, 4>();
        check_mul::<P384, 6>();
    }
}
