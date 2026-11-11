//! A table that names what it holds by slot. Its slots are made a piece at a
//! time as they are wanted and never move, so a slot names one value for as
//! long as it is held, and a slot let go is the next one taken. What the next
//! piece costs is known before it is made, so a caller charges it first;
//! letting a slot go never allocates, and a piece is kept with the table.

#![forbid(unsafe_code)]

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::chunked::CHUNK_BYTES;

/// Slots one piece holds.
pub const PIECE: usize = 256;

const NONE: u32 = u32::MAX;

enum Slot<T> {
    /// Free, linked to the next free slot.
    Free(u32),
    Held(T),
}

pub struct Slots<T> {
    pieces: Vec<Box<[Slot<T>]>>,
    /// The first free slot, `NONE` while every slot made is held.
    free: u32,
    held: usize,
}

impl<T> Default for Slots<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Slots<T> {
    /// The most slots a table makes: as many pieces as one list of
    /// [`CHUNK_BYTES`] names.
    pub const MAX: usize = CHUNK_BYTES / core::mem::size_of::<Box<[Slot<T>]>>() * PIECE;

    /// The heap one piece takes.
    pub const PIECE_BYTES: usize = PIECE * core::mem::size_of::<Slot<T>>();

    pub const fn new() -> Self {
        Self { pieces: Vec::new(), free: NONE, held: 0 }
    }

    /// Slots made, each held or free.
    pub fn len(&self) -> usize {
        self.pieces.len() * PIECE
    }

    pub fn is_empty(&self) -> bool {
        self.pieces.is_empty()
    }

    /// Slots holding a value.
    pub fn held(&self) -> usize {
        self.held
    }

    /// The heap the table holds.
    pub fn heap_bytes(&self) -> usize {
        Self::bytes(self.pieces.len(), self.pieces.capacity())
    }

    fn bytes(pieces: usize, listed: usize) -> usize {
        pieces * Self::PIECE_BYTES + listed * core::mem::size_of::<Box<[Slot<T>]>>()
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
        let listed = self.listed_for(pieces);
        if self.pieces.try_reserve_exact(listed - self.pieces.len()).is_err() {
            return false;
        }
        let mut piece = Vec::new();
        if piece.try_reserve_exact(PIECE).is_err() {
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
        *at = Slot::Held(f(slot));
        self.free = next;
        self.held += 1;
        Some(slot)
    }

    /// The value at `slot`, its slot free from here; `None` where it holds none.
    pub fn remove(&mut self, slot: u32) -> Option<T> {
        let free = self.free;
        let at = self.at_mut(slot)?;
        if let Slot::Free(_) = at {
            return None;
        }
        let Slot::Held(value) = core::mem::replace(at, Slot::Free(free)) else { unreachable!("held above") };
        self.free = slot;
        self.held -= 1;
        Some(value)
    }

    pub fn get(&self, slot: u32) -> Option<&T> {
        match self.pieces.get(slot as usize / PIECE)?.get(slot as usize % PIECE)? {
            Slot::Held(value) => Some(value),
            Slot::Free(_) => None,
        }
    }

    pub fn get_mut(&mut self, slot: u32) -> Option<&mut T> {
        match self.at_mut(slot)? {
            Slot::Held(value) => Some(value),
            Slot::Free(_) => None,
        }
    }

    fn at_mut(&mut self, slot: u32) -> Option<&mut Slot<T>> {
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
        assert_eq!((t.len(), t.held()), (4 * PIECE, 3 * PIECE + 1));
        assert!(held.iter().all(|&s| t.get(s) == Some(&(u64::from(s) * 7))));
        assert_eq!(t.remove(5), Some(35));
        assert_eq!(t.remove(700), Some(4900));
        assert_eq!(t.get(5), None, "a free slot holds nothing");
        assert_eq!(t.remove(5), None, "nor gives anything back");
        assert_eq!(t.held(), 3 * PIECE - 1);
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
