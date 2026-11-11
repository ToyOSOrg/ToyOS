//! Which slot of the kernel stacks' range a new stack is handed, and when a
//! retired slot's address is safe to hand out again.
//!
//! A slot whose stack dropped is *retired*: its leaves are clear, and a CPU
//! may still hold a translation of it. It is handed out again only after a
//! flush of every CPU that began after it retired, which makes it *clean*. A
//! slot at or above `fresh` was never used, so no CPU has a translation of
//! it. Fixed in size, so nothing here allocates: the range holds [`SLOTS`]
//! stacks at once and a take past that is refused. Pure: the kernel supplies
//! the lock and the flush.

#![forbid(unsafe_code)]

/// How many stacks the range holds at once: 64 processes at their 4096-thread
/// limit.
pub const SLOTS: u32 = 1 << 18;

/// Retired slots a take passes over for a never-used one before it asks for a
/// flush instead. A flush interrupts every CPU, so a take asks for one at
/// most once in this many; and a take that passes over fewer costs at most
/// that many slots' addresses, eight tables' worth, beyond what a flush every
/// time would use.
pub const FLUSH_BATCH: u32 = 64;

const WORDS: usize = (SLOTS / u64::BITS) as usize;

/// A set of slots.
struct Set {
    bits: [u64; WORDS],
    count: u32,
}

impl Set {
    const EMPTY: Self = Self { bits: [0; WORDS], count: 0 };

    fn insert(&mut self, slot: u32) {
        let (word, bit) = ((slot / u64::BITS) as usize, 1u64 << (slot % u64::BITS));
        assert!(self.bits[word] & bit == 0, "stack slots: slot {slot} is in a set twice");
        self.bits[word] |= bit;
        self.count += 1;
    }

    fn pop(&mut self) -> Option<u32> {
        if self.count == 0 {
            return None;
        }
        let word = self.bits.iter().position(|word| *word != 0)?;
        let bit = self.bits[word].trailing_zeros();
        self.bits[word] &= !(1 << bit);
        self.count -= 1;
        Some(word as u32 * u64::BITS + bit)
    }

    /// Move every slot of `from` into `self`, which holds none of them.
    fn absorb(&mut self, from: &mut Set) {
        for (mine, theirs) in self.bits.iter_mut().zip(from.bits.iter_mut()) {
            *mine |= core::mem::take(theirs);
        }
        self.count += core::mem::take(&mut from.count);
    }
}

/// What [`Slots::take`] answers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Take {
    /// A slot no CPU holds a translation of.
    Slot(u32),
    /// None is clean: flush every CPU, call [`Slots::flushed`] and take again.
    Flush,
    /// Every slot is in use, or the only ones that are not wait on a flush
    /// another taker is making.
    Spent,
}

pub struct Slots {
    /// Slots below this have been used.
    fresh: u32,
    clean: Set,
    retired: Set,
    /// The slots a flush in progress will make clean: those retired before it
    /// began.
    flushing: Set,
    in_flush: bool,
}

impl Default for Slots {
    fn default() -> Self {
        Self::new()
    }
}

impl Slots {
    pub const fn new() -> Self {
        Self { fresh: 0, clean: Set::EMPTY, retired: Set::EMPTY, flushing: Set::EMPTY, in_flush: false }
    }

    pub fn take(&mut self) -> Take {
        if let Some(slot) = self.clean.pop() {
            return Take::Slot(slot);
        }
        let waiting = self.retired.count;
        if !self.in_flush && (waiting >= FLUSH_BATCH || (self.fresh == SLOTS && waiting > 0)) {
            self.in_flush = true;
            self.flushing.absorb(&mut self.retired);
            return Take::Flush;
        }
        if self.fresh == SLOTS {
            return Take::Spent;
        }
        self.fresh += 1;
        Take::Slot(self.fresh - 1)
    }

    /// The flush [`Take::Flush`] asked for reached every CPU.
    pub fn flushed(&mut self) {
        assert!(self.in_flush, "stack slots: a flush nobody was asked for");
        self.in_flush = false;
        self.clean.absorb(&mut self.flushing);
    }

    /// `slot`'s stack dropped and its leaves are clear.
    pub fn retire(&mut self, slot: u32) {
        assert!(slot < self.fresh, "stack slots: slot {slot} was never taken");
        self.retired.insert(slot);
    }

