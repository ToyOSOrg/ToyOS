//! Which number a USB pointer's records carry as theirs: the lowest no bound
//! pointer holds, given back when the pointer goes. Two live pointers never
//! share one, or the owner of input would OR one's buttons into the other's,
//! and a replug takes a freed number again, so a `u8` never runs out.

use core::sync::atomic::{AtomicU64, Ordering};

/// One bit per number a `u8` names, set while a pointer holds it.
pub struct Sources([AtomicU64; 4]);

impl Sources {
    pub const fn new() -> Self {
        Self([const { AtomicU64::new(0) }; 4])
    }

    /// The lowest free number, taken; `None` while all 256 are held.
    pub fn claim(&self) -> Option<u8> {
        for (word, bits) in self.0.iter().enumerate() {
            let mut seen = bits.load(Ordering::Relaxed);
            while seen != u64::MAX {
                let bit = seen.trailing_ones();
                match bits.compare_exchange_weak(seen, seen | (1 << bit), Ordering::Relaxed, Ordering::Relaxed) {
                    Ok(_) => return Some((word as u32 * 64 + bit) as u8),
                    Err(now) => seen = now,
                }
            }
        }
        None
    }

    /// `source`, which a claim took, is free again.
    pub fn give_back(&self, source: u8) {
        let bit = 1u64 << (source % 64);
        let was = self.0[usize::from(source) / 64].fetch_and(!bit, Ordering::Relaxed);
        assert!(was & bit != 0, "pointer source {source} given back while free");
    }
}

impl Default for Sources {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::Sources;
    use std::vec::Vec;

    #[test]
    fn two_claims_take_distinct_numbers() {
        let sources = Sources::new();
        assert_eq!((sources.claim(), sources.claim()), (Some(0), Some(1)));
    }

    #[test]
    fn a_number_given_back_is_the_next_claims() {
        let sources = Sources::new();
        let held: Vec<u8> = (0..=200).map(|_| sources.claim().expect("a free number")).collect();
        sources.give_back(held[0]);
        sources.give_back(held[200]);
        assert_eq!(sources.claim(), Some(0));
        assert_eq!(sources.claim(), Some(200));
        assert_eq!(sources.claim(), Some(201));
    }

    #[test]
    fn every_number_held_refuses_a_claim_until_one_is_given_back() {
        let sources = Sources::new();
        let held: Vec<u8> = (0..256).map(|_| sources.claim().expect("a free number")).collect();
        assert_eq!(held, (0..=255).collect::<Vec<u8>>());
        assert_eq!(sources.claim(), None);
        sources.give_back(130);
        assert_eq!(sources.claim(), Some(130));
    }

    #[test]
    #[should_panic(expected = "pointer source 7 given back while free")]
    fn a_number_given_back_twice_panics() {
        let sources = Sources::new();
        (0..8).for_each(|_| assert!(sources.claim().is_some()));
        sources.give_back(7);
        sources.give_back(7);
    }
}
