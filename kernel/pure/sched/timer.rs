//! The timer plan: the one armed instant a CPU carries.
//!
//! A deadline lives in one place: its home CPU's set of the deadlines it owes,
//! one entry per task, which names the park it was armed for. A park that
//! another claim ended leaves its entry behind, and the entry claims nothing
//! when it falls due; the CPU drops such entries before it arms the timer, and
//! one a claim ends after that costs a spurious pass, never a missed deadline.
//!
//! **Not every arming is a deadline.** [`TimerPlan::no_later_than`] lets a caller
//! pull the arming forward for a reason nothing in this module knows about — a
//! CPU that wants waking to look for work again. It moves the instant in one
//! direction only, which is what keeps invariant T out of it: the invariant
//! bounds the armed instant from above by the earliest event the CPU owes, so
//! an extra wake before that instant is a spurious pass and never a missed
//! deadline.

use crate::sched::hw::Nanos;

/// What the one-shot timer must be programmed to at the end of a pass.
/// Produced by `finish()` after every parking change, applied *last* — which
/// is the whole proof of invariant T: there is no window between the last
/// change and the arming.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[must_use = "a timer plan that is not applied is invariant T violated"]
pub enum TimerPlan {
    Arm(Nanos),
    Stop,
}

impl TimerPlan {
    /// `quantum_end` is present only while a task is running; `deadline` is
    /// the earliest a parked task on this CPU owes.
    pub fn compute(quantum_end: Option<Nanos>, deadline: Option<Nanos>) -> Self {
        match (quantum_end, deadline) {
            (Some(q), Some(d)) => TimerPlan::Arm(q.min(d)),
            (Some(q), None) => TimerPlan::Arm(q),
            (None, Some(d)) => TimerPlan::Arm(d),
            (None, None) => TimerPlan::Stop,
        }
    }

    /// Pull the arming forward to `at`, if a caller has a reason to be woken
    /// that no parked deadline and no quantum accounts for — today
    /// [`crate::sched::cpu::Balance::PullWithRearm`]'s re-probe, and nothing else.
    ///
    /// One direction only, and that is what keeps invariant T out of it: the
    /// invariant bounds the armed instant from *above* by the earliest event the
    /// CPU owes, so a plan that only ever moves earlier cannot violate it. A
    /// method that could move the arming later would be a missed deadline
    /// waiting for a caller.
    pub fn no_later_than(self, at: Option<Nanos>) -> Self {
        match (self, at) {
            (_, None) => self,
            (TimerPlan::Stop, Some(at)) => TimerPlan::Arm(at),
            (TimerPlan::Arm(planned), Some(at)) => TimerPlan::Arm(planned.min(at)),
        }
    }

    pub fn armed(&self) -> Option<Nanos> {
        match self {
            TimerPlan::Arm(at) => Some(*at),
            TimerPlan::Stop => None,
        }
    }
}

/// Proof that the hardware holds a [`TimerPlan`], or a one-shot still to fire
/// that is due no later. [`crate::sched::cpu::SleepToken`]
/// cannot be built without one, so "halted with a deadline pending and the
/// timer unarmed" is unrepresentable rather than asserted.
pub struct TimerApplied {
    armed: Option<Nanos>,
}

impl TimerApplied {
    pub(crate) fn new(armed: Option<Nanos>) -> Self {
        Self { armed }
    }

    pub fn armed(&self) -> Option<Nanos> {
        self.armed
    }
}
