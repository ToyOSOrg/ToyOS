//! Which idle state a CPU waits in, chosen once per idle period, and the
//! machine-wide bound on how long a waiting CPU may take to wake.
//!
//! A CPU's states are what its firmware declared, shallowest first, each with
//! its worst-case exit latency and the residency below which it saves nothing
//! ([`Cost`]). The shallowest wait, `hlt`'s C1, is not among them: it is always
//! permitted and is what [`choose`] answers `None` for.
//!
//! **The rule**: the deepest declared state whose exit latency is within the
//! held bound, whose residency fits before the CPU's next timer, and for which
//! fewer than half of the CPU's last [`WINDOW`] unforeseen wakes came sooner
//! than its residency ([`History`]). The timer horizon is exact; a kick, an IPI
//! or a device interrupt is not foreseen, and the history is the estimate of
//! those. A CPU that enters a state with a timer armed wakes `exit` early
//! ([`Choice::wake_early_by`]), so a known deadline is met rather than made late
//! by the state's exit.
//!
//! **A CPU the history alone holds shallow comes back to look** ([`Choice::revisit_after`]):
//! the history moves only on wakes, and a CPU that stopped being woken would
//! otherwise wait shallow for as long as nothing comes, which on a part whose
//! turbo counts a core in C1 as active costs every other core its clock. It
//! looks again after the residency of the next deeper state it was kept from,
//! and a look that nothing interrupted is an idle period that long, after
//! which the short wakes that held the CPU no longer describe it
//! ([`History::quiet`]): a CPU left idle climbs one state per residency.

use alloc::collections::BTreeMap;

/// The most states a CPU's declaration holds.
pub const MAX_STATES: usize = 8;

/// How many unforeseen wakes [`History`] remembers per state.
pub const WINDOW: u32 = 8;

/// Of the last [`WINDOW`] unforeseen wakes, how many may have come sooner than
/// a state's residency for it still to be chosen: fewer than half. A policy,
/// judged by metal: with it a CPU that turns to short idles pays at most this
/// many deep exits before it waits shallow.
pub const EARLY_ALLOWED: u32 = WINDOW / 2 - 1;

/// One declared state's costs, in nanoseconds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cost {
    /// The worst case from the wake to running again.
    pub exit: u64,
    /// The shortest idle for which the state is worth entering.
    pub residency: u64,
}

/// For each state, which of the CPU's last [`WINDOW`] unforeseen wakes came
/// sooner than its residency: bit 0 the latest.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct History {
    early: [u8; MAX_STATES],
}

impl History {
    /// An idle period that a wake the CPU did not foresee ended after
    /// `slept` nanoseconds.
    pub fn unforeseen(&mut self, costs: &[Cost], slept: u64) {
        for (bits, cost) in self.early.iter_mut().zip(costs) {
            *bits = *bits << 1 | u8::from(slept < cost.residency);
        }
    }

    /// An idle period of `slept` that nothing but the CPU's own revisit
    /// ended: whatever pattern of short wakes held the CPU shallow has stopped
    /// for at least that long, so each state whose residency it covers starts
    /// again with nothing early.
    pub fn quiet(&mut self, costs: &[Cost], slept: u64) {
        for (bits, cost) in self.early.iter_mut().zip(costs) {
            if cost.residency <= slept {
                *bits = 0;
            }
        }
    }

    /// The whole history as one word, for a CPU to keep in an atomic of its own.
    pub const fn bits(&self) -> u64 {
        u64::from_le_bytes(self.early)
    }

    pub const fn from_bits(bits: u64) -> Self {
        Self { early: bits.to_le_bytes() }
    }

    fn admits(&self, state: usize) -> bool {
        self.early[state].count_ones() <= EARLY_ALLOWED
    }
}

/// What [`choose`] picked.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Choice {
    /// A declared state, or `None` for C1.
    pub state: Option<usize>,
    /// How much earlier than its deadline the CPU's timer must fire for the
    /// deadline to be met.
    pub wake_early_by: u64,
    /// Where a deeper state fit the horizon and the bound and the history
    /// alone kept the CPU from it: how soon to wake and choose again, the
    /// residency of the shallowest such state ([`History::quiet`]).
    pub revisit_after: Option<u64>,
}

