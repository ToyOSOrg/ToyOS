//! What a disk owes a flush, across every device that serves it.
//!
//! A write a device reported complete may sit in its volatile cache until a
//! SYNCHRONIZE CACHE (SBC-3 §5.24) that succeeded has emptied it: until then it
//! is a flush owed. **A device that leaves its port and comes back is not known
//! to have kept its power** — a reset that moved it and an unplug and replug of
//! it look alike to the host — so a disk taken back by a device that left owing
//! one carries the debt, and its next flush fails: no flush now can say those
//! writes are durable.
//!
//! **A debt carried is still a debt owed.** The instance that took the disk
//! back may leave in turn before its next flush, and the instance after it
//! carries the same debt; only the flush that reports the loss, or a flush
//! that succeeded over writes of its own instance, ends one.

/// One device instance's flush debt; [`Self::NONE`] for a disk first bound.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Debt {
    /// A write this instance reported complete since its last flush that
    /// succeeded.
    unflushed: bool,
    /// Carried from the instance before: writes it reported complete may be
    /// gone, and the next flush says so.
    carried: bool,
}

/// What a flush asked of a disk does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Flush {
    /// It fails, once, without going to the device: a carried debt.
    Lost,
    /// It goes to the device, and [`Debt::flushed`] follows if it succeeded.
    Issue,
}

impl Debt {
    pub const NONE: Self = Self { unflushed: false, carried: false };

    /// The debt of the instance that takes back a disk whose device left with
    /// `owed` ([`Self::owed`]).
    pub const fn adopted(owed: bool) -> Self {
        Self { unflushed: false, carried: owed }
    }

    /// The device reported a write complete, whole or in part.
    pub fn wrote(&mut self) {
        self.unflushed = true;
    }

    /// Whether a device that left now leaves a flush owed. `no_cache` is
    /// whether this instance said it has no write cache, which makes its own
    /// writes durable once complete and says nothing of a debt it carries.
    pub const fn owed(&self, no_cache: bool) -> bool {
        self.carried || (self.unflushed && !no_cache)
    }

    /// A flush is asked for.
    pub fn flush(&mut self) -> Flush {
        if core::mem::take(&mut self.carried) {
            Flush::Lost
        } else {
            Flush::Issue
        }
    }

    /// The flush [`Self::flush`] issued succeeded: every write before it is
    /// durable.
    pub fn flushed(&mut self) {
        self.unflushed = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both edges of the debt: owed after a write reported complete, and not
    /// owed after the flush that succeeded over it, nor by the instance that
    /// takes the disk back then.
    #[test]
    fn a_write_is_owed_until_a_flush_that_succeeded_and_not_after() {
        let mut debt = Debt::NONE;
        assert!(!debt.owed(false), "a disk first bound owes nothing");
        debt.wrote();
        assert!(debt.owed(false), "a write reported complete is owed");
        assert_eq!(debt.flush(), Flush::Issue);
        assert!(debt.owed(false), "a flush asked for and not yet succeeded ends nothing");
        debt.flushed();
        assert!(!debt.owed(false), "a flush that succeeded ends it");
        let mut back = Debt::adopted(debt.owed(false));
        assert!(!back.owed(false));
        assert_eq!(back.flush(), Flush::Issue, "a device that left owing nothing is flushed as ever");
    }

    /// A device with no write cache owes nothing for its own writes, and still
    /// owes the debt it took the disk back with.
    #[test]
    fn a_device_with_no_cache_owes_only_what_it_carries() {
        let mut debt = Debt::NONE;
        debt.wrote();
        assert!(!debt.owed(true));
        let mut back = Debt::adopted(true);
        back.wrote();
        assert!(back.owed(true), "a carried debt is not the device's cache to answer");
    }

    /// Instance 1 acknowledges a write and leaves; instance 2 takes the disk
    /// back owing it and leaves before any flush; instance 3 takes it back, and
    /// its next flush fails by name — once.
    #[test]
    fn a_debt_carried_is_carried_again_until_a_flush_reports_it() {
        let mut first = Debt::NONE;
        first.wrote();
        let second = Debt::adopted(first.owed(false));
        assert!(second.owed(false), "the instance that took the debt leaves owing it");
        let mut third = Debt::adopted(second.owed(false));
        assert_eq!(third.flush(), Flush::Lost, "the third instance's next flush fails");
        assert!(!third.owed(false));
        assert_eq!(third.flush(), Flush::Issue, "once");
    }

    /// A write on the instance that carries a debt is owed after the flush that
    /// reports the loss, until a flush of its own succeeds.
    #[test]
    fn a_write_on_a_carried_debt_outlives_the_flush_that_reports_the_loss() {
        let mut debt = Debt::adopted(true);
        debt.wrote();
        assert_eq!(debt.flush(), Flush::Lost);
        assert!(debt.owed(false));
        assert_eq!(debt.flush(), Flush::Issue);
        debt.flushed();
        assert!(!debt.owed(false));
    }
}
