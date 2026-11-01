//! RFC 8439 Poly1305 in radix 2^64: h = h0 + h1·2^64 + h2·2^128 with h2
//! small, r = r0 + r1·2^64. Clamping clears r1's two low bits and the top four
//! bits of each 32-bit word (Bernstein, "The Poly1305-AES message-authentication
//! code", FSE 2005), so r1·2^128 ≡ s1 = r1 + r1/4 (mod 2^130 - 5) exactly, and
//! every product sum below fits in 128 bits without reduction. No key-indexed
//! table or content-dependent branch. [`tag`] and [`verify`] are Poly1305
//! alone, for formats that compose it their own way.

use crate::Error;
use crate::block::{padded, place, terminated};
use crate::secret::{Secret, erase};

/// RFC 8439 §2.5's tag of `message` under the one-time `key`.
pub fn tag(key: &[u8; 32], message: &[u8]) -> [u8; 16] {
    let _protection = crate::timing::protect();
    let mut mac = Poly1305::new(key);
    mac.update(message);
    mac.finish()
}

/// `Ok` when `expected` is [`tag`], compared in constant time.
pub fn verify(key: &[u8; 32], message: &[u8], expected: &[u8; 16]) -> Result<(), Error> {
    verify_truncated(key, message, expected)
}

/// `Ok` when `expected` is the first `N` bytes of [`tag`], compared in
/// constant time: for a format that fixes a truncated tag's length. `N` is 1
/// to 16.
pub fn verify_truncated<const N: usize>(key: &[u8; 32], message: &[u8], expected: &[u8; N]) -> Result<(), Error> {
    const { assert!(N >= 1 && N <= 16) };
    let _protection = crate::timing::protect();
    let tag = Secret(tag(key, message));
    if crate::ct::equal(&tag.0[..N], expected) { Ok(()) } else { Err(Error::Authentication) }
}

pub(crate) struct Poly1305 {
    /// r0, r1 and s1, each below 2^61.
    r: [u64; 3],
    /// h0, h1, h2 with h2 below 8 between blocks.
    h: [u64; 3],
    pad: Secret<16>,
    buffer: [u8; 16],
    filled: usize,
}

pub(crate) const fn low(x: u128) -> u64 {
    x as u64
}

const fn product(a: u64, b: u64) -> u128 {
    a as u128 * b as u128
}

/// h·r with h2 below 8, partially reduced: h2 of the result is at most 4.
#[inline(always)]
pub(crate) fn multiply([h0, h1, h2]: [u64; 3], [r0, r1, s1]: [u64; 3]) -> [u64; 3] {
    // 2^128·r1 ≡ s1 and 2^192·r1 ≡ 2^64·s1; every term is below 2^125 and
    // each sum below 2^127.
    let t0 = product(h0, r0).wrapping_add(product(h1, s1));
    let t1 = product(h0, r1)
        .wrapping_add(product(h1, r0))
        .wrapping_add(product(h2, s1))
        .wrapping_add(t0 >> 64);
    let t2 = h2.wrapping_mul(r0).wrapping_add((t1 >> 64) as u64);
    // t2·2^128 = (t2 >> 2)·2^130 + (t2 & 3)·2^128, and 2^130 ≡ 5.
    let carry = (t2 >> 2).wrapping_mul(5);
    let sum = u128::from(low(t0)).wrapping_add(u128::from(carry));
    let h0 = low(sum);
    let sum = u128::from(low(t1)).wrapping_add(sum >> 64);
    [h0, low(sum), (t2 & 3).wrapping_add((sum >> 64) as u64)]
}

/// (h + block + 2^128 where `full`)·r: h2 stays below 8 between blocks.
#[inline(always)]
pub(crate) fn absorb(h: [u64; 3], block: &[u8; 16], r: [u64; 3], full: bool) -> [u64; 3] {
    let m = u128::from_le_bytes(*block);
    let sum = u128::from(h[0]).wrapping_add(u128::from(low(m)));
    let h0 = low(sum);
    let sum = u128::from(h[1]).wrapping_add(m >> 64).wrapping_add(sum >> 64);
    let h1 = low(sum);
    let h2 = h[2].wrapping_add((sum >> 64) as u64).wrapping_add(u64::from(full));
    multiply([h0, h1, h2], r)
}

