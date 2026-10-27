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
//! turbo counts a core in C1 as active costs every other core its clock.

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

    /// The states a new declaration replaced are not its states: what was
    /// learned of them says nothing about these.
    pub fn forget(&mut self) {
        *self = Self::default();
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
    /// alone kept the CPU from it: how soon to wake and choose again, that
    /// state's residency. A wake then is a period that long, which the history
    /// learns as one.
    pub revisit_after: Option<u64>,
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
    let deepest_permitted = states.clone().rev().find(|&s| permitted(s));
    let state = states.rev().find(|&s| permitted(s) && history.admits(s));
    Choice {
        state,
        wake_early_by: match (state, horizon) {
            (Some(state), Some(_)) => costs[state].exit,
            _ => 0,
        },
        revisit_after: deepest_permitted.filter(|&deepest| Some(deepest) != state).map(|deepest| costs[deepest].residency),
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
    fn a_new_declaration_starts_with_nothing_learned() {
        let mut h = History::default();
        for _ in 0..WINDOW {
            h.unforeseen(&T14, 10 * US);
        }
        h.forget();
        assert_eq!(pick(&h, None, None), Some(1));
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
        assert_eq!(choose(&T14, &short, None, None).revisit_after, Some(2096 * US));
        assert_eq!(choose(&T14, &short, Some(1000 * US), None).revisit_after, Some(506 * US), "C2 alone fits the horizon");
        assert_eq!(choose(&T14, &short, None, Some(300 * US)).revisit_after, Some(506 * US), "C2 alone fits the bound");
        assert_eq!(choose(&T14, &History::default(), Some(1000 * US), None).revisit_after, None, "the horizon kept it from C3, not the history");
        assert_eq!(choose(&T14, &short, Some(100 * US), None).revisit_after, None);
    }

    /// A CPU that was busy and then got nothing more to do is woken only by
    /// its revisits, each a period of the residency it was asked to look
    /// after: it reaches the deepest state within the window less the
    /// allowance, and then stays.
    #[test]
    fn a_cpu_left_idle_after_short_idles_reaches_the_deepest_state_on_its_revisits_alone() {
        let mut h = History::default();
        for _ in 0..WINDOW {
            h.unforeseen(&T14, 10 * US);
        }
        let mut revisits = 0;
        loop {
            let choice = choose(&T14, &h, None, None);
            let Some(after) = choice.revisit_after else { break };
            h.unforeseen(&T14, after);
            revisits += 1;
        }
        assert_eq!(choose(&T14, &h, None, None).state, Some(1));
        assert_eq!(revisits, WINDOW - EARLY_ALLOWED);
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
    #[should_panic(expected = "released and never held")]
    fn releasing_a_bound_never_held_is_a_kernel_bug() {
        Bounds::default().release(1);
    }
}
