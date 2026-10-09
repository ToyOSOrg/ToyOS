//! SHA-256 and SHA-512, FIPS 180-4 §6.2 and §6.4, over whole bytes: what the
//! loader holds a signed image's sections to, what `update` and `swap` hold
//! what they install to, and what the build names its inputs by.
//!
//! [`Sha256`] and [`Sha512`] take a message in any number of [`update`]s and
//! give its digest at [`finalize`]; `digest` is both at once. The digest of a
//! message is the same however it was split. A message is whole bytes: FIPS
//! 180-4 also hashes a trailing partial byte, and nothing here needs one.
//!
//! **Scalar, on every target.** The loader runs as a UEFI application on a
//! soft-float target and may not assume the SIMD or SHA-extension registers
//! are its own, and an instruction path is `unsafe`, which this crate
//! forbids.
//!
//! [`update`]: Sha256::update
//! [`finalize`]: Sha256::finalize

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

#[cfg(test)]
mod tests;

/// SHA-256's round constants, §4.2.2.
#[rustfmt::skip]
const K256: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// SHA-256's initial hash value, §5.3.3.
#[rustfmt::skip]
const H256: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/// SHA-512's round constants, §4.2.3.
#[rustfmt::skip]
const K512: [u64; 80] = [
    0x428a2f98d728ae22, 0x7137449123ef65cd, 0xb5c0fbcfec4d3b2f, 0xe9b5dba58189dbbc,
    0x3956c25bf348b538, 0x59f111f1b605d019, 0x923f82a4af194f9b, 0xab1c5ed5da6d8118,
    0xd807aa98a3030242, 0x12835b0145706fbe, 0x243185be4ee4b28c, 0x550c7dc3d5ffb4e2,
    0x72be5d74f27b896f, 0x80deb1fe3b1696b1, 0x9bdc06a725c71235, 0xc19bf174cf692694,
    0xe49b69c19ef14ad2, 0xefbe4786384f25e3, 0x0fc19dc68b8cd5b5, 0x240ca1cc77ac9c65,
    0x2de92c6f592b0275, 0x4a7484aa6ea6e483, 0x5cb0a9dcbd41fbd4, 0x76f988da831153b5,
    0x983e5152ee66dfab, 0xa831c66d2db43210, 0xb00327c898fb213f, 0xbf597fc7beef0ee4,
    0xc6e00bf33da88fc2, 0xd5a79147930aa725, 0x06ca6351e003826f, 0x142929670a0e6e70,
    0x27b70a8546d22ffc, 0x2e1b21385c26c926, 0x4d2c6dfc5ac42aed, 0x53380d139d95b3df,
    0x650a73548baf63de, 0x766a0abb3c77b2a8, 0x81c2c92e47edaee6, 0x92722c851482353b,
    0xa2bfe8a14cf10364, 0xa81a664bbc423001, 0xc24b8b70d0f89791, 0xc76c51a30654be30,
    0xd192e819d6ef5218, 0xd69906245565a910, 0xf40e35855771202a, 0x106aa07032bbd1b8,
    0x19a4c116b8d2d0c8, 0x1e376c085141ab53, 0x2748774cdf8eeb99, 0x34b0bcb5e19b48a8,
    0x391c0cb3c5c95a63, 0x4ed8aa4ae3418acb, 0x5b9cca4f7763e373, 0x682e6ff3d6b2b8a3,
    0x748f82ee5defb2fc, 0x78a5636f43172f60, 0x84c87814a1f0ab72, 0x8cc702081a6439ec,
    0x90befffa23631e28, 0xa4506cebde82bde9, 0xbef9a3f7b2c67915, 0xc67178f2e372532b,
    0xca273eceea26619c, 0xd186b8c721c0c207, 0xeada7dd6cde0eb1e, 0xf57d4f7fee6ed178,
    0x06f067aa72176fba, 0x0a637dc5a2c898a6, 0x113f9804bef90dae, 0x1b710b35131c471b,
    0x28db77f523047d84, 0x32caab7b40c72493, 0x3c9ebe0a15c9bebc, 0x431d67c49c100d4c,
    0x4cc5d4becb3e42b6, 0x597f299cfc657e2a, 0x5fcb6fab3ad6faec, 0x6c44198c4a475817,
];

/// SHA-512's initial hash value, §5.3.5.
#[rustfmt::skip]
const H512: [u64; 8] = [
    0x6a09e667f3bcc908, 0xbb67ae8584caa73b, 0x3c6ef372fe94f82b, 0xa54ff53a5f1d36f1,
    0x510e527fade682d1, 0x9b05688c2b3e6c1f, 0x1f83d9abfb41bd6b, 0x5be0cd19137e2179,
];

