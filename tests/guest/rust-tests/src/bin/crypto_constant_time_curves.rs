//! `toyos-crypto`'s P-256, P-384 and Ed25519 secret routines, timed fixed
//! against random (`../constant_time.rs`): key derivation, agreement and
//! signing. Its product is a line per routine on a bench's own CPU; the
//! `crypto_secret_timing_curves` metal row runs it on the T14 and the AMD
//! bench, in a boot of its own because it takes the bench for hours.
//!
//! Each fixed secret is a special value (the issue's rule): the scalar 1,
//! and for Ed25519 the all-zero seed.

#[path = "../constant_time.rs"]
mod constant_time;
#[path = "../arch/counter.rs"]
mod counter;

use constant_time::{finish, leaky_control, measure, null_control, secret, Class, Rng};
use toyos_crypto::ecc::{p256, p384};

/// A secret scalar: 1 for the fixed class, a uniform one otherwise. Both
/// draw until a string is below the order, so making either costs the same.
macro_rules! scalar {
    ($curve:ident, $bytes:literal) => {
        |class: Class, rng: &mut Rng| -> $curve::SecretKey {
            let random = loop {
                if let Ok(key) = $curve::SecretKey::from_bytes(rng.array()) {
                    break key;
                }
            };
            let mut one = [0; $bytes];
            one[$bytes - 1] = 1;
            let one = $curve::SecretKey::from_bytes(one).unwrap();
            match class {
                Class::Fixed => one,
                Class::Random => random,
            }
        }
    };
}

fn main() {
    let mut rng = Rng(0x0c0f_fee6);
    let mut held = leaky_control(&mut rng) & null_control(&mut rng);

    let peer = p256::SecretKey::from_bytes([0x5a; 32]).unwrap().public_key();
    measure("p-256 public key", &mut rng, scalar!(p256, 32), |k| {
        std::hint::black_box(k.public_key());
    });
    measure("p-256 agreement", &mut rng, scalar!(p256, 32), |k| {
        std::hint::black_box(k.agree(&peer).unwrap());
    });
    let peer = p384::SecretKey::from_bytes([0x5a; 48]).unwrap().public_key();
    measure("p-384 public key", &mut rng, scalar!(p384, 48), |k| {
        std::hint::black_box(k.public_key());
    });
    measure("p-384 agreement", &mut rng, scalar!(p384, 48), |k| {
        std::hint::black_box(k.agree(&peer).unwrap());
    });
    measure(
        "ed25519 key derivation and signature, 64 B",
        &mut rng,
        |class, rng| secret(class, rng, [0; 32]),
        |seed| {
            std::hint::black_box(toyos_crypto::ed25519::SigningKey::from_seed(seed).sign(&[0x6d; 64]).unwrap());
        },
    );
    // Again after hours of measurements, which a drifting machine would fail.
    held &= leaky_control(&mut rng) & null_control(&mut rng);
    finish(held);
}
