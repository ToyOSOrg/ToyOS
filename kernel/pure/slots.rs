//! A table that names what it holds by slot. Its slots are made a piece at a
//! time as they are wanted and never move, so a slot names one value for as
//! long as it is held, and a slot let go is the next one taken. What the next
//! piece costs is known before it is made, so a caller charges it first;
//! letting a slot go never allocates, and a piece is kept with the table.
//!
//! A held slot may be marked on each of the table's `LISTS` lists, each
//! apart from the others. A list runs through the slots themselves, so
//! marking, unmarking and taking the last marked cost the same however many
//! slots there are, and none allocates.

#![forbid(unsafe_code)]

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::chunked::CHUNK_BYTES;

/// Slots one piece holds.
pub const PIECE: usize = 256;

const NONE: u32 = u32::MAX;

enum Slot<T, const LISTS: usize> {
    /// Free, linked to the next free slot.
    Free(u32),
    Held(T, [Mark; LISTS]),
}

/// Whether a held slot is marked on one list, and its neighbours there.
#[derive(Clone, Copy)]
enum Mark {
    Off,
    On { prev: u32, next: u32 },
}

pub struct Slots<T, const LISTS: usize = 1> {
    pieces: Vec<Box<[Slot<T, LISTS>]>>,
    /// The first free slot, `NONE` while every slot made is held.
    free: u32,
    /// Each list's slot marked last, `NONE` while none is.
    marked: [u32; LISTS],
}

impl<T, const LISTS: usize> Default for Slots<T, LISTS> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const LISTS: usize> Slots<T, LISTS> {
    /// The most slots a table makes: as many pieces as one list of
    /// [`CHUNK_BYTES`] names.
    pub const MAX: usize = CHUNK_BYTES / core::mem::size_of::<Box<[Slot<T, LISTS>]>>() * PIECE;

    /// The heap one piece takes.
    pub const PIECE_BYTES: usize = PIECE * core::mem::size_of::<Slot<T, LISTS>>();

    pub const fn new() -> Self {
        Self { pieces: Vec::new(), free: NONE, marked: [NONE; LISTS] }
    }

    /// Slots made, each held or free.
    pub fn len(&self) -> usize {
        self.pieces.len() * PIECE
    }

    pub fn is_empty(&self) -> bool {
        self.pieces.is_empty()
    }

    /// The heap the table holds.
    pub fn heap_bytes(&self) -> usize {
        Self::bytes(self.pieces.len(), self.pieces.capacity())
    }

    fn bytes(pieces: usize, listed: usize) -> usize {
        pieces * Self::PIECE_BYTES + listed * core::mem::size_of::<Box<[Slot<T, LISTS>]>>()
    }

    /// The pieces the list names once it holds `pieces`: doubling, so a
    /// growth seldom moves it, and never past [`Self::MAX`].
    fn listed_for(&self, pieces: usize) -> usize {
        match pieces <= self.pieces.capacity() {
            true => self.pieces.capacity(),
            false => pieces.next_power_of_two().min(Self::MAX / PIECE),
        }
    }

    /// Whether a slot is free for the next [`insert_with`](Self::insert_with).
    pub fn has_free(&self) -> bool {
        self.free != NONE
    }

    /// [`heap_bytes`](Self::heap_bytes) once [`grow`](Self::grow) has made
    /// one more piece: `None` where the table holds [`Self::MAX`] slots.
    pub fn grown_bytes(&self) -> Option<usize> {
        let pieces = self.pieces.len() + 1;
        (pieces * PIECE <= Self::MAX).then(|| Self::bytes(pieces, self.listed_for(pieces)))
    }

    /// One more piece of free slots: `false`, with nothing changed, where the
    /// table holds [`Self::MAX`] or the heap refuses.
    pub fn grow(&mut self) -> bool {
        let pieces = self.pieces.len() + 1;
        if pieces * PIECE > Self::MAX {
            return false;
        }
        // The piece first: a refused list then drops it, and the list keeps
        // the capacity it had.
        let mut piece = Vec::new();
        if piece.try_reserve_exact(PIECE).is_err() {
            return false;
        }
        let listed = self.listed_for(pieces);
        if self.pieces.try_reserve_exact(listed - self.pieces.len()).is_err() {
            return false;
        }
        let first = self.len() as u32;
        let last = first + PIECE as u32 - 1;
        let free = self.free;
        piece.extend((first..=last).map(|slot| Slot::Free(if slot == last { free } else { slot + 1 })));
        self.pieces.push(piece.into_boxed_slice());
        self.free = first;
        true
    }

