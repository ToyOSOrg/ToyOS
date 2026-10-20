//! Partial 16-byte blocks as `u128` arithmetic, and whole ones moved and
//! XORed 16 bytes at a time. A variable-length copy or fill would call the C
//! library's `memcpy` or `memset`, a `rep movsb` or `rep stosb` on ToyOS,
//! whose start-up costs more than a short block's whole computation; a
//! per-byte assembly compiles to a branch per byte. Lengths are public and
//! the only thing branched on.

/// The low `len` bytes of a little-endian block, `len` at most 16.
#[inline]
fn low(len: usize) -> u128 {
    if len >= 16 { !0 } else { (1u128 << (8 * len)) - 1 }
}

/// Up to 16 bytes as the low bytes of a little-endian `u128`: two
/// overlapping loads of the widest size that fits.
#[inline]
fn word(bytes: &[u8]) -> u128 {
    let n = bytes.len();
    debug_assert!(n <= 16);
    let pair = |lo: u128, hi: u128, width: usize| lo | (hi >> (8 * (2 * width - n))) << (8 * width);
    if n == 16 {
        u128::from_le_bytes(bytes.try_into().unwrap())
    } else if n >= 8 {
        let lo = u64::from_le_bytes(bytes[..8].try_into().unwrap());
        let hi = u64::from_le_bytes(bytes[n - 8..].try_into().unwrap());
        pair(u128::from(lo), u128::from(hi), 8)
    } else if n >= 4 {
        let lo = u32::from_le_bytes(bytes[..4].try_into().unwrap());
        let hi = u32::from_le_bytes(bytes[n - 4..].try_into().unwrap());
        pair(u128::from(lo), u128::from(hi), 4)
    } else if n > 0 {
        u128::from(bytes[0]) | u128::from(bytes[n / 2]) << (8 * (n / 2)) | u128::from(bytes[n - 1]) << (8 * (n - 1))
    } else {
        0
    }
}

/// The low `bytes.len()` bytes of `v` into `bytes`, at most 16: two
/// overlapping stores of the widest size that fits, which agree where they
/// overlap.
#[inline]
fn write(bytes: &mut [u8], v: u128) {
    let n = bytes.len();
    debug_assert!(n <= 16);
    let b = v.to_le_bytes();
    if n == 16 {
        bytes.copy_from_slice(&b);
    } else if n >= 8 {
        bytes[..8].copy_from_slice(&b[..8]);
        let hi: [u8; 8] = b[n - 8..n].try_into().unwrap();
        bytes[n - 8..].copy_from_slice(&hi);
    } else if n >= 4 {
        bytes[..4].copy_from_slice(&b[..4]);
        let hi: [u8; 4] = b[n - 4..n].try_into().unwrap();
        bytes[n - 4..].copy_from_slice(&hi);
    } else if n > 0 {
        let (first, middle, last) = (b[0], b[n / 2], b[n - 1]);
        bytes[0] = first;
        bytes[n / 2] = middle;
        bytes[n - 1] = last;
    }
}

/// Up to 16 bytes, zero-padded.
#[inline]
pub(crate) fn padded(bytes: &[u8]) -> [u8; 16] {
    word(bytes).to_le_bytes()
}

/// `block` with `bytes` written from `at` on, `at + bytes.len()` at most 16.
#[inline]
pub(crate) fn place(block: &[u8; 16], at: usize, bytes: &[u8]) -> [u8; 16] {
    debug_assert!(at + bytes.len() <= 16);
    if at >= 16 {
        return *block;
    }
    let keep = u128::from_le_bytes(*block) & !(low(bytes.len()) << (8 * at));
    (keep | word(bytes) << (8 * at)).to_le_bytes()
}

/// The first `len` bytes of `block`, then `next`, then zeros.
#[inline]
pub(crate) fn terminated(block: &[u8; 16], len: usize, next: u8) -> [u8; 16] {
    let kept = u128::from_le_bytes(*block) & low(len);
    let next = if len < 16 { u128::from(next) << (8 * len) } else { 0 };
    (kept | next).to_le_bytes()
}

/// `src` into `dst` of the same length: whole 16-byte chunks, then the tail
/// merged into its block.
#[inline]
pub(crate) fn copy(dst: &mut [u8], src: &[u8]) {
    let (chunks, tail) = dst.as_chunks_mut::<16>();
    let (from, rest) = src.as_chunks::<16>();
    for (i, d) in chunks.iter_mut().enumerate() {
        *d = from[i];
    }
    write(tail, word(rest));
}

/// `data` XORed with the first `data.len()` bytes of `stream`.
#[inline]
pub(crate) fn xor(data: &mut [u8], stream: &[u8]) {
    let (blocks, tail) = data.as_chunks_mut::<16>();
    let (masks, rest) = stream[..16 * blocks.len()].as_chunks::<16>();
    debug_assert!(rest.is_empty());
    for i in 0..blocks.len() {
        blocks[i] = (u128::from_ne_bytes(blocks[i]) ^ u128::from_ne_bytes(masks[i])).to_ne_bytes();
    }
    let rest = &stream[16 * blocks.len()..][..tail.len()];
    write(tail, word(tail) ^ word(rest));
}

#[cfg(test)]
mod tests {
    #[test]
    fn partial_blocks_match_bytewise_assembly_for_every_length_and_offset() {
        let source: [u8; 16] = core::array::from_fn(|i| (i * 37 + 11) as u8);
        let block: [u8; 16] = core::array::from_fn(|i| (i * 59 + 3) as u8);
        for n in 0..=16 {
            let bytes = &source[..n];
            let expected: [u8; 16] = core::array::from_fn(|i| if i < n { source[i] } else { 0 });
            assert_eq!(super::padded(bytes), expected, "padded {n}");
            let next = 0x80;
            let expected: [u8; 16] = core::array::from_fn(|i| match i.cmp(&n) {
                core::cmp::Ordering::Less => block[i],
                core::cmp::Ordering::Equal => next,
                core::cmp::Ordering::Greater => 0,
            });
            assert_eq!(super::terminated(&block, n, next), expected, "terminated {n}");
            for at in 0..=16 - n {
                let expected: [u8; 16] = core::array::from_fn(|i| if i >= at && i < at + n { source[i - at] } else { block[i] });
                assert_eq!(super::place(&block, at, bytes), expected, "place {n} at {at}");
            }
            let mut out = [0xeeu8; 17];
            super::copy(&mut out[..n], bytes);
            assert_eq!(&out[..n], bytes, "copy {n}");
            assert_eq!(out[n], 0xee, "copy {n} writes nothing past its end");
        }
    }

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
