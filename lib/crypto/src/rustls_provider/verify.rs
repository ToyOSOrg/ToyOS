use crate::{ecc, ed25519, rsa};
use rustls::{
    SignatureScheme,
    crypto::WebPkiSupportedAlgorithms,
    pki_types::{AlgorithmIdentifier, InvalidSignature, SignatureVerificationAlgorithm, alg_id},
};
#[derive(Debug)]
enum Algorithm {
    P256,
    P256Sha384,
    P384,
    P384Sha256,
    Ed25519,
    Rsa { hash: rsa::Hash, pss: bool },
}
static P256: Algorithm = Algorithm::P256;
static P256_SHA384: Algorithm = Algorithm::P256Sha384;
static P384: Algorithm = Algorithm::P384;
static P384_SHA256: Algorithm = Algorithm::P384Sha256;
static ED25519: Algorithm = Algorithm::Ed25519;
static PKCS256: Algorithm = Algorithm::Rsa {
    hash: rsa::Hash::Sha256,
    pss: false,
};
static PKCS384: Algorithm = Algorithm::Rsa {
    hash: rsa::Hash::Sha384,
    pss: false,
};
static PKCS512: Algorithm = Algorithm::Rsa {
    hash: rsa::Hash::Sha512,
    pss: false,
};
static PSS256: Algorithm = Algorithm::Rsa {
    hash: rsa::Hash::Sha256,
    pss: true,
};
static PSS384: Algorithm = Algorithm::Rsa {
    hash: rsa::Hash::Sha384,
    pss: true,
};
static PSS512: Algorithm = Algorithm::Rsa {
    hash: rsa::Hash::Sha512,
    pss: true,
};