    /// The value `f` makes of the first free slot, held there: its slot.
    /// `None` where no slot is free.
    pub fn insert_with(&mut self, f: impl FnOnce(u32) -> T) -> Option<u32> {
        let slot = self.free;
        let at = self.at_mut(slot)?;
        let Slot::Free(next) = *at else { panic!("slots: the free list named held slot {slot}") };
        *at = Slot::Held(f(slot), [Mark::Off; LISTS]);
        self.free = next;
        Some(slot)
    }

    /// The value at `slot`, unmarked on every list and its slot free from
    /// here; `None` where it holds none.
    pub fn remove(&mut self, slot: u32) -> Option<T> {
        for list in 0..LISTS {
            self.unmark(list, slot);
        }
        let free = self.free;
        let at = self.at_mut(slot)?;
        if let Slot::Free(_) = at {
            return None;
        }
        let Slot::Held(value, _) = core::mem::replace(at, Slot::Free(free)) else { unreachable!("held above") };
        self.free = slot;
        Some(value)
    }

    pub fn get(&self, slot: u32) -> Option<&T> {
        match self.pieces.get(slot as usize / PIECE)?.get(slot as usize % PIECE)? {
            Slot::Held(value, _) => Some(value),
            Slot::Free(_) => None,
        }
    }

    pub fn get_mut(&mut self, slot: u32) -> Option<&mut T> {
        match self.at_mut(slot)? {
            Slot::Held(value, _) => Some(value),
            Slot::Free(_) => None,
        }
    }

    /// Mark the held `slot` on `list`: whether it was not marked there
    /// already.
    pub fn mark(&mut self, list: usize, slot: u32) -> bool {
        let last = self.marked[list];
        let Some(mark) = self.mark_of(list, slot) else { return false };
        if let Mark::On { .. } = mark {
            return false;
        }
        *mark = Mark::On { prev: NONE, next: last };
        if last != NONE {
            *self.link(list, last).0 = slot;
        }
        self.marked[list] = slot;
        true
    }

    /// Unmark `slot` on `list`: whether it was marked there.
    pub fn unmark(&mut self, list: usize, slot: u32) -> bool {
        let Some(mark) = self.mark_of(list, slot) else { return false };
        let Mark::On { prev, next } = *mark else { return false };
        *mark = Mark::Off;
        match prev {
            NONE => self.marked[list] = next,
            _ => *self.link(list, prev).1 = next,
        }
        if next != NONE {
            *self.link(list, next).0 = prev;
        }
        true
    }

    /// The slot marked last on `list`, unmarked there.
    pub fn take_marked(&mut self, list: usize) -> Option<u32> {
        let slot = self.marked[list];
        self.unmark(list, slot).then_some(slot)
    }

    pub fn any_marked(&self, list: usize) -> bool {
        self.marked[list] != NONE
    }

    fn mark_of(&mut self, list: usize, slot: u32) -> Option<&mut Mark> {
        match self.at_mut(slot)? {
            Slot::Held(_, marks) => Some(&mut marks[list]),
            Slot::Free(_) => None,
        }
    }

    /// The `slot` marked on `list`: its neighbours there.
    fn link(&mut self, list: usize, slot: u32) -> (&mut u32, &mut u32) {
        match self.mark_of(list, slot) {
            Some(Mark::On { prev, next }) => (prev, next),
            _ => panic!("slots: list {list} named slot {slot}, which is not marked there"),
        }
    }