impl Poly1305 {
    pub(crate) fn new(key: &[u8; 32]) -> Self {
        let r = u128::from_le_bytes(key[..16].try_into().unwrap()) & 0x0fff_fffc_0fff_fffc_0fff_fffc_0fff_ffff;
        let (r0, r1) = (low(r), (r >> 64) as u64);
        let mut pad = Secret([0; 16]);
        pad.0.copy_from_slice(&key[16..]);
        Self { r: [r0, r1, r1 + (r1 >> 2)], h: [0; 3], pad, buffer: [0; 16], filled: 0 }
    }

    /// Absorbs whole blocks, each with 2^128 added when `full`.
    #[inline(always)]
    fn blocks(&mut self, blocks: &[[u8; 16]], full: bool) {
        let done = if full { crate::arch::poly1305_blocks(&self.r, &mut self.h, blocks) } else { 0 };
        let mut h = self.h;
        for block in &blocks[done..] {
            h = absorb(h, block, self.r, full);
        }
        self.h = h;
    }

    /// The key and the accumulator, for absorbing whole blocks elsewhere;
    /// `None` while a partial block is buffered.
    #[cfg_attr(
        not(all(target_arch = "x86_64", not(target_os = "uefi"))),
        expect(dead_code, reason = "hosted x86-64's stitched ChaCha20 alone absorbs blocks elsewhere")
    )]
    pub(crate) fn parts(&mut self) -> Option<(&[u64; 3], &mut [u64; 3])> {
        (self.filled == 0).then_some((&self.r, &mut self.h))
    }

    pub(crate) fn update(&mut self, mut data: &[u8]) {
        if self.filled != 0 {
            let (head, rest) = data.split_at(data.len().min(16 - self.filled));
            self.buffer = place(&self.buffer, self.filled, head);
            self.filled += head.len();
            data = rest;
            if self.filled == 16 {
                let block = self.buffer;
                self.blocks(&[block], true);
                self.filled = 0;
            }
        }
        let (blocks, rest) = data.as_chunks::<16>();
        self.blocks(blocks, true);
        if !rest.is_empty() {
            self.buffer = padded(rest);
            self.filled = rest.len();
        }
    }

    /// `data` and zeros to the next whole block.
    pub(crate) fn update_padded(&mut self, data: &[u8]) {
        if self.filled != 0 {
            self.update(data);
            self.pad16();
            return;
        }
        let (blocks, tail) = data.as_chunks::<16>();
        self.blocks(blocks, true);
        if !tail.is_empty() {
            self.blocks(&[padded(tail)], true);
        }
    }

    pub(crate) fn pad16(&mut self) {
        if self.filled != 0 {
            let block = terminated(&self.buffer, self.filled, 0);
            self.blocks(&[block], true);
            self.filled = 0;
        }
    }

    pub(crate) fn finish(mut self) -> [u8; 16] {
        if self.filled != 0 {
            let block = terminated(&self.buffer, self.filled, 1);
            self.blocks(&[block], false);
        }
        let [h0, h1, h2] = self.h;
        // Fold h2's bits from 2^130 up; then h < 2p, and one conditional
        // subtraction of p = 2^130 - 5 is final.
        let carry = (h2 >> 2) * 5;
        let sum = u128::from(h0) + u128::from(carry);
        let h0 = low(sum);
        let sum = u128::from(h1) + (sum >> 64);
        let h1 = low(sum);
        let h2 = (h2 & 3) + (sum >> 64) as u64;
        // g = h + 5 - 2^130; h ≥ p exactly when g does not borrow.
        let sum = u128::from(h0) + 5;
        let g0 = low(sum);
        let sum = u128::from(h1) + (sum >> 64);
        let g1 = low(sum);
        let g2 = h2.wrapping_add((sum >> 64) as u64).wrapping_sub(4);
        let take_g = 1 ^ (g2 >> 63);
        let h0 = crate::ct::select(h0, g0, take_g);
        let h1 = crate::ct::select(h1, g1, take_g);
        let h = u128::from(h0) | (u128::from(h1) << 64);
        h.wrapping_add(u128::from_le_bytes(self.pad.0)).to_le_bytes()
    }
}

