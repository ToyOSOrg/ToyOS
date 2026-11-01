//! The holders of a listener or a datagram socket: each one process's connection to netstack,
//! told on a bell of its own that the socket has something to take, an accept or a datagram.
//!
//! **A holder is told once a look.** The bell is rung while the socket has something to take and
//! the holder has not been told since it last asked for it, so a holder that asks again after its
//! ring is rung again while something is left, and one that does not ask is rung no more. A
//! holder reads its bell, then asks; so no thing to take waits on a holder that is not looking,
//! and no bell takes more than one ring a request.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;

/// A holder's bell. It is netstack's own end of a pipe whose other end only that holder holds.
pub trait Bell {
    /// Rings, never waiting: a bell already rung and not yet read is rung still.
    fn ring(&mut self);
}

/// One holder of one socket, as the node numbers them: the socket's first is
/// [`HolderId::FIRST`], and no number is used twice for one socket.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HolderId(u32);

impl HolderId {
    /// The holder whose listen or bind made the socket.
    pub const FIRST: Self = Self(0);
}

struct Holding {
    bell: Box<dyn Bell>,
    /// Rung since its last ask.
    told: bool,
}

pub(crate) struct Holders {
    by_id: BTreeMap<HolderId, Holding>,
    next: u32,
}

impl Holders {
    /// A socket's holders: `first` alone.
    pub(crate) fn new(first: Box<dyn Bell>) -> Self {
        Self { by_id: BTreeMap::from([(HolderId::FIRST, Holding { bell: first, told: false })]), next: 1 }
    }

    pub(crate) fn add(&mut self, bell: Box<dyn Bell>) -> HolderId {
        let id = HolderId(self.next);
        self.next = self.next.saturating_add(1);
        self.by_id.insert(id, Holding { bell, told: false });
        id
    }

    /// Whether `holder` was one.
    pub(crate) fn remove(&mut self, holder: HolderId) -> bool {
        self.by_id.remove(&holder).is_some()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// The holders past the first, each of which takes a place.
    pub(crate) fn more(&self) -> usize {
        self.by_id.len().saturating_sub(1)
    }

    pub(crate) fn holds(&self, holder: HolderId) -> bool {
        self.by_id.contains_key(&holder)
    }

    /// `holder` asks: from here it is owed a ring for what it finds left. `false` is no holder.
    pub(crate) fn asked(&mut self, holder: HolderId) -> bool {
        self.by_id.get_mut(&holder).map(|holding| holding.told = false).is_some()
    }

    /// Rings every holder not told since its last ask, where the socket has something to take.
    pub(crate) fn tell(&mut self, ready: bool) {
        if !ready {
            return;
        }
        for holding in self.by_id.values_mut().filter(|holding| !holding.told) {
            holding.bell.ring();
            holding.told = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::rc::Rc;
    use core::cell::Cell;

    struct Counted(Rc<Cell<usize>>);

    impl Bell for Counted {
        fn ring(&mut self) {
            self.0.set(self.0.get().saturating_add(1));
        }
    }

    fn bell() -> (Box<dyn Bell>, Rc<Cell<usize>>) {
        let rings = Rc::new(Cell::new(0));
        (Box::new(Counted(rings.clone())), rings)
    }

    /// Every holder is told when there is something, once until it asks again, and one that
    /// asks while something is left is told again.
    #[test]
    fn a_holder_is_told_once_a_look() {
        let (first, a) = bell();
        let mut holders = Holders::new(first);
        let (second, b) = bell();
        let other = holders.add(second);
        holders.tell(false);
        assert_eq!((a.get(), b.get()), (0, 0), "nothing to take rings nobody");
        holders.tell(true);
        holders.tell(true);
        assert_eq!((a.get(), b.get()), (1, 1), "each told once");
        assert!(holders.asked(other));
        holders.tell(true);
        assert_eq!((a.get(), b.get()), (1, 2), "the one that asked is told what is left");
        assert!(holders.remove(other));
        assert!(!holders.asked(other), "a holder that left asks nothing");
        assert_eq!(holders.more(), 0);
    }
}
