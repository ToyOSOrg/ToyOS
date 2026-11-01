//! SHA-256, SHA-384 and SHA-512, FIPS 180-4, for TLS, SSH and image digests.
//! Compression has fixed rounds and addresses independent of message contents.
//! Buffering and padding depend on the public message length. Hash states and
//! the portable rounds' message schedule are erased because HMAC and signing
//! hash secret inputs; the instruction paths' register and stack temporaries
//! are not.

use crate::secret::erase;

pub(crate) const K256: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];
pub(crate) const K512: [u64; 80] = [
    0x428a2f98d728ae22,
    0x7137449123ef65cd,
    0xb5c0fbcfec4d3b2f,
    0xe9b5dba58189dbbc,
    0x3956c25bf348b538,
    0x59f111f1b605d019,
    0x923f82a4af194f9b,
    0xab1c5ed5da6d8118,
    0xd807aa98a3030242,
    0x12835b0145706fbe,
    0x243185be4ee4b28c,
    0x550c7dc3d5ffb4e2,
    0x72be5d74f27b896f,
    0x80deb1fe3b1696b1,
    0x9bdc06a725c71235,
    0xc19bf174cf692694,
    0xe49b69c19ef14ad2,
    0xefbe4786384f25e3,
    0x0fc19dc68b8cd5b5,
    0x240ca1cc77ac9c65,
    0x2de92c6f592b0275,
    0x4a7484aa6ea6e483,
    0x5cb0a9dcbd41fbd4,
    0x76f988da831153b5,
    0x983e5152ee66dfab,
    0xa831c66d2db43210,
    0xb00327c898fb213f,
    0xbf597fc7beef0ee4,
    0xc6e00bf33da88fc2,
    0xd5a79147930aa725,
    0x06ca6351e003826f,
    0x142929670a0e6e70,
    0x27b70a8546d22ffc,
    0x2e1b21385c26c926,
    0x4d2c6dfc5ac42aed,
    0x53380d139d95b3df,
    0x650a73548baf63de,
    0x766a0abb3c77b2a8,
    0x81c2c92e47edaee6,
    0x92722c851482353b,
    0xa2bfe8a14cf10364,
    0xa81a664bbc423001,
    0xc24b8b70d0f89791,
    0xc76c51a30654be30,
    0xd192e819d6ef5218,
    0xd69906245565a910,
    0xf40e35855771202a,
    0x106aa07032bbd1b8,
    0x19a4c116b8d2d0c8,
    0x1e376c085141ab53,
    0x2748774cdf8eeb99,
    0x34b0bcb5e19b48a8,
    0x391c0cb3c5c95a63,
    0x4ed8aa4ae3418acb,
    0x5b9cca4f7763e373,
    0x682e6ff3d6b2b8a3,
    0x748f82ee5defb2fc,
    0x78a5636f43172f60,
    0x84c87814a1f0ab72,
    0x8cc702081a6439ec,
    0x90befffa23631e28,
    0xa4506cebde82bde9,
    0xbef9a3f7b2c67915,
    0xc67178f2e372532b,
    0xca273eceea26619c,
    0xd186b8c721c0c207,
    0xeada7dd6cde0eb1e,
    0xf57d4f7fee6ed178,
    0x06f067aa72176fba,
    0x0a637dc5a2c898a6,
    0x113f9804bef90dae,
    0x1b710b35131c471b,
    0x28db77f523047d84,
    0x32caab7b40c72493,
    0x3c9ebe0a15c9bebc,
    0x431d67c49c100d4c,
    0x4cc5d4becb3e42b6,
    0x597f299cfc657e2a,
    0x5fcb6fab3ad6faec,
    0x6c44198c4a475817,
];

/// One SHA-2 round, the working variables renamed rather than moved: round
/// t computes into `$h` and `$d`, and round t+1 names them one place on.
macro_rules! round {
    ($w:ident, $k:ident, $bc:ident, $i:literal, $later:expr,
     [$a:ident, $b:ident, $c:ident, $d:ident, $e:ident, $f:ident, $g:ident, $h:ident],
     $big0:expr, $big1:expr, $small0:expr, $small1:expr) => {
        if $later {
            // W[t] = σ1(W[t-2]) + W[t-7] + σ0(W[t-15]) + W[t-16], in a window of 16.
            $w[$i] = $w[$i]
                .wrapping_add($small0($w[($i + 1) & 15]))
                .wrapping_add($w[($i + 9) & 15])
                .wrapping_add($small1($w[($i + 14) & 15]));
        }
        let t1 = $h
            .wrapping_add($big1($e))
            .wrapping_add($g ^ ($e & ($f ^ $g)))
            .wrapping_add($k[$i])
            .wrapping_add($w[$i]);
        // Maj(a, b, c) = ((a ^ b) & (b ^ c)) ^ b, and this round's a ^ b is
        // the next round's b ^ c.
        let ab = $a ^ $b;
        let t2 = $big0($a).wrapping_add((ab & $bc) ^ $b);
        $bc = ab;
        $d = $d.wrapping_add(t1);
        $h = t1.wrapping_add(t2);
    };
}

