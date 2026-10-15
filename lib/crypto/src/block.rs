//! Partial 16-byte blocks assembled byte by byte over a fixed 16 indices.
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
