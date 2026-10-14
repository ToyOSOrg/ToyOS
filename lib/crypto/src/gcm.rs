//! SP 800-38D GCM, restricted to the 96-bit nonces TLS uses. GHASH is a fixed
//! 128-step portable multiply or a CLMUL path, without data-indexed tables.
//! Content does not affect addresses or branches. Authentication precedes
//! plaintext release.

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
    hardware: bool,
}
impl Gcm {
    /// Zero storage for provider construction; initialize before any operation.
    #[cfg(feature = "rustls-provider")]
    pub(crate) fn storage(key_len: usize) -> Self {
        Self {
            aes: Aes::storage(key_len),
            h: Secret([0; 16]),
            hardware: crate::arch::clmul_available(),
        }
    }
    #[cfg(feature = "rustls-provider")]
    pub(crate) fn initialize(&mut self, key: &[u8]) {
        self.aes.initialize(key);
        self.h.0.fill(0);
        self.aes.encrypt(&mut self.h.0);
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
        Self {
            aes,
            h,
            hardware: crate::arch::clmul_available(),
        }
    }
    fn multiply(&self, a: u128, b: u128) -> u128 {
        if self.hardware {
            crate::arch::ghash(a, b)
        } else {
            multiply(a, b)
        }
    }
    fn check(aad: usize, data: usize) -> Result<(), Error> {
        if data as u128 > (1u128 << 36) - 32 || aad as u128 > u64::MAX as u128 / 8 {
            Err(Error::Length)
        } else {
            Ok(())
        }
    }
    fn tag(&self, nonce: &[u8; 12], aad: &[u8], data: &[u8]) -> Secret<16> {
        let mut state = Secret([0u8; 16]);
        for input in [aad, data] {
            let (blocks, tail) = input.as_chunks::<16>();
            let remaining = if self.hardware {
                crate::arch::ghash_blocks(&self.h.0, &mut state.0, blocks);
                tail
            } else {
                input
            };
            for chunk in remaining.chunks(16) {
                for (s, &b) in state.0.iter_mut().zip(chunk) {
                    *s ^= b;
                }
                state.0 = self
                    .multiply(u128::from_be_bytes(state.0), u128::from_be_bytes(self.h.0))
                    .to_be_bytes();
            }
        }
        let lengths = ((aad.len() as u128 * 8) << 64) | (data.len() as u128 * 8);
        state.0 = self
            .multiply(
                u128::from_be_bytes(state.0) ^ lengths,
                u128::from_be_bytes(self.h.0),
            )
            .to_be_bytes();
        let mut j = Secret([0u8; 16]);
        j.0[..12].copy_from_slice(nonce);
        j.0[15] = 1;
        self.aes.encrypt(&mut j.0);
        for (s, mask) in state.0.iter_mut().zip(&j.0) {
            *s ^= mask;
        }
        state
    }
    fn xor(&self, nonce: &[u8; 12], data: &mut [u8]) {
        if self.aes.xor_ctr(nonce, data) {
            return;
        }
        let mut stream = Secret([0; 16]);
        stream.0[..12].copy_from_slice(nonce);
        for (i, chunk) in data.chunks_mut(16).enumerate() {
            stream.0[..12].copy_from_slice(nonce);
            stream.0[12..].copy_from_slice(&(i as u32 + 2).to_be_bytes());
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
        self.xor(nonce, data);
        Ok(self.tag(nonce, aad, data).0)
    }
    pub(crate) fn open(
        &self,
        nonce: &[u8; 12],
        aad: &[u8],
        data: &mut [u8],
        tag: &[u8],
    ) -> Result<(), Error> {
        Self::check(aad.len(), data.len())?;
        let expected = self.tag(nonce, aad, data);
        if !crate::ct::equal(&expected.0, tag) {
            return Err(Error::Authentication);
        }
        self.xor(nonce, data);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clmul_matches_portable_field_and_reports_dispatch() {
        let admitted = crate::arch::clmul_available();
        std::println!("GHASH acceleration admitted: {admitted}");
        if admitted {
            for i in 0..256 {
                let a = (i as u128).wrapping_mul(0x9e3779b97f4a7c15fedcba9876543210);
                let b = !a.rotate_left(i % 128);
                assert_eq!(crate::arch::ghash(a, b), multiply(a, b));
            }
        }
        assert_eq!(Gcm::check(0, usize::MAX), Err(Error::Length));
    }

    #[test]
    fn accelerated_ghash_preserves_polynomial_basis_and_stream_recurrence() {
        if !crate::arch::clmul_available() {
            return;
        }
        for i in 0..128 {
            for j in 0..128 {
                let a = 1u128 << i;
                let b = 1u128 << j;
                let mut state = a.to_be_bytes();
                crate::arch::ghash_blocks(&b.to_be_bytes(), &mut state, &[[0; 16]]);
                assert_eq!(u128::from_be_bytes(state), multiply(a, b));
            }
        }
        let h = 0x0123456789abcdef123456789abcdef0u128;
        let blocks: [[u8; 16]; 19] = core::array::from_fn(|i| {
            (!h.wrapping_mul(i as u128 + 1))
                .rotate_left(i as u32)
                .to_be_bytes()
        });
        for n in 0..=blocks.len() {
            let mut state = h.to_be_bytes();
            crate::arch::ghash_blocks(&h.to_be_bytes(), &mut state, &blocks[..n]);
            let expected = blocks[..n]
                .iter()
                .fold(h, |y, x| multiply(y ^ u128::from_be_bytes(*x), h));
            assert_eq!(u128::from_be_bytes(state), expected);
        }
    }
}

/// Opt-in internal component measurements; no benchmark API in production.
#[cfg(test)]
mod component_bench {
    use super::*;
    use std::{hint::black_box, time::Instant};

    #[test]
    #[ignore = "manual release-mode component benchmark"]
    fn components() {
        assert!(!cfg!(debug_assertions), "use --release");
        std::println!("component,algorithm,size,sample,iterations,ns_per_op");
        for bits in [128, 256] {
            let key = if bits == 128 {
                Gcm::aes128(&[7; 16])
            } else {
                Gcm::aes256(&[7; 32])
            };
            for size in [0, 64, 128, 256, 1024, 16384] {
                let count = 2000.min((16 * 1024 * 1024 / size.max(1)).max(1));
                let mut data = std::vec![0x5a; size];
                // Synthetic public test key only. Each operation gets a unique
                // nonce within its sample; these bytes are never used as traffic.
                for sample in 0..=7 {
                    for turn in 0..4 {
                        let op = (sample + turn) % 4;
                        if op == 3 && size != 0 {
                            continue;
                        }
                        let started = Instant::now();
                        for i in 0..count {
                            let mut nonce = [0; 12];
                            nonce[..4].copy_from_slice(&(sample as u32).to_be_bytes());
                            nonce[4..8].copy_from_slice(&(op as u32).to_be_bytes());
                            nonce[8..].copy_from_slice(&(i as u32).to_be_bytes());
                            match op {
                                0 => key.xor(black_box(&nonce), black_box(&mut data)),
                                1 => {
                                    black_box(key.tag(
                                        black_box(&nonce),
                                        black_box(b"benchmark"),
                                        black_box(&data),
                                    ));
                                }
                                2 => {
                                    black_box(
                                        key.seal(
                                            black_box(&nonce),
                                            black_box(b"benchmark"),
                                            black_box(&mut data),
                                        )
                                        .unwrap(),
                                    );
                                }
                                _ => {
                                    if bits == 128 {
                                        black_box(Gcm::aes128(black_box(&[7; 16])));
                                    } else {
                                        black_box(Gcm::aes256(black_box(&[7; 32])));
                                    }
                                }
                            }
                        }
                        let ns = started.elapsed().as_nanos() as f64 / count as f64;
                        if sample != 0 {
                            std::println!(
                                "{},AES-{}-GCM,{},{},{},{:.6}",
                                ["ctr", "tag", "seal", "key-setup"][op],
                                bits,
                                size,
                                sample,
                                count,
                                ns
                            );
                        }
                    }
                }
            }
        }
    }
}