#[allow(unused_imports, reason = "the instruction paths' rounds share it; portable builds have none")]
pub(crate) use round;

/// SHA-512's Σ0 and Σ1, which the x86-64 schedule's rounds share.
pub(crate) const fn big0(x: u64) -> u64 {
    x.rotate_right(28) ^ x.rotate_right(34) ^ x.rotate_right(39)
}

pub(crate) const fn big1(x: u64) -> u64 {
    x.rotate_right(14) ^ x.rotate_right(18) ^ x.rotate_right(41)
}

/// FIPS 180-4 §6.2.2 and §6.4.2 over whole blocks: sixteen rounds unrolled
/// per pass over the schedule's window, which is erased once at the end.
macro_rules! rounds {
    ($name:ident, $word:ty, $bytes:literal, $block:literal, $k:ident,
     $big0:expr, $big1:expr, $small0:expr, $small1:expr) => {
        #[inline(always)]
        pub(crate) fn $name(state: &mut [$word; 8], blocks: &[[u8; $block]]) {
            let mut w = [0 as $word; 16];
            for block in blocks {
                let (words, _) = block.as_chunks::<$bytes>();
                for i in 0..16 {
                    w[i] = <$word>::from_be_bytes(words[i]);
                }
                let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
                let mut bc = b ^ c;
                let (passes, _) = $k.as_chunks::<16>();
                for pass in 0..passes.len() {
                    let k = &passes[pass];
                    let later = pass != 0;
                    round!(w, k, bc, 0, later, [a, b, c, d, e, f, g, h], $big0, $big1, $small0, $small1);
                    round!(w, k, bc, 1, later, [h, a, b, c, d, e, f, g], $big0, $big1, $small0, $small1);
                    round!(w, k, bc, 2, later, [g, h, a, b, c, d, e, f], $big0, $big1, $small0, $small1);
                    round!(w, k, bc, 3, later, [f, g, h, a, b, c, d, e], $big0, $big1, $small0, $small1);
                    round!(w, k, bc, 4, later, [e, f, g, h, a, b, c, d], $big0, $big1, $small0, $small1);
                    round!(w, k, bc, 5, later, [d, e, f, g, h, a, b, c], $big0, $big1, $small0, $small1);
                    round!(w, k, bc, 6, later, [c, d, e, f, g, h, a, b], $big0, $big1, $small0, $small1);
                    round!(w, k, bc, 7, later, [b, c, d, e, f, g, h, a], $big0, $big1, $small0, $small1);
                    round!(w, k, bc, 8, later, [a, b, c, d, e, f, g, h], $big0, $big1, $small0, $small1);
                    round!(w, k, bc, 9, later, [h, a, b, c, d, e, f, g], $big0, $big1, $small0, $small1);
                    round!(w, k, bc, 10, later, [g, h, a, b, c, d, e, f], $big0, $big1, $small0, $small1);
                    round!(w, k, bc, 11, later, [f, g, h, a, b, c, d, e], $big0, $big1, $small0, $small1);
                    round!(w, k, bc, 12, later, [e, f, g, h, a, b, c, d], $big0, $big1, $small0, $small1);
                    round!(w, k, bc, 13, later, [d, e, f, g, h, a, b, c], $big0, $big1, $small0, $small1);
                    round!(w, k, bc, 14, later, [c, d, e, f, g, h, a, b], $big0, $big1, $small0, $small1);
                    round!(w, k, bc, 15, later, [b, c, d, e, f, g, h, a], $big0, $big1, $small0, $small1);
                }
                let v = [a, b, c, d, e, f, g, h];
                for i in 0..8 {
                    state[i] = state[i].wrapping_add(v[i]);
                }
            }
            crate::secret::erase(&mut w);
        }
    };
}

