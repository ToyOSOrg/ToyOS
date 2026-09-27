//! A region's arena, cut into slots, and the runs of it an entry names.
//!
//! **A run exists only once it is bounded by the [`Geometry`]**: a peer's is
//! decoded ([`Run::decode`]) and this side's is cut ([`Geometry::run`]). The
//! adapter reaches a run's bytes only through its [`Span`].

use core::fmt;

use crate::{Untrusted, Violation};

/// Bytes `offset..offset + len` of the region.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Span {
    pub offset: usize,
    pub len: usize,
}

/// A region's arena: whole slots after its header page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Geometry {
    slot_bytes: u32,
    slots: u32,
}

impl Geometry {
    /// Every region is this long: the granule shared memory comes in.
    pub const BYTES: u32 = 0x20_0000;
    /// The header page the queues and cursors are on; the arena follows it.
    pub const HEADER_BYTES: u32 = 0x1000;
    const ARENA_BYTES: u32 = Self::BYTES - Self::HEADER_BYTES;

    /// The arena cut into slots of `slot_bytes`; `None` if not one fits.
    pub const fn new(slot_bytes: u32) -> Option<Self> {
        match Self::ARENA_BYTES.checked_div(slot_bytes) {
            Some(slots) if slots > 0 => Some(Self { slot_bytes, slots }),
            _ => None,
        }
    }

    pub const fn slots(&self) -> u32 {
        self.slots
    }

    /// Slots `first..first + count`; `None` for none, or past the arena.
    pub fn run(&self, first: u32, count: u32) -> Option<Run> {
        if count == 0 || first.checked_add(count)? > self.slots {
            return None;
        }
        let offset = first.checked_mul(self.slot_bytes)?.checked_add(Self::HEADER_BYTES)?;
        let len = count.checked_mul(self.slot_bytes)?;
        Some(Run { first, count, span: Span { offset: offset.try_into().ok()?, len: len.try_into().ok()? } })
    }
}

/// Slots of the arena, bounded by its geometry.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Run {
    first: u32,
    count: u32,
    span: Span,
}

/// Its slots alone: the span follows from them.
impl fmt::Debug for Run {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Run").field("first", &self.first).field("count", &self.count).finish()
    }
}

impl Run {
    /// The run a peer's two words name.
    pub fn decode(first: Untrusted<u32>, count: Untrusted<u32>, geometry: &Geometry) -> Result<Self, Violation> {
        let bounded = |word: Untrusted<u32>| word.at_most(u64::from(geometry.slots)).ok()?.try_into().ok();
        bounded(first)
            .zip(bounded(count))
            .and_then(|(first, count)| geometry.run(first, count))
            .ok_or(Violation::Run)
    }

    pub fn first(&self) -> u32 {
        self.first
    }

    pub fn count(&self) -> u32 {
        self.count
    }

    /// Its bytes in the region.
    pub fn span(&self) -> Span {
        self.span
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGES: Geometry = Geometry::new(4096).unwrap();

    #[test]
    fn an_arena_is_whole_slots_after_the_header() {
        assert_eq!(PAGES.slots(), 511);
        assert_eq!(Geometry::new(0), None);
        assert_eq!(Geometry::new(Geometry::BYTES), None, "a slot longer than the arena");
    }

    /// Every run a peer can name is inside the arena, whole slots of it; the
    /// last slot is a run and one past it is not, however the sum wraps.
    #[test]
    fn a_run_a_peer_names_is_inside_the_arena() {
        let run = Run::decode(Untrusted::new(510), Untrusted::new(1), &PAGES).unwrap();
        assert_eq!(run.span(), Span { offset: 0x1000 + 510 * 4096, len: 4096 });
        for (first, count) in [(510, 2), (0, 0), (511, 1), (1, u32::MAX), (u32::MAX, 1), (0, 512)] {
            assert_eq!(
                Run::decode(Untrusted::new(first), Untrusted::new(count), &PAGES),
                Err(Violation::Run),
                "{first}+{count}"
            );
        }
    }
}