/// What a CPU decided in its last pass before it sleeps, so the timer its
/// choice needs is part of the pass's one timer plan ([`wake_at`]) and the
/// wait enters what the pass chose.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Plan {
    pub choice: Choice,
    /// The chosen state's exit, 0 for C1.
    pub exit: u64,
    /// Which declaration the choice indexes, the machine's own token for it:
    /// a wait that finds another declared since enters C1.
    pub declaration: u64,
    /// The pass's instant, at which the choice was made.
    pub chosen_at: u64,
    /// When the scheduler's own timer was due before the choice moved it.
    pub deadline: Option<u64>,
}

impl Plan {
    /// C1, with nothing declared or nothing chosen: what a machine with no
    /// declaration plans.
    pub const fn c1(deadline: Option<u64>, chosen_at: u64) -> Self {
        Self { choice: Choice { state: None, wake_early_by: 0, revisit_after: None }, exit: 0, declaration: 0, chosen_at, deadline }
    }

    /// The instant the pass's timer must fire by for this plan, sooner than
    /// its deadline ([`wake_at`]).
    pub fn wake_at(&self) -> Option<u64> {
        wake_at(&self.choice, self.deadline, self.chosen_at)
    }

    /// What ended the period that began with this plan and ended at `woke`.
    pub fn ended(&self, woke: u64) -> Ended {
        ended(&self.choice, self.deadline, self.chosen_at, woke)
    }

    /// What `history` learns from the period this plan began, ended at
    /// `woke`. A revisit covers the residency it was armed for, however
    /// early within [`FORESEEN_SLACK`] it fired.
    pub fn learn(&self, history: &mut History, costs: &[Cost], woke: u64) {
        let slept = woke.saturating_sub(self.chosen_at);
        match self.ended(woke) {
            Ended::Deadline => {}
            Ended::Revisit => history.quiet(costs, slept.max(self.choice.revisit_after.unwrap_or(0))),
            Ended::Unforeseen => history.unforeseen(costs, slept),
        }
    }
}

/// The state to wait in. `horizon` is how long until the CPU's timer fires,
/// `None` where none is armed; `limit` the held wake bound, `None` where
/// nothing is held.
pub fn choose(costs: &[Cost], history: &History, horizon: Option<u64>, limit: Option<u64>) -> Choice {
    let states = 0..costs.len().min(MAX_STATES);
    let permitted = |state: usize| {
        let cost = costs[state];
        limit.is_none_or(|limit| cost.exit <= limit) && horizon.is_none_or(|horizon| cost.residency <= horizon)
    };
    let state = states.clone().rev().find(|&s| permitted(s) && history.admits(s));
    let next_deeper = states.filter(|&s| state.is_none_or(|chosen| s > chosen)).find(|&s| permitted(s));
    Choice {
        state,
        wake_early_by: match (state, horizon) {
            (Some(state), Some(_)) => costs[state].exit,
            _ => 0,
        },
        revisit_after: next_deeper.map(|deeper| costs[deeper].residency),
    }
}

/// How early the scheduler's own timer may fire and still be the wake it was
/// armed for: a one-shot counts its span in whole ticks of its timer, rounding
/// down, so it fires up to a tick early. A policy far above one tick of either
/// architecture's timer, and far below any state's residency.
pub const FORESEEN_SLACK: u64 = 1_000;

/// When the scheduler's own timer fires after this choice, where one is armed
/// for `armed`: its deadline, pulled in by the chosen state's exit.
fn deadline_fires(choice: &Choice, armed: Option<u64>) -> Option<u64> {
    armed.map(|at| at.saturating_sub(choice.wake_early_by))
}

/// The instant to fire this CPU's timer at, sooner than the scheduler armed
/// it, for `choice` made at `now`: the deadline pulled in by the exit, or the
/// revisit, whichever is sooner. Never at or after `armed`, so writing it can
/// only move the timer earlier; `None` leaves it as the scheduler armed it.
fn wake_at(choice: &Choice, armed: Option<u64>, now: u64) -> Option<u64> {
    let early = deadline_fires(choice, armed).filter(|_| choice.wake_early_by > 0);
    let revisit = choice.revisit_after.map(|after| now.saturating_add(after));
    early.into_iter().chain(revisit).min().filter(|&at| armed.is_none_or(|armed| at < armed))
}

/// Whether a period that ended at `woke` was ended by the scheduler's own
/// timer, firing for `fires`: what the history must not learn from, since the
/// horizon already said it.
fn foreseen(fires: Option<u64>, woke: u64) -> bool {
    fires.is_some_and(|fires| woke.saturating_add(FORESEEN_SLACK) >= fires)
}

