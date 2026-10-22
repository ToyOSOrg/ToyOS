//! Integer-oracle checks of reductions, carry chains and representation changes.
use crate::{
    field25519::{Multiply, Tight},
    montgomery::{Modulus, Residue},
    nist::{P256Field, P256Order, P384Field, P384Order},
    scalar25519::S,
};
use num_bigint::BigUint;
use std::vec::Vec;

fn fixtures(modulus: &BigUint, size: usize) -> Vec<BigUint> {
    let mut values = std::vec![
        BigUint::from(0u8),
        BigUint::from(1u8),
        modulus - 1u8,
        modulus - 2u8,
        modulus / 2u8
    ];
    let mut seed = 0x0123456789abcdefu64;
    for _ in 0..64 {
        let mut bytes = std::vec![0; size];
        for byte in &mut bytes {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            *byte = seed as u8;
        }
        values.push(BigUint::from_bytes_le(&bytes) % modulus);
    }
    values
}
fn fixed<const N: usize>(value: &BigUint) -> [u8; N] {
    let bytes = value.to_bytes_le();
    assert!(bytes.len() <= N);
    let mut out = [0; N];
    out[..bytes.len()].copy_from_slice(&bytes);
    out
}

#[test]
fn field25519_carries_and_noncanonical_inputs_match_integer_oracle() {
    let p = (BigUint::from(1u8) << 255) - 19u8;
    let mut values = fixtures(&p, 32);
    values.extend([p.clone(), &p + 1u8, (BigUint::from(1u8) << 255) - 1u8]);
    let int = |x: &Tight| BigUint::from_bytes_le(&x.bytes());
    let int_loose = |x: &crate::field25519::Loose| BigUint::from_bytes_le(&x.carry().bytes());
    for (i, a) in values.iter().enumerate() {
        let x = Tight::from_bytes(&fixed(a));
        let a = a % &p;
        assert_eq!(int(&x), a);
        assert_eq!(int(&x.square()), (&a * &a) % &p);
        assert_eq!(int(&x.mul_small(121665)), (&a * 121665u32) % &p);
        assert_eq!(int_loose(&x.neg()), (&p - &a) % &p);
        if a != BigUint::from(0u8) {
            assert_eq!(int(&x.mul(&x.invert())), BigUint::from(1u8));
            let e = (&p - 5u8) / 8u8;
            assert_eq!(int(&x.pow_p58()), a.modpow(&e, &p));
        }
        for b in [&values[(i * 17 + 3) % values.len()], &values[2]] {
            let y = Tight::from_bytes(&fixed(b));
            let b = b % &p;
            let sum = x.add(&y);
            let difference = x.sub(&y);
            assert_eq!(int_loose(&sum), (&a + &b) % &p);
            assert_eq!(int_loose(&difference), (&a + &p - &b) % &p);
            assert_eq!(int(&x.mul(&y)), (&a * &b) % &p);
            // Loose operands, at their largest, into every multiplication.
            assert_eq!(int(&sum.mul(&difference)), ((&a + &b) * (&a + &p - &b)) % &p);
            assert_eq!(int(&sum.square()), ((&a + &b) * (&a + &b)) % &p);
            assert_eq!(int(&difference.mul_small(121665)), ((&a + &p - &b) * 121665u32) % &p);
        }
    }
}

/// The largest limbs the operations reach, through every operation: the
/// bounds the types state are enough for the carries.
#[test]
fn field25519_largest_reachable_limbs_stay_within_their_bounds() {
    let p = (BigUint::from(1u8) << 255) - 19u8;
    let tight = Tight::from_limbs([(1 << 51) - 1, (1 << 51) + (1 << 19) - 1, (1 << 51) - 1, (1 << 51) - 1, (1 << 51) - 1]);
    let value = |l: &[u64; 5]| l.iter().enumerate().fold(BigUint::from(0u8), |v, (i, &x)| v + (BigUint::from(x) << (51 * i)));
    let t = value(crate::field25519::Operand::limbs(&tight)) % &p;
    let loose = tight.add(&tight);
    let l = value(crate::field25519::Operand::limbs(&loose)) % &p;
    assert_eq!(BigUint::from_bytes_le(&tight.bytes()), t);
    assert_eq!(BigUint::from_bytes_le(&loose.mul(&loose).bytes()), (&l * &l) % &p);
    assert_eq!(BigUint::from_bytes_le(&loose.square().bytes()), (&l * &l) % &p);
    assert_eq!(BigUint::from_bytes_le(&tight.sub(&tight).carry().bytes()), BigUint::from(0u8));
    assert_eq!(BigUint::from_bytes_le(&loose.mul_small(121665).bytes()), (&l * 121665u32) % &p);
    let widest = tight.sub(&Tight::ZERO);
    let w = value(crate::field25519::Operand::limbs(&widest)) % &p;
    assert_eq!(BigUint::from_bytes_le(&widest.mul(&loose).bytes()), (&w * &l) % &p);
    assert_eq!(BigUint::from_bytes_le(&widest.square().bytes()), (&w * &w) % &p);
    for limb in crate::field25519::Operand::limbs(&loose.mul(&loose)) {
        assert!(*limb < (1 << 51) + (1 << 19));
    }
}

