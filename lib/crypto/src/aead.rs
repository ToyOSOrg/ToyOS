//! Authenticated encryption for TLS. Sealing owns its key and a monotonic
//! nonce sequence; neither is cloneable. The invariant is per key import: the
//! caller must not import the same key into independent sealing owners.
//!
//! Nonces are `iv XOR (0^32 || sequence_be64)`. Public lengths select buffering;
//! contents and keys do not select branches or addresses. A refusal releases no
//! plaintext: AES-GCM and ChaCha20-Poly1305 may decrypt as they authenticate,
//! and either zeroes a refused buffer. The buffer holds unauthenticated
//! plaintext during `open`, so it must be the caller's alone: not memory
//! another process maps.

use crate::{Error, chacha, poly1305::Poly1305, secret::Secret};

#[allow(
    clippy::large_enum_variant,
    reason = "Keys stay inline because the core has no allocator"
)]
enum Key {
    ChaCha(Secret<32>),
    Gcm(crate::gcm::Gcm),
}

#[cfg(feature = "rustls-provider")]
impl Key {
    // Initialization crosses a non-inlined borrow after the provider allocates
    // its final owner; no expanded key is returned or moved by this function.
    #[inline(never)]
    fn initialize(&mut self, key: &[u8]) {
        match self {
            Self::ChaCha(secret) => secret.0.copy_from_slice(key),
            Self::Gcm(gcm) => gcm.initialize(key),
        }
    }
}

pub struct SealingKey {
    key: Key,
    iv: [u8; 12],
    next: u64,
    exhausted: bool,
}
pub struct OpeningKey {
    key: Key,
}

impl SealingKey {
    /// Provider-only storage; initialize in the final allocation before use.
    #[cfg(feature = "rustls-provider")]
    pub(crate) fn aes_storage(key_len: usize, iv: [u8; 12]) -> Self {
        Self {
            key: Key::Gcm(crate::gcm::Gcm::storage(key_len)),
            iv,
            next: 0,
            exhausted: false,
        }
    }
    #[cfg(feature = "rustls-provider")]
    pub(crate) fn initialize(&mut self, key: &[u8]) {
        self.key.initialize(key);
    }
    pub fn aes128_gcm(key: [u8; 16], iv: [u8; 12]) -> Self {
        let key = Secret(key);
        Self {
            key: Key::Gcm(crate::gcm::Gcm::aes128(&key.0)),
            iv,
            next: 0,
            exhausted: false,
        }
    }
    pub fn aes256_gcm(key: [u8; 32], iv: [u8; 12]) -> Self {
        let key = Secret(key);
        Self {
            key: Key::Gcm(crate::gcm::Gcm::aes256(&key.0)),
            iv,
            next: 0,
            exhausted: false,
        }
    }
    pub fn chacha20_poly1305(key: [u8; 32], iv: [u8; 12]) -> Self {
        Self {
            key: Key::ChaCha(Secret(key)),
            iv,
            next: 0,
            exhausted: false,
        }
    }
    /// Returns the nonce and detached tag. The sequence cannot be rewound.
    pub fn seal(
        &mut self,
        aad: &[u8],
        plaintext: &mut [u8],
    ) -> Result<([u8; 12], [u8; 16]), Error> {
        if self.exhausted {
            return Err(Error::CounterExhausted);
        }
        let mut nonce = self.iv;
        for (n, s) in nonce[4..].iter_mut().zip(self.next.to_be_bytes()) {
            *n ^= s;
        }
        if matches!(self.key, Key::Gcm(_)) && self.next >= u64::from(u32::MAX) {
            return Err(Error::CounterExhausted);
        }
        let tag = match &self.key {
            Key::ChaCha(key) => chacha_seal(&key.0, &nonce, aad, plaintext)?,
            Key::Gcm(key) => key.seal(&nonce, aad, plaintext)?,
        };
        if self.next == u64::MAX {
            self.exhausted = true;
        } else {
            self.next += 1;
        }
        Ok((nonce, tag))
    }
}
impl OpeningKey {
    /// Provider-only storage; initialize in the final allocation before use.
    #[cfg(feature = "rustls-provider")]
    pub(crate) fn aes_storage(key_len: usize) -> Self {
        Self {
            key: Key::Gcm(crate::gcm::Gcm::storage(key_len)),
        }
    }
    #[cfg(feature = "rustls-provider")]
    pub(crate) fn initialize(&mut self, key: &[u8]) {
        self.key.initialize(key);
    }
    pub fn aes128_gcm(key: [u8; 16]) -> Self {
        let key = Secret(key);
        Self {
            key: Key::Gcm(crate::gcm::Gcm::aes128(&key.0)),
        }
    }
    pub fn aes256_gcm(key: [u8; 32]) -> Self {
        let key = Secret(key);
        Self {
            key: Key::Gcm(crate::gcm::Gcm::aes256(&key.0)),
        }
    }
    pub fn chacha20_poly1305(key: [u8; 32]) -> Self {
        Self {
            key: Key::ChaCha(Secret(key)),
        }
    }
    pub fn open(
        &self,
        nonce: &[u8; 12],
        aad: &[u8],
        ciphertext: &mut [u8],
        tag: &[u8],
    ) -> Result<(), Error> {
        if tag.len() != 16 {
            return Err(Error::Length);
        }
        match &self.key {
            Key::ChaCha(key) => chacha_open(&key.0, nonce, aad, ciphertext, tag),
            Key::Gcm(key) => key.open(nonce, aad, ciphertext, tag),
        }
    }
}

