//! Inversion modulo an odd p by Bernstein and Yang's divsteps ("Fast
//! constant-time gcd computation and modular inversion", TCHES 2019), on a
//! fixed schedule: the step count is a proven bound for p's width, and no
//! branch or address depends on the operand.
//!
//! A p of at most 256 bits takes the variant whose delta starts at 1/2, for
//! which 590 divsteps suffice at that width (Wuille, libsecp256k1's
//! `doc/safegcd_implementation.md`): 10 batches of 59. A wider one takes
//! delta = 1 and the paper's bound, in batches of 62.
//!
//! The integers f, g, d and e are signed 62-bit limbs: each limb but the top
//! one lies in [0, 2^62), and the top one carries the sign. A batch reads
//! only the low limbs of f and g, and its matrix is scaled to 2^62 whatever
//! its step count, so applying it to the whole integers divides by 2^62 as a
//! shift by one limb.

use super::Modulus;
use crate::ct;

const MASK: u64 = (1 << 62) - 1;
/// Limbs for N <= 6 words, each integer below 2^(64N + 1) in magnitude.
const LIMBS: usize = 7;
type Limbs = [i64; LIMBS];

const fn limbs(n: usize) -> usize {
    (64 * n + 2).div_ceil(62)
}

/// The transition matrix of a batch: 2^62 (f', g') = (u f + v g, q f + r g),
/// with |u| + |v| and |q| + |r| at most 2^62.
struct Matrix {
    u: i64,
    v: i64,
    q: i64,
    r: i64,
}

/// x^-1 `scale` mod p for x, scale < p, and zero for x = 0. The time depends
/// on neither.
pub(super) fn invert<M: Modulus<N>, const N: usize>(x: &[u64; N], scale: &[u64; N]) -> [u64; N] {
    let n = limbs(N);
    let p: Limbs = const { split::<N>(&M::P) };
    // p^-1 mod 2^62, from N0 = -p^-1 mod 2^64.
    let inverse = M::N0.wrapping_neg() & MASK;
    let half = const { width::<M, N>() <= 256 };
    let mut f = p;
    let mut g = split::<N>(x);
    let mut d = [0; LIMBS];
    let mut e = split::<N>(scale);
    // eta = -delta from delta = 1, or zeta = -(delta + 1/2) from delta = 1/2:
    // both -1.
    let mut eta = -1;
    if half {
        for _ in 0..10 {
            let t;
            (eta, t) = divsteps::<59, true>(eta, f[0] as u64, g[0] as u64);
            apply(&t, &mut f, &mut g, n);
            update(&t, &mut d, &mut e, &p, inverse, n);
        }
    } else {
        for _ in 0..batches::<M, N>() {
            let t;
            (eta, t) = divsteps::<62, false>(eta, f[0] as u64, g[0] as u64);
            apply(&t, &mut f, &mut g, n);
            update(&t, &mut d, &mut e, &p, inverse, n);
        }
    }
    assert!(is_zero(&g, n), "divsteps left g nonzero");
    finish(&f, d, &p, n)
}

/// x^-1 `scale` mod p for 0 < x < p and scale < p, in variable time: for
/// public x alone. Each batch of 62 divsteps shifts out runs of g's zeros at
/// once and clears up to six of its low bits with one multiple of f, and
/// the batches stop once g is zero (Wuille, libsecp256k1's
/// `doc/safegcd_implementation.md`, section 6).
pub(super) fn invert_public<M: Modulus<N>, const N: usize>(x: &[u64; N], scale: &[u64; N]) -> [u64; N] {
    let n = limbs(N);
    let p: Limbs = const { split::<N>(&M::P) };
    let inverse = M::N0.wrapping_neg() & MASK;
    let mut f = p;
    let mut g = split::<N>(x);
    let mut d = [0; LIMBS];
    let mut e = split::<N>(scale);
    let mut eta = -1;
    let mut done = 0;
    while !is_zero(&g, n) {
        assert!(done < batches::<M, N>(), "divsteps left g nonzero");
        let t;
        (eta, t) = divsteps_public(eta, f[0] as u64, g[0] as u64);
        apply(&t, &mut f, &mut g, n);
        update(&t, &mut d, &mut e, &p, inverse, n);
        done += 1;
    }
    finish(&f, d, &p, n)
}

