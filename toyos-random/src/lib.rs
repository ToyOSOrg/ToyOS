//! The kernel's random generator, and the judgment of what may key it.
//!
//! **A [`Generator`] exists only keyed.** Its one constructor takes a [`Seed`],
//! and a `Seed` is 32 bytes [`Seed::judge`] did not refuse: bytes whose four
//! words are one value are what a source that failed or stuck leaves, zeros
//! and ones among them, and a length other than 32 is no seed either. The
//! bytes are never parsed: a seed is trusted for being secret and
//! unpredictable, which nothing here can check, and for nothing about its
//! form.
//!
//! **Every source is mixed in, none is substituted.** [`Generator::mix`] XORs
//! a seed into the key and replaces the key with the ChaCha20 block of the
//! result, so the key is unpredictable while any one seed mixed was, provided
//! no seed was chosen by somebody who knew the key it met.
//!
//! **A key gives one draw.** [`Generator::stream`] is fast key erasure: one
//! block under the key, whose first half replaces the key and whose second
//! keys the [`Stream`] the draw's bytes come from. The key a draw was made
//! under is gone when the draw begins, so the generator's memory read later
//! does not give an earlier draw, and one draw's bytes are ChaCha20 output
//! under a key no other draw has.
//!
//! Pure: the caller draws from the machine and holds the lock.

#![no_std]

mod chacha;

#[cfg(test)]
mod tests;

use core::fmt;
use core::sync::atomic::{Ordering, compiler_fence};

/// The bytes of a seed, and of a key.
pub const SEED_LEN: usize = 32;

/// Zero `buf` with writes the compiler may not drop, though nothing reads
/// `buf` again.
pub fn wipe<T: Copy + Default>(buf: &mut [T]) {
    for slot in buf.iter_mut() {
        // SAFETY: `slot` is a live, aligned, exclusive `T`, and `T: Copy`
        // has no destructor the overwrite skips.
        unsafe { core::ptr::write_volatile(slot, T::default()) };
    }
    compiler_fence(Ordering::SeqCst);
}

/// Why bytes are no seed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Not [`SEED_LEN`] bytes, but this many.
    Length(usize),
    /// The four 8-byte words are one value.
    Constant,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::Length(got) => write!(f, "{got} bytes where a seed is {SEED_LEN}"),
            Refusal::Constant => {
                f.write_str("its four words are one value, which is what a source that failed leaves")
            }
        }
    }
}

/// 32 bytes [`Seed::judge`] did not refuse. Neither printed nor copied, and
/// wiped when it goes.
pub struct Seed([u8; SEED_LEN]);

impl Seed {
    pub fn judge(bytes: &[u8]) -> Result<Seed, Refusal> {
        let Ok(bytes) = <&[u8; SEED_LEN]>::try_from(bytes) else {
            return Err(Refusal::Length(bytes.len()));
        };
        let (words, _) = bytes.as_chunks::<8>();
        if words.iter().all(|word| *word == words[0]) {
            return Err(Refusal::Constant);
        }
        Ok(Seed(*bytes))
    }
}

impl Drop for Seed {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

/// The key before the first seed, so that the first [`Generator::mix`] is the
/// same step as every later one.
const UNKEYED: [u8; SEED_LEN] = *b"ToyOS kernel random generator v1";

/// The nonce of a block that replaces the key by a mix, and of one that
/// replaces it by a draw: no block of one is a block of the other.
const MIX: [u8; 12] = *b"toyos-mix\0\0\0";
const DRAW: [u8; 12] = *b"toyos-draw\0\0";

/// The key every draw descends from.
pub struct Generator {
    key: [u8; SEED_LEN],
}

impl Generator {
    pub fn keyed(seed: Seed) -> Generator {
        let mut generator = Generator { key: UNKEYED };
        generator.mix(seed);
        generator
    }

    pub fn mix(&mut self, seed: Seed) {
        for (key, seed) in self.key.iter_mut().zip(&seed.0) {
            *key ^= seed;
        }
        let mut block = [0u8; 64];
        chacha::block(&self.key, 0, &MIX, &mut block);
        self.key.copy_from_slice(&block[..SEED_LEN]);
        wipe(&mut block);
    }

    /// One draw's bytes. The key this was made under is replaced before it
    /// returns.
    pub fn stream(&mut self) -> Stream {
        let mut block = [0u8; 64];
        chacha::block(&self.key, 0, &DRAW, &mut block);
        self.key.copy_from_slice(&block[..SEED_LEN]);
        let mut stream = Stream { key: [0; SEED_LEN], next: 0 };
        stream.key.copy_from_slice(&block[SEED_LEN..]);
        wipe(&mut block);
        stream
    }
}

impl Drop for Generator {
    fn drop(&mut self) {
        wipe(&mut self.key);
    }
}

/// One draw: the ChaCha20 keystream of a key of its own, under a 64-bit block
/// counter no draw can exhaust.
pub struct Stream {
    key: [u8; SEED_LEN],
    next: u64,
}

impl Stream {
    /// The stream's next bytes. Each call begins a block, and what `out` leaves
    /// of its last one is discarded.
    pub fn fill(&mut self, out: &mut [u8]) {
        for chunk in out.chunks_mut(64) {
            let mut nonce = [0u8; 12];
            nonce[..4].copy_from_slice(&((self.next >> 32) as u32).to_le_bytes());
            let mut block = [0u8; 64];
            chacha::block(&self.key, self.next as u32, &nonce, &mut block);
            self.next += 1;
            chunk.copy_from_slice(&block[..chunk.len()]);
            wipe(&mut block);
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        wipe(&mut self.key);
    }
}
