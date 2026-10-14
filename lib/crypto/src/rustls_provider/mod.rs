//! Thin adapters for unmodified rustls. TLS framing helpers, transcript handling,
//! certificate parsing and protocol state remain in rustls and webpki. Allocation
//! exists only at this feature boundary; the primitive core remains allocation-free.
//!
//! A static [`Backend`] binds key exchange to the caller's secure entropy source.
//! No default provider or global RNG is installed. Sealing checks rustls's sequence
//! against its owned monotonic counter. Secret operations inherit the primitive
//! contracts; signature verification and DER refusal use public data.
//!
//! The current inventory is TLS 1.3 and TLS 1.2 ECDHE-AEAD with X25519, P-256,
//! P-384 and X25519MLKEM768 (TLS 1.3 only). QUIC and private signing-key loading are not
//! advertised by this adapter. Client authentication may supply a custom rustls
//! certificate resolver. Provider and interoperability qualification is ongoing.
use alloc::{sync::Arc, vec};
use rustls::{
    CipherSuite, CipherSuiteCommon, Error, NamedGroup, SignatureScheme, SupportedCipherSuite,
    Tls12CipherSuite, Tls13CipherSuite,
    crypto::{CryptoProvider, KeyProvider, SecureRandom},
    pki_types::PrivateKeyDer,
    sign::SigningKey,
};
mod cipher;
mod hash;
mod kx;
mod verify;

pub struct Backend {
    groups: [kx::Group; 4],
    random: &'static dyn SecureRandom,
}
impl Backend {
    pub const fn new(random: &'static dyn SecureRandom) -> Self {
        Self {
            random,
            groups: [
                kx::Group {
                    name: NamedGroup::X25519MLKEM768,
                    random,
                },
                kx::Group {
                    name: NamedGroup::X25519,
                    random,
                },
                kx::Group {
                    name: NamedGroup::secp256r1,
                    random,
                },
                kx::Group {
                    name: NamedGroup::secp384r1,
                    random,
                },
            ],
        }
    }
    pub fn provider(&'static self) -> CryptoProvider {
        CryptoProvider {
            cipher_suites: vec![
                SupportedCipherSuite::Tls13(&TLS13_AES128),
                SupportedCipherSuite::Tls13(&TLS13_AES256),
                SupportedCipherSuite::Tls13(&TLS13_CHACHA),
                SupportedCipherSuite::Tls12(&TLS12_RSA_AES128),
                SupportedCipherSuite::Tls12(&TLS12_RSA_AES256),
                SupportedCipherSuite::Tls12(&TLS12_RSA_CHACHA),
                SupportedCipherSuite::Tls12(&TLS12_ECDSA_AES128),
                SupportedCipherSuite::Tls12(&TLS12_ECDSA_AES256),
                SupportedCipherSuite::Tls12(&TLS12_ECDSA_CHACHA),
            ],
            kx_groups: self
                .groups
                .iter()
                .map(|g| g as &dyn rustls::crypto::SupportedKxGroup)
                .collect(),
            signature_verification_algorithms: verify::SUPPORTED,
            secure_random: self.random,
            key_provider: &VERIFY_ONLY,
        }
    }
}
#[derive(Debug)]
struct VerifyOnly;
static VERIFY_ONLY: VerifyOnly = VerifyOnly;
impl KeyProvider for VerifyOnly {
    fn load_private_key(&self, _: PrivateKeyDer<'static>) -> Result<Arc<dyn SigningKey>, Error> {
        Err(Error::General(alloc::string::String::from(
            "private signing keys require a caller certificate resolver",
        )))
    }
}
macro_rules! suite13 {
    ($name:ident,$id:ident,$hash:ident,$aead:ident,$limit:expr) => {
        static $name: Tls13CipherSuite = Tls13CipherSuite {
            common: CipherSuiteCommon {
                suite: CipherSuite::$id,
                hash_provider: &hash::$hash,
                confidentiality_limit: $limit,
            },
            hkdf_provider: &hash::$hash,
            aead_alg: &cipher::Algorithm::$aead,
            quic: None,
        };
    };
}
suite13!(
    TLS13_AES128,
    TLS13_AES_128_GCM_SHA256,
    SHA256,
    Aes128,
    1 << 23
);
suite13!(
    TLS13_AES256,
    TLS13_AES_256_GCM_SHA384,
    SHA384,
    Aes256,
    1 << 23
);
suite13!(
    TLS13_CHACHA,
    TLS13_CHACHA20_POLY1305_SHA256,
    SHA256,
    ChaCha,
    u64::MAX
);
static RSA_SCHEMES: &[SignatureScheme] = &[
    SignatureScheme::RSA_PSS_SHA256,
    SignatureScheme::RSA_PSS_SHA384,
    SignatureScheme::RSA_PSS_SHA512,
    SignatureScheme::RSA_PKCS1_SHA256,
    SignatureScheme::RSA_PKCS1_SHA384,
    SignatureScheme::RSA_PKCS1_SHA512,
];
static EC_SCHEMES: &[SignatureScheme] = &[
    SignatureScheme::ECDSA_NISTP256_SHA256,
    SignatureScheme::ECDSA_NISTP384_SHA384,
    SignatureScheme::ED25519,
];
macro_rules! suite12 {
    ($name:ident,$id:ident,$hash:ident,$aead:ident,$sign:ident,$limit:expr) => {
        static $name: Tls12CipherSuite = Tls12CipherSuite {
            common: CipherSuiteCommon {
                suite: CipherSuite::$id,
                hash_provider: &hash::$hash,
                confidentiality_limit: $limit,
            },
            prf_provider: &rustls::crypto::tls12::PrfUsingHmac(&hash::$hash),
            kx: rustls::crypto::KeyExchangeAlgorithm::ECDHE,
            sign: $sign,
            aead_alg: &cipher::Algorithm::$aead,
        };
    };
}
suite12!(
    TLS12_RSA_AES128,
    TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256,
    SHA256,
    Aes128,
    RSA_SCHEMES,
    1 << 23
);
suite12!(
    TLS12_RSA_AES256,
    TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384,
    SHA384,
    Aes256,
    RSA_SCHEMES,
    1 << 23
);
suite12!(
    TLS12_RSA_CHACHA,
    TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256,
    SHA256,
    ChaCha,
    RSA_SCHEMES,
    u64::MAX
);
suite12!(
    TLS12_ECDSA_AES128,
    TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256,
    SHA256,
    Aes128,
    EC_SCHEMES,
    1 << 23
);
suite12!(
    TLS12_ECDSA_AES256,
    TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384,
    SHA384,
    Aes256,
    EC_SCHEMES,
    1 << 23
);
suite12!(
    TLS12_ECDSA_CHACHA,
    TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256,
    SHA256,
    ChaCha,
    EC_SCHEMES,
    u64::MAX
);

#[cfg(test)]
mod tests;
