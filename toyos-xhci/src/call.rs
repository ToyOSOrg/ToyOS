//! What one disk call may still spend on a controller once its transport has
//! broken.
//!
//! A break is followed, inside the same call, by commands, control requests
//! and a port-reset settle, each a spin with its own timeout — and the caller
//! holds the controller's lock with preemption off for all of them. Summed,
//! those timeouts are no bound anyone stated. [`AfterBreak`] is the one bound:
//! opened at the start of the wait that broke, it clips every later wait of
//! the call to what is left, and a step reached with nothing left is not
//! taken.
//!
//! **A controller that left a command unanswered is sent no other in the same
//! call.** Its command ring is one queue: whatever follows waits behind the
//! command it did not answer, and costs a whole timeout to say so again.

/// Nanoseconds since boot.
pub type Nanos = u64;

/// Why a step was not taken.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NotTaken {
    /// The call's budget is spent.
    Spent,
    /// An earlier command of this call got no answer.
    ControllerSilent,
}

impl core::fmt::Display for NotTaken {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Spent => "this call's budget after a break is spent",
            Self::ControllerSilent => "the controller left an earlier command of this call unanswered",
        })
    }
}

/// One call's budget after a break; [`Self::CLOSED`] between calls and in a
/// call whose transport has not broken, where it clips and refuses nothing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AfterBreak {
    ends: Option<Nanos>,
    silent: bool,
}

impl AfterBreak {
    pub const CLOSED: Self = Self { ends: None, silent: false };

    /// The transport broke on a wait that began at `wait_began`. The first
    /// break of a call opens its budget and a later one does not extend it.
    pub fn open(&mut self, wait_began: Nanos, budget: Nanos) {
        if self.ends.is_none() {
            self.ends = Some(wait_began.saturating_add(budget));
        }
    }

    /// When a wait starting at `now`, with `own` as its own timeout, gives up.
    pub fn wait_ends(&self, now: Nanos, own: Nanos) -> Nanos {
        let own = now.saturating_add(own);
        self.ends.map_or(own, |ends| own.min(ends))
    }

    /// Whether a wait that gave up at `now` was cut short by this budget and
    /// not by its own timeout.
    pub fn cut(&self, began: Nanos, now: Nanos, own: Nanos) -> bool {
        self.ends.is_some_and(|ends| now >= ends && now < began.saturating_add(own))
    }

    /// Whether a command may go to the controller at `now`.
    pub fn command(&self, now: Nanos) -> Result<(), NotTaken> {
        if self.silent {
            return Err(NotTaken::ControllerSilent);
        }
        self.request(now)
    }

    /// Whether anything that is waited for may start at `now`.
    pub fn request(&self, now: Nanos) -> Result<(), NotTaken> {
        match self.ends {
            Some(ends) if now >= ends => Err(NotTaken::Spent),
            _ => Ok(()),
        }
    }

    /// A command got no answer.
    pub fn unanswered(&mut self) {
        self.silent = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: Nanos = 1_000_000_000;
    const OWN: Nanos = 2 * SECOND;
    const BUDGET: Nanos = 4 * SECOND;

    #[test]
    fn a_call_whose_transport_has_not_broken_is_clipped_and_refused_nothing() {
        let call = AfterBreak::CLOSED;
        assert_eq!(call.wait_ends(10 * SECOND, OWN), 12 * SECOND);
        assert_eq!(call.command(u64::MAX), Ok(()));
        assert_eq!(call.request(u64::MAX), Ok(()));
    }

    /// The run the budget exists for: a wait that broke after its whole
    /// timeout, then a command the controller never answers, then everything
    /// the driver would still have sent. The call is over at the budget.
    #[test]
    fn a_silent_controller_costs_one_command_and_the_call_ends_inside_the_budget() {
        let mut call = AfterBreak::CLOSED;
        let began = 100 * SECOND;
        let mut now = began + OWN;
        call.open(began, BUDGET);
        assert_eq!(call.command(now), Ok(()));
        let gives_up = call.wait_ends(now, OWN);
        assert_eq!(gives_up, began + BUDGET);
        now = gives_up;
        call.unanswered();
        assert_eq!(call.command(now), Err(NotTaken::ControllerSilent));
        assert_eq!(call.request(now), Err(NotTaken::Spent));
        assert_eq!(call.wait_ends(now, OWN), now, "a settle started now does not spin");
    }

    /// A silent controller is refused by that name even with budget left: the
    /// next command would queue behind the one it did not answer.
    #[test]
    fn a_silent_controller_is_sent_nothing_even_with_budget_left() {
        let mut call = AfterBreak::CLOSED;
        call.open(0, BUDGET);
        call.unanswered();
        assert_eq!(call.command(SECOND), Err(NotTaken::ControllerSilent));
        assert_eq!(call.request(SECOND), Ok(()), "a register wait is not a command");
    }

    #[test]
    fn a_second_break_in_the_same_call_does_not_extend_the_budget() {
        let mut call = AfterBreak::CLOSED;
        call.open(0, BUDGET);
        call.open(3 * SECOND, BUDGET);
        assert_eq!(call.wait_ends(3 * SECOND, OWN), BUDGET);
        assert_eq!(call.request(BUDGET), Err(NotTaken::Spent));
    }

    /// A wait inside the budget keeps its own timeout, and one that ends on it
    /// is not called cut.
    #[test]
    fn a_wait_is_cut_only_when_the_budget_ended_it() {
        let mut call = AfterBreak::CLOSED;
        call.open(0, BUDGET);
        assert_eq!(call.wait_ends(SECOND, OWN), 3 * SECOND);
        assert!(!call.cut(SECOND, 3 * SECOND, OWN));
        assert_eq!(call.wait_ends(3 * SECOND, OWN), BUDGET);
        assert!(call.cut(3 * SECOND, BUDGET, OWN));
        assert!(!AfterBreak::CLOSED.cut(0, OWN, OWN));
    }
}
