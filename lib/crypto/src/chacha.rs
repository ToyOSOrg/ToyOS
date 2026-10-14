//! RFC 8439 ChaCha20; the original 64-bit nonce construction serves SSH.
//! Rounds and memory addresses are independent of key and message contents.

use crate::{
    Error,
    secret::{Secret, SecretWords32},
};

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
    for (word, bytes) in s[4..12].iter_mut().zip(key.as_chunks::<4>().0) {
        *word = u32::from_le_bytes(*bytes);
    }
}

fn rounds(state: &[u32; 16]) -> Secret<64> {
    let mut x = SecretWords32(*state);
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
    for ((out, &word), &initial) in out.0.as_chunks_mut::<4>().0.iter_mut().zip(&x.0).zip(state) {
        *out = word.wrapping_add(initial).to_le_bytes();
    }
    out
}

pub(crate) fn block(key: &[u8; 32], nonce: &[u8; 12], counter: u32) -> Secret<64> {
    let mut state = SecretWords32([0; 16]);
    words(key, &mut state.0);
    state.0[12] = counter;
    for i in 0..3 {
        state.0[13 + i] = u32::from_le_bytes(nonce[4 * i..4 * i + 4].try_into().unwrap());
    }
    rounds(&state.0)
}

pub(crate) fn legacy_block(key: &[u8; 32], nonce: &[u8; 8], counter: u64) -> Secret<64> {
    let mut state = SecretWords32([0; 16]);
    words(key, &mut state.0);
    state.0[12] = counter as u32;
    state.0[13] = (counter >> 32) as u32;
    state.0[14] = u32::from_le_bytes(nonce[..4].try_into().unwrap());
    state.0[15] = u32::from_le_bytes(nonce[4..].try_into().unwrap());
    rounds(&state.0)
}

pub(crate) fn xor(
    key: &[u8; 32],
    nonce: &[u8; 12],
    counter: u32,
    data: &mut [u8],
) -> Result<(), Error> {
    let count = data.len().div_ceil(64) as u64;
    if count > u64::from(u32::MAX) - u64::from(counter) + 1 {
        return Err(Error::Length);
    }
    let extended =
        u64::from(counter) | (u64::from(u32::from_le_bytes(nonce[..4].try_into().unwrap())) << 32);
    let done = crate::arch::chacha_xor(key, extended, nonce[4..].try_into().unwrap(), data);
    for (i, chunk) in data[done..].chunks_mut(64).enumerate() {
        let stream = block(key, nonce, counter + (done / 64 + i) as u32);
        for (byte, &mask) in chunk.iter_mut().zip(&stream.0) {
            *byte ^= mask;
        }
    }
    Ok(())
}

pub(crate) fn legacy_xor(
    key: &[u8; 32],
    nonce: &[u8; 8],
    counter: u64,
    data: &mut [u8],
) -> Result<(), Error> {
    if (data.len().div_ceil(64) as u64)
        .checked_add(counter)
        .is_none()
    {
        return Err(Error::Length);
    }
    let done = crate::arch::chacha_xor(key, counter, nonce, data);
    for (i, chunk) in data[done..].chunks_mut(64).enumerate() {
        let stream = legacy_block(key, nonce, counter + (done / 64 + i) as u64);
        for (byte, &mask) in chunk.iter_mut().zip(&stream.0) {
            *byte ^= mask;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatched_streams_preserve_counter_carries_and_batch_tails() {
        let key = core::array::from_fn(|i| (i * 7 + 3) as u8);
        let nonce = [0xff; 12];
        for len in [0usize, 1, 63, 64, 65, 255, 256, 257, 511, 512, 513, 1025] {
            let blocks = len.div_ceil(64).max(1);
            let counter = u32::MAX - blocks as u32 + 1;
            let mut actual = [0xa5; 1025];
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
                let mut actual = [0xa5; 1025];
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