rounds!(
    sha256_rounds,
    u32,
    4,
    64,
    K256,
    |x: u32| x.rotate_right(2) ^ x.rotate_right(13) ^ x.rotate_right(22),
    |x: u32| x.rotate_right(6) ^ x.rotate_right(11) ^ x.rotate_right(25),
    |x: u32| x.rotate_right(7) ^ x.rotate_right(18) ^ (x >> 3),
    |x: u32| x.rotate_right(17) ^ x.rotate_right(19) ^ (x >> 10)
);
rounds!(
    sha512_rounds,
    u64,
    8,
    128,
    K512,
    big0,
    big1,
    |x: u64| x.rotate_right(1) ^ x.rotate_right(8) ^ (x >> 7),
    |x: u64| x.rotate_right(19) ^ x.rotate_right(61) ^ (x >> 6)
);

fn compress256(state: &mut [u32; 8], blocks: &[[u8; 64]]) {
    if !crate::arch::sha256_blocks(state, blocks) {
        sha256_rounds(state, blocks);
    }
}

fn compress512(state: &mut [u64; 8], blocks: &[[u8; 128]]) {
    if !crate::arch::sha512_blocks(state, blocks) {
        sha512_rounds(state, blocks);
    }
}

pub(crate) mod sealed {
    pub trait Sealed {}
}

/// The SHA-2 algorithms admitted by HMAC and HKDF. Implementations are sealed.
pub trait Digest: sealed::Sealed + Sized {
    const BLOCK_LEN: usize;
    const OUTPUT_LEN: usize;
    fn new() -> Self;
    const MAX_BYTES: u128;
    fn update(&mut self, message: &[u8]) -> Result<(), crate::Error>;
    /// The digest into `output`, which must be `OUTPUT_LEN` bytes, then the
    /// hasher reset to its initial state.
    fn finish_into(&mut self, output: &mut [u8]) -> Result<(), crate::Error>;
    fn fork(&self) -> Self;
}

