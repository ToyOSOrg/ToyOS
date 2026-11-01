//! OpenSSH chacha20-poly1305@openssh.com, distinct from RFC 8439 AEAD.
//! The first key encrypts the payload and derives Poly1305; the second encrypts
//! the four-byte packet length. The MAC covers encrypted length and payload
//! without padding or length trailers. The nonce is the big-endian SSH packet
//! sequence in eight bytes. Secret operations have fixed schedules; lengths
//! and sequence exhaustion are public. Re-key before the sequence wraps.
//! Opening may decrypt as it authenticates, and a refusal zeroes the payload.

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
        let payload = self.payload();
        chacha::blocks([self.header(), payload, payload, payload], [0, 0, 1, 2], &nonce, 2 + length.div_ceil(64), stream);
        true
    }
    fn crypt_header(&self, seq: u32, header: &mut [u8; 4]) {
        // Four bytes from block 0 cannot exhaust the counter.
        chacha::legacy_xor(self.header(), &u64::from(seq).to_be_bytes(), 0, header).unwrap();
    }
    fn crypt_payload(&self, seq: u32, payload: &mut [u8]) -> Result<(), Error> {
        chacha::legacy_xor(self.payload(), &u64::from(seq).to_be_bytes(), 1, payload)
    }
    /// The payload XORed and the tag over `header` and the payload's
    /// ciphertext, in one pass where an instruction path runs.
    fn one_pass(&self, seq: u32, header: &[u8; 4], payload: &mut [u8], seal: bool) -> Option<Secret<16>> {
        let mac = chacha::openssh(self.payload(), &u64::from(seq).to_be_bytes(), header, payload, seal)?;
        Some(Secret(mac.finish()))
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
        let _protection = crate::timing::protect();
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
                self.keys.crypt_header(seq, header);
                match self.keys.one_pass(seq, header, payload, true) {
                    Some(tag) => tag,
                    None => {
                        self.keys.crypt_payload(seq, payload)?;
                        self.keys.tag(seq, header, payload)
                    }
                }
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
        let _protection = crate::timing::protect();
        let mut header = encrypted;
        self.keys.crypt_header(seq, &mut header);
        u32::from_be_bytes(header)
    }
    pub fn open(
        &self,
        seq: u32,
        header: &mut [u8; 4],
        payload: &mut [u8],
        tag: &[u8],
    ) -> Result<(), Error> {
        let _protection = crate::timing::protect();
        if tag.len() != 16 || payload.len() > u32::MAX as usize {
            return Err(Error::Length);
        }
        let mut stream = Secret([0u8; 256]);
        // Without the one-pass path, the MAC comes first and only an
        // authentic packet is decrypted.
        let opened = if self.keys.short(seq, payload.len(), &mut stream.0) {
            let opened = ct::equal(&mac(stream.0[64..96].try_into().unwrap(), header, payload).0, tag);
            if opened {
                crypt_short(&stream.0, header, payload);
            }
            opened
        } else if let Some(expected) = self.keys.one_pass(seq, header, payload, false) {
            let opened = ct::equal(&expected.0, tag);
            if opened {
                self.keys.crypt_header(seq, header);
            }
            opened
        } else {
            let opened = ct::equal(&self.keys.tag(seq, header, payload).0, tag);
            if opened {
                self.keys.crypt_header(seq, header);
                self.keys.crypt_payload(seq, payload)?;
            }
            opened
        };
        if !opened {
            crate::secret::wipe(payload);
            return Err(Error::Authentication);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    /// Packets against the construction composed from portable blocks and
    /// `poly1305::tag`, at every length the paths divide at; a forged one
    /// opens to zeros. On an AVX2 host without AVX-512 the packets past 128
    /// bytes take the one-pass kernel.
    #[test]
    fn packets_match_the_construction_composed_from_blocks() {
        let key: [u8; 64] = core::array::from_fn(|i| (i * 29 + 5) as u8);
        let seq = 0x0102_0304;
        let nonce = u64::from(seq).to_be_bytes();
        let stream = |key: &[u8], counter: u64, data: &mut [u8]| {
            for (i, chunk) in data.chunks_mut(64).enumerate() {
                let block = chacha::legacy_block(key.try_into().unwrap(), &nonce, counter + i as u64);
                for (byte, mask) in chunk.iter_mut().zip(block.0) {
                    *byte ^= mask;
                }
            }
        };
        for len in [0, 1, 11, 12, 13, 128, 129, 447, 448, 449, 575, 576, 577, 588, 589, 1100, 4096, 4097] {
            let plain: Vec<u8> = (0..len).map(|i| (i * 7 + 3) as u8).collect();
            let mut want_header = (len as u32).to_be_bytes();
            stream(&key[32..], 0, &mut want_header);
            let mut want = plain.clone();
            stream(&key[..32], 1, &mut want);
            let poly_key = chacha::legacy_block(key[..32].try_into().unwrap(), &nonce, 0);
            let want_tag = crate::poly1305::tag(poly_key.0[..32].try_into().unwrap(), &[&want_header[..], &want].concat());
            let (mut header, mut sealed) = ((len as u32).to_be_bytes(), plain.clone());
            let (_, tag) = SealingKey::new(key, seq).seal(&mut header, &mut sealed).unwrap();
            assert_eq!((header, &sealed, tag), (want_header, &want, want_tag), "{len} bytes");
            let opener = OpeningKey::new(key);
            let (mut forged_header, mut forged) = (header, sealed.clone());
            let mut bad = tag;
            bad[len % 16] ^= 1;
            assert_eq!(opener.open(seq, &mut forged_header, &mut forged, &bad), Err(Error::Authentication), "{len} bytes");
            assert_eq!((forged_header, forged), (header, std::vec![0; len]), "{len} bytes: the refusal left more than zeros");
            opener.open(seq, &mut header, &mut sealed, &tag).unwrap();
            assert_eq!(sealed, plain, "{len} bytes");
        }
    }
}