    /// `slot` was taken and nothing was mapped in it.
    pub fn unused(&mut self, slot: u32) {
        assert!(slot < self.fresh, "stack slots: slot {slot} was never taken");
        self.clean.insert(slot);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::boxed::Box;
    use std::collections::BTreeSet;
    use std::vec::Vec;

    fn slots() -> Box<Slots> {
        Box::new(Slots::new())
    }


    #[test]
    fn fresh_slots_come_in_order_and_nothing_flushes_below_the_batch() {
        let mut slots = slots();
        for want in 0..FLUSH_BATCH {
            assert_eq!(slots.take(), Take::Slot(want));
        }
        for slot in 0..FLUSH_BATCH - 1 {
            slots.retire(slot);
        }
        assert_eq!(slots.take(), Take::Slot(FLUSH_BATCH), "a take passes over fewer retired slots than the batch");
        slots.retire(FLUSH_BATCH - 1);
        assert_eq!(slots.take(), Take::Flush);
    }

    #[test]
    fn a_slot_retired_during_a_flush_waits_for_the_next() {
        let mut slots = slots();
        for _ in 0..=FLUSH_BATCH {
            slots.take();
        }
        for slot in 0..FLUSH_BATCH {
            slots.retire(slot);
        }
        assert_eq!(slots.take(), Take::Flush);
        // Retired after the flush began: its translation may outlive it.
        slots.retire(FLUSH_BATCH);
        // Another taker meanwhile neither flushes again nor gets a retired slot.
        assert_eq!(slots.take(), Take::Slot(FLUSH_BATCH + 1));
        slots.flushed();
        let mut handed = BTreeSet::new();
        for _ in 0..FLUSH_BATCH {
            let Take::Slot(slot) = slots.take() else { panic!("a flushed slot is clean") };
            handed.insert(slot);
        }
        assert_eq!(handed, (0..FLUSH_BATCH).collect());
        assert_eq!(slots.take(), Take::Slot(FLUSH_BATCH + 2), "the slot retired during the flush is not clean");
    }

    #[test]
    fn a_spent_range_flushes_for_one_retired_slot_and_refuses_with_none() {
        let mut slots = slots();
        for want in 0..SLOTS {
            assert_eq!(slots.take(), Take::Slot(want));
        }
        assert_eq!(slots.take(), Take::Spent);
        slots.retire(7);
        assert_eq!(slots.take(), Take::Flush);
        assert_eq!(slots.take(), Take::Spent, "the one slot not in use waits on the flush");
        slots.flushed();
        assert_eq!(slots.take(), Take::Slot(7));
        assert_eq!(slots.take(), Take::Spent);
    }

    #[test]
    fn a_slot_taken_and_not_mapped_is_handed_out_again_without_a_flush() {
        let mut slots = slots();
        assert_eq!(slots.take(), Take::Slot(0));
        slots.unused(0);
        assert_eq!(slots.take(), Take::Slot(0));
    }

    /// Over a long run of takes and drops in an order a generator picks, with
    /// another take and another drop landing inside every flush: no slot is
    /// handed out while live, none that retired since the last flush began,
    /// and never more than the batch and those racing takes beyond what was
    /// live at once is used.
    #[test]
    fn no_slot_retired_since_the_last_flush_is_handed_out() {
        let mut slots = slots();
        let mut live: Vec<u32> = Vec::new();
        let mut stale = BTreeSet::new();
        let (mut peak, mut raced) = (0, 0);
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..200_000 {
            if live.is_empty() || next() % 3 != 0 {
                let check = |slot: u32, live: &Vec<u32>, stale: &BTreeSet<u32>| {
                    assert!(!live.contains(&slot), "slot {slot} handed out while live");
                    assert!(!stale.contains(&slot), "slot {slot} handed out with no flush since it retired");
                };
                let slot = match slots.take() {
                    Take::Slot(slot) => slot,
                    Take::Spent => panic!("the range is nowhere near spent"),
                    Take::Flush => {
                        // The flush is under way: it covers what retired before it.
                        let covered = stale.clone();
                        let Take::Slot(racing) = slots.take() else { panic!("a take beside a flush takes a fresh slot") };
                        check(racing, &live, &stale);
                        live.push(racing);
                        raced += 1;
                        let dropped = live.swap_remove(0);
                        slots.retire(dropped);
                        stale.insert(dropped);
                        slots.flushed();
                        stale.retain(|slot| !covered.contains(slot));
                        let Take::Slot(slot) = slots.take() else { panic!("a flushed slot is clean") };
                        slot
                    }
                };
                check(slot, &live, &stale);
                live.push(slot);
                peak = peak.max(live.len() as u32);
            } else {
                let slot = live.swap_remove((next() % live.len() as u64) as usize);
                slots.retire(slot);
                stale.insert(slot);
            }
            assert!(slots.fresh <= peak + FLUSH_BATCH + raced, "{} slots used for a peak of {peak}", slots.fresh);
        }
    }
}
