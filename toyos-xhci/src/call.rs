//! What one disk call may still spend on a controller once its transport has
//! broken, rung by rung.
//!
//! A break is followed, inside the same call, by the recovery ladder
//! ([`crate::ladder`]): commands, control requests, a port reset's settle and a
//! verification, each a spin with its own timeout — and the caller holds the
//! controller's lock with interrupts off for all of them. Summed, those
//! timeouts are no bound anyone stated. [`AfterBreak`] is the bound, and it is
//! one per rung: [`Bounds`] names what the wait that broke and each rung may
//! spend, the call ends at their sum, and **every wait is clipped to where the
//! rungs still ahead of it begin**. So a rung is never entered with less than
//! its own bound, whatever the rungs and the re-issued commands before it
//! spent, and the last rung — the one that leaves the device reset — always
//! runs.
//!
//! **A controller that left a command unanswered is sent no other in the same
//! call.** Its command ring is one queue: whatever follows waits behind the
//! command it did not answer, and costs a whole timeout to say so again. A
//! port reset is a register write and is still made.

use crate::ladder::Rung;

/// Nanoseconds since boot.
pub type Nanos = u64;

/// Why a step was not taken.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NotTaken {
    /// What this part of the call may spend is spent.
    Spent,
    /// An earlier command of this call got no answer.
    ControllerSilent,
}

impl core::fmt::Display for NotTaken {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Spent => "the bound on this part of the call is spent",
            Self::ControllerSilent => "the controller left an earlier command of this call unanswered",
        })
    }
}

/// What each part of a call whose transport broke may spend.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Bounds {
    /// The wait that broke, and every command re-issued after a rung that
    /// took, together.
    pub wait: Nanos,
    pub class_reset: Nanos,
    pub port_reset: Nanos,
    pub offline: Nanos,
}

/// What one disk call may spin for once its transport has broken, part by part:
/// the kernel's, and the one declaration a test times a call against.
///
/// A rung's bound is what its commands, requests and reset cost a device that
/// answers, and the rest of it is how long a TEST UNIT READY nothing answers is
/// waited for. `wait` is the driver's own timeout on one command, which the
/// kernel asserts.
pub const AFTER_BREAK: Bounds = Bounds {
    wait: 2_000_000_000,
    class_reset: 750_000_000,
    port_reset: 1_500_000_000,
    offline: 500_000_000,
};

impl Bounds {
    /// Everything the call may spin for from the start of the wait that broke.
    pub const fn whole(&self) -> Nanos {
        self.wait + self.class_reset + self.port_reset + self.offline
    }

    const fn of(&self, rung: Rung) -> Nanos {
        match rung {
            Rung::ClassReset => self.class_reset,
            Rung::PortReset => self.port_reset,
            Rung::Offline => self.offline,
        }
    }