    fn at_mut(&mut self, slot: u32) -> Option<&mut Slot<T, LISTS>> {
        self.pieces.get_mut(slot as usize / PIECE)?.get_mut(slot as usize % PIECE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Slots are made a piece at a time from 0, a value stays at its slot
    /// while the table grows, and a slot let go is the next one taken.
    #[test]
    fn a_slot_names_its_value_until_it_goes_and_is_taken_next() {
        let mut t: Slots<u64> = Slots::new();
        assert_eq!(t.insert_with(u64::from), None, "no piece made");
        let mut held = alloc::vec::Vec::new();
        for n in 0..3 * PIECE as u32 + 1 {
            if !t.has_free() {
                assert!(t.grow());
            }
            held.push(t.insert_with(|slot| u64::from(slot) * 7).unwrap());
            assert_eq!(*held.last().unwrap(), n);
        }
        assert_eq!(t.len(), 4 * PIECE);
        assert!(held.iter().all(|&s| t.get(s) == Some(&(u64::from(s) * 7))));
        assert_eq!(t.remove(5), Some(35));
        assert_eq!(t.remove(700), Some(4900));
        assert_eq!(t.get(5), None, "a free slot holds nothing");
        assert_eq!(t.remove(5), None, "nor gives anything back");
        assert_eq!(t.insert_with(|_| 1), Some(700));
        assert_eq!(t.insert_with(|_| 2), Some(5));
        assert_eq!(t.insert_with(|_| 3), Some(3 * PIECE as u32 + 1), "then the piece's own free slots");
        assert_eq!(t.get_mut(5).map(|v| core::mem::replace(v, 9)), Some(2));
        assert_eq!(t.get(5), Some(&9));
    }

    /// What a growth is said to cost before it is made is what the table then
    /// holds, so a charge taken first covers it.
    #[test]
    fn a_growth_costs_what_it_said_it_would() {
        let mut t: Slots<u64> = Slots::new();
        assert_eq!(t.heap_bytes(), 0);
        for _ in 0..9 {
            let said = t.grown_bytes().unwrap();
            assert!(t.grow());
            assert_eq!(t.heap_bytes(), said);
        }
    }

    /// The marked slots are taken last marked first, each once: a second mark
    /// is none, an unmark or a removal takes a slot off the list wherever it
    /// is on it, a free slot is never marked, and marks hold across a growth.
    #[test]
    fn a_marked_slot_is_taken_once_unless_unmarked_or_removed_first() {
        let mut t: Slots<u32> = Slots::new();
        assert!(t.grow());
        for n in 0..6 {
            t.insert_with(|slot| slot).unwrap();
            assert!(t.mark(0, n));
        }
        assert!(!t.mark(0, 3), "marked twice");
        assert!(!t.mark(0, PIECE as u32 - 1) && !t.mark(0, NONE), "a free slot and none");
        assert!(t.unmark(0, 0) && t.unmark(0, 3) && t.unmark(0, 5), "the first, a middle one and the last");
        assert!(!t.unmark(0, 3), "unmarked twice");
        assert_eq!(t.remove(2), Some(2));
        while t.has_free() {
            t.insert_with(|slot| slot).unwrap();
        }
        assert!(t.grow());
        assert_eq!(t.insert_with(|slot| slot), Some(PIECE as u32));
        assert!(t.mark(0, 0) && t.mark(0, PIECE as u32));
        let taken: alloc::vec::Vec<u32> = core::iter::from_fn(|| t.take_marked(0)).collect();
        assert_eq!(taken, [PIECE as u32, 0, 4, 1]);
        assert!(!t.any_marked(0));
        assert!(t.mark(0, 4), "a taken slot is marked again");
        assert!(t.any_marked(0));
    }

    /// A slot's mark on one list is apart from its mark on another: each list
    /// takes its own, and a removal takes the slot off both.
    #[test]
    fn each_list_marks_apart_and_a_removal_unmarks_every_list() {
        let mut t: Slots<u32, 2> = Slots::new();
        assert!(t.grow());
        for _ in 0..4 {
            t.insert_with(|slot| slot).unwrap();
        }
        assert!(t.mark(0, 1) && t.mark(1, 1) && t.mark(1, 2) && t.mark(0, 3));
        assert!(t.unmark(1, 1) && !t.unmark(1, 3), "one list's unmark leaves the other's mark");
        assert_eq!(t.remove(3), Some(3));
        assert_eq!(core::iter::from_fn(|| t.take_marked(0)).collect::<alloc::vec::Vec<_>>(), [1]);
        assert_eq!(core::iter::from_fn(|| t.take_marked(1)).collect::<alloc::vec::Vec<_>>(), [2]);
        assert!(!t.any_marked(0) && !t.any_marked(1));
    }

    /// Past the slots made, nothing is named, and a removal changes nothing.
    #[test]
    fn a_slot_past_the_table_names_nothing() {
        let mut t: Slots<u8> = Slots::new();
        assert!(t.grow());
        assert_eq!((t.get(PIECE as u32), t.get(NONE)), (None, None));
        assert_eq!(t.remove(NONE), None);
        assert!(t.has_free());
    }
}