/// What ended an idle period.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ended {
    /// The scheduler's own timer, which the horizon already said.
    Deadline,
    /// The CPU's revisit, nothing else having come first.
    Revisit,
    /// A kick, an IPI or a device: what the history learns from.
    Unforeseen,
}

/// What ended an idle period chosen as `choice` at `now`, with the
/// scheduler's timer armed for `armed`, that ended at `woke`.
fn ended(choice: &Choice, armed: Option<u64>, now: u64, woke: u64) -> Ended {
    let revisit = choice.revisit_after.map(|after| now.saturating_add(after));
    if foreseen(deadline_fires(choice, armed), woke) {
        Ended::Deadline
    } else if foreseen(revisit, woke) {
        Ended::Revisit
    } else {
        Ended::Unforeseen
    }
}

/// The wake bounds held, as a multiset: the machine's bound is the smallest.
#[derive(Default, Debug)]
pub struct Bounds {
    held: BTreeMap<u64, u32>,
}

impl Bounds {
    pub const fn new() -> Self {
        Self { held: BTreeMap::new() }
    }

    pub fn hold(&mut self, bound: u64) {
        *self.held.entry(bound).or_insert(0) += 1;
    }

    /// Release one hold of `bound`, which must be held.
    pub fn release(&mut self, bound: u64) {
        let count = self.held.get_mut(&bound).unwrap_or_else(|| panic!("a wake bound of {bound} ns released and never held"));
        *count -= 1;
        if *count == 0 {
            self.held.remove(&bound);
        }
    }