impl Drop for Poly1305 {
    fn drop(&mut self) {
        erase(&mut self.r);
        erase(&mut self.h);
        erase(&mut self.buffer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use num_bigint::BigUint;

    /// RFC 8439 §2.5.1 over integers: h = (h + m)·r mod 2^130 - 5, then
    /// (h + s) mod 2^128.
    fn reference(key: &[u8; 32], message: &[u8]) -> [u8; 16] {
        let p = (BigUint::from(1u8) << 130) - 5u8;
        let clamp = BigUint::from(0x0fff_fffc_0fff_fffc_0fff_fffc_0fff_ffffu128);
        let r = BigUint::from_bytes_le(&key[..16]) & clamp;
        let mut h = BigUint::from(0u8);
        for chunk in message.chunks(16) {
            let m = BigUint::from_bytes_le(chunk) + (BigUint::from(1u8) << (8 * chunk.len()));
            h = (h + m) * &r % &p;
        }
        let tag: BigUint = (h + BigUint::from_bytes_le(&key[16..])) % (BigUint::from(1u8) << 128u32);
        let mut out = [0; 16];
        let bytes = tag.to_bytes_le();
        out[..bytes.len()].copy_from_slice(&bytes);
        out
    }

    #[test]
    fn matches_integers_on_seeded_and_extreme_inputs() {
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut message = std::vec![0u8; 600];
        for case in 0..if cfg!(miri) { 16 } else { 10_000 } {
            let mut key: [u8; 32] = core::array::from_fn(|_| next() as u8);
            let len = next() as usize % message.len();
            // Every fourth case pushes h and r to their bounds: all-ones key
            // halves and messages.
            let extreme = case % 4;
            if extreme == 1 {
                key = [0xff; 32];
            }
            for byte in &mut message[..len] {
                *byte = if extreme >= 2 { 0xff } else { next() as u8 };
            }
            let message = &message[..len];
            let mut ours = Poly1305::new(&key);
            let mut rest = message;
            while !rest.is_empty() {
                let n = (next() as usize % 70).min(rest.len());
                ours.update(&rest[..n]);
                rest = &rest[n..];
            }
            assert_eq!(ours.finish(), reference(&key, message), "case {case}, {len} bytes");
        }
    }

    #[test]
    fn rfc8439_section_2_5_2() {
        let key: [u8; 32] = [
            0x85, 0xd6, 0xbe, 0x78, 0x57, 0x55, 0x6d, 0x33, 0x7f, 0x44, 0x52, 0xfe, 0x42, 0xd5, 0x06, 0xa8, 0x01, 0x03,
            0x80, 0x8a, 0xfb, 0x0d, 0xb2, 0xfd, 0x4a, 0xbf, 0xf6, 0xaf, 0x41, 0x49, 0xf5, 0x1b,
        ];
        let message = b"Cryptographic Forum Research Group";
        let expected = [0xa8, 0x06, 0x1d, 0xc1, 0x30, 0x51, 0x36, 0xc6, 0xc2, 0x2b, 0x8b, 0xaf, 0x0c, 0x01, 0x27, 0xa9];
        assert_eq!(tag(&key, message), expected);
        assert_eq!(verify(&key, message, &expected), Ok(()));
        assert_eq!(verify_truncated::<10>(&key, message, expected.first_chunk().unwrap()), Ok(()), "a truncated tag is its prefix");
        let mut forged = expected;
        forged[9] ^= 1;
        assert_eq!(verify_truncated::<10>(&key, message, forged.first_chunk().unwrap()), Err(Error::Authentication));
        forged = expected;
        forged[15] ^= 1;
        assert_eq!(verify(&key, message, &forged), Err(Error::Authentication), "the full tag's last byte");
    }
}
