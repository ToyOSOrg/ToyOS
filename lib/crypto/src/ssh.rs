//! OpenSSH chacha20-poly1305@openssh.com, distinct from RFC 8439 AEAD.
//! The first key encrypts the payload and derives Poly1305; the second encrypts
//! the four-byte packet length. The MAC covers encrypted length and payload
//! without padding or length trailers. The nonce is the big-endian SSH packet
//! sequence in eight bytes. Secret operations have fixed schedules; lengths
//! and sequence exhaustion are public. Re-key before the sequence wraps.

use crate::{Error, chacha, ct, poly1305::Poly1305, secret::Secret};
struct Keys {
    payload: Secret<32>,
    header: Secret<32>,
}
impl Keys {
    fn new(bytes: [u8; 64]) -> Self {
        let bytes = Secret(bytes);
        Self {
            payload: Secret(bytes.0[..32].try_into().unwrap()),
            header: Secret(bytes.0[32..].try_into().unwrap()),
        }
    }
    fn tag(&self, seq: u32, header: &[u8; 4], payload: &[u8]) -> Secret<16> {
        let nonce = u64::from(seq).to_be_bytes();
        let block = chacha::legacy_block(&self.payload.0, &nonce, 0);
        let mut mac = Poly1305::new(block.0[..32].try_into().unwrap());
        mac.update(header);
        mac.update(payload);
        Secret(mac.finish())
    }
    fn crypt(&self, seq: u32, header: &mut [u8; 4], payload: &mut [u8]) -> Result<(), Error> {
        let nonce = u64::from(seq).to_be_bytes();
        chacha::legacy_xor(&self.header.0, &nonce, 0, header)?;
        chacha::legacy_xor(&self.payload.0, &nonce, 1, payload)
    }
}
pub struct SealingKey {
    keys: Keys,
    next: Option<u32>,
}
pub struct OpeningKey {
    keys: Keys,
}
impl SealingKey {
    /// `next_sequence` comes from SSH, whose packet sequence survives re-keying.
    pub fn new(key: [u8; 64], next_sequence: u32) -> Self {
        Self {
            keys: Keys::new(key),
            next: Some(next_sequence),
        }
    }
    pub fn seal(
        &mut self,
        header: &mut [u8; 4],
        payload: &mut [u8],
    ) -> Result<(u32, [u8; 16]), Error> {
        if payload.len() > u32::MAX as usize {
            return Err(Error::Length);
        }
        let seq = self.next.ok_or(Error::CounterExhausted)?;
        self.keys.crypt(seq, header, payload)?;
        let tag = self.keys.tag(seq, header, payload);
        self.next = seq.checked_add(1);
        Ok((seq, tag.0))
    }
}
impl OpeningKey {
    pub fn new(key: [u8; 64]) -> Self {
        Self {
            keys: Keys::new(key),
        }
    }
    /// Unauthenticated length is only a framing hint. The caller must bound
    /// allocations and authenticate the complete packet before using plaintext.
    pub fn packet_length(&self, seq: u32, encrypted: [u8; 4]) -> u32 {
        let mut header = encrypted;
        chacha::legacy_xor(
            &self.keys.header.0,
            &u64::from(seq).to_be_bytes(),
            0,
            &mut header,
        )
        .unwrap();
        u32::from_be_bytes(header)
    }
    pub fn open(
        &self,
        seq: u32,
        header: &mut [u8; 4],
        payload: &mut [u8],
        tag: &[u8],
    ) -> Result<(), Error> {
        if tag.len() != 16 || payload.len() > u32::MAX as usize {
            return Err(Error::Length);
        }
        let expected = self.keys.tag(seq, header, payload);
        if !ct::equal(&expected.0, tag) {
            return Err(Error::Authentication);
        }
        self.keys.crypt(seq, header, payload)
    }
}
