//! Which tid a new thread of a process gets.
//!
//! **A tid names one thread of its process for that process's life**: it is
//! never issued twice, and [`Tid::MAX`] is never issued, since it is the
//! per-CPU word for no thread. Once every tid below it is issued, the
//! process's spawns are refused ([`crate::spawn::Admit::TidsSpent`]).

use crate::Tid;

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Tids {
    /// The lowest tid never issued; [`Tid::MAX`] once every one has been.
    next: Tid,
}

impl Default for Tids {
    fn default() -> Self {
        Self { next: Tid(0) }
    }
}

impl Tids {
    /// A process's tids, its next new one `next`, as one that has issued
    /// every tid below it.
    #[cfg(test)]
    pub fn issued_below(next: Tid) -> Self {
        Self { next }
    }

    /// Whether every tid below [`Tid::MAX`] has been issued.
    pub fn spent(&self) -> bool {
        self.next == Tid::MAX
    }

    /// A tid this process has never issued, or `None` once they are
    /// [`Self::spent`].
    pub fn take(&mut self) -> Option<Tid> {
        if self.spent() {
            return None;
        }
        let tid = self.next;
        self.next = Tid(tid.0 + 1);
        Some(tid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tids_are_issued_in_order_from_zero() {
        let mut tids = Tids::default();
        assert_eq!([tids.take(), tids.take(), tids.take()], [Some(Tid(0)), Some(Tid(1)), Some(Tid(2))]);
    }

    /// The last tid issued is the one below [`Tid::MAX`], and the next take is
    /// refused rather than overflowing into it.
    #[test]
    fn the_last_tid_is_one_below_max_and_none_follows_it() {
        let mut tids = Tids::issued_below(Tid(u32::MAX - 1));
        assert!(!tids.spent());
        assert_eq!(tids.take(), Some(Tid(u32::MAX - 1)));
        assert!(tids.spent());
        assert_eq!(tids.take(), None);
        assert_eq!(tids.take(), None);
    }
}
