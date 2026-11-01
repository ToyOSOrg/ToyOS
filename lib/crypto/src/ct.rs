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

/// Whether `a` and `b` hold the same bytes, without branching or indexing on their contents:
/// compare a received authenticator with the expected one through this.
pub fn equal(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut difference = 0u64;
    for (i, &a) in a.iter().enumerate() {
        difference |= u64::from(a ^ b[i]);
    }
    opaque(nonzero(difference)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_selection_and_equality_at_their_edges() {
        for (bit, m) in [(0, 0), (1, u64::MAX), (2, 0), (3, u64::MAX)] {
            assert_eq!(mask(bit), m);
        }
        for (a, b) in [(0, u64::MAX), (0x0123_4567_89ab_cdef, 0xfedc_ba98_7654_3210)] {
            assert_eq!((select(a, b, 0), select(a, b, 1)), (a, b));
        }
        for value in [0, 1, 1 << 63, u64::MAX] {
            assert_eq!(nonzero(value), u64::from(value != 0));
            assert_eq!(opaque(value), value);
        }
        assert!(equal(&[], &[]));
        assert!(!equal(&[0], &[]));
        let a = [0x5au8; 33];
        for i in 0..a.len() {
            let mut b = a;
            b[i] ^= 0x80;
            assert!(!equal(&a, &b), "a difference at byte {i}");
        }
        assert!(equal(&a, &[0x5a; 33]));
    }
}
