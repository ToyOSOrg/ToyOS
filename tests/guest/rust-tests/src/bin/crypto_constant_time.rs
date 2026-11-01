//! `toyos-crypto`'s secret routines but the NIST curves' and Ed25519's,
//! timed fixed against random (`../constant_time.rs`): the AEADs' and
//! OpenSSH's sealing and refusal at 64 B and 1 KiB, HMAC, HKDF,
//! PBKDF2, scrypt's Salsa20/8, X25519 and ML-KEM-768's encapsulation and
//! decapsulation. scrypt's ROMix
//! is not timed: the entry it reads is a function of the secret by design
//! (RFC 7914 §5). Its
//! product is a line per routine on a bench's own CPU; the
//! `crypto_secret_timing` metal row runs it on the T14 and the AMD bench.
//!
//! Each fixed secret is a special value (the issue's rule): the all-zero
//! key for ciphers and MACs, and for X25519 the all-zero string, which
//! clamping makes 2^254, its lowest-weight scalar.

#[path = "../constant_time.rs"]
mod constant_time;
#[path = "../arch/counter.rs"]
mod counter;

use constant_time::{finish, leaky_control, measure, null_control, secret, Class, Rng};
use toyos_crypto::aead::{OpeningKey, SealingKey};
use toyos_crypto::hash::Sha256;
use toyos_crypto::kdf::{pbkdf2, Hkdf, Hmac};
use toyos_crypto::mlkem::{DecapsulationKey, EncapsulationKey};
use toyos_crypto::x25519;

/// A TLS record's worth: one 64-byte payload and its 5-byte header.
const PAYLOAD: [u8; 64] = [0x70; 64];
/// A record long enough for every AEAD's stitched path, whose refusal zeroes
/// what it decrypted.
const RECORD: [u8; 1024] = [0x70; 1024];
const AAD: [u8; 5] = [0x17, 0x03, 0x03, 0x00, 0x50];
const IV: [u8; 12] = [0x24; 12];

fn key<const N: usize>(class: Class, rng: &mut Rng) -> [u8; N] {
    secret(class, rng, [0; N])
}

/// Entropy of given bytes, for ML-KEM's seeds and messages.
struct Given<'a>(&'a [u8]);

impl toyos_crypto::Random for Given<'_> {
    fn fill(&mut self, out: &mut [u8]) -> Result<(), toyos_crypto::Error> {
        let (head, rest) = self.0.split_at(out.len());
        out.copy_from_slice(head);
        self.0 = rest;
        Ok(())
    }
}

type Seal = fn([u8; 32]) -> SealingKey;
type Open = fn([u8; 32]) -> OpeningKey;

