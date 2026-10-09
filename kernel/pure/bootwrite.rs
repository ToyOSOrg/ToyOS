//! What a CPU that asked the boot processor for a write to `SMI_CMD` decides
//! on one turn of its wait, from the clock and what the boot processor has
//! published.
//!
//! The wait has two subjects and each is held to the span on its own:
//!
//! - **The kick being taken.** Until the boot processor publishes that it has
//!   the write, the span runs from the ask, and its end is a boot processor
//!   that takes no interrupt ([`Turn::Deaf`]).
//! - **The firmware's handler.** The boot processor publishes that it has the
//!   write before it makes it, with the time; from there the span runs from
//!   that time, and its end is a firmware that has held the boot processor
//!   the whole of it ([`Turn::Outlasted`]). The boot processor judges the
//!   same span itself once the write has retired ([`outlasted`]), so a
//!   handler that long ends the machine whichever CPU asked for it.
//!
//! Neither span has the other inside it: a handler's time is never the
//! boot processor's deafness.

#![forbid(unsafe_code)]

/// One turn of the asker's wait.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Turn {
    Wait,
    /// The write has retired, and so has the firmware's handler.
    Retired,
    /// The boot processor has not taken the kick in the span.
    Deaf,
    /// The firmware has held the boot processor the span, from the time it
    /// took the write.
    Outlasted,
}

/// Whether a write that has held the boot processor `held_ns` has outlasted
/// `span_ns`.
pub const fn outlasted(span_ns: u64, held_ns: u64) -> bool {
    held_ns >= span_ns
}

/// The turn at `now_ns` of a wait asked at `asked_ns`: `taken_ns` is when the
/// boot processor took the write, where it has, and `retired` whether the
/// write has retired. A write that has retired is never judged by the clock.
pub const fn turn(span_ns: u64, asked_ns: u64, taken_ns: Option<u64>, retired: bool, now_ns: u64) -> Turn {
    if retired {
        return Turn::Retired;
    }
    let (since, end) = match taken_ns {
        None => (asked_ns, Turn::Deaf),
        Some(taken_ns) => (taken_ns, Turn::Outlasted),
    };
    if outlasted(span_ns, now_ns.saturating_sub(since)) { end } else { Turn::Wait }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPAN: u64 = 5_000_000_000;
    const MS: u64 = 1_000_000;

    /// A kick taken late and a handler that then runs nearly the span: the
    /// two together are far past it, and neither is.
    #[test]
    fn a_handlers_time_is_not_the_kicks_and_the_kicks_is_not_the_handlers() {
        let (asked, taken) = (7 * MS, 7 * MS + SPAN - 1);
        assert_eq!(turn(SPAN, asked, None, false, taken), Turn::Wait);
        for now in [taken, taken + MS, asked + SPAN, asked + SPAN + MS, taken + SPAN - 1] {
            assert_eq!(turn(SPAN, asked, Some(taken), false, now), Turn::Wait, "{now}");
        }
        assert_eq!(turn(SPAN, asked, Some(taken), false, taken + SPAN), Turn::Outlasted);
    }

    #[test]
    fn a_kick_not_taken_in_the_span_is_a_deaf_boot_processor() {
        let asked = 3 * MS;
        assert_eq!(turn(SPAN, asked, None, false, asked), Turn::Wait);
        assert_eq!(turn(SPAN, asked, None, false, asked + SPAN - 1), Turn::Wait);
        assert_eq!(turn(SPAN, asked, None, false, asked + SPAN), Turn::Deaf);
        assert_eq!(turn(SPAN, asked, None, false, u64::MAX), Turn::Deaf);
    }

    #[test]
    fn a_handler_that_holds_the_span_has_outlasted_it_on_either_cpu() {
        let (asked, taken) = (3 * MS, 4 * MS);
        assert_eq!(turn(SPAN, asked, Some(taken), false, taken + SPAN - 1), Turn::Wait);
        assert_eq!(turn(SPAN, asked, Some(taken), false, taken + SPAN), Turn::Outlasted);
        // The boot processor's own judgement, of the same span.
        assert!(!outlasted(SPAN, SPAN - 1));
        assert!(outlasted(SPAN, SPAN));
    }

    /// The asker reads the clock before it reads whether the write retired,
    /// so a write found retired is one, whatever the clock says.
    #[test]
    fn a_write_that_has_retired_is_never_judged_by_the_clock() {
        for taken in [None, Some(4 * MS)] {
            assert_eq!(turn(SPAN, 3 * MS, taken, true, u64::MAX), Turn::Retired);
        }
    }

    /// A clock read before the boot processor's own reads no time before it.
    #[test]
    fn a_take_the_clock_has_not_reached_is_waited_for() {
        assert_eq!(turn(SPAN, 3 * MS, Some(5 * MS), false, 4 * MS), Turn::Wait);
    }
}