    /// What the rungs above `rung` are owed.
    const fn above(&self, rung: Rung) -> Nanos {
        match rung {
            Rung::ClassReset => self.port_reset + self.offline,
            Rung::PortReset => self.offline,
            Rung::Offline => 0,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Open {
    bounds: Bounds,
    /// Where the call ends: the start of the wait that broke plus
    /// [`Bounds::whole`].
    call_ends: Nanos,
    /// Where a wait started now is clipped.
    window_ends: Nanos,
}

/// One call's bounds after a break; [`Self::CLOSED`] between calls and in a
/// call whose transport has not broken, where it clips and refuses nothing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AfterBreak {
    open: Option<Open>,
    silent: bool,
}

impl AfterBreak {
    pub const CLOSED: Self = Self { open: None, silent: false };

    /// The transport broke on a wait that began at `wait_began`. The first
    /// break of a call opens it and a later one does not extend it. Until a
    /// rung is entered every wait ends where the first rung's bound begins.
    pub fn open(&mut self, wait_began: Nanos, bounds: Bounds) {
        if self.open.is_none() {
            let call_ends = wait_began.saturating_add(bounds.whole());
            let window_ends = call_ends - bounds.of(Rung::ClassReset) - bounds.above(Rung::ClassReset);
            self.open = Some(Open { bounds, call_ends, window_ends });
        }
    }

    /// `rung` begins at `now`: its waits get its own bound, and never reach
    /// into what the rungs above it are owed.
    pub fn enter(&mut self, rung: Rung, now: Nanos) {
        if let Some(open) = &mut self.open {
            let own = now.saturating_add(open.bounds.of(rung));
            open.window_ends = own.min(open.call_ends - open.bounds.above(rung));
        }
    }

    /// `rung` took and the caller's command goes out again: its waits end
    /// where the rungs above `rung` begin.
    pub fn took(&mut self, rung: Rung) {
        if let Some(open) = &mut self.open {
            open.window_ends = open.call_ends - open.bounds.above(rung);
        }
    }

    /// When a wait starting at `now`, with `own` as its own timeout, gives up.
    pub fn wait_ends(&self, now: Nanos, own: Nanos) -> Nanos {
        let own = now.saturating_add(own);
        self.open.map_or(own, |open| own.min(open.window_ends))
    }

    /// How long a wait starting at `now` may spin: nothing, once its window
    /// has ended, however long ago that was.
    pub fn wait_left(&self, now: Nanos, own: Nanos) -> Nanos {
        self.wait_ends(now, own).saturating_sub(now)
    }

    /// Whether a wait that gave up at `now` was cut short by its window and
    /// not by its own timeout.
    pub fn cut(&self, began: Nanos, now: Nanos, own: Nanos) -> bool {
        self.open.is_some_and(|open| now >= open.window_ends && now < began.saturating_add(own))
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
        match self.open {
            Some(open) if now >= open.window_ends => Err(NotTaken::Spent),
            _ => Ok(()),
        }
    }

    /// A command got no answer.
    pub fn unanswered(&mut self) {
        self.silent = true;
    }

    /// The disk this call is on is held for its device to come back
    /// ([`crate::identity`]), as found at `now`: until when the call may wait
    /// for the verdict. A call whose transport had not broken opens here, as if
    /// a wait had broken now.
    ///
    /// **The hold is part of the call, not an addition to it.** It ends where
    /// the last rung begins, and the command sent again on a device that came
    /// back is clipped there too, as after a port rung that took: a break of it
    /// climbs on what is left, and the call still ends at its bounds' sum. The
    /// wait for a returning device never binds it; that is the port machine's,
    /// wherever it runs.
    pub fn hold(&mut self, now: Nanos, bounds: Bounds) -> Nanos {
        self.open(now, bounds);
        self.took(Rung::PortReset);
        self.open.map_or(now, |open| open.window_ends)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: Nanos = 1_000_000_000;
    const OWN: Nanos = 2 * SECOND;
    const BOUNDS: Bounds = Bounds {
        wait: 2 * SECOND,
        class_reset: SECOND,
        port_reset: 3 * SECOND / 2,
        offline: SECOND / 4,
    };
    const RUNGS: [Rung; 3] = [Rung::ClassReset, Rung::PortReset, Rung::Offline];

    #[test]
    fn a_call_whose_transport_has_not_broken_is_clipped_and_refused_nothing() {
        let mut call = AfterBreak::CLOSED;
        assert_eq!(call.wait_ends(10 * SECOND, OWN), 12 * SECOND);
        assert_eq!(call.wait_left(10 * SECOND, OWN), OWN);
        assert_eq!(call.command(u64::MAX), Ok(()));
        assert_eq!(call.request(u64::MAX), Ok(()));
        call.enter(Rung::Offline, 0);
        call.took(Rung::ClassReset);
        assert_eq!(call, AfterBreak::CLOSED, "a rung of a call that never broke opens nothing");
    }

    /// Every rung is spun to the end of its window, the re-issues between them
    /// too, and each rung is still entered with the whole of its own bound.
    #[test]
    fn a_rung_is_never_starved_by_what_was_spent_before_it() {
        let began = 100 * SECOND;
        let mut call = AfterBreak::CLOSED;
        // The wait that broke ran out its own timeout.
        let mut now = began + OWN;
        call.open(began, BOUNDS);
        for rung in RUNGS {
            call.enter(rung, now);
            assert_eq!(call.wait_left(now, u64::MAX), BOUNDS.of(rung), "{rung:?} is owed its whole bound");
            assert_eq!(call.command(now), Ok(()));
            // Spun to the end of its window.
            now = call.wait_ends(now, u64::MAX);
            assert_eq!(call.request(now), Err(NotTaken::Spent));
            // It is called taken, and the command that goes out again spins
            // for all it may.
            call.took(rung);
            now = call.wait_ends(now, u64::MAX);
        }
        assert_eq!(now, began + BOUNDS.whole(), "and the call ends where its bounds sum to");
    }

    /// A rung entered early keeps its own bound and no more: what it leaves
    /// goes to nobody.
    #[test]
    fn a_rung_entered_early_gets_its_own_bound_and_not_the_calls() {
        let mut call = AfterBreak::CLOSED;
        call.open(0, BOUNDS);
        call.enter(Rung::ClassReset, 0);
        assert_eq!(call.wait_ends(0, u64::MAX), BOUNDS.class_reset);
        call.enter(Rung::PortReset, SECOND / 2);
        assert_eq!(call.wait_ends(SECOND / 2, u64::MAX), SECOND / 2 + BOUNDS.port_reset);
        call.enter(Rung::Offline, SECOND);
        assert_eq!(call.wait_ends(SECOND, u64::MAX), SECOND + BOUNDS.offline);
    }

    /// A call that begins on a device already a rung up — its breaks are
    /// counted across calls — enters that rung first and finds it whole.
    #[test]
    fn a_call_that_enters_the_ladder_a_rung_up_finds_that_rung_whole() {
        for (at, rung) in RUNGS.into_iter().enumerate() {
            let mut call = AfterBreak::CLOSED;
            call.open(0, BOUNDS);
            let now = OWN;
            call.enter(rung, now);
            assert_eq!(call.wait_left(now, u64::MAX), BOUNDS.of(rung), "rung {at}");
        }
    }

    /// A command re-issued after a rung that took may not reach into the rungs
    /// its own break would climb.
    #[test]
    fn a_command_sent_again_is_clipped_to_where_the_next_rung_begins() {
        let mut call = AfterBreak::CLOSED;
        call.open(0, BOUNDS);
        call.enter(Rung::ClassReset, 0);
        call.took(Rung::ClassReset);
        assert_eq!(call.wait_ends(0, u64::MAX), BOUNDS.whole() - BOUNDS.port_reset - BOUNDS.offline);
        call.took(Rung::PortReset);
        assert_eq!(call.wait_ends(0, u64::MAX), BOUNDS.whole() - BOUNDS.offline);
        // Inside that, a wait keeps its own timeout.
        assert_eq!(call.wait_ends(SECOND, SECOND), 2 * SECOND);
    }

    /// A silent controller is refused by that name in every rung, with bound
    /// left: the next command would queue behind the one it did not answer. A
    /// register wait — the port reset's — is not a command.
    #[test]
    fn a_silent_controller_is_sent_no_command_in_any_rung_and_its_port_is_still_reset() {
        let mut call = AfterBreak::CLOSED;
        call.open(0, BOUNDS);
        call.enter(Rung::ClassReset, 0);
        call.unanswered();
        for rung in RUNGS {
            call.enter(rung, SECOND);
            assert_eq!(call.command(SECOND), Err(NotTaken::ControllerSilent), "{rung:?}");
            assert_eq!(call.request(SECOND), Ok(()), "{rung:?}");
        }
    }

    #[test]
    fn a_second_break_in_the_same_call_does_not_extend_it() {
        let mut call = AfterBreak::CLOSED;
        call.open(0, BOUNDS);
        call.open(3 * SECOND, BOUNDS);
        call.enter(Rung::Offline, 0);
        call.took(Rung::Offline);
        assert_eq!(call.wait_ends(0, u64::MAX), BOUNDS.whole());
    }

    /// What a disk call does once its transport broke or its disk was found
    /// held, as far as time goes: every wait, rung, hold and command sent again
    /// the driver makes is one of these.
    #[derive(Clone, Copy, Debug)]
    enum Does {
        Enter(Rung),
        Took(Rung),
        /// Waits for a returning device to the end of what the hold allows.
        Hold,
        /// A wait that spins to the end of its window, whatever its own timeout.
        Spin,
    }

    const EVERYTHING: [Does; 8] = [
        Does::Enter(Rung::ClassReset),
        Does::Enter(Rung::PortReset),
        Does::Enter(Rung::Offline),
        Does::Took(Rung::ClassReset),
        Does::Took(Rung::PortReset),
        Does::Took(Rung::Offline),
        Does::Hold,
        Does::Spin,
    ];

    /// Every path a call can take, each step spun to its worst case, ends
    /// inside the bounds' sum from where it opened: a break, a ladder, a hold
    /// for a device that left, the command sent again on the one that came
    /// back, a break of that and the rungs after it, and a hold again. This is
    /// the whole of what a call spins for with `IF` clear once it has broken or
    /// found its disk held, so it is what the kernel's `CALL_AFTER_BREAK` is
    /// held under the TLB-ack tripwire for.
    #[test]
    fn every_path_through_a_call_ends_inside_its_bounds() {
        const STEPS: usize = 6;
        let began = 100 * SECOND;
        let whole = BOUNDS.whole();
        // Where a call opens: a wait that broke at once, one that ran out its
        // own timeout, and a call that found its disk already held.
        for start in 0..3 {
            let mut path = [0usize; STEPS];
            loop {
                let mut call = AfterBreak::CLOSED;
                let mut now = began;
                match start {
                    0 => call.open(began, BOUNDS),
                    1 => {
                        call.open(began, BOUNDS);
                        now = began + OWN;
                    }
                    _ => {
                        let ends = call.hold(now, BOUNDS);
                        assert_eq!(ends, began + whole - BOUNDS.offline, "a hold opens a call");
                        now = ends;
                    }
                }
                let steps = path.map(|s| EVERYTHING[s]);
                for (at, &does) in steps.iter().enumerate() {
                    let taken = &steps[..=at];
                    match does {
                        Does::Enter(rung) => call.enter(rung, now),
                        Does::Took(rung) => call.took(rung),
                        Does::Hold => {
                            let ends = call.hold(now, BOUNDS);
                            assert!(ends <= began + whole - BOUNDS.offline, "start {start}, {taken:?}");
                            now = now.max(ends);
                        }
                        Does::Spin => now = now.max(call.wait_ends(now, u64::MAX)),
                    }
                    assert!(now <= began + whole, "start {start}, {taken:?}: {now} past {}", began + whole);
                    assert!(call.wait_ends(now, u64::MAX) <= began + whole, "start {start}, {taken:?}");
                }
                // The next path, as a number in base `EVERYTHING.len()`.
                let Some(carry) = path.iter().position(|&s| s + 1 < EVERYTHING.len()) else { break };
                path[carry] += 1;
                path[..carry].fill(0);
            }
        }
    }

    /// The command sent again on a device that came back may not reach into
    /// what the last rung is owed, and a break of it still finds that rung.
    #[test]
    fn a_command_sent_again_after_a_hold_leaves_the_last_rung_its_bound() {
        let mut call = AfterBreak::CLOSED;
        call.open(0, BOUNDS);
        call.enter(Rung::PortReset, OWN);
        // The device left at once, and came back at once.
        let ends = call.hold(OWN, BOUNDS);
        assert_eq!(ends, BOUNDS.whole() - BOUNDS.offline);
        assert_eq!(call.wait_ends(OWN, u64::MAX), ends, "the command sent again spins to the hold's end");
        // It broke there: the rungs below the last find nothing left.
        call.enter(Rung::ClassReset, ends);
        assert_eq!(call.request(ends), Err(NotTaken::Spent));
        call.enter(Rung::Offline, ends);
        assert_eq!(call.wait_left(ends, u64::MAX), BOUNDS.offline, "and the last one its whole bound");
        assert_eq!(call.wait_ends(ends, u64::MAX), BOUNDS.whole());
    }

    /// A wait inside its window keeps its own timeout, and one that ends on it
    /// is not called cut.
    #[test]
    fn a_wait_is_cut_only_when_its_window_ended_it() {
        let mut call = AfterBreak::CLOSED;
        call.open(0, BOUNDS);
        call.enter(Rung::PortReset, 0);
        assert_eq!(call.wait_ends(0, SECOND), SECOND);
        assert!(!call.cut(0, SECOND, SECOND));
        assert_eq!(call.wait_ends(SECOND, OWN), BOUNDS.port_reset);
        assert!(call.cut(SECOND, BOUNDS.port_reset, OWN));
        assert!(!AfterBreak::CLOSED.cut(0, OWN, OWN));
    }
}
