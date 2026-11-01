//! SP 800-38D GCM, restricted to the 96-bit nonces TLS uses. GHASH is a fixed
//! 128-step portable multiply or a CLMUL path, without data-indexed tables.
//! Content does not affect addresses or branches. Opening decrypts as it
//! authenticates, so a refusal zeroes the buffer: no unauthenticated
//! plaintext leaves.

use crate::{Error, aes::Aes, secret::Secret};

fn multiply(a: u128, mut b: u128) -> u128 {
    let mut product = 0;
    for bit in (0..128).rev() {
        let mask = 0u128.wrapping_sub(u128::from(crate::ct::opaque(((a >> bit) & 1) as u64)));
        product ^= b & mask;
        let reduce = 0u128.wrapping_sub(u128::from(crate::ct::opaque((b & 1) as u64)));
        b = (b >> 1) ^ (0xe1000000000000000000000000000000 & reduce);
    }
    product
}

pub(crate) struct Gcm {
    aes: Aes,
    h: Secret<16>,
    /// H's powers for the aggregated instruction path, when it is admitted.
    powers: Secret<48, u128>,
    aggregated: bool,
}
impl Gcm {
    /// Zero storage for provider construction; initialize before any operation.
    #[cfg(feature = "rustls-provider")]
    pub(crate) fn storage(key_len: usize) -> Self {
        Self {
            aes: Aes::storage(key_len),
            h: Secret([0; 16]),
            powers: Secret([0; 48]),
            aggregated: false,
        }
    }
    #[cfg(feature = "rustls-provider")]
    pub(crate) fn initialize(&mut self, key: &[u8]) {
        self.aes.initialize(key);
        self.h.0.fill(0);
        self.aes.encrypt(&mut self.h.0);
        self.aggregated = crate::arch::ghash_powers(&self.h.0, &mut self.powers.0);
    }
    pub(crate) fn aes128(key: &[u8; 16]) -> Self {
        Self::new(Aes::aes128(key))
    }
    pub(crate) fn aes256(key: &[u8; 32]) -> Self {
        Self::new(Aes::aes256(key))
    }
    fn new(aes: Aes) -> Self {
        let mut h = Secret([0; 16]);
        aes.encrypt(&mut h.0);
        let mut powers = Secret([0; 48]);
        let aggregated = crate::arch::ghash_powers(&h.0, &mut powers.0);
        Self { aes, h, powers, aggregated }
    }
    fn check(aad: usize, data: usize) -> Result<(), Error> {
        if data as u128 > (1u128 << 36) - 32 || aad as u128 > u64::MAX as u128 / 8 {
            Err(Error::Length)
        } else {
            Ok(())
        }
    }
    /// Folds whole blocks into `state`.
    fn hash_blocks(&self, state: &mut [u8; 16], blocks: &[[u8; 16]]) {
        if blocks.is_empty() {
            return;
        }
        if self.aggregated && crate::arch::ghash_blocks(&self.powers.0, state, blocks) {
            return;
        }
        let h = u128::from_be_bytes(self.h.0);
        for block in blocks {
            let x = u128::from_be_bytes(*state) ^ u128::from_be_bytes(*block);
            *state = crate::arch::ghash_multiply(x, h).unwrap_or_else(|| multiply(x, h)).to_be_bytes();
        }
    }
    /// Folds `input` into `state`, its last partial block padded with zeros.
    fn hash(&self, state: &mut [u8; 16], input: &[u8]) {
        let (blocks, tail) = input.as_chunks::<16>();
        self.hash_blocks(state, blocks);
        if !tail.is_empty() {
            let last = Secret(crate::block::padded(tail));
            self.hash_blocks(state, core::slice::from_ref(&last.0));
        }
    }
    fn lengths(aad: usize, data: usize) -> [u8; 16] {
        (((aad as u128 * 8) << 64) | (data as u128 * 8)).to_be_bytes()
    }
    /// GHASH over the padded AAD, the padded data and the lengths. A short
    /// message's blocks after the AAD's whole ones share one batch.
    fn digest(&self, aad: &[u8], data: &[u8]) -> Secret<16> {
        let mut state = Secret([0u8; 16]);
        let (aad_blocks, aad_tail) = aad.as_chunks::<16>();
        let (blocks, tail) = data.as_chunks::<16>();
        if blocks.len() > 5 {
            self.hash(&mut state.0, aad);
            self.hash(&mut state.0, data);
            self.hash_blocks(&mut state.0, &[Self::lengths(aad.len(), data.len())]);
            return state;
        }
        self.hash_blocks(&mut state.0, aad_blocks);
        let mut run = Secret([[0u8; 16]; 8]);
        let mut n = 0;
        for part in [aad_tail, blocks.as_flattened(), tail] {
            for chunk in part.chunks(16) {
                run.0[n] = crate::block::padded(chunk);
                n += 1;
            }
        }
        run.0[n] = Self::lengths(aad.len(), data.len());
        self.hash_blocks(&mut state.0, &run.0[..n + 1]);
        state
    }
    fn tag(&self, nonce: &[u8; 12], mut state: Secret<16>) -> Secret<16> {
        let mut j = Secret([0u8; 16]);
        j.0[..12].copy_from_slice(nonce);
        j.0[15] = 1;
        self.aes.encrypt(&mut j.0);
        for (i, s) in state.0.iter_mut().enumerate() {
            *s ^= j.0[i];
        }
        state
    }
    /// CTR from counter `first`, as GCM's 32-bit counter.
    fn xor(&self, nonce: &[u8; 12], first: u32, data: &mut [u8]) {
        if data.is_empty() || crate::arch::gcm_ctr(&self.aes.keys.0, self.aes.rounds, nonce, first, data) {
            return;
        }
        let mut stream = Secret([0; 16]);
        for (i, chunk) in data.chunks_mut(16).enumerate() {
            stream.0[..12].copy_from_slice(nonce);
            stream.0[12..].copy_from_slice(&first.wrapping_add(i as u32).to_be_bytes());
            self.aes.encrypt(&mut stream.0);
            for (b, &s) in chunk.iter_mut().zip(&stream.0) {
                *b ^= s;
            }
        }
    }
    pub(crate) fn seal(
        &self,
        nonce: &[u8; 12],
        aad: &[u8],
        data: &mut [u8],
    ) -> Result<[u8; 16], Error> {
        Self::check(aad.len(), data.len())?;
        if !self.aggregated {
            self.xor(nonce, 2, data);
            return Ok(self.tag(nonce, self.digest(aad, data)).0);
        }
        let keys = &self.aes.keys.0;
        if let Some(tag) = crate::arch::gcm_short(keys, self.aes.rounds, &self.powers.0, nonce, aad, data, None) {
            return Ok(tag);
        }
        Ok(self.tag(nonce, self.crypt(nonce, aad, data, false)).0)
    }
    /// CTR from counter 2 and GHASH over the AAD, the ciphertext and the
    /// lengths, stitched where an instruction path admits it: the
    /// ciphertext it writes when sealing, the ciphertext it reads when
    /// `open`.
    fn crypt(&self, nonce: &[u8; 12], aad: &[u8], data: &mut [u8], open: bool) -> Secret<16> {
        let mut state = Secret([0u8; 16]);
        self.hash(&mut state.0, aad);
        let keys = &self.aes.keys.0;
        let done = crate::arch::gcm_crypt(keys, self.aes.rounds, &self.powers.0, nonce, &mut state.0, data, open);
        let rest = &mut data[done..];
        // GCM bounded the length, so the block count fits the counter.
        let first = (done / 16) as u32 + 2;
        if open {
            self.hash(&mut state.0, rest);
            self.xor(nonce, first, rest);
        } else {
            self.xor(nonce, first, rest);
            self.hash(&mut state.0, rest);
        }
        self.hash_blocks(&mut state.0, &[Self::lengths(aad.len(), data.len())]);
        state
    }
    pub(crate) fn open(
        &self,
        nonce: &[u8; 12],
        aad: &[u8],
        data: &mut [u8],
        tag: &[u8],
    ) -> Result<(), Error> {
        Self::check(aad.len(), data.len())?;
        let authentic = if !self.aggregated {
            let authentic = crate::ct::equal(&self.tag(nonce, self.digest(aad, data)).0, tag);
            if authentic {
                self.xor(nonce, 2, data);
            }
            authentic
        } else {
            let mut stream = Secret([[0u8; 16]; 14]);
            let keys = &self.aes.keys.0;
            match crate::arch::gcm_short(keys, self.aes.rounds, &self.powers.0, nonce, aad, data, Some(&mut stream.0)) {
                Some(expected) => {
                    let authentic = crate::ct::equal(&Secret(expected).0, tag);
                    if authentic {
                        crate::block::xor(data, stream.0.as_flattened());
                    }
                    authentic
                }
                None => crate::ct::equal(&self.tag(nonce, self.crypt(nonce, aad, data, true)).0, tag),
            }
        };
        if !authentic {
            // The buffer may hold unauthenticated plaintext. `wipe`, not a
            // fill: on ToyOS a fill is a `memset` call.
            crate::secret::wipe(data);
            return Err(Error::Authentication);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn portable(h: u128, state: u128, blocks: &[[u8; 16]]) -> u128 {
        blocks.iter().fold(state, |y, x| multiply(y ^ u128::from_be_bytes(*x), h))
    }

    #[test]
    fn dispatched_ghash_matches_the_bitwise_multiply() {
        let gcm = Gcm::aes128(&[7; 16]);
        std::println!("GHASH aggregated: {}", gcm.aggregated);
        let h = u128::from_be_bytes(gcm.h.0);
        // Miri interprets every bit operation: a spread of each, not all.
        let stride = if cfg!(miri) { 37 } else { 1 };
        // Every basis product, through the one-block path.
        for i in (0..128).step_by(stride) {
            let mut state = (1u128 << i).to_be_bytes();
            gcm.hash_blocks(&mut state, &[[0; 16]]);
            assert_eq!(u128::from_be_bytes(state), multiply(1 << i, h), "x^{i}");
        }
        // Every count across the aggregated, batched and tail paths: up to
        // seven 16-block steps, so every way three to a reduction divides
        // them, and a tail.
        let blocks: [[u8; 16]; 128] = core::array::from_fn(|i| {
            (!h.wrapping_mul(i as u128 + 1)).rotate_left(i as u32).to_be_bytes()
        });
        for n in (0..=blocks.len()).step_by(if cfg!(miri) { 9 } else { 1 }) {
            let start = h.rotate_left(n as u32);
            let mut state = start.to_be_bytes();
            gcm.hash_blocks(&mut state, &blocks[..n]);
            assert_eq!(u128::from_be_bytes(state), portable(h, start, &blocks[..n]), "{n} blocks");
        }
        // Every bit of every block in a reduction's 48 meets its own power.
        for lane in (0..48).step_by(if cfg!(miri) { 23 } else { 1 }) {
            for bit in (0..128).step_by(stride) {
                let mut basis = [[0; 16]; 48];
                basis[lane][bit / 8] = 1 << (bit % 8);
                let mut state = [0; 16];
                gcm.hash_blocks(&mut state, &basis);
                assert_eq!(u128::from_be_bytes(state), portable(h, 0, &basis), "lane {lane} bit {bit}");
            }
        }
        assert_eq!(Gcm::check(0, usize::MAX), Err(Error::Length));
    }

    #[test]
    fn counters_continue_across_the_stitched_and_tail_paths() {
        for key in [&[3u8; 16][..], &[5u8; 32][..]] {
            let gcm = if key.len() == 16 {
                Gcm::aes128(key.try_into().unwrap())
            } else {
                Gcm::aes256(key.try_into().unwrap())
            };
            let nonce = [0xfe; 12];
            // Miri interprets each AES round: an empty message, a tail, and a
            // batch with a tail. Elsewhere, every batch and step count to
            // seven steps of 256 bytes with tails, and past the counter's
            // first carry out of its low byte, at block 254.
            let lengths: std::vec::Vec<usize> = if cfg!(miri) {
                std::vec![0, 17, 113]
            } else {
                let mut lengths = std::vec![0, 1, 15, 16, 17, 48, 49, 64, 65, 96, 97, 112, 113, 127, 128, 129, 192, 223, 224, 225];
                for steps in 1..=7 {
                    for tail in [0, 1, 16, 17, 128, 129, 255] {
                        lengths.push(256 * steps + tail - 1);
                        lengths.push(256 * steps + tail);
                    }
                }
                lengths.extend([4063, 4064, 4065, 4096 + 255, 16384 + 17]);
                lengths
            };
            for n in lengths {
                let input: std::vec::Vec<u8> = (0..n).map(|i| (i * 31 + 7) as u8).collect();
                let mut sealed = input.clone();
                let tag = gcm.seal(&nonce, b"aad", &mut sealed).unwrap();
                let mut stream = input.clone();
                for (i, chunk) in stream.chunks_mut(16).enumerate() {
                    let mut block = [0u8; 16];
                    block[..12].copy_from_slice(&nonce);
                    block[12..].copy_from_slice(&(i as u32 + 2).to_be_bytes());
                    gcm.aes.encrypt_portable(&mut block);
                    for (b, s) in chunk.iter_mut().zip(block) {
                        *b ^= s;
                    }
                }
                assert_eq!(sealed, stream, "{n} bytes, {}-byte key", key.len());
                let mut state = [0u8; 16];
                let h = u128::from_be_bytes(gcm.h.0);
                let mut padded = b"aad".to_vec();
                padded.resize(16, 0);
                let mut data = sealed.clone();
                data.resize(n.div_ceil(16) * 16, 0);
                let blocks: std::vec::Vec<[u8; 16]> =
                    padded.chunks(16).chain(data.chunks(16)).map(|c| c.try_into().unwrap()).collect();
                let lengths = ((3u128 * 8) << 64) | (n as u128 * 8);
                let y = multiply(portable(h, 0, &blocks) ^ lengths, h);
                state.copy_from_slice(&y.to_be_bytes());
                let mut j = [0u8; 16];
                j[..12].copy_from_slice(&nonce);
                j[15] = 1;
                gcm.aes.encrypt_portable(&mut j);
                for (s, m) in state.iter_mut().zip(j) {
                    *s ^= m;
                }
                assert_eq!(tag, state, "tag over {n} bytes");
                // A forged tag leaves no plaintext, however far the open got.
                let mut forged = tag;
                forged[n % 16] ^= 1;
                let mut refused = sealed.clone();
                assert_eq!(gcm.open(&nonce, b"aad", &mut refused, &forged), Err(Error::Authentication));
                assert!(refused.iter().all(|&b| b == 0), "a refusal of {n} bytes left data");
                gcm.open(&nonce, b"aad", &mut sealed, &tag).unwrap();
                assert_eq!(sealed, input);
            }
        }
    }
}
