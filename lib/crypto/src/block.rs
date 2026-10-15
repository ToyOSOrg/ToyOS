//! Partial 16-byte blocks assembled byte by byte over a fixed 16 indices, and
//! whole ones XORed 16 bytes at a time.
//! A variable-length copy or fill would call the C library's `memcpy` or
//! `memset`, a `rep movsb` or `rep stosb` on ToyOS, whose start-up costs
//! more than a short block's whole computation. Lengths are public.

/// Up to 16 bytes, zero-padded.
#[inline]
pub(crate) fn padded(bytes: &[u8]) -> [u8; 16] {
    core::array::from_fn(|i| bytes.get(i).copied().unwrap_or(0))
}

/// `block` with `bytes` written from `at` on, `at + bytes.len()` at most 16.
#[inline]
pub(crate) fn place(block: &[u8; 16], at: usize, bytes: &[u8]) -> [u8; 16] {
    core::array::from_fn(|i| if i >= at { bytes.get(i - at).copied().unwrap_or(block[i]) } else { block[i] })
}

/// The first `len` bytes of `block`, then `next`, then zeros.
#[inline]
pub(crate) fn terminated(block: &[u8; 16], len: usize, next: u8) -> [u8; 16] {
    core::array::from_fn(|i| match i.cmp(&len) {
        core::cmp::Ordering::Less => block[i],
        core::cmp::Ordering::Equal => next,
        core::cmp::Ordering::Greater => 0,
    })
}

/// `src` into `dst` of the same length: whole 16-byte chunks, then a tail of
/// up to 15 bytes over a fixed 16 conditional stores.
#[inline]
pub(crate) fn copy(dst: &mut [u8], src: &[u8]) {
    let (chunks, tail) = dst.as_chunks_mut::<16>();
    let (from, rest) = src.as_chunks::<16>();
    for (i, d) in chunks.iter_mut().enumerate() {
        *d = from[i];
    }
    let rest = padded(rest);
    for (i, byte) in rest.iter().enumerate() {
        if let Some(d) = tail.get_mut(i) {
            *d = *byte;
        }
    }
}

/// `data` XORed with the first `data.len()` bytes of `stream`, 16 bytes at a
/// time and the tail byte by byte.
#[inline]
pub(crate) fn xor(data: &mut [u8], stream: &[u8]) {
    let (blocks, tail) = data.as_chunks_mut::<16>();
    let (masks, rest) = stream[..16 * blocks.len()].as_chunks::<16>();
    debug_assert!(rest.is_empty());
    for i in 0..blocks.len() {
        blocks[i] = (u128::from_ne_bytes(blocks[i]) ^ u128::from_ne_bytes(masks[i])).to_ne_bytes();
    }
    let rest = &stream[16 * blocks.len()..][..tail.len()];
    for i in 0..tail.len() {
        tail[i] ^= rest[i];
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn xor_takes_the_stream_from_its_start_for_every_length() {
        let stream: [u8; 256] = core::array::from_fn(|i| (i * 7 + 1) as u8);
        for len in 0..=200 {
            let mut data: std::vec::Vec<u8> = (0..len).map(|i| (i * 13) as u8).collect();
            let expected: std::vec::Vec<u8> = data.iter().zip(stream).map(|(d, s)| d ^ s).collect();
            super::xor(&mut data, &stream);
            assert_eq!(data, expected, "{len} bytes");
        }
    }
}
