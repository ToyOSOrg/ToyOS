//! Who holds each of `N` numbered things an interrupt is delivered through — a
//! CPU's vectors, a remapping table's entries: the lowest free number is taken,
//! given back by number, and read by number from an interrupt handler with no
//! lock. Running out is the machine's limit, answered as `None`.

use core::sync::atomic::{AtomicU32, Ordering};

/// The word a free number holds; a held one holds its owner plus one.
const FREE: u32 = 0;

pub struct Owners<const N: usize>([AtomicU32; N]);

impl<const N: usize> Owners<N> {
    pub const fn new() -> Self {
        Self([const { AtomicU32::new(FREE) }; N])
    }

    /// The lowest free number, now `owner`'s; `None` while all `N` are held.
    pub fn take(&self, owner: u32) -> Option<usize> {
        let held = owner.checked_add(1).expect("an owner is below u32::MAX");
        self.0
            .iter()
            .position(|word| word.compare_exchange(FREE, held, Ordering::AcqRel, Ordering::Relaxed).is_ok())
    }

    /// `number`, which [`Self::take`] answered, is free again.
    pub fn give_back(&self, number: usize) {
        let was = self.0[number].swap(FREE, Ordering::AcqRel);
        assert!(was != FREE, "number {number} given back while free");
    }

    /// Who holds `number`: `None` while it is free, and for one past `N`.
    pub fn owner(&self, number: usize) -> Option<u32> {
        match self.0.get(number)?.load(Ordering::Acquire) {
            FREE => None,
            held => Some(held - 1),
        }
    }
}

impl<const N: usize> Default for Owners<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::Owners;
    use std::sync::Arc;
    use std::vec::Vec;

    #[test]
    fn the_lowest_free_number_is_taken_and_names_its_owner() {
        let owners = Owners::<4>::new();
        assert_eq!((owners.take(7), owners.take(0), owners.take(7)), (Some(0), Some(1), Some(2)));
        assert_eq!((0..5).map(|n| owners.owner(n)).collect::<Vec<_>>(), [Some(7), Some(0), Some(7), None, None]);
    }

    #[test]
    fn a_number_given_back_is_the_next_one_taken() {
        let owners = Owners::<4>::new();
        (0..3).for_each(|owner| assert!(owners.take(owner).is_some()));
        owners.give_back(1);
        assert_eq!(owners.owner(1), None);
        assert_eq!(owners.take(9), Some(1));
        assert_eq!(owners.take(9), Some(3));
    }

    #[test]
    fn every_number_held_refuses_and_changes_nothing() {
        let owners = Owners::<2>::new();
        assert_eq!((owners.take(1), owners.take(2)), (Some(0), Some(1)));
        assert_eq!(owners.take(3), None);
        assert_eq!((owners.owner(0), owners.owner(1)), (Some(1), Some(2)));
    }

    #[test]
    fn racing_takers_are_given_distinct_numbers() {
        const N: usize = 64;
        let owners = Arc::new(Owners::<N>::new());
        let takers: Vec<_> = (0..8u32)
            .map(|owner| {
                let owners = Arc::clone(&owners);
                std::thread::spawn(move || (0..N).filter_map(|_| owners.take(owner)).collect::<Vec<_>>())
            })
            .collect();
        let mut taken: Vec<usize> = takers.into_iter().flat_map(|t| t.join().expect("a taker")).collect();
        taken.sort_unstable();
        assert_eq!(taken, (0..N).collect::<Vec<_>>());
    }

    #[test]
    #[should_panic(expected = "number 1 given back while free")]
    fn a_number_given_back_twice_panics() {
        let owners = Owners::<2>::new();
        (0..2).for_each(|owner| assert!(owners.take(owner).is_some()));
        owners.give_back(1);
        owners.give_back(1);
    }
}
