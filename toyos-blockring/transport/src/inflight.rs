//! The requests a client has on the wire, by tag.
//!
//! **Each tag is answered exactly once**: by the completion that names it
//! ([`Inflight::answer`]) or, when the session ends, by [`Inflight::end`] —
//! never by both, and never by a completion naming a tag from before its
//! slot was last filled. A tag is the slot's index under the slot's own
//! sequence, so a tag answered, ended or replayed names nothing. The index
//! takes the bits the table needs and the sequence the rest, and wraps: a tag
//! replayed a wrap later answers the request then in its slot, which a server
//! could answer by naming that request's own tag, so it grants the peer
//! nothing.

use core::fmt;

use crate::{Untrusted, Violation};

#[derive(Clone, PartialEq, Eq, Hash)]
struct Slot<T> {
    seq: u32,
    value: Option<T>,
}

/// At most `D` requests in flight, each carrying what its answer needs.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Inflight<T, const D: usize> {
    slots: [Slot<T>; D],
}

impl<T, const D: usize> Inflight<T, D> {
    const INDEX_BITS: u32 = usize::BITS.wrapping_sub(D.saturating_sub(1).leading_zeros());
    const INDEX_MASK: u32 = 1u32.wrapping_shl(Self::INDEX_BITS).wrapping_sub(1);
    const SEQ_MASK: u32 = u32::MAX.wrapping_shr(Self::INDEX_BITS);

    pub fn new() -> Self {
        const { assert!(D > 0 && Self::INDEX_BITS <= 16, "a tag keeps at least 16 bits of sequence") };
        Self { slots: core::array::from_fn(|_| Slot { seq: 0, value: None }) }
    }

    fn tag(seq: u32, index: u32) -> u32 {
        seq.wrapping_shl(Self::INDEX_BITS) | index
    }

    /// Keep `value` in flight under a tag no earlier request had in this slot;
    /// `Err(value)` with `D` in flight.
    pub fn insert(&mut self, value: T) -> Result<u32, T> {
        let Some((index, slot)) = (0u32..).zip(self.slots.iter_mut()).find(|(_, slot)| slot.value.is_none()) else {
            return Err(value);
        };
        slot.seq = slot.seq.wrapping_add(1) & Self::SEQ_MASK;
        slot.value = Some(value);
        Ok(Self::tag(slot.seq, index))
    }

    /// What the peer's `tag` answers; a tag nothing is in flight under is a
    /// violation.
    pub fn answer(&mut self, tag: Untrusted<u32>) -> Result<T, Violation> {
        let index = tag.map(|t| t & Self::INDEX_MASK).index(D).map_err(|_| Violation::Tag)?;
        let slot = self.slots.get_mut(index).ok_or(Violation::Tag)?;
        if !tag.map(|t| t.wrapping_shr(Self::INDEX_BITS)).is(slot.seq) {
            return Err(Violation::Tag);
        }
        slot.value.take().ok_or(Violation::Tag)
    }

    /// What is in flight, in no order.
    pub fn values(&self) -> impl Iterator<Item = &T> {
        self.slots.iter().filter_map(|slot| slot.value.as_ref())
    }

    /// Every tag in flight, by its slot.
    pub fn tags(&self) -> impl Iterator<Item = u32> + '_ {
        (0u32..).zip(&self.slots).filter(|(_, slot)| slot.value.is_some()).map(|(index, slot)| Self::tag(slot.seq, index))
    }

    /// The session ended: `each` is given every tag in flight, once, with what
    /// it carried, and none of them is answered again.
    pub fn end(&mut self, mut each: impl FnMut(u32, T)) {
        for (index, slot) in (0u32..).zip(self.slots.iter_mut()) {
            #[cfg(not(feature = "end-keeps-inflight"))]
            let value = slot.value.take();
            #[cfg(feature = "end-keeps-inflight")]
            let value = None;
            if let Some(value) = value {
                each(Self::tag(slot.seq, index), value);
            }
        }
    }
}

impl<T, const D: usize> Default for Inflight<T, D> {
    fn default() -> Self {
        Self::new()
    }
}

/// Only what is in flight, by slot.
impl<T: fmt::Debug, const D: usize> fmt::Debug for Inflight<T, D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries((0u32..).zip(&self.slots).filter_map(|(i, slot)| Some((i, slot.value.as_ref()?)))).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tag_is_answered_once_and_its_slot_takes_a_new_one() {
        let mut inflight = Inflight::<&str, 2>::new();
        let a = inflight.insert("a").unwrap();
        let b = inflight.insert("b").unwrap();
        assert_eq!(inflight.insert("c"), Err("c"));
        assert_eq!(inflight.answer(Untrusted::new(a)), Ok("a"));
        assert_eq!(inflight.answer(Untrusted::new(a)), Err(Violation::Tag), "answered twice");
        let c = inflight.insert("c").unwrap();
        assert_ne!(c, a, "the slot's next tag is not its last");
        let mut held: Vec<&str> = inflight.values().copied().collect();
        held.sort_unstable();
        assert_eq!(held, ["b", "c"], "what is in flight, and only that");
        assert_eq!(inflight.tags().collect::<Vec<_>>(), [c, b], "the tags in flight, by slot");
        assert_eq!(inflight.answer(Untrusted::new(a)), Err(Violation::Tag), "a replay answers nothing");
        assert_eq!(inflight.answer(Untrusted::new(2)), Err(Violation::Tag), "an index past the table");
        assert_eq!(inflight.answer(Untrusted::new(b)), Ok("b"));
        assert_eq!(inflight.answer(Untrusted::new(c)), Ok("c"));
        assert!(inflight.insert("d").is_ok() && inflight.insert("e").is_ok(), "every slot is free again");
    }

    /// A slot filled 2^16 times over still refuses its first tag: the index
    /// takes six bits of a 64-slot table's tag, not sixteen.
    #[test]
    fn a_slot_refilled_past_sixteen_bits_refuses_its_first_tag() {
        let mut inflight = Inflight::<(), 64>::new();
        let first = inflight.insert(()).unwrap();
        inflight.answer(Untrusted::new(first)).unwrap();
        for _ in 0..u16::MAX {
            let tag = inflight.insert(()).unwrap();
            inflight.answer(Untrusted::new(tag)).unwrap();
        }
        inflight.insert(()).unwrap();
        assert_eq!(inflight.answer(Untrusted::new(first)), Err(Violation::Tag));
    }

    #[test]
    fn an_end_answers_every_tag_once_and_a_late_completion_nothing() {
        let mut inflight = Inflight::<u8, 4>::new();
        let tags: Vec<u32> = (0..3).map(|n| inflight.insert(n).unwrap()).collect();
        assert_eq!(inflight.answer(Untrusted::new(tags[1])), Ok(1));
        let mut ended = Vec::new();
        inflight.end(|tag, value| ended.push((tag, value)));
        assert_eq!(ended, [(tags[0], 0), (tags[2], 2)]);
        for tag in tags {
            assert_eq!(inflight.answer(Untrusted::new(tag)), Err(Violation::Tag));
        }
        let mut again = 0;
        inflight.end(|_, _| again += 1);
        assert_eq!(again, 0, "an end answers nothing a first one answered");
    }
}
