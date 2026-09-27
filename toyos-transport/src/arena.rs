//! A region's arena, and who may touch which run of it.
//!
//! **A run exists only once it is bounded by the [`Geometry`]**, and it
//! travels as one of three tokens, none of them `Clone`: [`Own`] on the side
//! that allocated it, which lending consumes into [`Lent`] until a completion
//! naming it hands it back; and [`Held`] on the side that decoded it from an
//! entry ([`Run::decode`]). The adapter reaches a run's bytes only through a
//! token's [`Run::span`].

use crate::{Span, Untrusted, Violation};

/// What a schema's region is: none, or a header page and an arena of slots of
/// the schema's size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Layout {
    /// Entries travel as frames on the connection; a region sent is refused.
    Inline,
    Region { slot_bytes: u32 },
}

/// A region's arena, decoded once.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Geometry {
    slot_bytes: u32,
    slots: u32,
}

impl Geometry {
    /// Every region is this long: the granule shared memory comes in.
    pub const BYTES: u64 = 0x20_0000;
    /// The header page the queues and cursors are on; the arena follows it.
    pub const HEADER_BYTES: u32 = 0x1000;
    const ARENA_BYTES: u32 = 0x20_0000 - Self::HEADER_BYTES;

    /// The arena of a region `bytes` long, cut as `layout` says.
    pub fn decode(bytes: Untrusted<u64>, layout: Layout) -> Result<Self, Violation> {
        let Layout::Region { slot_bytes } = layout else { return Err(Violation::Region) };
        bytes.exactly(Self::BYTES).map_err(|_| Violation::Region)?;
        let slots = Self::ARENA_BYTES.checked_div(slot_bytes).filter(|&n| n > 0).ok_or(Violation::Region)?;
        Ok(Self { slot_bytes, slots })
    }

    pub fn slots(&self) -> u32 {
        self.slots
    }

    /// Slots `first..first + count`, for the allocator that hands them out
    /// once each to own; `None` for none or past the arena.
    pub fn own(&self, first: u32, count: u32) -> Option<Own> {
        self.run(first, count).map(Own)
    }

    fn run(&self, first: u32, count: u32) -> Option<Run> {
        if count == 0 || first.checked_add(count)? > self.slots {
            return None;
        }
        let offset = first.checked_mul(self.slot_bytes)?.checked_add(Self::HEADER_BYTES)?;
        let len = count.checked_mul(self.slot_bytes)?;
        Some(Run { first, count, span: Span { offset: offset.try_into().ok()?, len: len.try_into().ok()? } })
    }
}

/// Slots of the arena, bounded by its geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Run {
    first: u32,
    count: u32,
    span: Span,
}

impl Run {
    /// The run an entry's two words name, held until its answer.
    pub fn decode(first: Untrusted<u32>, count: Untrusted<u32>, geometry: &Geometry) -> Result<Held, Violation> {
        let bounded = |word: Untrusted<u32>| word.at_most(u64::from(geometry.slots)).ok()?.try_into().ok();
        bounded(first)
            .zip(bounded(count))
            .and_then(|(first, count)| geometry.run(first, count))
            .map(Held)
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

/// A run this side allocated and has not lent.
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct Own(Run);

impl Own {
    pub fn run(&self) -> &Run {
        &self.0
    }

    /// Put it in an entry: the run is the peer's until its answer.
    pub fn lend(self) -> (Lent, Run) {
        (Lent(self.0), self.0)
    }
}

/// A run in an entry the peer has not answered.
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct Lent(Run);

impl Lent {
    /// The completion that names it came back: it is this side's again.
    pub fn back(self) -> Own {
        Own(self.0)
    }
}

/// A run the peer lent, bounded, until this side answers the entry that named
/// it.
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct Held(Run);

impl Held {
    pub fn run(&self) -> &Run {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGES: Layout = Layout::Region { slot_bytes: 4096 };

    fn geometry() -> Geometry {
        Geometry::decode(Untrusted::new(Geometry::BYTES), PAGES).unwrap()
    }

    #[test]
    fn a_region_is_the_granule_and_a_schema_with_one() {
        assert_eq!(geometry().slots(), 511);
        for bytes in [0, Geometry::BYTES - 1, Geometry::BYTES + 1, 2 * Geometry::BYTES] {
            assert_eq!(Geometry::decode(Untrusted::new(bytes), PAGES), Err(Violation::Region));
        }
        assert_eq!(Geometry::decode(Untrusted::new(Geometry::BYTES), Layout::Inline), Err(Violation::Region));
        assert_eq!(
            Geometry::decode(Untrusted::new(Geometry::BYTES), Layout::Region { slot_bytes: 0 }),
            Err(Violation::Region)
        );
    }

    /// Every run a peer can name is inside the arena, whole slots of it; the
    /// last slot is a run and one past it is not, however the sum wraps.
    #[test]
    fn a_run_a_peer_names_is_inside_the_arena() {
        let g = geometry();
        let held = Run::decode(Untrusted::new(510), Untrusted::new(1), &g).unwrap();
        assert_eq!(held.run().span(), Span { offset: 0x1000 + 510 * 4096, len: 4096 });
        for (first, count) in [(510, 2), (0, 0), (511, 1), (1, u32::MAX), (u32::MAX, 1), (0, 512)] {
            assert_eq!(
                Run::decode(Untrusted::new(first), Untrusted::new(count), &g),
                Err(Violation::Run),
                "{first}+{count}"
            );
        }
    }

    #[test]
    fn lending_consumes_and_the_answer_gives_back() {
        let own = geometry().own(3, 2).unwrap();
        let (lent, run) = own.lend();
        assert_eq!((run.first(), run.count()), (3, 2));
        assert_eq!(lent.back().run(), &run);
        assert_eq!(geometry().own(510, 2), None);
    }
}