/// Batches of 62 delta = 1 divsteps that suffice for p. Theorem 11.2:
/// divstep^m(1, f, g) leaves g = 0 for m = (49 d + 57) / 17 where
/// f^2 + 4 g^2 <= 5 2^(2d) and d >= 46; f = p and g < p meet it with d p's
/// width in bits.
const fn batches<M: Modulus<N>, const N: usize>() -> usize {
    let bits = width::<M, N>();
    assert!(bits >= 46 && limbs(N) <= LIMBS && M::P[0] & 1 == 1);
    ((49 * bits + 57) / 17).div_ceil(62) as usize
}

fn is_zero(g: &Limbs, n: usize) -> bool {
    let mut rest = 0;
    for &limb in &g[..n] {
        rest |= limb;
    }
    rest == 0
}

/// x^-1 scale mod p from the final f = gcd(p, x) = +-1 and d, d f = x^-1
/// scale mod p. d lies in (-2p, p): add p if it is negative, negate it with
/// f, and add p if it is negative again.
fn finish<const N: usize>(f: &Limbs, mut d: Limbs, p: &Limbs, n: usize) -> [u64; N] {
    add_if_negative(&mut d, p, n);
    negate_if(&mut d, ct::opaque((f[n - 1] >> 63) as u64), n);
    add_if_negative(&mut d, p, n);
    join(&d)
}

/// p's width in bits.
const fn width<M: Modulus<N>, const N: usize>() -> u64 {
    64 * N as u64 - M::P[N - 1].leading_zeros() as u64
}

/// STEPS divsteps on the low limbs of f and g: the new eta and the matrix,
/// scaled by 2^(62 - STEPS) to 2^62. With HALF, eta is zeta = -(delta +
/// 1/2), whose step after a swap is -zeta - 2 rather than -eta - 1.
#[inline(always)]
fn divsteps<const STEPS: u32, const HALF: bool>(mut eta: i64, mut f: u64, mut g: u64) -> (i64, Matrix) {
    let scale = 1u64 << (62 - STEPS);
    let (mut u, mut v, mut q, mut r) = (scale, 0u64, 0u64, scale);
    for _ in 0..STEPS {
        // `odd`: g is odd; `swap`: also delta > 0, so the step becomes
        // (1 - delta, g, (g - f) / 2) rather than (1 + delta, f, (g + f) / 2).
        let odd = ct::opaque(((g << 63) as i64 >> 63) as u64);
        let swap = ct::opaque((eta >> 63) as u64) & odd;
        // h is 2g' in g's row: g, g + f, or g - f with the swap, which also
        // makes f' = f + h = g.
        let h = g.wrapping_add(f & odd).wrapping_sub((f << 1) & swap);
        let hq = q.wrapping_add(((u & odd) ^ swap).wrapping_sub(swap));
        let hr = r.wrapping_add(((v & odd) ^ swap).wrapping_sub(swap));
        f = f.wrapping_add(h & swap);
        g = h >> 1;
        // The matrix keeps g's row whole and doubles f's instead of halving.
        u = u.wrapping_add(hq & swap) << 1;
        v = v.wrapping_add(hr & swap) << 1;
        (q, r) = (hq, hr);
        eta = match HALF {
            true => (eta ^ swap as i64).wrapping_sub(1),
            false => (eta ^ swap as i64).wrapping_add(!swap as i64),
        };
    }
    (eta, Matrix { u: u as i64, v: v as i64, q: q as i64, r: r as i64 })
}

