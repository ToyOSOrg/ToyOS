//! Integer-oracle checks of reductions, carry chains and representation changes.
use crate::{
    field25519::F,
    montgomery::{Mod, V},
    scalar25519::S,
};
use rsa::BigUint;
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
    for (i, a) in values.iter().enumerate() {
        let a_field = F::from_bytes(&fixed(a));
        assert_eq!(BigUint::from_bytes_le(&a_field.bytes()), a % &p);
        for b in [&values[(i * 17 + 3) % values.len()], &values[2]] {
            let b_field = F::from_bytes(&fixed(b));
            assert_eq!(
                BigUint::from_bytes_le(&a_field.add(&b_field).bytes()),
                (a + b) % &p
            );
            assert_eq!(
                BigUint::from_bytes_le(&a_field.sub(&b_field).bytes()),
                ((a % &p) + &p - (b % &p)) % &p
            );
            assert_eq!(
                BigUint::from_bytes_le(&a_field.mul(&b_field).bytes()),
                (a * b) % &p
            );
        }
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

fn check_montgomery<const N: usize>(m: &Mod<N>) {
    let p = BigUint::from_bytes_le(
        &m.p.iter()
            .flat_map(|word| word.to_le_bytes())
            .collect::<Vec<_>>(),
    );
    let values = fixtures(&p, N * 8);
    let words = |value: &BigUint| {
        let source = value.to_bytes_le();
        let mut bytes = std::vec![0; N * 8];
        bytes[..source.len()].copy_from_slice(&source);
        V(core::array::from_fn(|i| {
            u64::from_le_bytes(bytes[8 * i..8 * i + 8].try_into().unwrap())
        }))
    };
    let integer = |value: &V<N>| {
        BigUint::from_bytes_le(
            &value
                .0
                .iter()
                .flat_map(|word| word.to_le_bytes())
                .collect::<Vec<_>>(),
        )
    };
    assert_eq!(integer(&m.raw(&m.one())), BigUint::from(1u8));
    for (i, a) in values.iter().enumerate() {
        let a = words(a);
        let a_encoded = m.encode_raw(&a);
        assert_eq!(integer(&m.raw(&a_encoded)), integer(&a));
        for b in [&values[(i * 17 + 3) % values.len()], &values[2]] {
            let b_encoded = m.encode_raw(&words(b));
            let sum = m.add(&a_encoded, &b_encoded);
            let difference = m.sub(&a_encoded, &b_encoded);
            let product = m.mul(&a_encoded, &b_encoded);
            for value in [&sum, &difference, &product] {
                assert_eq!(m.valid(value), 1);
            }
            assert_eq!(integer(&m.raw(&sum)), (integer(&a) + b) % &p);
            assert_eq!(integer(&m.raw(&difference)), (integer(&a) + &p - b) % &p);
            assert_eq!(integer(&m.raw(&product)), (integer(&a) * b) % &p);
        }
        if (1..=4).contains(&i) {
            assert_eq!(m.mul(&a_encoded, &m.invert(&a_encoded)).eq(&m.one()), 1);
        }
    }
}
#[test]
fn nist_montgomery_domains_and_carries_match_integer_oracle() {
    let c = crate::nist::p256();
    check_montgomery(&c.p);
    check_montgomery(&c.n);
    let c = crate::nist::p384();
    check_montgomery(&c.p);
    check_montgomery(&c.n);
}
