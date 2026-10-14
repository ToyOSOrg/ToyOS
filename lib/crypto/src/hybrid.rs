//! Consumer-specific X25519/ML-KEM-768 combiners. TLS (RFC 10024) passes the
//! raw 64-byte concatenation into its key schedule; SSH (RFC 10042) hashes that
//! concatenation with SHA-256. These are not general-purpose hybrid KEMs: the
//! calling protocol must bind the exchange transcript and confirm its keys.
//! Fixed-size validated secret types enforce the all-zero X25519 check before
//! combination. Neither contents nor keys choose branches or addresses.
use crate::{hash::Sha256, mlkem, secret::Secret, x25519};
pub struct TlsSecret(Secret<64>);
pub struct SshSecret(Secret<32>);
impl TlsSecret {
    pub fn as_bytes(&self) -> &[u8; 64] {
        &self.0.0
    }
}
impl SshSecret {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0.0
    }
}
pub fn for_tls(kem: mlkem::SharedSecret, ecdh: x25519::SharedSecret) -> TlsSecret {
    let mut out = Secret([0; 64]);
    out.0[..32].copy_from_slice(kem.as_bytes());
    out.0[32..].copy_from_slice(ecdh.as_bytes());
    TlsSecret(out)
}
pub fn for_ssh(kem: mlkem::SharedSecret, ecdh: x25519::SharedSecret) -> SshSecret {
    let mut hash = Sha256::new();
    hash.update(kem.as_bytes())
        .expect("fixed-size shared secret");
    hash.update(ecdh.as_bytes())
        .expect("fixed-size shared secret");
    SshSecret(Secret(hash.finalize()))
}