// Only the bounded RSAPublicKey value is decoded here; X.509 stays in webpki.
fn element<'a>(input: &mut &'a [u8], tag: u8) -> Result<&'a [u8], InvalidSignature> {
    if input.len() < 2 || input[0] != tag {
        return Err(InvalidSignature);
    }
    let first = input[1];
    let (header, len) = if first < 128 {
        (2, usize::from(first))
    } else {
        let count = usize::from(first & 127);
        if count == 0 || count > 2 || input.len() < 2 + count || input[2] == 0 {
            return Err(InvalidSignature);
        }
        let len = input[2..2 + count]
            .iter()
            .fold(0usize, |a, &b| (a << 8) | usize::from(b));
        if len < 128 {
            return Err(InvalidSignature);
        }
        (2 + count, len)
    };
    if len > input.len() - header {
        return Err(InvalidSignature);
    }
    let value = &input[header..header + len];
    *input = &input[header + len..];
    Ok(value)
}
fn integer<'a>(input: &mut &'a [u8]) -> Result<&'a [u8], InvalidSignature> {
    let value = element(input, 2)?;
    if value.is_empty() || value[0] & 128 != 0 {
        return Err(InvalidSignature);
    }
    if value[0] == 0 {
        if value.len() < 2 || value[1] & 128 == 0 {
            return Err(InvalidSignature);
        }
        Ok(&value[1..])
    } else {
        Ok(value)
    }
}
fn rsa_key(mut encoded: &[u8]) -> Result<rsa::PublicKey, InvalidSignature> {
    if encoded.len() > 530 {
        return Err(InvalidSignature);
    }
    let mut sequence = element(&mut encoded, 0x30)?;
    if !encoded.is_empty() {
        return Err(InvalidSignature);
    }
    let modulus = integer(&mut sequence)?;
    let exponent = integer(&mut sequence)?;
    if !sequence.is_empty() {
        return Err(InvalidSignature);
    }
    rsa::PublicKey::from_components(modulus, exponent).map_err(|_| InvalidSignature)
}
impl SignatureVerificationAlgorithm for Algorithm {
    fn public_key_alg_id(&self) -> AlgorithmIdentifier {
        match self {
            Self::P256 | Self::P256Sha384 => alg_id::ECDSA_P256,
            Self::P384 | Self::P384Sha256 => alg_id::ECDSA_P384,
            Self::Ed25519 => alg_id::ED25519,
            Self::Rsa { .. } => alg_id::RSA_ENCRYPTION,
        }
    }
    fn signature_alg_id(&self) -> AlgorithmIdentifier {
        match self {
            Self::P256 | Self::P384Sha256 => alg_id::ECDSA_SHA256,
            Self::P384 | Self::P256Sha384 => alg_id::ECDSA_SHA384,
            Self::Ed25519 => alg_id::ED25519,
            Self::Rsa {
                hash: rsa::Hash::Sha256,
                pss: false,
            } => alg_id::RSA_PKCS1_SHA256,
            Self::Rsa {
                hash: rsa::Hash::Sha384,
                pss: false,
            } => alg_id::RSA_PKCS1_SHA384,
            Self::Rsa { pss: false, .. } => alg_id::RSA_PKCS1_SHA512,
            Self::Rsa {
                hash: rsa::Hash::Sha256,
                pss: true,
            } => alg_id::RSA_PSS_SHA256,
            Self::Rsa {
                hash: rsa::Hash::Sha384,
                pss: true,
            } => alg_id::RSA_PSS_SHA384,
            Self::Rsa { pss: true, .. } => alg_id::RSA_PSS_SHA512,
        }
    }
    fn verify_signature(
        &self,
        key: &[u8],
        message: &[u8],
        signature: &[u8],
    ) -> Result<(), InvalidSignature> {
        let result = match self {
            // SEC 1 bits2int truncates to the order width; shorter hashes are left-padded.
            Self::P256Sha384 => ecc::p256::PublicKey::from_sec1(key).and_then(|p| {
                let digest = crate::hash::Sha384::digest(message)?;
                p.verify_digest(digest[..32].try_into().unwrap(), signature)
            }),
            Self::P384Sha256 => ecc::p384::PublicKey::from_sec1(key).and_then(|p| {
                let mut digest = [0; 48];
                digest[16..].copy_from_slice(&crate::hash::Sha256::digest(message)?);
                p.verify_digest(&digest, signature)
            }),
            Self::P256 => {
                ecc::p256::PublicKey::from_sec1(key).and_then(|p| p.verify(message, signature))
            }
            Self::P384 => {
                ecc::p384::PublicKey::from_sec1(key).and_then(|p| p.verify(message, signature))
            }
            Self::Ed25519 => {
                ed25519::PublicKey::from_bytes(key.try_into().map_err(|_| InvalidSignature)?)
                    .and_then(|p| p.verify(message, signature))
            }
            Self::Rsa { hash, pss } => {
                let key = rsa_key(key)?;
                let hash = *hash;
                if *pss {
                    key.verify_pss(hash, message, signature)
                } else {
                    key.verify_pkcs1_v1_5(hash, message, signature)
                }
            }
        };
        result.map_err(|_| InvalidSignature)
    }
}
pub(super) static SUPPORTED: WebPkiSupportedAlgorithms = WebPkiSupportedAlgorithms {
    all: &[
        &P256,
        &P256_SHA384,
        &P384,
        &P384_SHA256,
        &ED25519,
        &PSS256,
        &PSS384,
        &PSS512,
        &PKCS256,
        &PKCS384,
        &PKCS512,
    ],
    mapping: &[
        (
            SignatureScheme::ECDSA_NISTP256_SHA256,
            &[&P256, &P384_SHA256],
        ),
        (
            SignatureScheme::ECDSA_NISTP384_SHA384,
            &[&P384, &P256_SHA384],
        ),
        (SignatureScheme::ED25519, &[&ED25519]),
        (SignatureScheme::RSA_PSS_SHA256, &[&PSS256]),
        (SignatureScheme::RSA_PSS_SHA384, &[&PSS384]),
        (SignatureScheme::RSA_PSS_SHA512, &[&PSS512]),
        (SignatureScheme::RSA_PKCS1_SHA256, &[&PKCS256]),
        (SignatureScheme::RSA_PKCS1_SHA384, &[&PKCS384]),
        (SignatureScheme::RSA_PKCS1_SHA512, &[&PKCS512]),
    ],
};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rsa_der_refuses_noncanonical_integers_lengths_and_trailing_bytes() {
        let mut n = alloc::vec![0x80;256];
        n[255] = 1;
        let encoded = |n: &[u8], e: &[u8]| {
            let mut inner = alloc::vec![2, 0x82, (n.len() >> 8) as u8, n.len() as u8];
            inner.extend_from_slice(n);
            inner.extend_from_slice(&[2, e.len() as u8]);
            inner.extend_from_slice(e);
            let mut out = alloc::vec![0x30, 0x82, (inner.len() >> 8) as u8, inner.len() as u8];
            out.extend_from_slice(&inner);
            out
        };
        let mut positive = alloc::vec![0];
        positive.extend_from_slice(&n);
        let valid = encoded(&positive, &[1, 0, 1]);
        assert!(rsa_key(&valid).is_ok());
        assert!(rsa_key(&encoded(&n, &[1, 0, 1])).is_err());
        positive.insert(0, 0);
        assert!(rsa_key(&encoded(&positive, &[1, 0, 1])).is_err());
        assert!(rsa_key(&encoded(&positive[1..], &[0, 1, 0, 1])).is_err());
        let mut trailing = valid.clone();
        trailing.push(0);
        assert!(rsa_key(&trailing).is_err());
        let mut nonminimal = valid.clone();
        nonminimal[1] = 0x83;
        nonminimal.insert(2, 0);
        assert!(rsa_key(&nonminimal).is_err());
        for end in 0..valid.len() {
            assert!(rsa_key(&valid[..end]).is_err());
        }
        for value in [
            &[2, 0][..],
            &[2, 1, 0][..],
            &[2, 2, 0, 1][..],
            &[2, 1, 128][..],
            &[2, 0x80][..],
            &[2, 0x81, 1, 1][..],
        ] {
            assert!(integer(&mut &value[..]).is_err());
        }
    }
}