/// One block into the hash value: SHA-256's computation, §6.2.2, with
/// §4.1.2's functions.
fn compress256(state: &mut [u32; 8], block: &[u8; 64]) {
    let mut w = [0u32; 64];
    for (word, bytes) in w.iter_mut().zip(block.chunks_exact(4)) {
        *word = u32::from_be_bytes(bytes.try_into().expect("chunks of four"));
    }
    for t in 16..64 {
        let s0 = w[t - 15].rotate_right(7) ^ w[t - 15].rotate_right(18) ^ (w[t - 15] >> 3);
        let s1 = w[t - 2].rotate_right(17) ^ w[t - 2].rotate_right(19) ^ (w[t - 2] >> 10);
        w[t] = w[t - 16]
            .wrapping_add(s0)
            .wrapping_add(w[t - 7])
            .wrapping_add(s1);
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for (k, w) in K256.iter().zip(w) {
        let t1 = h
            .wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25))
            .wrapping_add((e & f) ^ (!e & g))
            .wrapping_add(*k)
            .wrapping_add(w);
        let t2 = (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22))
            .wrapping_add((a & b) ^ (a & c) ^ (b & c));
        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }
    for (word, v) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
        *word = word.wrapping_add(v);
    }
}

/// One block into the hash value: SHA-512's computation, §6.4.2, with
/// §4.1.3's functions.
fn compress512(state: &mut [u64; 8], block: &[u8; 128]) {
    let mut w = [0u64; 80];
    for (word, bytes) in w.iter_mut().zip(block.chunks_exact(8)) {
        *word = u64::from_be_bytes(bytes.try_into().expect("chunks of eight"));
    }
    for t in 16..80 {
        let s0 = w[t - 15].rotate_right(1) ^ w[t - 15].rotate_right(8) ^ (w[t - 15] >> 7);
        let s1 = w[t - 2].rotate_right(19) ^ w[t - 2].rotate_right(61) ^ (w[t - 2] >> 6);
        w[t] = w[t - 16]
            .wrapping_add(s0)
            .wrapping_add(w[t - 7])
            .wrapping_add(s1);
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for (k, w) in K512.iter().zip(w) {
        let t1 = h
            .wrapping_add(e.rotate_right(14) ^ e.rotate_right(18) ^ e.rotate_right(41))
            .wrapping_add((e & f) ^ (!e & g))
            .wrapping_add(*k)
            .wrapping_add(w);
        let t2 = (a.rotate_right(28) ^ a.rotate_right(34) ^ a.rotate_right(39))
            .wrapping_add((a & b) ^ (a & c) ^ (b & c));
        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }
    for (word, v) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
        *word = word.wrapping_add(v);
    }
}

/// The two hashes differ in their word, block, length field, constants and
/// functions, and in nothing about how a message is buffered and padded.
macro_rules! hash {
    ($(#[$doc:meta])* $name:ident: $word:ty, block $block:literal, length $length:literal,
     digest $out:literal, $init:expr, $compress:ident) => {
        $(#[$doc])*
        pub struct $name {
            state: [$word; 8],
            block: [u8; $block],
            /// How much of `block` holds message bytes not yet compressed.
            filled: usize,
            /// The message's length so far, in bytes.
            bytes: u64,
        }

        impl $name {
            pub const fn new() -> Self {
                Self { state: $init, block: [0; $block], filled: 0, bytes: 0 }
            }

            /// The digest of `message`.
            pub fn digest(message: impl AsRef<[u8]>) -> [u8; $out] {
                let mut hash = Self::new();
                hash.update(message);
                hash.finalize()
            }

            /// `bytes` appended to the message.
            pub fn update(&mut self, bytes: impl AsRef<[u8]>) {
                self.absorb(bytes.as_ref());
            }

            fn absorb(&mut self, mut bytes: &[u8]) {
                self.bytes += bytes.len() as u64;
                if self.filled > 0 {
                    let take = bytes.len().min($block - self.filled);
                    self.block[self.filled..self.filled + take].copy_from_slice(&bytes[..take]);
                    self.filled += take;
                    bytes = &bytes[take..];
                    if self.filled < $block {
                        return;
                    }
                    $compress(&mut self.state, &self.block);
                    self.filled = 0;
                }
                let mut blocks = bytes.chunks_exact($block);
                for block in &mut blocks {
                    $compress(&mut self.state, block.try_into().expect("an exact chunk"));
                }
                let rest = blocks.remainder();
                self.block[..rest.len()].copy_from_slice(rest);
                self.filled = rest.len();
            }

            /// The message's digest: padded as §5.1 pads it — a one bit, the
            /// fewest zero bits that end the block where the length field
            /// begins, and the length in bits, big-endian.
            pub fn finalize(mut self) -> [u8; $out] {
                let bits = (u128::from(self.bytes) * 8).to_be_bytes();
                let (over, length) = bits.split_at(16 - $length);
                assert!(
                    over.iter().all(|&b| b == 0),
                    "FIPS 180-4 §5.1 bounds a message by the bits its length field holds"
                );
                let mut pad = [0u8; $block];
                pad[0] = 0x80;
                self.absorb(&pad[..($block - $length - 1 + $block - self.filled) % $block + 1]);
                self.absorb(length);
                let mut out = [0u8; $out];
                for (bytes, word) in out.chunks_exact_mut(size_of::<$word>()).zip(self.state) {
                    bytes.copy_from_slice(&word.to_be_bytes());
                }
                out
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }
    };
}

hash! {
    /// SHA-256, §6.2: a 32-byte digest of a message shorter than 2^64 bits.
    Sha256: u32, block 64, length 8, digest 32, H256, compress256
}

hash! {
    /// SHA-512, §6.4: a 64-byte digest.
    Sha512: u64, block 128, length 16, digest 64, H512, compress512
}
