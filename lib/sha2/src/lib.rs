//! SHA-256 and SHA-512, FIPS 180-4 §6.2 and §6.4, over whole bytes: what the
//! loader holds a signed image's sections to, what `update` and `swap` hold
//! what they install to, and what the build names its inputs by.
//!
//! [`Sha256`] and [`Sha512`] take a message in any number of [`update`]s and
//! give its digest at [`finalize`]; `digest` is both at once. The digest of a
//! message is the same however it was split. A message is whole bytes: FIPS
//! 180-4 also hashes a trailing partial byte, and nothing here needs one.
//!
//! **Scalar, on every target.** An instruction path is `unsafe`, which this
//! crate forbids: `toyos-sha2-hw`'s compresses SHA-256's blocks on a CPU that
//! has the instructions, through [`Sha256::with`], and this crate buffers and
//! pads around it.
//!
//! [`update`]: Sha256::update
//! [`finalize`]: Sha256::finalize

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

#[cfg(test)]
mod tests;

/// SHA-256's round constants, §4.2.2: an instruction path's too.
#[rustfmt::skip]
pub const K256: [u32; 64] = [
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

/// `$step::<I>$args` for each `I` of a pass's sixteen rounds, written out so
/// that every index into the schedule and the working variables is a
/// constant.
macro_rules! sixteen {
    ($step:ident $args:tt) => {
        sixteen!(@ $step $args 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15)
    };
    (@ $step:ident $args:tt $($i:literal)*) => {
        $($step::<$i> $args;)*
    };
}

/// The two compression functions differ in their word, block, constants and
/// rotations, and in nothing about how a block is computed.
macro_rules! compress {
    ($(#[$doc:meta])* $name:ident: $word:ty, block $block:literal, $k:expr,
     sigma0 $s0:expr, sigma1 $s1:expr, schedule0 $r0:expr, schedule1 $r1:expr) => {
        $(#[$doc])*
        fn $name(state: &mut [$word; 8], blocks: &[[u8; $block]]) {
            /// Round `I` of a pass, the working variables standing where
            /// they are rather than moved along: this round's `a` is
            /// `v[(8 - I % 8) % 8]`, and it writes only its new `e` and `a`.
            #[inline(always)]
            fn round<const I: usize>(v: &mut [$word; 8], k: &[$word; 16], w: &[$word; 16]) {
                let at = |n: usize| (n + 8 - I % 8) % 8;
                let (a, b, c) = (v[at(0)], v[at(1)], v[at(2)]);
                let (e, f, g) = (v[at(4)], v[at(5)], v[at(6)]);
                let [x, y, z] = $s1;
                let t1 = v[at(7)]
                    .wrapping_add(k[I])
                    .wrapping_add(w[I])
                    .wrapping_add(g ^ (e & (f ^ g)))
                    .wrapping_add(e.rotate_right(x) ^ e.rotate_right(y) ^ e.rotate_right(z));
                let [x, y, z] = $s0;
                let t2 = (a.rotate_right(x) ^ a.rotate_right(y) ^ a.rotate_right(z))
                    .wrapping_add(b ^ ((a ^ b) & (b ^ c)));
                v[at(3)] = v[at(3)].wrapping_add(t1);
                v[at(7)] = t1.wrapping_add(t2);
            }

            /// The schedule's word `I` of the next pass in place of this
            /// pass's: `w[I]` holds W(t-16) going in and W(t) coming out.
            #[inline(always)]
            fn schedule<const I: usize>(w: &mut [$word; 16]) {
                let (w15, w2) = (w[(I + 1) % 16], w[(I + 14) % 16]);
                let [x, y, z] = $r0;
                let s0 = w15.rotate_right(x) ^ w15.rotate_right(y) ^ (w15 >> z);
                let [x, y, z] = $r1;
                let s1 = w2.rotate_right(x) ^ w2.rotate_right(y) ^ (w2 >> z);
                w[I] = w[I]
                    .wrapping_add(s0)
                    .wrapping_add(w[(I + 9) % 16])
                    .wrapping_add(s1);
            }

            let mut hash = *state;
            for block in blocks {
                let mut w: [$word; 16] = [0; 16];
                for (word, bytes) in w.iter_mut().zip(block.as_chunks().0) {
                    *word = <$word>::from_be_bytes(*bytes);
                }
                let mut v = hash;
                for (pass, k) in $k.as_chunks::<16>().0.iter().enumerate() {
                    if pass > 0 {
                        sixteen!(schedule(&mut w));
                    }
                    sixteen!(round(&mut v, k, &w));
                }
                for (word, v) in hash.iter_mut().zip(v) {
                    *word = word.wrapping_add(v);
                }
            }
            *state = hash;
        }
    };
}

compress! {
    /// Blocks into the hash value: SHA-256's computation, §6.2.2, with
    /// §4.1.2's functions.
    compress256: u32, block 64, K256,
    sigma0 [2, 13, 22], sigma1 [6, 11, 25], schedule0 [7, 18, 3], schedule1 [17, 19, 10]
}

compress! {
    /// Blocks into the hash value: SHA-512's computation, §6.4.2, with
    /// §4.1.3's functions.
    compress512: u64, block 128, K512,
    sigma0 [28, 34, 39], sigma1 [14, 18, 41], schedule0 [1, 8, 7], schedule1 [19, 61, 6]
}

/// The two hashes differ in their word, block, length field, constants and
/// functions, and in nothing about how a message is buffered and padded.
macro_rules! hash {
    ($(#[$doc:meta])* $name:ident: $word:ty, block $block:literal, length $length:literal,
     digest $out:literal, $init:expr, $compress:ident) => {
        $(#[$doc])*
        pub struct $name {
            state: [$word; 8],
            /// What compresses whole blocks: this crate's, or the instruction
            /// path [`Sha256::with`] was given.
            compress: fn(&mut [$word; 8], &[[u8; $block]]),
            block: [u8; $block],
            /// How much of `block` holds message bytes not yet compressed.
            filled: usize,
            /// The message's length so far, in bytes.
            bytes: u64,
        }

        impl $name {
            pub const fn new() -> Self {
                Self { state: $init, compress: $compress, block: [0; $block], filled: 0, bytes: 0 }
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
                    (self.compress)(&mut self.state, core::slice::from_ref(&self.block));
                    self.filled = 0;
                }
                let (blocks, rest) = bytes.as_chunks::<$block>();
                (self.compress)(&mut self.state, blocks);
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
                for (bytes, word) in out.as_chunks_mut::<{ size_of::<$word>() }>().0.iter_mut().zip(self.state) {
                    *bytes = word.to_be_bytes();
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

/// What compresses whole blocks into SHA-256's hash value, as §6.2.2 does.
pub type Compress256 = fn(&mut [u32; 8], &[[u8; 64]]);

impl Sha256 {
    /// A hash whose blocks `compress` computes: an instruction path's, which
    /// must give §6.2.2's hash value for every run of blocks, the empty one
    /// among them.
    pub const fn with(compress: Compress256) -> Self {
        Self { compress, ..Self::new() }
    }
}
