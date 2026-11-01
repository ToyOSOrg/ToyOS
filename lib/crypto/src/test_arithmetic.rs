//! The field and scalar arithmetic held against `num-bigint`, an independent
//! big-integer reference: limb and modulus boundary values, seeded values,
//! chains that feed each result back unreduced, and the algebra's laws.
//!
//! [`Field`] is the only part that names the crate's types. Another
//! representation of the same field is checked by implementing it.

use crate::{
    field25519::{Multiply, Tight},
    montgomery::{Computation, Modulus, Residue},
    nist::{P256Field, P256Order, P384Field, P384Order},
    scalar25519::S,
};
use core::marker::PhantomData;
use num_bigint::BigUint;
use std::vec::Vec;

/// Seeded values per field, and steps of each chain.
const CASES: usize = if cfg!(miri) { 8 } else { 2_000 };

/// A field's arithmetic as its consumers reach it, with integers at its edge.
trait Field {
    type E;
    fn modulus(&self) -> BigUint;
    /// Inputs `from` takes are below this, not necessarily below the modulus.
    fn input_bound(&self) -> BigUint;
    fn from(&self, value: &BigUint) -> Self::E;
    /// The canonical value, below the modulus.
    fn value(&self, e: &Self::E) -> BigUint;
    fn add(&self, a: &Self::E, b: &Self::E) -> Self::E;
    fn sub(&self, a: &Self::E, b: &Self::E) -> Self::E;
    fn mul(&self, a: &Self::E, b: &Self::E) -> Self::E;
    fn square(&self, a: &Self::E) -> Self::E;
    /// a^(p-2): the inverse, and zero for zero.
    fn invert(&self, a: &Self::E) -> Self::E;
}

fn le<const N: usize>(value: &BigUint) -> [u8; N] {
    let bytes = value.to_bytes_le();
    assert!(bytes.len() <= N, "{value} is wider than {N} bytes");
    let mut out = [0; N];
    out[..bytes.len()].copy_from_slice(&bytes);
    out
}

/// The field's arithmetic over [`Tight`]: a sum or difference is carried
/// before it is an operand again. Loose operands into a multiplication are
/// [`field25519_loose_operands_match_integers`]'s.
struct Curve25519;

impl Field for Curve25519 {
    type E = Tight;
    fn modulus(&self) -> BigUint {
        (BigUint::from(1u8) << 255) - 19u8
    }
    /// RFC 7748 decodes 255 bits, so values from p up are taken.
    fn input_bound(&self) -> BigUint {
        BigUint::from(1u8) << 255
    }
    fn from(&self, value: &BigUint) -> Tight {
        Tight::from_bytes(&le(value))
    }
    fn value(&self, e: &Tight) -> BigUint {
        BigUint::from_bytes_le(&e.bytes())
    }
    fn add(&self, a: &Tight, b: &Tight) -> Tight {
        a.add(b).carry()
    }
    fn sub(&self, a: &Tight, b: &Tight) -> Tight {
        a.sub(b).carry()
    }
    fn mul(&self, a: &Tight, b: &Tight) -> Tight {
        a.mul(b)
    }
    fn square(&self, a: &Tight) -> Tight {
        a.square()
    }
    fn invert(&self, a: &Tight) -> Tight {
        a.invert()
    }
}

/// The same field through the `const` forms that build compile-time tables.
struct Curve25519Const;

impl Field for Curve25519Const {
    type E = Tight;
    fn modulus(&self) -> BigUint {
        Curve25519.modulus()
    }
    fn input_bound(&self) -> BigUint {
        Curve25519.input_bound()
    }
    fn from(&self, value: &BigUint) -> Tight {
        Curve25519.from(value)
    }
    fn value(&self, e: &Tight) -> BigUint {
        Curve25519.value(e)
    }
    fn add(&self, a: &Tight, b: &Tight) -> Tight {
        a.add(b).carry()
    }
    fn sub(&self, a: &Tight, b: &Tight) -> Tight {
        a.sub(b).carry()
    }
    fn mul(&self, a: &Tight, b: &Tight) -> Tight {
        a.times(b)
    }
    fn square(&self, a: &Tight) -> Tight {
        a.squared()
    }
    fn invert(&self, a: &Tight) -> Tight {
        a.inverted()
    }
}

