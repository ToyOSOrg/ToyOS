//! OpenSSH chacha20-poly1305@openssh.com, distinct from RFC 8439 AEAD.
//! The first key encrypts the payload and derives Poly1305; the second encrypts
//! the four-byte packet length. The MAC covers encrypted length and payload
//! without padding or length trailers. The nonce is the big-endian SSH packet
//! sequence in eight bytes. Secret operations have fixed schedules; lengths
//! and sequence exhaustion are public. Re-key before the sequence wraps.

use crate::{Error, chacha, ct, poly1305::Poly1305, secret::Secret};
/// The payload key, then the header key.
struct Keys(Secret<64>);
impl Keys {
    fn new(bytes: [u8; 64]) -> Self {
        Self(Secret(bytes))
    }
    fn payload(&self) -> &[u8; 32] {
        self.0.0[..32].try_into().unwrap()
    }
    fn header(&self) -> &[u8; 32] {
        self.0.0[32..].try_into().unwrap()
    }
    fn tag(&self, seq: u32, header: &[u8; 4], payload: &[u8]) -> Secret<16> {
        let block = chacha::legacy_block(self.payload(), &u64::from(seq).to_be_bytes(), 0);
        mac(block.0[..32].try_into().unwrap(), header, payload)
    }
    /// For a payload of at most 128 bytes: the header key's block 0, then
    /// the payload key's blocks 0 to 2, computed together.
    fn short(&self, seq: u32, length: usize, stream: &mut [u8; 256]) -> bool {
        if length > 128 {
            return false;
        }
        let nonce = u64::from(seq).to_be_bytes();
        let mut states = Secret([[0u32; 16]; 4]);
        for (i, state) in states.0.iter_mut().enumerate() {
            let (key, counter) = if i == 0 { (self.header(), 0) } else { (self.payload(), i as u64 - 1) };
            chacha::fill_legacy_state(state, key, &nonce, counter);
        }
        chacha::blocks(&states.0, 2 + length.div_ceil(64), stream);
        true
    }
    fn crypt(&self, seq: u32, header: &mut [u8; 4], payload: &mut [u8]) -> Result<(), Error> {
        let nonce = u64::from(seq).to_be_bytes();
        chacha::legacy_xor(self.header(), &nonce, 0, header)?;
        chacha::legacy_xor(self.payload(), &nonce, 1, payload)
    }
}
fn mac(key: &[u8; 32], header: &[u8; 4], payload: &[u8]) -> Secret<16> {
    let mut mac = Poly1305::new(key);
    mac.update(header);
    mac.update(payload);
    Secret(mac.finish())
}
/// Header and payload XORed with `short`'s keystream.
fn crypt_short(stream: &[u8; 256], header: &mut [u8; 4], payload: &mut [u8]) {
    for (i, byte) in header.iter_mut().enumerate() {
        *byte ^= stream[i];
    }
    crate::block::xor(payload, &stream[128..]);
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
        let mut stream = Secret([0u8; 256]);
        let tag = match self.keys.short(seq, payload.len(), &mut stream.0) {
            true => {
                crypt_short(&stream.0, header, payload);
                mac(stream.0[64..96].try_into().unwrap(), header, payload)
            }
            false => {
                self.keys.crypt(seq, header, payload)?;
                self.keys.tag(seq, header, payload)
            }
        };
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
            self.keys.header(),
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
        let mut stream = Secret([0u8; 256]);
        let short = self.keys.short(seq, payload.len(), &mut stream.0);
        let expected = match short {
            true => mac(stream.0[64..96].try_into().unwrap(), header, payload),
            false => self.keys.tag(seq, header, payload),
        };
        if !ct::equal(&expected.0, tag) {
            return Err(Error::Authentication);
        }
        if short {
            crypt_short(&stream.0, header, payload);
            Ok(())
        } else {
            self.keys.crypt(seq, header, payload)
        }
    }
}
