//! Which pid a new process gets.
//!
//! **A pid names one process for the life of the machine.** Once an entry
//! holds a pid it is never issued again, so a pid carried across a lock
//! release names that process or nothing. [`Pid::MAX`] is never issued: it is
//! the per-CPU word for no process.
//!
//! A spawn takes its pid at admission, before anything is built, and a spawn
//! refused after that gives it back: no entry held it, so nothing can name
//! it, and a refused spawn spends none. Once every pid below [`Pid::MAX`] is
//! issued, admission refuses by name.

use alloc::vec::Vec;

use crate::proclife::Pid;

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Pids {
    /// The lowest pid never taken; [`Pid::MAX`] once every one has been.
    next: Pid,
    /// Taken by a spawn refused since, held by no entry: the next taken.
    /// At most one per spawn in flight at once.
    returned: Vec<Pid>,
}

impl Default for Pids {
    fn default() -> Self {
        Self { next: Pid(0), returned: Vec::new() }
    }
}

impl Pids {
    /// A table whose next new pid is `next`, as one that has issued every
    /// pid below it.
    #[cfg(test)]
    pub fn issued_below(next: Pid) -> Self {
        Self { next, returned: Vec::new() }
    }

    /// A pid no entry has held, or `None` once every pid below [`Pid::MAX`]
    /// has been issued.
    pub fn take(&mut self) -> Option<Pid> {
        if let Some(pid) = self.returned.pop() {
            return Some(pid);
        }
        if self.next == Pid::MAX {
            return None;
        }
        let pid = self.next;
        self.next = Pid(pid.0 + 1);
        Some(pid)
    }

    /// `pid`, which [`Self::take`] answered to a spawn refused before any
    /// entry held it.
    pub fn give_back(&mut self, pid: Pid) {
        assert!(pid < self.next, "Pids::give_back: pid {pid} was never taken");
        assert!(!self.returned.contains(&pid), "Pids::give_back: pid {pid} given back twice");
        self.returned.push(pid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pids_are_issued_in_order_from_zero() {
        let mut pids = Pids::default();
        assert_eq!([pids.take(), pids.take(), pids.take()], [Some(Pid(0)), Some(Pid(1)), Some(Pid(2))]);
    }

    /// The last pid issued is the one below [`Pid::MAX`], and the next take
    /// is refused rather than overflowing into it.
    #[test]
    fn the_last_pid_is_one_below_max_and_none_follows_it() {
        let mut pids = Pids::issued_below(Pid(u32::MAX - 1));
        assert_eq!(pids.take(), Some(Pid(u32::MAX - 1)));
        assert_eq!(pids.take(), None);
        assert_eq!(pids.take(), None);
    }

    /// A pid given back is the next one taken, after every pid is issued too.
    #[test]
    fn a_pid_given_back_is_taken_again() {
        let mut pids = Pids::issued_below(Pid(u32::MAX - 2));
        let first = pids.take().unwrap();
        let last = pids.take().unwrap();
        assert_eq!(pids.take(), None);
        pids.give_back(first);
        pids.give_back(last);
        assert_eq!([pids.take(), pids.take(), pids.take()], [Some(last), Some(first), None]);
    }
}