#[test]
fn scalar25519_reduction_and_signing_arithmetic_match_integer_oracle() {
    let order = BigUint::parse_bytes(
        b"1000000000000000000000000000000014def9dea2f79cd65812631a5cf5d3ed",
        16,
    )
    .unwrap();
    let wide = (BigUint::from(1u8) << 512) - 1u8;
    let mut values = fixtures(&wide, 64);
    values.extend([wide, order.clone(), &order - 1u8]);
    for (i, a) in values.iter().enumerate() {
        let scalar = S::reduce(&fixed::<64>(a));
        assert_eq!(BigUint::from_bytes_le(&scalar.bytes()), a % &order);
        #[cfg(feature = "sign")]
        {
            let b = &values[(i * 17 + 3) % values.len()];
            let b_scalar = S::reduce(&fixed::<64>(b));
            assert_eq!(
                BigUint::from_bytes_le(&scalar.add(&b_scalar).bytes()),
                (a + b) % &order
            );
            assert_eq!(
                BigUint::from_bytes_le(&scalar.mul(&b_scalar).bytes()),
                (a * b) % &order
            );
        }
        #[cfg(not(feature = "sign"))]
        let _ = i;
    }
}

fn check_montgomery<M: Modulus<N>, const N: usize>() {
    let integer = |words: &[u64; N]| {
        BigUint::from_bytes_le(&words.iter().flat_map(|word| word.to_le_bytes()).collect::<Vec<_>>())
    };
    let p = integer(&M::P);
    let r = BigUint::from(1u8) << (64 * N);
    assert_eq!(integer(&M::ONE), &r % &p);
    assert_eq!(integer(&M::R2), (&r * &r) % &p);
    assert_eq!(M::P[0].wrapping_mul(M::N0), u64::MAX);
    let words = |value: &BigUint| {
        let source = value.to_bytes_le();
        let mut bytes = std::vec![0; N * 8];
        bytes[..source.len()].copy_from_slice(&source);
        core::array::from_fn(|i| u64::from_le_bytes(bytes[8 * i..8 * i + 8].try_into().unwrap()))
    };
    let values = fixtures(&p, N * 8);
    assert!(Residue::<M, N>::from_integer(&M::P).is_none());
    assert_eq!(integer(&Residue::<M, N>::one().integer()), BigUint::from(1u8));
    let all_ones = (BigUint::from(1u8) << (64 * N)) - 1u8;
    assert_eq!(integer(&Residue::<M, N>::from_wide(&words(&all_ones)).integer()), &all_ones % &p);
    for (i, a) in values.iter().enumerate() {
        let a_encoded = Residue::<M, N>::from_integer(&words(a)).unwrap();
        assert_eq!(&integer(&a_encoded.integer()), a);
        for b in [&values[(i * 17 + 3) % values.len()], &values[2]] {
            let b_encoded = Residue::<M, N>::from_integer(&words(b)).unwrap();
            let sum = a_encoded.add(&b_encoded);
            let difference = a_encoded.sub(&b_encoded);
            let product = a_encoded.mul(&b_encoded);
            assert_eq!(integer(&sum.integer()), (a + b) % &p);
            assert_eq!(integer(&difference.integer()), (a + &p - b) % &p);
            assert_eq!(integer(&product.integer()), (a * b) % &p);
            assert_eq!(integer(&a_encoded.square().integer()), (a * a) % &p);
        }
        if (1..=4).contains(&i) {
            assert_eq!(a_encoded.mul(&a_encoded.invert()).eq(&Residue::one()), 1);
        }
    }
}
#[test]
fn nist_montgomery_domains_and_carries_match_integer_oracle() {
    check_montgomery::<P256Field, 4>();
    check_montgomery::<P256Order, 4>();
    check_montgomery::<P384Field, 6>();
    check_montgomery::<P384Order, 6>();
}