fn words<const N: usize>(value: &BigUint) -> [u64; N] {
    let mut words = [0; N];
    for (i, word) in value.iter_u64_digits().enumerate() {
        words[i] = word;
    }
    words
}

fn integer<const N: usize>(words: &[u64; N]) -> BigUint {
    BigUint::from_slice(&words.iter().flat_map(|w| [*w as u32, (*w >> 32) as u32]).collect::<Vec<_>>())
}

/// A NIST modulus in Montgomery form: the field or the group order.
struct Montgomery<M, const N: usize>(PhantomData<M>);

impl<M: Modulus<N>, const N: usize> Field for Montgomery<M, N> {
    type E = Residue<M, N>;
    fn modulus(&self) -> BigUint {
        integer(&M::P)
    }
    /// What `from_wide` takes: any N words, below 2p for these moduli.
    fn input_bound(&self) -> BigUint {
        BigUint::from(1u8) << (64 * N)
    }
    fn from(&self, value: &BigUint) -> Residue<M, N> {
        Residue::from_wide(&words(value))
    }
    fn value(&self, e: &Residue<M, N>) -> BigUint {
        let value = integer(&e.integer());
        assert!(value < self.modulus(), "a residue at or above the modulus");
        value
    }
    fn add(&self, a: &Residue<M, N>, b: &Residue<M, N>) -> Residue<M, N> {
        a.add(b)
    }
    fn sub(&self, a: &Residue<M, N>, b: &Residue<M, N>) -> Residue<M, N> {
        a.sub(b)
    }
    fn mul(&self, a: &Residue<M, N>, b: &Residue<M, N>) -> Residue<M, N> {
        a.mul(b)
    }
    fn square(&self, a: &Residue<M, N>) -> Residue<M, N> {
        a.square()
    }
    fn invert(&self, a: &Residue<M, N>) -> Residue<M, N> {
        a.invert()
    }
}

/// The same moduli through the `const` forms that build compile-time tables.
struct MontgomeryConst<M, const N: usize>(PhantomData<M>);

impl<M: Modulus<N>, const N: usize> Field for MontgomeryConst<M, N> {
    type E = Residue<M, N>;
    fn modulus(&self) -> BigUint {
        integer(&M::P)
    }
    fn input_bound(&self) -> BigUint {
        BigUint::from(1u8) << (64 * N)
    }
    fn from(&self, value: &BigUint) -> Residue<M, N> {
        Residue::from_wide(&words(value))
    }
    fn value(&self, e: &Residue<M, N>) -> BigUint {
        let value = integer(&e.integer());
        assert!(value < self.modulus(), "a residue at or above the modulus");
        value
    }
    fn add(&self, a: &Residue<M, N>, b: &Residue<M, N>) -> Residue<M, N> {
        a.plus(b)
    }
    fn sub(&self, a: &Residue<M, N>, b: &Residue<M, N>) -> Residue<M, N> {
        a.minus(b)
    }
    fn mul(&self, a: &Residue<M, N>, b: &Residue<M, N>) -> Residue<M, N> {
        a.times(b)
    }
    fn square(&self, a: &Residue<M, N>) -> Residue<M, N> {
        a.times(a)
    }
    fn invert(&self, a: &Residue<M, N>) -> Residue<M, N> {
        a.inverted()
    }
}

struct XorShift(u64);

impl XorShift {
    fn below(&mut self, bound: &BigUint) -> BigUint {
        let bytes: Vec<u8> = (0..bound.bits().div_ceil(8) + 8)
            .map(|_| {
                self.0 ^= self.0 << 13;
                self.0 ^= self.0 >> 7;
                self.0 ^= self.0 << 17;
                self.0 as u8
            })
            .collect();
        BigUint::from_bytes_le(&bytes) % bound
    }
    fn index(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }
}

