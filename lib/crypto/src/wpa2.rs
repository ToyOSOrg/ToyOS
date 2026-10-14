//! WPA2-Personal's compatibility primitives, available only with `wpa2`.
//! SHA-1 is intentionally not exposed as a general-purpose hash. PBKDF2 uses
//! exactly 4096 iterations. Contents do not choose branches or addresses;
//! password, SSID and output lengths are public. Failed unwrap wipes its output.

use crate::{
    Error,
    aes::Aes,
    ct,
    secret::{Secret, wipe, wipe32},
};
struct Sha1 {
    state: [u32; 5],
    buffer: [u8; 64],
    used: usize,
    bytes: u64,
}
impl Drop for Sha1 {
    fn drop(&mut self) {
        wipe32(&mut self.state);
        wipe(&mut self.buffer)
    }
}
impl Sha1 {
    fn new() -> Self {
        Self {
            state: [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476, 0xc3d2e1f0],
            buffer: [0; 64],
            used: 0,
            bytes: 0,
        }
    }
    fn compress(&mut self, b: &[u8; 64]) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(b[4 * i..4 * i + 4].try_into().unwrap())
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1)
        }
        let mut v = self.state;
        for (i, &word) in w.iter().enumerate() {
            let (f, k) = match i {
                0..20 => ((v[1] & v[2]) | (!v[1] & v[3]), 0x5a827999),
                20..40 => (v[1] ^ v[2] ^ v[3], 0x6ed9eba1),
                40..60 => ((v[1] & v[2]) | (v[1] & v[3]) | (v[2] & v[3]), 0x8f1bbcdc),
                _ => (v[1] ^ v[2] ^ v[3], 0xca62c1d6),
            };
            let t = v[0]
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(v[4])
                .wrapping_add(k)
                .wrapping_add(word);
            v = [t, v[0], v[1].rotate_left(30), v[2], v[3]]
        }
        for (s, v) in self.state.iter_mut().zip(v) {
            *s = s.wrapping_add(v)
        }
        wipe32(&mut w);
        wipe32(&mut v)
    }
    fn update(&mut self, data: &[u8]) -> Result<(), Error> {
        let bytes = self
            .bytes
            .checked_add(data.len() as u64)
            .ok_or(Error::Length)?;
        if bytes > u64::MAX / 8 {
            return Err(Error::Length);
        }
        self.bytes = bytes;
        self.absorb(data);
        Ok(())
    }
    fn absorb(&mut self, mut data: &[u8]) {
        while !data.is_empty() {
            let n = (64 - self.used).min(data.len());
            self.buffer[self.used..self.used + n].copy_from_slice(&data[..n]);
            self.used += n;
            data = &data[n..];
            if self.used == 64 {
                let b = Secret(self.buffer);
                self.compress(&b.0);
                self.used = 0
            }
        }
    }
    fn finish(mut self) -> Secret<20> {
        let length = self.bytes * 8;
        self.absorb(&[0x80]);
        while self.used != 56 {
            self.absorb(&[0])
        }
        self.absorb(&length.to_be_bytes());
        let mut out = Secret([0; 20]);
        for i in 0..5 {
            out.0[4 * i..4 * i + 4].copy_from_slice(&self.state[i].to_be_bytes())
        }
        out
    }
}
fn mac(key: &[u8], parts: &[&[u8]]) -> Result<Secret<20>, Error> {
    let mut k = Secret([0; 64]);
    if key.len() > 64 {
        let mut h = Sha1::new();
        h.update(key)?;
        k.0[..20].copy_from_slice(&h.finish().0)
    } else {
        k.0[..key.len()].copy_from_slice(key)
    }
    for b in &mut k.0 {
        *b ^= 0x36
    }
    let mut h = Sha1::new();
    h.update(&k.0)?;
    for part in parts {
        h.update(part)?
    }
    let inner = h.finish();
    for b in &mut k.0 {
        *b ^= 0x36 ^ 0x5c
    }
    let mut h = Sha1::new();
    h.update(&k.0)?;
    h.update(&inner.0)?;
    Ok(h.finish())
}
pub struct PairwiseMasterKey(Secret<32>);
impl PairwiseMasterKey {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0.0
    }
}
/// IEEE 802.11 passphrase form; a raw hexadecimal PSK belongs to the caller.
pub fn derive_pmk(passphrase: &[u8], ssid: &[u8]) -> Result<PairwiseMasterKey, Error> {
    if !(8..=63).contains(&passphrase.len()) || ssid.is_empty() || ssid.len() > 32 {
        return Err(Error::Length);
    }
    let mut output = Secret([0; 32]);
    for (i, chunk) in output.0.chunks_mut(20).enumerate() {
        let counter = ((i + 1) as u32).to_be_bytes();
        let mut u = mac(passphrase, &[ssid, &counter])?;
        let mut t = Secret(u.0);
        for _ in 1..4096 {
            u = mac(passphrase, &[&u.0])?;
            for j in 0..20 {
                t.0[j] ^= u.0[j]
            }
        }
        chunk.copy_from_slice(&t.0[..chunk.len()])
    }
    Ok(PairwiseMasterKey(output))
}
pub fn hmac_sha1(key: &[u8], parts: &[&[u8]]) -> Result<[u8; 20], Error> {
    Ok(mac(key, parts)?.0)
}
/// 802.11i PRF: HMAC-SHA1(key, label || 0 || data || counter), counter from 0.
pub fn prf(key: &[u8], label: &[u8], data: &[u8], output: &mut [u8]) -> Result<(), Error> {
    if output.len() > 256 * 20 {
        return Err(Error::Length);
    }
    if key.len() as u128 > u128::from(u64::MAX / 8)
        || 66 + label.len() as u128 + data.len() as u128 > u128::from(u64::MAX / 8)
    {
        return Err(Error::Length);
    }
    for (i, chunk) in output.chunks_mut(20).enumerate() {
        let t = mac(key, &[label, &[0], data, &[i as u8]])?;
        chunk.copy_from_slice(&t.0[..chunk.len()])
    }
    Ok(())
}
pub struct KeyEncryptionKey(Aes);
impl KeyEncryptionKey {
    pub fn new(key: [u8; 16]) -> Self {
        let key = Secret(key);
        Self(Aes::aes128(&key.0))
    }
    /// RFC 3394 default-IV unwrap. Output has exactly ciphertext.len()-8 bytes.
    pub fn unwrap(&self, ciphertext: &[u8], output: &mut [u8]) -> Result<(), Error> {
        if ciphertext.len() < 24
            || !ciphertext.len().is_multiple_of(8)
            || output.len() != ciphertext.len() - 8
        {
            return Err(Error::Length);
        }
        let n = output.len() / 8;
        output.copy_from_slice(&ciphertext[8..]);
        let mut a = Secret([0; 8]);
        a.0.copy_from_slice(&ciphertext[..8]);
        for j in (0..6).rev() {
            for i in (1..=n).rev() {
                let mut b = Secret([0; 16]);
                let t = (n as u64) * (j as u64) + (i as u64);
                b.0[..8].copy_from_slice(&(u64::from_be_bytes(a.0) ^ t).to_be_bytes());
                b.0[8..].copy_from_slice(&output[8 * (i - 1)..8 * i]);
                self.0.decrypt(&mut b.0);
                a.0.copy_from_slice(&b.0[..8]);
                output[8 * (i - 1)..8 * i].copy_from_slice(&b.0[8..]);
            }
        }
        if !ct::equal(&a.0, &[0xa6; 8]) {
            wipe(output);
            return Err(Error::Authentication);
        }
        Ok(())
    }
}
