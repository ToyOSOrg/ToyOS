//! SHA-256, SHA-384 and SHA-512, FIPS 180-4, for TLS, SSH and image digests.
//! Compression has fixed rounds and addresses independent of message contents.
//! Buffering and padding depend on the public message length. Hash states and
//! compression scratch are erased because HMAC and signing hash secret inputs.

use crate::secret::{wipe, wipe32, wipe64};

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
const K512: [u64; 80] = [
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

fn compress256(h: &mut [u32; 8], block: &[u8; 64]) {
    let mut w = [0u32; 64];
    for (word, bytes) in w[..16].iter_mut().zip(block.as_chunks::<4>().0) {
        *word = u32::from_be_bytes(*bytes);
    }
    for i in 16..64 {
        let a = w[i - 15];
        let b = w[i - 2];
        w[i] = w[i - 16]
            .wrapping_add(a.rotate_right(7) ^ a.rotate_right(18) ^ (a >> 3))
            .wrapping_add(w[i - 7])
            .wrapping_add(b.rotate_right(17) ^ b.rotate_right(19) ^ (b >> 10));
    }
    let mut v = *h;
    for i in 0..64 {
        let [a, b, c, d, e, f, g, z] = v;
        let t = z
            .wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25))
            .wrapping_add((e & f) ^ (!e & g))
            .wrapping_add(K256[i])
            .wrapping_add(w[i]);
        let u = (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22))
            .wrapping_add((a & b) ^ (a & c) ^ (b & c));
        v = [t.wrapping_add(u), a, b, c, d.wrapping_add(t), e, f, g];
    }
    for i in 0..8 {
        h[i] = h[i].wrapping_add(v[i]);
    }
    wipe32(&mut w);
    wipe32(&mut v);
}

fn compress512(h: &mut [u64; 8], block: &[u8; 128]) {
    let mut w = [0u64; 80];
    for (word, bytes) in w[..16].iter_mut().zip(block.as_chunks::<8>().0) {
        *word = u64::from_be_bytes(*bytes);
    }
    for i in 16..80 {
        let a = w[i - 15];
        let b = w[i - 2];
        w[i] = w[i - 16]
            .wrapping_add(a.rotate_right(1) ^ a.rotate_right(8) ^ (a >> 7))
            .wrapping_add(w[i - 7])
            .wrapping_add(b.rotate_right(19) ^ b.rotate_right(61) ^ (b >> 6));
    }
    let mut v = *h;
    for i in 0..80 {
        let [a, b, c, d, e, f, g, z] = v;
        let t = z
            .wrapping_add(e.rotate_right(14) ^ e.rotate_right(18) ^ e.rotate_right(41))
            .wrapping_add((e & f) ^ (!e & g))
            .wrapping_add(K512[i])
            .wrapping_add(w[i]);
        let u = (a.rotate_right(28) ^ a.rotate_right(34) ^ a.rotate_right(39))
            .wrapping_add((a & b) ^ (a & c) ^ (b & c));
        v = [t.wrapping_add(u), a, b, c, d.wrapping_add(t), e, f, g];
    }
    for i in 0..8 {
        h[i] = h[i].wrapping_add(v[i]);
    }
    wipe64(&mut w);
    wipe64(&mut v);
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
    fn finish_into(self, output: &mut [u8]) -> Result<(), crate::Error>;
    #[cfg(feature = "rustls-provider")]
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
                    self.block[self.filled..self.filled + n].copy_from_slice(&message[..n]);
                    self.filled += n;
                    message = &message[n..];
                    if self.filled == $block {
                        $compress(&mut self.state, &self.block);
                        self.filled = 0;
                    }
                }
                while message.len() >= $block {
                    $compress(&mut self.state, message[..$block].try_into().unwrap());
                    message = &message[$block..];
                }
                if !message.is_empty() {
                    self.block[self.filled..self.filled + message.len()].copy_from_slice(message);
                    self.filled += message.len();
                }
                Ok(())
            }
            pub fn finalize(mut self) -> [u8; $out] {
                self.block[self.filled] = 0x80;
                self.filled += 1;
                self.block[self.filled..].fill(0);
                if self.filled > $block - $length {
                    $compress(&mut self.state, &self.block);
                    self.block.fill(0);
                }
                let bits = (self.bytes * 8).to_be_bytes();
                self.block[$block - $length..].copy_from_slice(&bits[16 - $length..]);
                $compress(&mut self.state, &self.block);
                let mut result = [0; $out];
                for (chunk, word) in result
                    .chunks_exact_mut(core::mem::size_of::<$word>())
                    .zip(self.state.iter())
                {
                    chunk.copy_from_slice(&word.to_be_bytes());
                }
                result
            }
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
            #[cfg(feature = "rustls-provider")]
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
            fn new() -> Self {
                Self::new()
            }
            fn update(&mut self, message: &[u8]) -> Result<(), crate::Error> {
                self.update(message)
            }
            fn finish_into(self, output: &mut [u8]) -> Result<(), crate::Error> {
                if output.len() != $out {
                    return Err(crate::Error::Length);
                }
                let mut digest = self.finalize();
                output.copy_from_slice(&digest);
                crate::secret::wipe(&mut digest);
                Ok(())
            }
        }
        impl Drop for $name {
            fn drop(&mut self) {
                wipe(&mut self.block);
                for word in &mut self.state {
                    // Each word is live and exclusively borrowed.
                    unsafe { core::ptr::write_volatile(word, 0) };
                }
                core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
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