/// Values where a carry, a borrow or a reduction changes: 0, 1 and 2; the
/// modulus less 1 and 2; its halves; each limb width's 2^k - 1 and 2^k; the
/// modulus less those; and every all-ones and every single-bit value at a
/// 51- or 64-bit limb boundary below the input bound.
fn boundaries(modulus: &BigUint, bound: &BigUint) -> Vec<BigUint> {
    let one = BigUint::from(1u8);
    let mut out = std::vec![BigUint::from(0u8), one.clone(), BigUint::from(2u8), modulus - 1u8, modulus - 2u8, modulus >> 1, (modulus >> 1) + 1u8];
    for k in (51..bound.bits()).step_by(51).chain((64..bound.bits()).step_by(64)).chain([bound.bits() - 1]) {
        let power = &one << k;
        if &power < modulus {
            out.extend([modulus - &power, modulus - (&power - 1u8)]);
        }
        out.extend([&power - 1u8, power]);
    }
    out.extend([modulus.clone(), modulus + 1u8, bound - 1u8]);
    out.retain(|v| v < bound);
    out
}

fn check<A: Field>(field: &A, seed: u64) {
    let p = field.modulus();
    let bound = field.input_bound();
    let mut rng = XorShift(seed);
    let mut values = boundaries(&p, &bound);
    let edges = values.len();
    values.extend((0..CASES).map(|_| rng.below(&bound)));

    // Each pair of edges, and each seeded value against a random partner.
    // Miri interprets every limb operation: the first edges' pairs there.
    let paired = if cfg!(miri) { 4 } else { edges };
    let pairs = (0..paired).flat_map(|i| (0..paired).map(move |j| (i, j))).chain((edges..values.len()).map(|i| (i, i - edges)));
    for (i, j) in pairs {
        let (a, b) = (&values[i], &values[j]);
        let (x, y) = (field.from(a), field.from(b));
        let (a, b) = (a % &p, b % &p);
        assert_eq!(field.value(&x), a, "decoding {a}");
        assert_eq!(field.value(&field.add(&x, &y)), (&a + &b) % &p, "{a} + {b}");
        assert_eq!(field.value(&field.sub(&x, &y)), (&a + &p - &b) % &p, "{a} - {b}");
        assert_eq!(field.value(&field.mul(&x, &y)), (&a * &b) % &p, "{a} * {b}");
        assert_eq!(field.value(&field.square(&x)), (&a * &a) % &p, "{a}^2");
    }

    // Inversion gives the inverse, and zero for zero, for every value.
    for a in values.iter().take(if cfg!(miri) { 3 } else { values.len() }) {
        let x = field.from(a);
        let inverse = field.invert(&x);
        assert_eq!(field.value(&inverse), (a % &p).modinv(&p).unwrap_or_default(), "1 / {a}");
        if a % &p != BigUint::from(0u8) {
            assert_eq!(field.value(&field.mul(&x, &inverse)), BigUint::from(1u8), "{a} / {a}");
        }
    }

    // A chain: every result is an operand again as the operation left it, so
    // what one operation returns unreduced the next must take.
    let mut pool: Vec<(A::E, BigUint)> = values.iter().take(edges).map(|v| (field.from(v), v % &p)).collect();
    for step in 0..CASES {
        let (i, j) = (rng.index(pool.len()), rng.index(pool.len()));
        let (x, a) = (&pool[i].0, &pool[i].1);
        let (y, b) = (&pool[j].0, &pool[j].1);
        let (e, want) = match step % 4 {
            0 => (field.add(x, y), (a + b) % &p),
            1 => (field.sub(x, y), (a + &p - b) % &p),
            2 => (field.mul(x, y), (a * b) % &p),
            _ => (field.square(x), (a * a) % &p),
        };
        assert_eq!(field.value(&e), want, "chain step {step}");
        let k = rng.index(pool.len());
        pool[k] = (e, want);
    }

    // The laws, on seeded triples: distributivity, and subtraction undoing
    // addition.
    for t in 0..CASES / 4 {
        let [x, y, z] = [0, 1, 2].map(|k| field.from(&values[edges + (3 * t + k) % CASES]));
        let left = field.mul(&x, &field.add(&y, &z));
        let right = field.add(&field.mul(&x, &y), &field.mul(&x, &z));
        assert_eq!(field.value(&left), field.value(&right), "distributivity {t}");
        assert_eq!(field.value(&field.add(&field.sub(&x, &y), &y)), field.value(&x), "(x - y) + y {t}");
    }
}