macro_rules! hash {
    ($name:ident,$word:ty,$block:literal,$length:literal,$out:literal,$initial:expr,$compress:ident) => {
        pub struct $name {
            state: [$word; 8],
            block: [u8; $block],
            filled: usize,
            bytes: u128,
        }
        impl $name {
            #[inline]
            pub const fn new() -> Self {
                Self {
                    state: $initial,
                    block: [0; $block],
                    filled: 0,
                    bytes: 0,
                }
            }
            /// A length refusal leaves the state unchanged.
            pub fn update(&mut self, mut message: &[u8]) -> Result<(), crate::Error> {
                let bytes = self
                    .bytes
                    .checked_add(message.len() as u128)
                    .ok_or(crate::Error::Length)?;
                if bytes >= (1u128 << ($length * 8 - 3)) {
                    return Err(crate::Error::Length);
                }
                self.bytes = bytes;
                if self.filled != 0 {
                    let n = message.len().min($block - self.filled);
                    crate::block::copy(&mut self.block[self.filled..self.filled + n], &message[..n]);
                    self.filled += n;
                    message = &message[n..];
                    if self.filled == $block {
                        $compress(&mut self.state, core::slice::from_ref(&self.block));
                        self.filled = 0;
                    }
                }
                let (blocks, rest) = message.as_chunks::<$block>();
                if !blocks.is_empty() {
                    $compress(&mut self.state, blocks);
                }
                message = rest;
                if !message.is_empty() {
                    crate::block::copy(&mut self.block[self.filled..self.filled + message.len()], message);
                    self.filled += message.len();
                }
                Ok(())
            }
            #[inline(always)]
            pub fn finalize(mut self) -> [u8; $out] {
                let mut result = [0; $out];
                self.finish(&mut result);
                result
            }
            /// The digest into `out`, then the hasher as [`Self::new`] made it.
            #[inline(always)]
            fn finish(&mut self, out: &mut [u8; $out]) {
                // The padding over fixed 16-byte chunks: a fill of the
                // variable remainder would be a library `memset` call.
                let filled = self.filled;
                let (chunks, _) = self.block.as_chunks_mut::<16>();
                for j in 0..$block / 16 {
                    if 16 * j + 16 > filled {
                        let at = filled.saturating_sub(16 * j);
                        chunks[j] = crate::block::terminated(&chunks[j], at, if at < 16 && 16 * j <= filled { 0x80 } else { 0 });
                    }
                }
                if filled + 1 > $block - $length {
                    $compress(&mut self.state, core::slice::from_ref(&self.block));
                    self.block = [0; $block];
                }
                let bits = (self.bytes * 8).to_be_bytes();
                self.block[$block - $length..].copy_from_slice(&bits[16 - $length..]);
                $compress(&mut self.state, core::slice::from_ref(&self.block));
                for (i, chunk) in out.as_chunks_mut::<{ core::mem::size_of::<$word>() }>().0.iter_mut().enumerate() {
                    *chunk = self.state[i].to_be_bytes();
                }
                (self.state, self.filled, self.bytes) = ($initial, 0, 0);
            }
            #[inline(always)]
            pub fn digest(message: &[u8]) -> Result<[u8; $out], crate::Error> {
                let mut h = Self::new();
                h.update(message)?;
                Ok(h.finalize())
            }
        }
        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }
        impl sealed::Sealed for $name {}
        impl Digest for $name {
            fn fork(&self) -> Self {
                Self {
                    state: self.state,
                    block: self.block,
                    filled: self.filled,
                    bytes: self.bytes,
                }
            }
            const BLOCK_LEN: usize = $block;
            const OUTPUT_LEN: usize = $out;
            const MAX_BYTES: u128 = (1u128 << ($length * 8 - 3)) - 1;
            #[inline]
            fn new() -> Self {
                Self::new()
            }
            #[inline]
            fn update(&mut self, message: &[u8]) -> Result<(), crate::Error> {
                self.update(message)
            }
            #[inline(always)]
            fn finish_into(&mut self, output: &mut [u8]) -> Result<(), crate::Error> {
                let output: &mut [u8; $out] = output.try_into().map_err(|_| crate::Error::Length)?;
                self.finish(output);
                Ok(())
            }
        }
        impl Drop for $name {
            fn drop(&mut self) {
                erase(&mut self.block);
                erase(&mut self.state);
            }
        }
    };
}
hash!(
    Sha256,
    u32,
    64,
    8,
    32,
    [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19
    ],
    compress256
);
hash!(
    Sha384,
    u64,
    128,
    16,
    48,
    [
        0xcbbb9d5dc1059ed8,
        0x629a292a367cd507,
        0x9159015a3070dd17,
        0x152fecd8f70e5939,
        0x67332667ffc00b31,
        0x8eb44a8768581511,
        0xdb0c2e0d64f98fa7,
        0x47b5481dbefa4fa4
    ],
    compress512
);
hash!(
    Sha512,
    u64,
    128,
    16,
    64,
    [
        0x6a09e667f3bcc908,
        0xbb67ae8584caa73b,
        0x3c6ef372fe94f82b,
        0xa54ff53a5f1d36f1,
        0x510e527fade682d1,
        0x9b05688c2b3e6c1f,
        0x1f83d9abfb41bd6b,
        0x5be0cd19137e2179
    ],
    compress512
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatched_compression_matches_the_portable_rounds() {
        let bytes: std::vec::Vec<u8> = (0..128 * 9).map(|i: usize| (i * 151 + i / 7) as u8).collect();
        for n in 0..=9 {
            let (blocks, _) = bytes[..128 * n].as_chunks::<128>();
            let (start, mut portable) = ([0x0123_4567_89ab_cdefu64; 8], [0x0123_4567_89ab_cdefu64; 8]);
            let mut dispatched = start;
            compress512(&mut dispatched, blocks);
            sha512_rounds(&mut portable, blocks);
            assert_eq!(dispatched, portable, "SHA-512, {n} blocks");
            let (blocks, _) = bytes[..64 * n].as_chunks::<64>();
            let (mut dispatched, mut portable) = ([0x89ab_cdefu32; 8], [0x89ab_cdefu32; 8]);
            compress256(&mut dispatched, blocks);
            sha256_rounds(&mut portable, blocks);
            assert_eq!(dispatched, portable, "SHA-256, {n} blocks");
        }
    }
    #[test]
    fn length_refusals_leave_state_unchanged() {
        let mut h = Sha256::new();
        h.bytes = Sha256::MAX_BYTES;
        assert_eq!(h.update(&[0]), Err(crate::Error::Length));
        assert_eq!(h.bytes, Sha256::MAX_BYTES);
        assert_eq!(h.filled, 0);
        let mut h = Sha512::new();
        h.bytes = Sha512::MAX_BYTES;
        assert_eq!(h.update(&[0]), Err(crate::Error::Length));
        assert_eq!(h.bytes, Sha512::MAX_BYTES);
        assert_eq!(
            Sha256::new().finish_into(&mut [0; 31]),
            Err(crate::Error::Length)
        );
    }
}
