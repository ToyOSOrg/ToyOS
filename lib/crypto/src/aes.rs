//! FIPS 197 AES-128/256, for GCM and WPA2 unwrap. The portable S-box evaluates
//! inversion in GF(2^8), with fixed operations and no secret-indexed table.
//! Round counts and dispatch depend only on public key size and capabilities.

use crate::secret::{Secret, wipe};

#[inline(always)]
fn xtime(a: u8) -> u8 {
    (a << 1) ^ (0x1b & crate::ct::mask(u64::from(a >> 7)) as u8)
}
fn mul(mut a: u8, mut b: u8) -> u8 {
    let mut out = 0;
    for _ in 0..8 {
        out ^= a & crate::ct::mask(u64::from(b & 1)) as u8;
        a = xtime(a);
        b >>= 1;
    }
    out
}
fn inverse(x: u8) -> u8 {
    let x2 = mul(x, x);
    let x4 = mul(x2, x2);
    let x8 = mul(x4, x4);
    let x16 = mul(x8, x8);
    let x32 = mul(x16, x16);
    let x64 = mul(x32, x32);
    let x128 = mul(x64, x64);
    mul(mul(mul(mul(mul(mul(x2, x4), x8), x16), x32), x64), x128)
}
fn sbox(x: u8) -> u8 {
    let y = inverse(x);
    y ^ y.rotate_left(1) ^ y.rotate_left(2) ^ y.rotate_left(3) ^ y.rotate_left(4) ^ 0x63
}
fn subword(word: &mut [u8; 4], hardware: bool) {
    if !hardware || !crate::arch::aes_subword(word) {
        for byte in word {
            *byte = sbox(*byte);
        }
    }
}
#[cfg(feature = "wpa2")]
fn inv_sbox(x: u8) -> u8 {
    inverse(x.rotate_left(1) ^ x.rotate_left(3) ^ x.rotate_left(6) ^ 0x05)
}

