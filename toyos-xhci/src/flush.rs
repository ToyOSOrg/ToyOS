//! What a disk owes a flush, and how often it has lost one, across every
//! device that serves it.
//!
//! A write a device reported complete may sit in its volatile cache until a
//! SYNCHRONIZE CACHE (SBC-3 §5.24) that succeeded has emptied it: until then it
//! is a flush owed. **A device that leaves its port and comes back is not known
//! to have kept its power** — a reset that moved it and an unplug and replug of
//! it look alike to the host — so a device that left owing one may have lost
//! those writes, and the disk counts that loss when it is taken back
//! ([`Debt::losses`]). The count only grows: a loss is a fact about writes
//! already reported, and no later flush brings them back.
//!
//! **Who is told is not this module's to decide.** A disk has several writers
//! and a flush is the whole disk's, so the first flush after the loss answers
//! for nobody in particular; the block layer holds each writer's own writes
//! against the count and fails the flush of each writer whose writes were
//! reported before it moved (`kernel/src/block.rs`).

/// One device instance's flush debt and the disk's loss count;
/// [`Self::NONE`] for a disk first bound.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Debt {
    /// A write this instance reported complete since its last flush that
    /// succeeded.
    unflushed: bool,
    /// How many devices serving this disk left owing a flush.
    losses: u64,
}

impl Debt {
    pub const NONE: Self = Self { unflushed: false, losses: 0 };

    /// The debt of the instance that takes back a disk whose device left
    /// handing on `losses` ([`Self::left`]).
    pub const fn adopted(losses: u64) -> Self {
        Self { unflushed: false, losses }
    }

    /// The device reported a write complete, whole or in part.
    pub fn wrote(&mut self) {
        self.unflushed = true;
    }

    /// Whether a device that left now leaves a flush owed. `no_cache` is
    /// whether this instance said it has no write cache, which makes its
    /// writes durable once complete.
    pub const fn owed(&self, no_cache: bool) -> bool {
        self.unflushed && !no_cache
    }

    /// The loss count a device leaving now hands the instance that takes the
    /// disk back: one more if it left owing a flush.
    pub const fn left(&self, no_cache: bool) -> u64 {
        self.losses + self.owed(no_cache) as u64
    }

    /// How many devices serving this disk left owing a flush before this one
    /// took it back.
    pub const fn losses(&self) -> u64 {
        self.losses
    }

    /// A flush succeeded: every write this instance reported before it is
    /// durable.
    pub fn flushed(&mut self) {
        self.unflushed = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both edges of the debt: owed after a write reported complete, and not
    /// owed after the flush that succeeded over it — so a device that leaves
    /// then hands on no loss.
    #[test]
    fn a_write_is_owed_until_a_flush_that_succeeded_and_not_after() {
        let mut debt = Debt::NONE;
        assert!(!debt.owed(false), "a disk first bound owes nothing");
        debt.wrote();
        assert!(debt.owed(false), "a write reported complete is owed");
        assert_eq!(debt.left(false), 1, "a device that leaves now has lost it");
        debt.flushed();
        assert!(!debt.owed(false), "a flush that succeeded ends it");
        let back = Debt::adopted(debt.left(false));
        assert_eq!(back.losses(), 0, "a device that left owing nothing lost nothing");
    }

    /// A device with no write cache owes nothing for its own writes, and a
    /// loss counted before it is still counted.
    #[test]
    fn a_device_with_no_cache_owes_nothing_and_keeps_the_count() {
        let mut debt = Debt::adopted(2);
        debt.wrote();
        assert!(!debt.owed(true));
        assert_eq!(debt.left(true), 2);
        assert_eq!(debt.left(false), 3);
    }

    /// Instance 1 reports a write and leaves; instance 2 takes the disk back
    /// having counted that loss, and leaves before any write or flush;
    /// instance 3 still carries the count, and a flush of its own does not
    /// take the loss back.
    #[test]
    fn a_loss_is_counted_once_and_never_uncounted() {
        let mut first = Debt::NONE;
        first.wrote();
        let second = Debt::adopted(first.left(false));
        assert_eq!(second.losses(), 1);
        assert!(!second.owed(false), "the instance that took the disk back wrote nothing");
        let mut third = Debt::adopted(second.left(false));
        assert_eq!(third.losses(), 1, "leaving owing nothing loses nothing more");
        third.wrote();
        third.flushed();
        assert_eq!(third.losses(), 1, "a flush now cannot bring the lost writes back");
    }
}
