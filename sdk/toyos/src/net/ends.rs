//! A process's ends of netstack's objects, each reached by one handle: the table a stream's
//! module and a socket's keep, and the rights a holder keeps of what it holds.
//!
//! **An end goes with its handle and its last call.** A call counts itself in through the
//! handle; a handle closed under it reaches the end no more, and the end is handed back for its
//! module to let go once the call has returned. Its slot stays reserved until then, so no new end
//! takes the slot while the module clears what it keeps beside it.

use toyos_abi::handle::Rights;
use toyos_abi::syscall;
use toyos_abi::RawHandle;

use crate::{AsHandle, BorrowedHandle, OwnedHandle};

use super::NetError;

/// What a holder keeps of its connection: it reads, writes and waits on it, and duplicates it
/// for an end's own handle and a descriptor's. No `TRANSFER`, so no spawn or message can move
/// one.
pub(super) const HELD_CONNECTION: Rights = Rights::DUP.union(Rights::WAIT).union(Rights::READ).union(Rights::WRITE);
/// What a holder keeps of a bell netstack rings: it reads and waits on it, and duplicates it.
pub(super) const HELD_BELL: Rights = Rights::DUP.union(Rights::WAIT).union(Rights::READ);
/// What a holder keeps of the bell it rings netstack on: it signals it, and duplicates it.
pub(super) const HELD_RUNG: Rights = Rights::DUP.union(Rights::WRITE);

/// `handle` held with `rights` alone, the original closed, as a number the table keeps from
/// here: refused, the original is closed all the same.
pub(super) fn narrowed(handle: OwnedHandle, rights: Rights) -> Result<RawHandle, NetError> {
    handle.as_handle().try_clone_narrowed(rights).map(OwnedHandle::into_raw).map_err(|_| NetError::Io)
}

/// A handle an end keeps, lent for a call.
///
/// # Safety
/// `raw` is one of an end's handles, which only its module's `free` closes, once no handle and
/// no call reaches the end: it is live for as long as a call is counted in.
pub(super) unsafe fn kept<'a>(raw: RawHandle) -> BorrowedHandle<'a> {
    // SAFETY: the caller's.
    unsafe { BorrowedHandle::borrow_raw(raw) }
}

/// Close a handle an end kept, from its module's `free`.
///
/// # Safety
/// No handle and no call reaches the end any more, and `raw` is one of its own.
pub(super) unsafe fn close_kept(raw: RawHandle) {
    // SAFETY: the caller's.
    unsafe { syscall::close(raw) }
}

/// A slot of the table. `repr(u8)` puts `Free` at zero, so a table costs a program that makes no
/// end no initialised data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum Slot<E> {
    Free,
    /// Reserved: by a call making an end that has not answered, or by an end its module is
    /// letting go of.
    Making,
    /// An end, the handle that reaches it until that is closed, and the calls counted in.
    Live { end: E, handle: Option<RawHandle>, calls: u32 },
}

pub(super) struct Ends<E: Copy, const N: usize> {
    pub(super) slots: [Slot<E>; N],
}

impl<E: Copy, const N: usize> Ends<E, N> {
    pub(super) const fn new() -> Self {
        Self { slots: [const { Slot::Free }; N] }
    }

    /// A free slot made [`Slot::Making`].
    pub(super) fn reserve(&mut self) -> Option<usize> {
        let slot = self.slots.iter().position(|slot| matches!(slot, Slot::Free))?;
        self.slots[slot] = Slot::Making;
        Some(slot)
    }

    pub(super) fn unreserve(&mut self, slot: usize) {
        self.slots[slot] = Slot::Free;
    }

    /// The made end, reached by `handle`.
    pub(super) fn enter(&mut self, slot: usize, end: E, handle: RawHandle) {
        self.slots[slot] = Slot::Live { end, handle: Some(handle), calls: 0 };
    }

    pub(super) fn find(&self, handle: RawHandle) -> Option<usize> {
        self.slots.iter().position(|slot| matches!(slot, Slot::Live { handle: Some(held), .. } if *held == handle))
    }

