//! RFC 8439 ChaCha20; the original 64-bit nonce construction serves SSH.
//! Rounds and memory addresses are independent of key and message contents.
//! [`xor`] is the keystream alone, for formats that compose ChaCha20 their
//! own way; TLS and SSH use [`crate::aead`] and [`crate::ssh`].

use crate::{Error, poly1305::Poly1305, secret::Secret};

#[inline(always)]
fn quarter(x: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    x[a] = x[a].wrapping_add(x[b]);
    x[d] = (x[d] ^ x[a]).rotate_left(16);
    x[c] = x[c].wrapping_add(x[d]);
    x[b] = (x[b] ^ x[c]).rotate_left(12);
    x[a] = x[a].wrapping_add(x[b]);
    x[d] = (x[d] ^ x[a]).rotate_left(8);
    x[c] = x[c].wrapping_add(x[d]);
    x[b] = (x[b] ^ x[c]).rotate_left(7);
}

fn words(key: &[u8; 32], s: &mut [u32; 16]) {
    s[..4].copy_from_slice(&[0x61707865, 0x3320646e, 0x79622d32, 0x6b206574]);
    let (words, _) = key.as_chunks::<4>();
    for (i, word) in s[4..12].iter_mut().enumerate() {
        *word = u32::from_le_bytes(words[i]);
    }
}

fn rounds(state: &[u32; 16]) -> Secret<64> {
    let mut x = Secret(*state);
    for _ in 0..10 {
        quarter(&mut x.0, 0, 4, 8, 12);
        quarter(&mut x.0, 1, 5, 9, 13);
        quarter(&mut x.0, 2, 6, 10, 14);
        quarter(&mut x.0, 3, 7, 11, 15);
        quarter(&mut x.0, 0, 5, 10, 15);
        quarter(&mut x.0, 1, 6, 11, 12);
        quarter(&mut x.0, 2, 7, 8, 13);
        quarter(&mut x.0, 3, 4, 9, 14);
    }
    let mut out = Secret([0; 64]);
    for (i, out) in out.0.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        *out = x.0[i].wrapping_add(state[i]).to_le_bytes();
    }
    out
}

pub(crate) fn block(key: &[u8; 32], nonce: &[u8; 12], counter: u32) -> Secret<64> {
    let mut state = Secret([0; 16]);
    fill_state(&mut state.0, key, nonce, counter);
    rounds(&state.0)
}

pub(crate) fn legacy_block(key: &[u8; 32], nonce: &[u8; 8], counter: u64) -> Secret<64> {
    let mut state = Secret([0; 16]);
    fill_legacy_state(&mut state.0, key, nonce, counter);
    rounds(&state.0)
}

/// The original construction's state: a 64-bit counter and an 8-byte nonce.
fn fill_legacy_state(state: &mut [u32; 16], key: &[u8; 32], nonce: &[u8; 8], counter: u64) {
    words(key, state);
    state[12] = counter as u32;
    state[13] = (counter >> 32) as u32;
    state[14] = u32::from_le_bytes(nonce[..4].try_into().unwrap());
    state[15] = u32::from_le_bytes(nonce[4..].try_into().unwrap());
}

/// RFC 8439's state for block `counter`.
fn fill_state(state: &mut [u32; 16], key: &[u8; 32], nonce: &[u8; 12], counter: u32) {
    let (counter, nonce) = ietf(nonce, counter);
    fill_legacy_state(state, key, &nonce, counter);
}

/// The keystream of the first `n` of four lanes, at most four: lane i is
/// block `counters[i]` of `keys[i]` under the original construction's
/// `nonce`. Two or more run together where SIMD admits them, in about one
/// block's latency.
pub(crate) fn blocks(keys: [&[u8; 32]; 4], counters: [u64; 4], nonce: &[u8; 8], n: usize, out: &mut [u8; 256]) {
    if n >= 2 && crate::arch::chacha_blocks(keys, counters, nonce, n, out) {
        return;
    }
    let (chunks, _) = out.as_chunks_mut::<64>();
    for i in 0..n {
        chunks[i] = legacy_block(keys[i], nonce, counters[i]).0;
    }
}

/// Blocks `counter` to `counter + n - 1` of `key` under an RFC 8439
/// `nonce`, as [`blocks`] computes them; the caller bounded the counter.
pub(crate) fn ietf_blocks(key: &[u8; 32], nonce: &[u8; 12], counter: u32, n: usize, out: &mut [u8; 256]) {
    let (counter, nonce) = ietf(nonce, counter);
    let c = |i: u64| counter.wrapping_add(i);
    blocks([key; 4], [c(0), c(1), c(2), c(3)], &nonce, n, out);
}

/// IETF's 32-bit counter and first nonce word as the original 64-bit
/// counter; bounded input cannot carry into the nonce word.
fn ietf(nonce: &[u8; 12], counter: u32) -> (u64, [u8; 8]) {
    let word = u32::from_le_bytes(nonce[..4].try_into().unwrap());
    (u64::from(counter) | u64::from(word) << 32, nonce[4..].try_into().unwrap())
}

fn check_ietf(length: usize, counter: u32) -> Result<(), Error> {
    if length.div_ceil(64) as u64 > u64::from(u32::MAX) - u64::from(counter) + 1 {
        return Err(Error::Length);
    }
    Ok(())
}

fn check_legacy(length: usize, counter: u64) -> Result<(), Error> {
    match (length.div_ceil(64) as u64).checked_add(counter) {
        Some(_) => Ok(()),
        None => Err(Error::Length),
    }
}

