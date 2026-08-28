//! A PCI capability list, walked so it always ends.
//!
//! The device supplies each "next" pointer, so a malformed or cyclic one ends
//! the walk rather than running off the window or forever. PCI spec §6.7: a
//! pointer is a dword-aligned byte offset, above the 64-byte standard header.

/// One past the standard configuration header; capabilities live at or above it.
pub const FIRST_CAP: u8 = 0x40;

/// A walk of a capability list that visits each capability at most once.
#[derive(Debug, Default)]
pub struct CapWalk {
    seen: [u64; 4],
}

impl CapWalk {
    pub const fn new() -> Self {
        Self { seen: [0; 4] }
    }

    /// The next capability's offset, or `None` to end the walk: the terminator
    /// (0), a pointer the spec forbids, or one already visited (a cycle).
    pub fn step(&mut self, raw: u8) -> Option<u8> {
        if raw == 0 || raw < FIRST_CAP || raw & 0x3 != 0 {
            return None;
        }
        let (word, bit) = ((raw >> 6) as usize, 1u64 << (raw & 0x3F));
        if self.seen[word] & bit != 0 {
            return None;
        }
        self.seen[word] |= bit;
        Some(raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_terminator_ends_the_walk() {
        assert_eq!(CapWalk::new().step(0), None);
    }

    /// PCI spec §6.7: a capability pointer is dword-aligned.
    #[test]
    fn a_pointer_that_is_not_dword_aligned_is_refused() {
        for raw in [0x41u8, 0x42, 0x43, 0x4F, 0xFD, 0xFE, 0xFF] {
            assert_eq!(CapWalk::new().step(raw), None, "{raw:#x}");
        }
    }

    /// Capabilities live above the 64-byte standard header.
    #[test]
    fn a_pointer_below_the_standard_header_is_refused() {
        for raw in [0x04u8, 0x20, 0x3C] {
            assert_eq!(CapWalk::new().step(raw), None, "{raw:#x}");
        }
        assert_eq!(CapWalk::new().step(FIRST_CAP), Some(FIRST_CAP));
    }

    #[test]
    fn a_forward_chain_is_followed_to_its_end() {
        let mut w = CapWalk::new();
        assert_eq!(w.step(0x40), Some(0x40));
        assert_eq!(w.step(0x50), Some(0x50));
        assert_eq!(w.step(0xF8), Some(0xF8));
        assert_eq!(w.step(0), None);
    }

    /// A visited set, not an "increasing" test: a list may be laid out out of
    /// order, but a pointer back to a link already taken is a cycle and ends it.
    #[test]
    fn a_pointer_to_a_visited_link_ends_the_walk() {
        let mut w = CapWalk::new();
        assert_eq!(w.step(0x40), Some(0x40));
        assert_eq!(w.step(0x40), None);

        let mut w = CapWalk::new();
        assert_eq!(w.step(0x60), Some(0x60));
        assert_eq!(w.step(0x50), Some(0x50));
        assert_eq!(w.step(0x60), None);
    }

    #[test]
    fn the_walk_is_bounded_by_the_distinct_offsets_it_can_hold() {
        let mut w = CapWalk::new();
        let mut visited = 0;
        let mut raw = FIRST_CAP;
        while w.step(raw).is_some() {
            visited += 1;
            match raw.checked_add(4) {
                Some(next) => raw = next,
                None => break,
            }
        }
        assert_eq!(visited, (0x100 - FIRST_CAP as usize) / 4);
        assert_eq!(w.step(FIRST_CAP), None);
    }
}
