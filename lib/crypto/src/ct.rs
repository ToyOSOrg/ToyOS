//! Content-independent comparisons and selection. Lengths are public.
//!
//! The register barrier prevents LLVM from retaining a boolean's provenance,
//! which would let it turn a masked select into a branch; deployed machine
//! code must still be inspected and tested for timing leakage.

#[inline(always)]
pub(crate) fn opaque(value: u64) -> u64 {
    crate::arch::value_barrier(value)
}

#[inline(always)]
pub(crate) fn mask(bit: u64) -> u64 {
    0u64.wrapping_sub(opaque(bit & 1))
}

#[inline(always)]
pub(crate) fn select(a: u64, b: u64, bit: u64) -> u64 {
    let m = mask(bit);
    (a & !m) | (b & m)
}

#[inline(always)]
pub(crate) fn nonzero(value: u64) -> u64 {
    (value | value.wrapping_neg()) >> 63
}

pub(crate) fn equal(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut difference = 0u64;
    for (i, &a) in a.iter().enumerate() {
        difference |= u64::from(a ^ b[i]);
    }
    opaque(nonzero(difference)) == 0
}