pub(crate) struct Aes {
    keys: Secret<240>,
    rounds: usize,
    hardware: bool,
}
impl Aes {
    pub(crate) fn aes128(key: &[u8; 16]) -> Self {
        Self::expand(key, 10)
    }
    pub(crate) fn aes256(key: &[u8; 32]) -> Self {
        Self::expand(key, 14)
    }
    fn expand(key: &[u8], rounds: usize) -> Self {
        let hardware = crate::arch::aes_available();
        let mut keys = Secret([0u8; 240]);
        Self::expand_into(&mut keys.0, key, rounds, hardware);
        Self {
            keys,
            rounds,
            hardware,
        }
    }
    #[cfg(feature = "rustls-provider")]
    pub(crate) fn storage(key_len: usize) -> Self {
        Self {
            keys: Secret([0; 240]),
            rounds: match key_len {
                16 => 10,
                32 => 14,
                _ => unreachable!("AES key length"),
            },
            hardware: crate::arch::aes_available(),
        }
    }
    #[cfg(feature = "rustls-provider")]
    pub(crate) fn initialize(&mut self, key: &[u8]) {
        assert_eq!(key.len(), if self.rounds == 10 { 16 } else { 32 });
        Self::expand_into(&mut self.keys.0, key, self.rounds, self.hardware);
    }
    fn expand_into(keys: &mut [u8; 240], key: &[u8], rounds: usize, hardware: bool) {
        keys[..key.len()].copy_from_slice(key);
        let mut rcon = 1;
        for at in (key.len()..16 * (rounds + 1)).step_by(4) {
            let mut t: [u8; 4] = keys[at - 4..at].try_into().unwrap();
            if at % key.len() == 0 {
                t.rotate_left(1);
                subword(&mut t, hardware);
                t[0] ^= rcon;
                rcon = xtime(rcon);
            } else if key.len() == 32 && at % 32 == 16 {
                subword(&mut t, hardware);
            }
            for (i, &byte) in t.iter().enumerate() {
                keys[at + i] = keys[at - key.len() + i] ^ byte;
            }
            wipe(&mut t);
        }
    }
    pub(crate) fn encrypt(&self, block: &mut [u8; 16]) {
        if self.hardware {
            crate::arch::aes_encrypt(&self.keys.0, self.rounds, block);
            return;
        }
        self.encrypt_portable(block);
    }
    pub(crate) fn xor_ctr(&self, nonce: &[u8; 12], data: &mut [u8]) -> bool {
        self.hardware && crate::arch::aes_ctr(&self.keys.0, self.rounds, nonce, data)
    }
    fn encrypt_portable(&self, block: &mut [u8; 16]) {
        for (b, k) in block.iter_mut().zip(&self.keys.0[..16]) {
            *b ^= k;
        }
        for round in 1..=self.rounds {
            for byte in block.iter_mut() {
                *byte = sbox(*byte);
            }
            shift_rows(block, false);
            if round != self.rounds {
                mix_columns(block);
            }
            for (b, k) in block
                .iter_mut()
                .zip(&self.keys.0[round * 16..round * 16 + 16])
            {
                *b ^= k;
            }
        }
    }
    #[cfg(feature = "wpa2")]
    pub(crate) fn decrypt(&self, block: &mut [u8; 16]) {
        for (b, k) in block
            .iter_mut()
            .zip(&self.keys.0[self.rounds * 16..self.rounds * 16 + 16])
        {
            *b ^= k;
        }
        for round in (0..self.rounds).rev() {
            shift_rows(block, true);
            for byte in block.iter_mut() {
                *byte = inv_sbox(*byte);
            }
            for (b, k) in block
                .iter_mut()
                .zip(&self.keys.0[round * 16..round * 16 + 16])
            {
                *b ^= k;
            }
            if round != 0 {
                for c in block.as_chunks_mut::<4>().0 {
                    let u = xtime(xtime(c[0] ^ c[2]));
                    let v = xtime(xtime(c[1] ^ c[3]));
                    c[0] ^= u;
                    c[2] ^= u;
                    c[1] ^= v;
                    c[3] ^= v;
                }
                mix_columns(block);
            }
        }
    }
}
fn shift_rows(block: &mut [u8; 16], inverse: bool) {
    let mut old = *block;
    for row in 0..4 {
        for column in 0..4 {
            let from = if inverse {
                (column + 4 - row) % 4
            } else {
                (column + row) % 4
            };
            block[4 * column + row] = old[4 * from + row];
        }
    }
    wipe(&mut old);
}
fn mix_columns(block: &mut [u8; 16]) {
    for c in block.as_chunks_mut::<4>().0 {
        let [a, b, d, e] = *c;
        let sum = a ^ b ^ d ^ e;
        c[0] = a ^ sum ^ xtime(a ^ b);
        c[1] = b ^ sum ^ xtime(b ^ d);
        c[2] = d ^ sum ^ xtime(d ^ e);
        c[3] = e ^ sum ^ xtime(e ^ a);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fips197_portable_matches_instruction_path() {
        let key = core::array::from_fn(|i| i as u8);
        let aes = Aes::aes128(&key);
        let plain = [
            0, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
            0xff,
        ];
        let expected = [
            0x69, 0xc4, 0xe0, 0xd8, 0x6a, 0x7b, 0x04, 0x30, 0xd8, 0xcd, 0xb7, 0x80, 0x70, 0xb4,
            0xc5, 0x5a,
        ];
        let mut portable = plain;
        aes.encrypt_portable(&mut portable);
        assert_eq!(portable, expected);
        let mut accelerated = plain;
        aes.encrypt(&mut accelerated);
        assert_eq!(accelerated, expected);
        if aes.hardware {
            for x in 0..=255u8 {
                let word = [x, !x, x ^ 0x55, x.rotate_left(3)];
                let mut substituted = word;
                if crate::arch::aes_subword(&mut substituted) {
                    assert_eq!(substituted, word.map(sbox));
                }
            }
        }
        #[cfg(feature = "wpa2")]
        {
            aes.decrypt(&mut accelerated);
            assert_eq!(accelerated, plain);
        }
    }
}