#[test]
fn field25519_matches_integers() {
    check(&Curve25519, 0x0123_4567_89ab_cdef);
}

#[test]
fn compile_time_field25519_forms_match_integers() {
    check(&Curve25519Const, 0x0fed_cba9_8765_4321);
}

#[test]
fn field25519_negation_and_small_constants_match_integers() {
    let p = Curve25519.modulus();
    for a in boundaries(&p, &Curve25519.input_bound()) {
        let x = Curve25519.from(&a);
        assert_eq!(Curve25519.value(&x.neg().carry()), (&p - &a % &p) % &p, "-{a}");
        assert_eq!(Curve25519.value(&x.mul_small(121665)), (&a * 121665u32) % &p, "121665 * {a}");
        if &a % &p != BigUint::from(0u8) {
            let e = (&p - 5u8) / 8u8;
            assert_eq!(Curve25519.value(&x.pow_p58()), (&a % &p).modpow(&e, &p), "{a}^((p-5)/8)");
        }
    }
}

/// Sums and differences left loose, at their largest, into every
/// multiplication.
#[test]
fn field25519_loose_operands_match_integers() {
    let p = Curve25519.modulus();
    let int = |x: &Tight| BigUint::from_bytes_le(&x.bytes());
    let values = boundaries(&p, &Curve25519.input_bound());
    for (i, a) in values.iter().enumerate() {
        let x = Curve25519.from(a);
        let a = a % &p;
        for b in [&values[(i * 17 + 3) % values.len()], &values[2]] {
            let y = Curve25519.from(b);
            let b = b % &p;
            let (sum, difference) = (x.add(&y), x.sub(&y));
            assert_eq!(int(&sum.mul(&difference)), ((&a + &b) * (&a + &p - &b)) % &p);
            assert_eq!(int(&sum.square()), ((&a + &b) * (&a + &b)) % &p);
            assert_eq!(int(&difference.mul_small(121665)), ((&a + &p - &b) * 121665u32) % &p);
            assert_eq!(int(&x.neg().mul(&sum)), ((&p - &a) * (&a + &b)) % &p);
        }
    }
}

#[test]
fn nist_fields_and_orders_match_integers() {
    /// The check in the arithmetic the CPU's dispatch chooses, named.
    struct Dispatched;
    impl Computation<4> for Dispatched {
        type Output = &'static str;
        fn run<F: Modulus<4>>(self) -> &'static str {
            check(&Montgomery::<F, 4>(PhantomData), 1);
            core::any::type_name::<F>()
        }
    }
    // Which P-256 field path this run checks, for a reader of `--nocapture`.
    std::println!("p-256 field path: {}", P256Field::dispatch(Dispatched));
    check(&Montgomery::<P256Field, 4>(PhantomData), 1);
    check(&Montgomery::<P256Order, 4>(PhantomData), 2);
    check(&Montgomery::<P384Field, 6>(PhantomData), 3);
    check(&Montgomery::<P384Order, 6>(PhantomData), 4);
}

#[test]
fn compile_time_montgomery_forms_match_integers() {
    check(&MontgomeryConst::<P256Field, 4>(PhantomData), 5);
    check(&MontgomeryConst::<P256Order, 4>(PhantomData), 6);
    check(&MontgomeryConst::<P384Field, 6>(PhantomData), 7);
    check(&MontgomeryConst::<P384Order, 6>(PhantomData), 8);
}

/// The variable-time inverse of public integers, from 1 to p - 1: the edges
/// and seeded values.
#[test]
fn public_inverses_match_integers() {
    fn run<M: Modulus<N>, const N: usize>(seed: u64) {
        let p = integer(&M::P);
        let mut rng = XorShift(seed);
        let mut values = boundaries(&p, &p);
        values.extend((0..CASES).map(|_| rng.below(&p)));
        for a in values.iter().filter(|a| **a != BigUint::from(0u8)) {
            let inverse = Residue::<M, N>::inverse_of_public(&words(a));
            assert_eq!(integer(&inverse.integer()), a.modinv(&p).unwrap(), "1 / {a}");
        }
    }
    run::<P256Field, 4>(9);
    run::<P256Order, 4>(10);
    run::<P384Field, 6>(11);
    run::<P384Order, 6>(12);
}

