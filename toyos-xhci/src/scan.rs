//! What the boot scan does next with the controller's one operation slot.
//!
//! The scan runs before there is a scheduler, so its loop is written in place
//! and every step of it is a wait. Two things can hold it: the operation the
//! controller owes an answer for ([`job::Outstanding`](crate::job::Outstanding)
//! is one slot), and an endpoint a completion code broke that no recovery has
//! taken yet. They end differently, and that difference is this module.
//!
//! **An outstanding operation is never abandoned here.** The slot is the
//! scan's only channel: the next port's Enable Slot goes into it, and a scan
//! that returned while one was still in flight would leave the port after it
//! submitting over a live operation. The slot empties on its own — every
//! operation carries a deadline and a pass that finds it expired takes the
//! operation as silent — so waiting for it is bounded by that deadline and
//! needs no bound of its own.
//!
//! **A broken endpoint has no deadline**, which is why the scan carries a quiet
//! bound at all: `recover_endpoints` can only start while the slot is free, a
//! device can break again the moment it is recovered, and nothing in that says
//! when to stop. The bound is silence — re-armed by every event the controller
//! produces — and it is spendable only on this half.

use crate::port::Nanos;

/// What the scan's loop does with what it has just seen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Settle {
    /// Nothing is outstanding and no endpoint is broken: the scan may go on to
    /// the next port, which is the only state in which its slot is free.
    Done,
    /// Keep draining. The controller owes an answer, and its own deadline is
    /// what ends this.
    Wait,
    /// Start the recovery a broken endpoint is owed; the slot is free for it.
    Recover,
    /// Nothing has been heard for the whole bound and the endpoint is still
    /// broken. The slot is free, so the scan goes on without it.
    GaveUp,
}

/// The scan's next step, given whether the controller owes an answer
/// (`busy`), whether a completion code left an endpoint broken (`broken`), and
/// where the silence bound stands.
///
/// `busy` decides first and alone: see the module header.
pub fn settle(busy: bool, broken: bool, now: Nanos, quiet_until: Nanos) -> Settle {
    if busy {
        return Settle::Wait;
    }
    if !broken {
        return Settle::Done;
    }
    if now >= quiet_until {
        return Settle::GaveUp;
    }
    Settle::Recover
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The defect this file exists for.** A boot scan that gave up while the
    /// controller still owed an answer left its one slot occupied, and the next
    /// port's `device::begin` submitted an Enable Slot over it — which is a
    /// panic in `job::Outstanding::submit`, reached by nothing worse than a
    /// stick that did not answer its first command. No `busy` state has any
    /// answer but [`Settle::Wait`], whatever the clock says.
    #[test]
    fn an_outstanding_operation_is_never_given_up_on() {
        for broken in [false, true] {
            for now in [0, 99, 100, 101, Nanos::MAX] {
                assert_eq!(settle(true, broken, now, 100), Settle::Wait, "{broken} {now}");
            }
        }
    }

    /// The scan's ordinary end: the slot is free and no device is owed a
    /// recovery, whether or not the bound has passed.
    #[test]
    fn a_free_slot_and_nothing_broken_is_the_end_of_the_scan() {
        assert_eq!(settle(false, false, 0, 100), Settle::Done);
        assert_eq!(settle(false, false, Nanos::MAX, 100), Settle::Done);
    }

    /// The half the bound is for: a broken endpoint is recovered while the
    /// controller is still being heard from, and left once it is not.
    #[test]
    fn a_broken_endpoint_is_recovered_until_the_silence_bound_passes() {
        assert_eq!(settle(false, true, 0, 100), Settle::Recover);
        assert_eq!(settle(false, true, 99, 100), Settle::Recover);
        assert_eq!(settle(false, true, 100, 100), Settle::GaveUp);
        assert_eq!(settle(false, true, Nanos::MAX, 100), Settle::GaveUp);
    }

    /// The loop as a whole, over a controller that answers nothing: the slot
    /// empties on its deadline and the scan ends, so a bound already spent
    /// before the operation was even submitted costs no correctness.
    #[test]
    fn a_silent_controller_ends_the_scan_at_the_operations_deadline() {
        // The bound was spent by a long blocking bind, as it is on the machine.
        let quiet_until = 0;
        let deadline = 2_000;
        let mut now = 0;
        loop {
            let busy = now < deadline;
            match settle(busy, false, now, quiet_until) {
                Settle::Wait => now += 1,
                Settle::Done => break,
                other => panic!("{other:?} at {now}"),
            }
            assert!(now <= deadline, "the scan waited past the operation's deadline");
        }
        assert_eq!(now, deadline);
    }
}