    /// The smallest bound held.
    pub fn least(&self) -> Option<u64> {
        self.held.keys().next().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const US: u64 = 1000;

    /// The T14's two deep states as acpiserver declares them: exit = `_CST`'s
    /// latency, residency twice it.
    const T14: [Cost; 2] = [Cost { exit: 253 * US, residency: 506 * US }, Cost { exit: 1048 * US, residency: 2096 * US }];

    fn pick(history: &History, horizon: Option<u64>, limit: Option<u64>) -> Option<usize> {
        choose(&T14, history, horizon, limit).state
    }

    #[test]
    fn a_cpu_with_nothing_due_and_no_bound_waits_deepest() {
        assert_eq!(pick(&History::default(), None, None), Some(1));
    }

    #[test]
    fn the_timer_horizon_admits_only_what_fits_before_it() {
        let h = History::default();
        assert_eq!(pick(&h, Some(5000 * US), None), Some(1));
        assert_eq!(pick(&h, Some(2096 * US), None), Some(1));
        assert_eq!(pick(&h, Some(2095 * US), None), Some(0));
        assert_eq!(pick(&h, Some(506 * US), None), Some(0));
        assert_eq!(pick(&h, Some(505 * US), None), None);
    }

    #[test]
    fn a_held_bound_excludes_every_state_slower_to_wake_than_it() {
        let h = History::default();
        assert_eq!(pick(&h, None, Some(1048 * US)), Some(1));
        assert_eq!(pick(&h, None, Some(1047 * US)), Some(0));
        assert_eq!(pick(&h, None, Some(253 * US)), Some(0));
        assert_eq!(pick(&h, None, Some(252 * US)), None);
    }

    #[test]
    fn a_state_entered_before_a_timer_wakes_early_by_its_exit_and_without_one_does_not() {
        let h = History::default();
        assert_eq!(choose(&T14, &h, Some(5000 * US), None), Choice { state: Some(1), wake_early_by: 1048 * US, revisit_after: None });
        assert_eq!(choose(&T14, &h, None, None), Choice { state: Some(1), wake_early_by: 0, revisit_after: None });
        assert_eq!(choose(&T14, &h, Some(100 * US), None), Choice { state: None, wake_early_by: 0, revisit_after: None }, "C1 owes no early wake");
    }

    /// IPC ping-pong: every idle ends in a few microseconds, by a kick, with
    /// no timer armed. Without the history the CPU would pay the deepest exit
    /// on each wake.
    #[test]
    fn short_unforeseen_idles_bring_the_cpu_to_c1_within_the_allowance() {
        let mut h = History::default();
        let mut deep = 0;
        for _ in 0..2 * WINDOW {
            if pick(&h, None, None).is_some() {
                deep += 1;
            }
            h.unforeseen(&T14, 10 * US);
        }
        assert_eq!(deep, EARLY_ALLOWED + 1, "a CPU turned to short idles pays at most the allowance and one more deep exit");
        assert_eq!(pick(&h, None, None), None);
    }

    #[test]
    fn idles_between_the_two_residencies_settle_on_the_shallower_state() {
        let mut h = History::default();
        for _ in 0..WINDOW {
            h.unforeseen(&T14, 1000 * US);
        }
        assert_eq!(pick(&h, None, None), Some(0));
    }

    #[test]
    fn long_idles_after_short_ones_return_the_cpu_to_the_deepest_state() {
        let mut h = History::default();
        for _ in 0..WINDOW {
            h.unforeseen(&T14, 10 * US);
        }
        assert_eq!(pick(&h, None, None), None);
        let mut waited = 0;
        while pick(&h, None, None) != Some(1) {
            h.unforeseen(&T14, 10_000 * US);
            waited += 1;
        }
        assert_eq!(waited, WINDOW - EARLY_ALLOWED);
    }

    #[test]
    fn no_declared_state_is_c1() {
        assert_eq!(choose(&[], &History::default(), None, None), Choice { state: None, wake_early_by: 0, revisit_after: None });
    }

    /// Only the history keeps a CPU from a state the horizon and the bound
    /// allow, so only then does it come back to look; the horizon and the
    /// bound are exact and need no second look.
    #[test]
    fn a_cpu_held_shallow_by_its_history_alone_revisits_after_the_deeper_states_residency() {
        let mut short = History::default();
        for _ in 0..WINDOW {
            short.unforeseen(&T14, 10 * US);
        }
        assert_eq!(choose(&T14, &short, None, None).revisit_after, Some(506 * US), "the next deeper state is C2");
        assert_eq!(choose(&T14, &short, Some(1000 * US), None).revisit_after, Some(506 * US), "C2 alone fits the horizon");
        assert_eq!(choose(&T14, &short, None, Some(300 * US)).revisit_after, Some(506 * US), "C2 alone fits the bound");
        assert_eq!(choose(&T14, &History::default(), Some(1000 * US), None).revisit_after, None, "the horizon kept it from C3, not the history");
        assert_eq!(choose(&T14, &short, Some(100 * US), None).revisit_after, None);
    }

    /// A CPU that was busy and then got nothing more to do is woken only by
    /// its revisits: it climbs a state per revisit, C2 after C2's residency
    /// and C3 after C3's, and then stays.
    #[test]
    fn a_cpu_left_idle_after_short_idles_climbs_a_state_per_revisit() {
        let mut h = History::default();
        for _ in 0..WINDOW {
            h.unforeseen(&T14, 10 * US);
        }
        let mut path = std::vec::Vec::new();
        loop {
            let choice = choose(&T14, &h, None, None);
            path.push(choice.state);
            let Some(after) = choice.revisit_after else { break };
            h.quiet(&T14, after);
        }
        assert_eq!(path, [None, Some(0), Some(1)]);
    }

    /// A revisit clears only what it covered: a period of C2's residency says
    /// nothing of an idle as long as C3's.
    #[test]
    fn a_quiet_period_clears_the_states_it_covers_and_no_deeper() {
        let mut h = History::default();
        for _ in 0..WINDOW {
            h.unforeseen(&T14, 10 * US);
        }
        h.quiet(&T14, 506 * US);
        assert_eq!(choose(&T14, &h, None, None).state, Some(0));
        assert_eq!(choose(&T14, &h, None, None).revisit_after, Some(2096 * US));
    }

    #[test]
    fn a_period_is_ended_by_the_deadline_the_revisit_or_neither() {
        let held = Choice { state: None, wake_early_by: 0, revisit_after: Some(506 * US) };
        assert_eq!(ended(&held, Some(10_000 * US), 0, 10_000 * US), Ended::Deadline);
        assert_eq!(ended(&held, Some(10_000 * US), 0, 506 * US), Ended::Revisit);
        assert_eq!(ended(&held, None, 0, 506 * US - FORESEEN_SLACK), Ended::Revisit, "a tick short");
        assert_eq!(ended(&held, None, 0, 300 * US), Ended::Unforeseen);
        let deep = Choice { state: Some(1), wake_early_by: 1048 * US, revisit_after: None };
        assert_eq!(ended(&deep, Some(10_000 * US), 0, 8952 * US), Ended::Deadline, "the early wake is the deadline's");
        assert_eq!(ended(&deep, None, 0, 9_000 * US), Ended::Unforeseen);
    }

    #[test]
    fn a_revisit_a_tick_early_clears_the_state_it_was_armed_for() {
        let mut h = History::default();
        for _ in 0..WINDOW {
            h.unforeseen(&T14, 10 * US);
        }
        let choice = choose(&T14, &h, None, None);
        assert_eq!((choice.state, choice.revisit_after), (None, Some(506 * US)));
        let plan = Plan { choice, exit: 0, declaration: 1, chosen_at: 0, deadline: None };
        plan.learn(&mut h, &T14, 506 * US - FORESEEN_SLACK);
        assert_eq!(pick(&h, None, None), Some(0), "C2 is admitted again, C3 still learns");
    }

    #[test]
    fn an_unforeseen_wake_is_learned_and_the_deadline_is_not() {
        let fresh = History::default();
        let choice = choose(&T14, &fresh, Some(10_000 * US), None);
        let plan = Plan { choice, exit: 1048 * US, declaration: 1, chosen_at: 0, deadline: Some(10_000 * US) };
        let mut h = fresh;
        plan.learn(&mut h, &T14, 10 * US);
        assert_eq!(h, History::from_bits(0x0101), "a 10 µs wake came sooner than both residencies");
        let mut h = fresh;
        plan.learn(&mut h, &T14, 10_000 * US - 1048 * US);
        assert_eq!(h, fresh, "the deadline, pulled in by the exit, is the scheduler's own");
    }

    #[test]
    fn the_least_bound_held_is_the_machines_and_goes_with_its_last_hold() {
        let mut b = Bounds::default();
        assert_eq!(b.least(), None);
        b.hold(1_450 * US);
        b.hold(300 * US);
        b.hold(300 * US);
        assert_eq!(b.least(), Some(300 * US));
        b.release(300 * US);
        assert_eq!(b.least(), Some(300 * US));
        b.release(300 * US);
        assert_eq!(b.least(), Some(1_450 * US));
        b.release(1_450 * US);
        assert_eq!(b.least(), None);
    }

    #[test]
    fn the_timer_fires_at_the_deadline_less_the_exit_or_the_revisit_whichever_is_sooner() {
        let deep = Choice { state: Some(1), wake_early_by: 1048 * US, revisit_after: None };
        assert_eq!(deadline_fires(&deep, Some(10_000 * US)), Some(8952 * US));
        assert_eq!(wake_at(&deep, Some(10_000 * US), 0), Some(8952 * US));
        let held = Choice { state: Some(0), wake_early_by: 253 * US, revisit_after: Some(2096 * US) };
        assert_eq!(wake_at(&held, Some(10_000 * US), 1000 * US), Some(3096 * US), "the revisit is sooner");
        assert_eq!(wake_at(&held, Some(2_000 * US), 1000 * US), Some(1747 * US), "the deadline is sooner");
        let c1 = Choice { state: None, wake_early_by: 0, revisit_after: Some(506 * US) };
        assert_eq!(wake_at(&c1, None, 0), Some(506 * US), "a CPU with no timer still comes back to look");
        assert_eq!(deadline_fires(&c1, Some(10_000 * US)), Some(10_000 * US));
        assert_eq!(wake_at(&c1, Some(300 * US), 0), None, "a revisit after the deadline would move the timer later");
        let plain = Choice { state: None, wake_early_by: 0, revisit_after: None };
        assert_eq!(wake_at(&plain, Some(10_000 * US), 0), None, "nothing sooner than the scheduler armed");
    }

    #[test]
    fn a_wake_is_the_schedulers_timer_from_a_tick_before_it_on_and_unforeseen_before_that() {
        let fires = Some(5_000 * US);
        assert!(foreseen(fires, 5_000 * US));
        assert!(foreseen(fires, 6_000 * US));
        assert!(foreseen(fires, 5_000 * US - FORESEEN_SLACK), "a one-shot that counted down a tick short");
        assert!(!foreseen(fires, 5_000 * US - FORESEEN_SLACK - 1));
        assert!(!foreseen(fires, 100 * US), "a kick long before the deadline");
        assert!(!foreseen(None, 5_000 * US), "with no timer armed, every wake is unforeseen");
    }

    #[test]
    #[should_panic(expected = "released and never held")]
    fn releasing_a_bound_never_held_is_a_kernel_bug() {
        Bounds::default().release(1);
    }
}