/// 62 delta = 1 divsteps on the low limbs of f and g in variable time, as
/// [`divsteps`] computes them: a run of z zeros of g is z steps that halve g
/// at once; an odd g with delta > 0 swaps as one step would, to (g, -f),
/// after which up to min(1 - delta, 6) steps each add f or not, which one
/// multiple w f with g + w f = 0 mod 2^k decides together.
fn divsteps_public(mut eta: i64, mut f: u64, mut g: u64) -> (i64, Matrix) {
    let (mut u, mut v, mut q, mut r) = (1i64, 0i64, 0i64, 1i64);
    let mut left = 62;
    loop {
        let zeros = (g | 1 << left).trailing_zeros();
        g >>= zeros;
        u <<= zeros;
        v <<= zeros;
        eta -= zeros as i64;
        left -= zeros;
        if left == 0 {
            break;
        }
        if eta < 0 {
            (eta, f, g, u, v, q, r) = (-eta, g, f.wrapping_neg(), q, r, -u, -v);
        }
        let k = (eta + 1).min(left as i64).min(6) as u32;
        // f^-1 mod 64: f is its own inverse mod 8, and one Newton step
        // doubles that.
        let f_inverse = f.wrapping_mul(2u64.wrapping_sub(f.wrapping_mul(f)));
        let w = g.wrapping_mul(f_inverse).wrapping_neg() & ((1 << k) - 1);
        g = g.wrapping_add(f.wrapping_mul(w));
        q += u * w as i64;
        r += v * w as i64;
    }
    (eta, Matrix { u, v, q, r })
}

/// a b + c, which the callers' bounds keep within an i128.
#[inline(always)]
fn mac(a: i64, b: i64, c: i128) -> i128 {
    (a as i128).wrapping_mul(b as i128).wrapping_add(c)
}

/// (f, g) <- (u f + v g, q f + r g) / 2^62, exact.
#[inline(always)]
fn apply(t: &Matrix, f: &mut Limbs, g: &mut Limbs, n: usize) {
    let mut cf = mac(t.u, f[0], mac(t.v, g[0], 0));
    let mut cg = mac(t.q, f[0], mac(t.r, g[0], 0));
    debug_assert!(cf as u64 & MASK == 0 && cg as u64 & MASK == 0);
    cf >>= 62;
    cg >>= 62;
    for i in 1..n {
        cf = mac(t.u, f[i], mac(t.v, g[i], cf));
        cg = mac(t.q, f[i], mac(t.r, g[i], cg));
        f[i - 1] = (cf as u64 & MASK) as i64;
        g[i - 1] = (cg as u64 & MASK) as i64;
        cf >>= 62;
        cg >>= 62;
    }
    f[n - 1] = cf as i64;
    g[n - 1] = cg as i64;
}

/// (d, e) <- (u d + v e, q d + r e) / 2^62 mod p, each staying in (-2p, p):
/// p is added to a negative input and the multiple of p that clears the low
/// 62 bits subtracted, so |u d + v e| < 2^62 p becomes a sum in (-2^63 p,
/// 2^62 p) before the shift.
#[inline(always)]
fn update(t: &Matrix, d: &mut Limbs, e: &mut Limbs, p: &Limbs, inverse: u64, n: usize) {
    let sd = ct::opaque((d[n - 1] >> 63) as u64) as i64;
    let se = ct::opaque((e[n - 1] >> 63) as u64) as i64;
    let md = (t.u & sd).wrapping_add(t.v & se);
    let me = (t.q & sd).wrapping_add(t.r & se);
    let mut cd = mac(t.u, d[0], mac(t.v, e[0], 0));
    let mut ce = mac(t.q, d[0], mac(t.r, e[0], 0));
    let md = md.wrapping_sub((inverse.wrapping_mul(cd as u64).wrapping_add(md as u64) & MASK) as i64);
    let me = me.wrapping_sub((inverse.wrapping_mul(ce as u64).wrapping_add(me as u64) & MASK) as i64);
    cd = mac(p[0], md, cd);
    ce = mac(p[0], me, ce);
    debug_assert!(cd as u64 & MASK == 0 && ce as u64 & MASK == 0);
    cd >>= 62;
    ce >>= 62;
    for i in 1..n {
        cd = mac(t.u, d[i], mac(t.v, e[i], mac(p[i], md, cd)));
        ce = mac(t.q, d[i], mac(t.r, e[i], mac(p[i], me, ce)));
        d[i - 1] = (cd as u64 & MASK) as i64;
        e[i - 1] = (ce as u64 & MASK) as i64;
        cd >>= 62;
        ce >>= 62;
    }
    d[n - 1] = cd as i64;
    e[n - 1] = ce as i64;
}