/// The compile-time Montgomery constants, and the canonical decoding that
/// refuses every value from the modulus up.
#[test]
fn nist_constants_and_canonical_decoding_match_integers() {
    fn run<M: Modulus<N>, const N: usize>() {
        let p = integer(&M::P);
        let r = BigUint::from(1u8) << (64 * N);
        assert_eq!(integer(&M::ONE), &r % &p);
        assert_eq!(integer(&M::R2), (&r * &r) % &p);
        assert_eq!(M::P[0].wrapping_mul(M::N0), u64::MAX);
        assert_eq!(integer(&Residue::<M, N>::one().integer()), BigUint::from(1u8));
        for value in boundaries(&p, &r) {
            let decoded = Residue::<M, N>::from_integer(&words(&value));
            assert_eq!(decoded.is_some(), value < p, "{value}");
            if let Some(residue) = decoded {
                assert_eq!(integer(&residue.integer()), value);
            }
        }
    }
    run::<P256Field, 4>();
    run::<P256Order, 4>();
    run::<P384Field, 6>();
    run::<P384Order, 6>();
}

/// The Ed25519 group order's arithmetic: reduction of any width, the
/// canonical (S < L) test, and the signing operation.
#[test]
fn scalar25519_matches_integers() {
    use crate::scalar25519::PublicScalar;
    let order = BigUint::parse_bytes(b"1000000000000000000000000000000014def9dea2f79cd65812631a5cf5d3ed", 16).unwrap();
    let mut rng = XorShift(0xfeed_f00d);
    let wide = BigUint::from(1u8) << 512;
    let mut values = boundaries(&order, &wide);
    values.extend(boundaries(&order, &(BigUint::from(1u8) << 256)));
    values.extend([&order << 252, (&order << 256) - 1u8, &order * 2u8, &order * 3u8 - 1u8]);
    // Multiples of L near 2^512 and their predecessors: where the reduction's
    // last subtraction is taken and where it is not.
    let top = &wide / &order;
    for j in 0u8..4 {
        let multiple = (&top - j) * &order;
        values.extend([&multiple - 1u8, multiple]);
    }
    values.retain(|v| v < &wide);
    values.extend((0..CASES).map(|_| rng.below(&wide)));
    values.extend((0..CASES).map(|_| rng.below(&(BigUint::from(1u8) << 256))));
    let int = |b: [u8; 32]| BigUint::from_bytes_le(&b);
    for (i, a) in values.iter().enumerate() {
        let scalar = S::reduce(&le::<64>(a));
        assert_eq!(int(scalar.bytes()), a % &order, "{a} mod L");
        assert_eq!(int(PublicScalar::reduce(&le::<64>(a)).bytes()), a % &order, "public {a} mod L");
        if a.bits() <= 256 {
            let canonical = PublicScalar::canonical(&le::<32>(a));
            assert_eq!(canonical.as_ref().map(|s| int(s.bytes())), (a < &order).then(|| a.clone()), "S < L for {a}");
        }
        #[cfg(feature = "sign")]
        {
            if a.bits() <= 256 {
                assert_eq!(S::reduce32(&le::<32>(a)).bytes(), scalar.bytes(), "{a} from 32 bytes");
            }
            let b = &values[(i * 17 + 3) % values.len()];
            let c = &values[(i * 5 + 1) % values.len()];
            let (b_scalar, c_scalar) = (S::reduce(&le::<64>(b)), S::reduce(&le::<64>(c)));
            assert_eq!(int(scalar.mul_add(&b_scalar, &c_scalar).bytes()), (a * b + c) % &order, "{a} * {b} + {c}");
        }
        #[cfg(not(feature = "sign"))]
        let _ = i;
    }
}
