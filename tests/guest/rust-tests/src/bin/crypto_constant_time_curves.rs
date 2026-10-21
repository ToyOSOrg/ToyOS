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

use constant_time::{leaky_control, measure, Class, Rng};
use toyos_crypto::ecc::{p256, p384};

/// A secret scalar: 1 for the fixed class, a uniform one otherwise, redrawn
/// on the rare string at or past the order.
macro_rules! scalar {
    ($curve:ident, $bytes:literal) => {
        |class: Class, rng: &mut Rng| -> $curve::SecretKey {
            match class {
                Class::Fixed => {
                    let mut one = [0; $bytes];
                    one[$bytes - 1] = 1;
                    $curve::SecretKey::from_bytes(one).unwrap()
                }
                Class::Random => loop {
                    if let Ok(key) = $curve::SecretKey::from_bytes(rng.array()) {
                        break key;
                    }
                },
            }
        }
    };
}

fn main() {
    let mut rng = Rng(0x0c0f_fee6);
    leaky_control(&mut rng);

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
        |class, rng| match class {
            Class::Fixed => [0; 32],
            Class::Random => rng.array(),
        },
        |seed| {
            std::hint::black_box(toyos_crypto::ed25519::SigningKey::from_seed(seed).sign(&[0x6d; 64]).unwrap());
        },
    );
}