/// The keystream from block `counter` on, XORed into `data`.
fn stream(key: &[u8; 32], counter: u64, nonce: &[u8; 8], data: &mut [u8]) {
    if data.is_empty() {
        return;
    }
    let done = crate::arch::chacha_xor(key, counter, nonce, data);
    for (i, chunk) in data[done..].chunks_mut(64).enumerate() {
        let stream = legacy_block(key, nonce, counter.wrapping_add((done / 64 + i) as u64));
        for (i, byte) in chunk.iter_mut().enumerate() {
            *byte ^= stream.0[i];
        }
    }
}

/// RFC 8439 §2.4: the keystream from block `counter` of (`key`, `nonce`)
/// XORed into `data`. `Length` when the blocks would pass 2^32 - 1.
pub fn xor(key: &[u8; 32], nonce: &[u8; 12], counter: u32, data: &mut [u8]) -> Result<(), Error> {
    let _protection = crate::timing::protect();
    check_ietf(data.len(), counter)?;
    let (counter, nonce) = ietf(nonce, counter);
    stream(key, counter, &nonce, data);
    Ok(())
}

/// RFC 8439 §2.8's ChaCha20 and Poly1305 in one pass from block 0 of (`key`,
/// `nonce`), where an instruction path runs: the MAC over the zero-padded
/// `aad` and ciphertext, ready for the length block. `data`'s length is
/// bounded as `crate::aead` bounds it.
pub(crate) fn aead(key: &[u8; 32], nonce: &[u8; 12], aad: &[u8], data: &mut [u8], seal: bool) -> Option<Poly1305> {
    let (counter, nonce) = ietf(nonce, 0);
    crate::arch::chacha20_poly1305(key, counter, &nonce, aad, data, seal, true)
}

/// OpenSSH's payload pass in one, where an instruction path runs: block 0 of
/// (`key`, `nonce`) keys the MAC over `header` and the payload's ciphertext,
/// and the payload is XORed with the blocks after it. A payload of at most
/// 2^32 - 1 bytes cannot carry the 64-bit counter.
pub(crate) fn openssh(key: &[u8; 32], nonce: &[u8; 8], header: &[u8; 4], payload: &mut [u8], seal: bool) -> Option<Poly1305> {
    crate::arch::chacha20_poly1305(key, 0, nonce, header, payload, seal, false)
}

pub(crate) fn legacy_xor(key: &[u8; 32], nonce: &[u8; 8], counter: u64, data: &mut [u8]) -> Result<(), Error> {
    check_legacy(data.len(), counter)?;
    stream(key, counter, nonce, data);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatched_streams_preserve_counter_carries_and_batch_tails() {
        let key = core::array::from_fn(|i| (i * 7 + 3) as u8);
        let nonce = [0xff; 12];
        for len in [0usize, 1, 63, 64, 65, 255, 256, 257, 383, 384, 385, 511, 512, 513, 640, 767, 768, 1025, 1152, 1792, 2111] {
            let blocks = len.div_ceil(64).max(1);
            let counter = u32::MAX - blocks as u32 + 1;
            let mut actual = [0xa5; 2111];
            let mut expected = actual;
            for (i, chunk) in expected[..len].chunks_mut(64).enumerate() {
                let stream = block(&key, &nonce, counter + i as u32);
                for (byte, mask) in chunk.iter_mut().zip(stream.0) {
                    *byte ^= mask;
                }
            }
            xor(&key, &nonce, counter, &mut actual[..len]).unwrap();
            assert_eq!(actual, expected, "IETF length {len}");

            for counter in [u64::from(u32::MAX) - 1, u64::MAX - blocks as u64] {
                let mut actual = [0xa5; 2111];
                let mut expected = actual;
                let nonce = &nonce[4..].try_into().unwrap();
                for (i, chunk) in expected[..len].chunks_mut(64).enumerate() {
                    let stream = legacy_block(&key, nonce, counter + i as u64);
                    for (byte, mask) in chunk.iter_mut().zip(stream.0) {
                        *byte ^= mask;
                    }
                }
                legacy_xor(&key, nonce, counter, &mut actual[..len]).unwrap();
                assert_eq!(actual, expected, "legacy length {len}, counter {counter}");
            }
        }
    }

    #[test]
    fn lanes_match_one_block_at_a_time() {
        // Three keys and scattered counters, word carries included, as SSH's
        // packets take them; every key byte and counter word differs per lane.
        let keys: [[u8; 32]; 4] = core::array::from_fn(|lane| core::array::from_fn(|i| (lane * 89 + i * 7 + 1) as u8));
        let keys = [&keys[0], &keys[1], &keys[1], &keys[3]];
        let counters = [0, u64::from(u32::MAX), u64::MAX, 0x0123_4567_89ab_cdef];
        let nonce = [0x10, 0x32, 0x54, 0x76, 0x98, 0xba, 0xdc, 0xfe];
        for n in 1..=4 {
            let mut together = Secret([0xa5u8; 256]);
            blocks(keys, counters, &nonce, n, &mut together.0);
            for i in 0..n {
                assert_eq!(together.0[64 * i..64 * i + 64], legacy_block(keys[i], &nonce, counters[i]).0, "lane {i} of {n}");
            }
            assert!(together.0[64 * n..].iter().all(|&b| b == 0xa5), "{n} lanes write only their blocks");
        }
    }

    #[test]
    fn exhausted_counter_refuses_before_mutating_any_batch() {
        let mut data = [0xa5; 257];
        assert_eq!(
            xor(&[3; 32], &[5; 12], u32::MAX - 3, &mut data),
            Err(Error::Length)
        );
        assert_eq!(data, [0xa5; 257]);
        assert_eq!(
            legacy_xor(&[3; 32], &[5; 8], u64::MAX - 3, &mut data),
            Err(Error::Length)
        );
        assert_eq!(data, [0xa5; 257]);
    }
}