/// a + p where a is negative, then the limbs carried.
#[inline(always)]
fn add_if_negative(a: &mut Limbs, p: &Limbs, n: usize) {
    let m = ct::opaque((a[n - 1] >> 63) as u64) as i64;
    let mut c = 0i64;
    for i in 0..n {
        c = c.wrapping_add(a[i]).wrapping_add(p[i] & m);
        a[i] = c;
        c >>= 62;
        if i < n - 1 {
            a[i] &= MASK as i64;
        }
    }
}

/// -a where `mask` is all ones, then the limbs carried.
#[inline(always)]
fn negate_if(a: &mut Limbs, mask: u64, n: usize) {
    let m = mask as i64;
    let mut c = 0i64;
    for i in 0..n {
        c = c.wrapping_add((a[i] ^ m).wrapping_sub(m));
        a[i] = c;
        c >>= 62;
        if i < n - 1 {
            a[i] &= MASK as i64;
        }
    }
}

/// N words, below 2^(64N), as nonnegative limbs.
const fn split<const N: usize>(x: &[u64; N]) -> Limbs {
    let mut out = [0; LIMBS];
    let mut i = 0;
    while i < limbs(N) {
        let (word, shift) = (62 * i / 64, 62 * i % 64);
        let mut limb = x[word] >> shift;
        if shift > 2 && word + 1 < N {
            limb |= x[word + 1] << (64 - shift);
        }
        out[i] = (limb & MASK) as i64;
        i += 1;
    }
    out
}