fn check_chacha_length(length: usize) -> Result<(), Error> {
    if length as u128 > u32::MAX as u128 * 64 {
        Err(Error::Length)
    } else {
        Ok(())
    }
}
/// Poly1305 under `key` with the zero-padded AAD absorbed. RFC 8439 §2.8's
/// MAC input is whole blocks throughout, so nothing is buffered.
fn chacha_mac(key: &[u8; 32], aad: &[u8]) -> Poly1305 {
    let mut mac = Poly1305::new(key);
    mac.update_padded(aad);
    mac
}
fn chacha_finish(mut mac: Poly1305, aad: usize, ciphertext: usize) -> Secret<16> {
    let mut lengths = [0u8; 16];
    lengths[..8].copy_from_slice(&(aad as u64).to_le_bytes());
    lengths[8..].copy_from_slice(&(ciphertext as u64).to_le_bytes());
    mac.update_padded(&lengths);
    Secret(mac.finish())
}
/// Blocks 0 to 3 for 1 to 192 bytes of data, computed together: the
/// Poly1305 key, then the data's keystream. It runs ahead of the one-pass
/// path, which costs more for so few blocks on the AMD bench. An empty
/// message takes block 0 alone, whose latency is lower.
fn short(length: usize) -> bool {
    (1..=192).contains(&length)
}
fn chacha_short(key: &[u8; 32], nonce: &[u8; 12], length: usize, stream: &mut [u8; 256]) {
    chacha::ietf_blocks(key, nonce, 0, 1 + length.div_ceil(64), stream);
}
fn chacha_seal(
    key: &[u8; 32],
    nonce: &[u8; 12],
    aad: &[u8],
    data: &mut [u8],
) -> Result<[u8; 16], Error> {
    check_chacha_length(data.len())?;
    let mac = if short(data.len()) {
        let mut stream = Secret([0u8; 256]);
        chacha_short(key, nonce, data.len(), &mut stream.0);
        crate::block::xor(data, &stream.0[64..]);
        let mut mac = chacha_mac(stream.0[..32].try_into().unwrap(), aad);
        mac.update_padded(data);
        mac
    } else if let Some(mac) = chacha::aead(key, nonce, aad, data, true) {
        mac
    } else {
        let mut mac = chacha_mac(chacha::block(key, nonce, 0).0[..32].try_into().unwrap(), aad);
        chacha::xor(key, nonce, 1, data)?;
        mac.update_padded(data);
        mac
    };
    Ok(chacha_finish(mac, aad.len(), data.len()).0)
}
fn chacha_open(
    key: &[u8; 32],
    nonce: &[u8; 12],
    aad: &[u8],
    data: &mut [u8],
    tag: &[u8],
) -> Result<(), Error> {
    check_chacha_length(data.len())?;
    let authentic = |mac: Poly1305, length: usize| crate::ct::equal(&chacha_finish(mac, aad.len(), length).0, tag);
    // Without the one-pass path, the MAC comes first and only an authentic
    // record is decrypted.
    let opened = if short(data.len()) {
        let mut stream = Secret([0u8; 256]);
        chacha_short(key, nonce, data.len(), &mut stream.0);
        let mut mac = chacha_mac(stream.0[..32].try_into().unwrap(), aad);
        mac.update_padded(data);
        let opened = authentic(mac, data.len());
        if opened {
            crate::block::xor(data, &stream.0[64..]);
        }
        opened
    } else if let Some(mac) = chacha::aead(key, nonce, aad, data, false) {
        authentic(mac, data.len())
    } else {
        let mut mac = chacha_mac(chacha::block(key, nonce, 0).0[..32].try_into().unwrap(), aad);
        mac.update_padded(data);
        let opened = authentic(mac, data.len());
        if opened {
            chacha::xor(key, nonce, 1, data)?;
        }
        opened
    };
    if !opened {
        crate::secret::wipe(data);
        return Err(Error::Authentication);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A forged record's open returns zeros in place of its plaintext, at
    /// every length the paths divide at: past 192 bytes an AVX2 host decrypts
    /// as it authenticates, so the zeros are all that stands between the
    /// caller and the keystream.
    #[test]
    fn a_forged_chacha_record_opens_to_zeros() {
        let key = [0x17; 32];
        for len in [1, 63, 64, 65, 192, 193, 447, 448, 449, 575, 576, 577, 1024, 1088, 4096, 16385] {
            let plain: std::vec::Vec<u8> = (0..len).map(|i| (i * 13 + 1) as u8).collect();
            let mut sealed = plain.clone();
            let (nonce, tag) = SealingKey::chacha20_poly1305(key, [9; 12]).seal(b"aad", &mut sealed).unwrap();
            let opener = OpeningKey::chacha20_poly1305(key);
            let mut forgeries: [_; 3] = core::array::from_fn(|_| (sealed.clone(), tag, &b"aad"[..]));
            forgeries[0].0[len / 2] ^= 0x80;
            forgeries[1].1[15] ^= 1;
            forgeries[2].2 = b"aae";
            for (i, (mut record, tag, aad)) in forgeries.into_iter().enumerate() {
                assert_eq!(opener.open(&nonce, aad, &mut record, &tag), Err(Error::Authentication), "{len} bytes, forgery {i}");
                assert!(record.iter().all(|&b| b == 0), "{len} bytes, forgery {i}: the refusal left more than zeros");
            }
            opener.open(&nonce, b"aad", &mut sealed, &tag).unwrap();
            assert_eq!(sealed, plain, "{len} bytes");
        }
    }
    #[test]
    fn nonce_sequence_exhaustion_is_terminal() {
        let mut key = SealingKey::chacha20_poly1305([0; 32], [0; 12]);
        let (n, _) = key.seal(&[], &mut []).unwrap();
        assert_eq!(n, [0; 12]);
        let (n, _) = key.seal(&[], &mut []).unwrap();
        assert_eq!(n[11], 1);
        key.next = u64::MAX;
        key.seal(&[], &mut []).unwrap();
        assert_eq!(key.seal(&[], &mut []), Err(Error::CounterExhausted));
        let mut key = SealingKey::aes128_gcm([0; 16], [0; 12]);
        key.next = u64::from(u32::MAX);
        assert_eq!(key.seal(&[], &mut []), Err(Error::CounterExhausted));
    }
}