    /// The end `handle` reaches.
    pub(super) fn get(&self, handle: RawHandle) -> Option<E> {
        match self.slots[self.find(handle)?] {
            Slot::Live { end, .. } => Some(end),
            Slot::Free | Slot::Making => None,
        }
    }

    /// A call through `handle` begins: its end, counted.
    pub(super) fn call(&mut self, handle: RawHandle) -> Option<(usize, E)> {
        let slot = self.find(handle)?;
        let Slot::Live { end, calls, .. } = &mut self.slots[slot] else { return None };
        *calls += 1;
        Some((slot, *end))
    }

    /// The live end at `slot`, which a counted call keeps.
    pub(super) fn at(&self, slot: usize) -> Option<E> {
        match self.slots[slot] {
            Slot::Live { end, .. } => Some(end),
            Slot::Free | Slot::Making => None,
        }
    }

    /// A call ends: the end, where it was its last use.
    pub(super) fn returned(&mut self, slot: usize) -> Option<E> {
        let Slot::Live { calls, .. } = &mut self.slots[slot] else { unreachable!("a call counted in an end that is not live") };
        *calls -= 1;
        self.free_unused(slot)
    }

    /// `handle` reaches its end no more: `None` for a handle that reached none, else its end's
    /// slot and the end where it was its last use.
    pub(super) fn forget(&mut self, handle: RawHandle) -> Option<Option<(usize, E)>> {
        let slot = self.find(handle)?;
        if let Slot::Live { handle, .. } = &mut self.slots[slot] {
            *handle = None;
        }
        Some(self.free_unused(slot).map(|end| (slot, end)))
    }

    /// The end, where nothing reaches it any more: its slot [`Slot::Making`] until its module
    /// has let go of what it keeps beside it.
    fn free_unused(&mut self, slot: usize) -> Option<E> {
        match self.slots[slot] {
            Slot::Live { end, handle: None, calls: 0 } => {
                self.slots[slot] = Slot::Making;
                Some(end)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(n: u32) -> RawHandle {
        RawHandle(n)
    }

    /// The end goes only with its handle and its last call: a handle closed under a call lets
    /// the end go only once the call returns, and its slot is not taken again before its module
    /// gives it back.
    #[test]
    fn an_end_goes_with_its_handle_and_its_last_call() {
        let mut ends: Ends<u8, 4> = Ends::new();
        let slot = ends.reserve().unwrap();
        ends.enter(slot, 7, h(10));
        assert_eq!((ends.find(h(10)), ends.get(h(10))), (Some(slot), Some(7)), "the handle reaches the end");
        let (called, during) = ends.call(h(10)).unwrap();
        assert_eq!(during, 7);
        assert_eq!(ends.forget(h(10)), Some(None), "a call is left");
        assert_eq!(ends.find(h(10)), None, "the handle reaches nothing");
        assert_eq!(ends.returned(called), Some(7), "the last call frees it");
        assert_eq!(ends.reserve(), Some(slot + 1), "a freed slot is not taken before its module gives it back");
        ends.unreserve(slot);
        assert_eq!(ends.slots[slot], Slot::Free);
    }

    /// A handle no end was made for reaches nothing, and forgetting it frees nothing.
    #[test]
    fn a_handle_no_end_was_made_for_reaches_nothing() {
        let mut ends: Ends<u8, 4> = Ends::new();
        let slot = ends.reserve().unwrap();
        ends.enter(slot, 7, h(10));
        assert_eq!((ends.find(h(12)), ends.call(h(12)), ends.forget(h(12))), (None, None, None));
        assert_eq!(ends.get(h(10)), Some(7));
    }

    /// Past the table's bound no slot is reserved, and a reservation given back is a slot again.
    #[test]
    fn past_the_bound_no_slot_is_reserved() {
        let mut ends: Ends<u8, 4> = Ends::new();
        let slots: Vec<usize> = (0..4).map(|_| ends.reserve().unwrap()).collect();
        assert_eq!(ends.reserve(), None);
        ends.unreserve(slots[2]);
        assert_eq!(ends.reserve(), Some(slots[2]));
    }
}