/// Nonnegative limbs below 2^(64N) as N words: word i starts 2i bits into
/// limb 64i / 62, so two limbs fill it.
fn join<const N: usize>(a: &Limbs) -> [u64; N] {
    let mut out = [0; N];
    for (i, word) in out.iter_mut().enumerate() {
        let (limb, shift) = (64 * i / 62, 64 * i % 62);
        *word = (a[limb] as u64) >> shift;
        if limb + 1 < LIMBS {
            *word |= (a[limb + 1] as u64) << (62 - shift);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nist::{P256Field, P256Order, P384Field, P384Order};
    use num_bigint::BigInt;

    fn integer(a: &Limbs, n: usize) -> BigInt {
        (0..n).rev().fold(BigInt::from(0), |v, i| (v << 62) + a[i])
    }

    fn limbs_of(x: &BigInt, n: usize) -> Limbs {
        let mut out = [0; LIMBS];
        let mut rest = x.clone();
        for limb in out.iter_mut().take(n - 1) {
            let low: BigInt = &rest & BigInt::from(MASK);
            *limb = i64::try_from(&low).unwrap();
            rest = (rest - low) >> 62;
        }
        out[n - 1] = i64::try_from(&rest).unwrap();
        out
    }

    /// `update`'s range claim at its edges: d and e anywhere in (-2p, p),
    /// against matrices with |u| + |v| and |q| + |r| at 2^62.
    fn check_update<M: Modulus<N>, const N: usize>() {
        let n = limbs(N);
        let p_limbs = split::<N>(&M::P);
        let p = integer(&p_limbs, n);
        let inverse = M::N0.wrapping_neg() & MASK;
        let values = [-2 * &p + 1, -&p - 1, -&p, BigInt::from(-1), BigInt::from(0), BigInt::from(1), &p >> 1, &p - 1];
        let top = 1i64 << 62;
        let half = 1i64 << 61;
        let rows = [(top, 0), (0, top), (-top, 0), (0, -top), (half, half), (half, -half), (-half, half), (-half, -half), (1, 0), (top - 1, 1), (1 - top, -1)];
        for d in &values {
            for e in &values {
                for &(u, v) in &rows {
                    for &(q, r) in &rows {
                        let t = Matrix { u, v, q, r };
                        let (mut dl, mut el) = (limbs_of(d, n), limbs_of(e, n));
                        update(&t, &mut dl, &mut el, &p_limbs, inverse, n);
                        for (out, (a, b)) in [(integer(&dl, n), (u, v)), (integer(&el, n), (q, r))] {
                            assert!(out > -2 * &p && out < p, "{a} {d} + {b} {e} left (-2p, p)");
                            let want = BigInt::from(a) * d + BigInt::from(b) * e;
                            assert_eq!(((out << 62) - want) % &p, BigInt::from(0), "{a} {d} + {b} {e}");
                        }
                        for i in 0..n - 1 {
                            assert!((0..1 << 62).contains(&dl[i]) && (0..1 << 62).contains(&el[i]));
                        }
                    }
                }
            }
        }
    }

    /// One divstep by its definition, with delta doubled so that 1/2 is 1:
    /// (1 - delta, g, (g - f) / 2) for delta > 0 and g odd, otherwise
    /// (1 + delta, f, (g + (g mod 2) f) / 2).
    fn step(delta2: i64, f: i128, g: i128) -> (i64, i128, i128) {
        if delta2 > 0 && g & 1 == 1 { (2 - delta2, g, (g - f) >> 1) } else { (2 + delta2, f, (g + (g & 1) * f) >> 1) }
    }

    /// A batch's matrix and eta for odd f and any g of up to 61 bits and
    /// either sign, against `steps` divsteps by the definition: 2^62 (f', g')
    /// = (u f + v g, q f + r g), and the new delta. `to_delta2` reads the
    /// batch's eta as doubled delta.
    fn check_batch(steps: u32, to_delta2: fn(i64) -> i64, batch: fn(i64, u64, u64) -> (i64, Matrix)) {
        let mut seed = 0x5eed_d1d5_u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for case in 0..20_000 {
            let f = ((next() as i64) >> 3) | 1;
            let g = (next() as i64) >> 3;
            let eta = (next() % 41) as i64 - 20;
            let (eta_out, t) = batch(eta, f as u64, g as u64);
            let (mut delta2, mut ff, mut gg) = (to_delta2(eta), f as i128, g as i128);
            for _ in 0..steps {
                (delta2, ff, gg) = step(delta2, ff, gg);
            }
            let (f, g) = (f as i128, g as i128);
            assert_eq!(t.u as i128 * f + t.v as i128 * g, ff << 62, "case {case}: f row");
            assert_eq!(t.q as i128 * f + t.r as i128 * g, gg << 62, "case {case}: g row");
            assert_eq!(to_delta2(eta_out), delta2, "case {case}: delta");
        }
    }

    #[test]
    fn batches_are_their_divsteps() {
        check_batch(62, |eta| -2 * eta, divsteps::<62, false>);
        check_batch(59, |zeta| -2 * zeta - 1, divsteps::<59, true>);
        check_batch(62, |eta| -2 * eta, divsteps_public);
    }

    #[test]
    fn updates_stay_in_range_and_congruent() {
        check_update::<P256Field, 4>();
        check_update::<P256Order, 4>();
        check_update::<P384Field, 6>();
        check_update::<P384Order, 6>();
    }
}