fn main() {
    let mut rng = Rng(0x0c0f_fee5);
    let mut held = leaky_control(&mut rng) & null_control(&mut rng);

    let aes128: Seal = |k| SealingKey::aes128_gcm(k[..16].try_into().unwrap(), IV);
    let aes256: Seal = |k| SealingKey::aes256_gcm(k, IV);
    let chacha: Seal = |k| SealingKey::chacha20_poly1305(k, IV);
    // A refusal compares the expected tag, a function of the key, with the
    // one received: under the fixed key all but its last byte agree.
    let aeads: [(&str, Seal, Open); 3] = [
        ("aes-128-gcm", aes128, |k| OpeningKey::aes128_gcm(k[..16].try_into().unwrap())),
        ("aes-256-gcm", aes256, OpeningKey::aes256_gcm),
        ("chacha20-poly1305", chacha, OpeningKey::chacha20_poly1305),
    ];
    for (name, seal, open) in aeads {
        measure(&format!("{name} key setup and seal, 64 B"), &mut rng, key::<32>, |k| {
            let mut data = PAYLOAD;
            std::hint::black_box(seal(k).seal(&AAD, &mut data).unwrap());
        });
        let mut sealed = PAYLOAD;
        let (_, mut forged) = seal([0; 32]).seal(&AAD, &mut sealed).unwrap();
        forged[15] ^= 1;
        measure(&format!("{name} key setup and refusal, 64 B"), &mut rng, key::<32>, |k| {
            let mut data = sealed;
            assert!(open(k).open(&IV, &AAD, &mut data, &forged).is_err());
        });
    }
    for (name, seal, open) in &aeads {
        measure(&format!("{name} key setup and seal, 1 KiB"), &mut rng, key::<32>, |k| {
            let mut data = RECORD;
            std::hint::black_box(seal(k).seal(&AAD, &mut data).unwrap());
        });
        let mut sealed = RECORD;
        let (_, mut forged) = seal([0; 32]).seal(&AAD, &mut sealed).unwrap();
        forged[15] ^= 1;
        measure(&format!("{name} key setup and refusal, 1 KiB"), &mut rng, key::<32>, |k| {
            let mut data = sealed;
            assert!(open(k).open(&IV, &AAD, &mut data, &forged).is_err());
        });
    }

    measure("chacha20-poly1305@openssh.com seal, 64 B", &mut rng, key::<64>, |k| {
        let (mut header, mut data) = ([0, 0, 0, 64], PAYLOAD);
        std::hint::black_box(toyos_crypto::ssh::SealingKey::new(k, 3).seal(&mut header, &mut data).unwrap());
    });
    let (mut header, mut sealed) = ([0, 0, 0, 64], PAYLOAD);
    let (_, mut forged) = toyos_crypto::ssh::SealingKey::new([0; 64], 3).seal(&mut header, &mut sealed).unwrap();
    forged[15] ^= 1;
    measure("chacha20-poly1305@openssh.com refusal, 64 B", &mut rng, key::<64>, |k| {
        let (mut h, mut data) = (header, sealed);
        assert!(toyos_crypto::ssh::OpeningKey::new(k).open(3, &mut h, &mut data, &forged).is_err());
    });

    measure("chacha20-poly1305@openssh.com seal, 1 KiB", &mut rng, key::<64>, |k| {
        let (mut header, mut data) = ([0, 0, 4, 0], RECORD);
        std::hint::black_box(toyos_crypto::ssh::SealingKey::new(k, 3).seal(&mut header, &mut data).unwrap());
    });
    let (mut header, mut sealed) = ([0, 0, 4, 0], RECORD);
    let (_, mut forged) = toyos_crypto::ssh::SealingKey::new([0; 64], 3).seal(&mut header, &mut sealed).unwrap();
    forged[15] ^= 1;
    measure("chacha20-poly1305@openssh.com refusal, 1 KiB", &mut rng, key::<64>, |k| {
        let (mut h, mut data) = (header, sealed);
        assert!(toyos_crypto::ssh::OpeningKey::new(k).open(3, &mut h, &mut data, &forged).is_err());
    });

    measure("hmac-sha256, 32 B key, 64 B", &mut rng, key::<32>, |k| {
        let mut tag = [0; 32];
        Hmac::<Sha256>::authenticate(&k, &PAYLOAD, &mut tag).unwrap();
        std::hint::black_box(tag);
    });
    let mut expected = [0; 32];
    Hmac::<Sha256>::authenticate(&[0; 32], &PAYLOAD, &mut expected).unwrap();
    expected[31] ^= 1;
    measure("hmac-sha256 refusal, 64 B", &mut rng, key::<32>, |k| {
        let mut mac = Hmac::<Sha256>::new(&k).unwrap();
        mac.update(&PAYLOAD).unwrap();
        assert!(mac.verify(&expected).is_err());
    });
    measure("hkdf-sha256 expand, 32 B", &mut rng, key::<32>, |prk| {
        let mut out = [0; 32];
        Hkdf::<Sha256>::from_prk(&prk).unwrap().expand(&[b"tls13 key"], &mut out).unwrap();
        std::hint::black_box(out);
    });
    // scrypt's PBKDF2 takes the passphrase as its password and, at its end,
    // ROMix's secret output as its salt: both are the secret here.
    measure("pbkdf2-hmac-sha256, 32 B password and salt, 2 iterations, 64 B", &mut rng, key::<64>, |k| {
        let mut out = [0; 64];
        pbkdf2::<Sha256>(&k[..32], &k[32..], 2.try_into().unwrap(), &mut out).unwrap();
        std::hint::black_box(out);
    });
    measure("scrypt salsa20/8 core", &mut rng, key::<64>, |k| {
        let mut block: [u32; 16] = core::array::from_fn(|i| u32::from_le_bytes(k[4 * i..4 * i + 4].try_into().unwrap()));
        toyos_crypto::scrypt::salsa20_8(&mut block);
        std::hint::black_box(block);
    });

    let peer = x25519::SecretKey::from_bytes([0x5a; 32]).public_key();
    measure("x25519 agreement", &mut rng, key::<32>, |k| {
        std::hint::black_box(x25519::SecretKey::from_bytes(k).agree(&peer).unwrap());
    });
    measure("x25519 public key", &mut rng, key::<32>, |k| {
        std::hint::black_box(x25519::SecretKey::from_bytes(k).public_key());
    });

    // Key generation is not timed here: its matrix expansion rejects
    // samples by rho, a public output of the secret seed (FIPS 203
    // Algorithm 13), so no two classes can share it.
    let dk = || DecapsulationKey::generate(&mut Given(&[0; 64])).unwrap();
    let ek = EncapsulationKey::from_bytes(dk().public_key().as_bytes()).unwrap();
    measure("ml-kem-768 encapsulation", &mut rng, key::<32>, |m| {
        std::hint::black_box(ek.encapsulate(&mut Given(&m)).unwrap());
    });
    // Under one key, so one rho: the fixed class decapsulates a valid
    // ciphertext, the random class a uniform one, rejected implicitly.
    // Whether the re-encryption matched is the secret.
    let (valid, _) = ek.encapsulate(&mut Given(&[0x33; 32])).unwrap();
    measure(
        "ml-kem-768 decapsulation, valid or rejected",
        &mut rng,
        |class, rng| (dk(), secret(class, rng, valid)),
        |(dk, ct)| {
            std::hint::black_box(dk.decapsulate(&ct).unwrap());
        },
    );
    held &= leaky_control(&mut rng) & null_control(&mut rng);
    finish(held);
}
