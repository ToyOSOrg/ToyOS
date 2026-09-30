//! One CPU's longest interrupts-off and preemption-off windows since its last
//! report: the record the kernel's `mask-windows` build keeps per CPU, feeds
//! at every transition, and prints beside the IRQ census.
//!
//! **An interrupts-off window** runs from the instant a CPU stops taking
//! maskable interrupts to the instant it takes them again: [`Windows::masked`]
//! follows the one and [`Windows::unmasking`] precedes the other.
//!
//! **A preemption-off window** runs while the CPU's preempt count is above
//! zero. A scheduler pass ends it and starts the next ([`Windows::scheduled`]),
//! because a pass is where a waiting thread gets the CPU, and the idle halt
//! inside a pass is no window ([`Windows::halting`], [`Windows::woken`]),
//! because a wake ends it. [`Windows::raised`] follows every raise of the count
//! by one and [`Windows::lowering`] precedes every lowering by one; each acts
//! only on a crossing of zero.
//!
//! **Every transition is checked against the state it finds**, and one that
//! could not have followed it is [`Unseen`]: a transition before it that
//! nothing reported. The record is written by its CPU alone and [`Windows::take`]
//! empties it from any. Nothing here reads a clock: `now` is the caller's
//! counter, read only by a transition that needs it.

use core::fmt;

use crate::sync::{AtomicU64, Ordering};

/// A CPU that has not joined the scheduler, or that stopped being tracked:
/// every transition is accepted and records nothing.
const UNTRACKED: u64 = u64::MAX;

/// One CPU's windows. The counter is never zero once firmware has run, so a
/// stamp of zero is no open window.
pub struct Windows {
    /// [`UNTRACKED`], 0 while interrupts are open, else the counter at which they were masked.
    irqs_off: AtomicU64,
    /// 0 while the preempt count is zero or the CPU halts inside a pass, else
    /// the counter at which the count left zero or a pass or a wake last ran.
    preempt_off: AtomicU64,
    irqs_longest: AtomicU64,
    preempt_longest: AtomicU64,
}

/// A transition the record says could not have happened, named by the one
/// before it that nothing reported.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unseen {
    /// Masked, with a window open.
    AnUnmask,
    /// Unmasking, or interrupted with interrupts masked, with no window open.
    AMask,
    /// The count left zero, or a halt ended, with a window open.
    ALowering,
    /// The count returned to zero, a pass ran, or a halt began, with no window open.
    ARaise,
}

impl fmt::Display for Unseen {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Unseen::AnUnmask => "masked interrupts with a window open: an unmask reached no hook",
            Unseen::AMask => "found interrupts masked with no window open: a mask reached no hook",
            Unseen::ALowering => {
                "left a preempt count of zero with a window open: a lowering reached no hook"
            }
            Unseen::ARaise => {
                "ended a preemption-off window that was never opened: a raise reached no hook"
            }
        })
    }
}

impl Windows {
    /// No `Default` beside it, for `fair::Frontier::new`'s reason: every
    /// record is a `static`, which only a `const fn` can build.
    #[cfg(not(feature = "loom"))]
    #[allow(clippy::new_without_default)]
    pub const fn new() -> Self {
        Self {
            irqs_off: AtomicU64::new(UNTRACKED),
            preempt_off: AtomicU64::new(0),
            irqs_longest: AtomicU64::new(0),
            preempt_longest: AtomicU64::new(0),
        }
    }

    // Loom's atomics have no const constructor, so this arm alone drops `const`.
    #[cfg(feature = "loom")]
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            irqs_off: AtomicU64::new(UNTRACKED),
            preempt_off: AtomicU64::new(0),
            irqs_longest: AtomicU64::new(0),
            preempt_longest: AtomicU64::new(0),
        }
    }

    fn tracked(&self) -> bool {
        self.irqs_off.load(Ordering::Relaxed) != UNTRACKED
    }

    fn record(longest: &AtomicU64, since: u64, now: u64) {
        let span = now.saturating_sub(since);
        if span > longest.load(Ordering::Relaxed) {
            longest.fetch_max(span, Ordering::Relaxed);
        }
    }

    /// The CPU joins the scheduler with interrupts masked and its preempt
    /// count at `count`, and is tracked from here.
    pub fn start(&self, count: u32, now: u64) {
        self.preempt_off.store(if count == 0 { 0 } else { now }, Ordering::Relaxed);
        self.irqs_off.store(now, Ordering::Relaxed);
    }

    /// Nothing more is tracked or checked: what a caller does before it
    /// reports an [`Unseen`], so its own masking finds nothing to check.
    pub fn stop(&self) {
        self.irqs_off.store(UNTRACKED, Ordering::Relaxed);
    }

    /// Interrupts were open and have just been masked.
    pub fn masked(&self, now: impl FnOnce() -> u64) -> Result<(), Unseen> {
        match self.irqs_off.load(Ordering::Relaxed) {
            UNTRACKED => Ok(()),
            0 => {
                self.irqs_off.store(now(), Ordering::Relaxed);
                Ok(())
            }
            _ => Err(Unseen::AnUnmask),
        }
    }

    /// The CPU was interrupted with interrupts masked.
    pub fn found_masked(&self) -> Result<(), Unseen> {
        match self.irqs_off.load(Ordering::Relaxed) {
            0 => Err(Unseen::AMask),
            _ => Ok(()),
        }
    }

    /// Interrupts are masked and about to be opened.
    pub fn unmasking(&self, now: impl FnOnce() -> u64) -> Result<(), Unseen> {
        match self.irqs_off.load(Ordering::Relaxed) {
            UNTRACKED => Ok(()),
            0 => Err(Unseen::AMask),
            since => {
                Self::record(&self.irqs_longest, since, now());
                self.irqs_off.store(0, Ordering::Relaxed);
                Ok(())
            }
        }
    }

    /// The preempt count has just been raised by one, to `count`.
    pub fn raised(&self, count: u32, now: impl FnOnce() -> u64) -> Result<(), Unseen> {
        if count != 1 || !self.tracked() {
            return Ok(());
        }
        self.open_preempt(now)
    }

    /// The preempt count, at `count`, is about to be lowered by one.
    pub fn lowering(&self, count: u32, now: impl FnOnce() -> u64) -> Result<(), Unseen> {
        if count != 1 || !self.tracked() {
            return Ok(());
        }
        self.close_preempt(now())
    }

    /// The preempt count has just been set from `old` to `new` whole.
    pub fn set(&self, old: u32, new: u32, now: impl FnOnce() -> u64) -> Result<(), Unseen> {
        if !self.tracked() {
            return Ok(());
        }
        match (old, new) {
            (0, 0) => Ok(()),
            (0, _) => self.open_preempt(now),
            (_, 0) => self.close_preempt(now()),
            _ => Ok(()),
        }
    }

    /// A scheduler pass has decided what runs here: the window it ran in
    /// ends, and the next starts, since the count is still raised.
    pub fn scheduled(&self, now: impl FnOnce() -> u64) -> Result<(), Unseen> {
        if !self.tracked() {
            return Ok(());
        }
        let now = now();
        self.close_preempt(now)?;
        self.preempt_off.store(now, Ordering::Relaxed);
        Ok(())
    }

    /// The CPU halts inside a pass, waiting for whatever runs next.
    pub fn halting(&self, now: impl FnOnce() -> u64) -> Result<(), Unseen> {
        if !self.tracked() {
            return Ok(());
        }
        self.close_preempt(now())
    }

    /// The halt [`Windows::halting`] began has ended, still inside its pass.
    pub fn woken(&self, now: impl FnOnce() -> u64) -> Result<(), Unseen> {
        if !self.tracked() {
            return Ok(());
        }
        self.open_preempt(now)
    }

    fn open_preempt(&self, now: impl FnOnce() -> u64) -> Result<(), Unseen> {
        if self.preempt_off.load(Ordering::Relaxed) != 0 {
            return Err(Unseen::ALowering);
        }
        self.preempt_off.store(now(), Ordering::Relaxed);
        Ok(())
    }

    fn close_preempt(&self, now: u64) -> Result<(), Unseen> {
        match self.preempt_off.load(Ordering::Relaxed) {
            0 => Err(Unseen::ARaise),
            since => {
                Self::record(&self.preempt_longest, since, now);
                self.preempt_off.store(0, Ordering::Relaxed);
                Ok(())
            }
        }
    }

    /// The longest interrupts-off and preemption-off windows closed since the
    /// last take, in counter ticks; the next take starts from none.
    pub fn take(&self) -> (u64, u64) {
        (
            self.irqs_longest.swap(0, Ordering::Relaxed),
            self.preempt_longest.swap(0, Ordering::Relaxed),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{Unseen, Windows};

    /// A record tracked from `at` with interrupts open and the count at zero,
    /// as a CPU stands once its start's guard has let go.
    fn open_at(at: u64) -> Windows {
        let w = Windows::new();
        w.start(0, at);
        w.unmasking(|| at).unwrap();
        w.take();
        w
    }

    #[test]
    fn an_interrupts_off_window_is_mask_to_unmask_and_the_longest_is_taken_once() {
        let w = open_at(1);
        w.masked(|| 10).unwrap();
        w.unmasking(|| 25).unwrap();
        w.masked(|| 30).unwrap();
        w.unmasking(|| 34).unwrap();
        assert_eq!(w.take(), (15, 0));
        assert_eq!(w.take(), (0, 0), "a take starts the next report from none");
    }

    #[test]
    fn only_a_crossing_of_zero_opens_or_closes_a_preemption_off_window() {
        let w = open_at(1);
        w.raised(1, || 10).unwrap();
        w.raised(2, || panic!("a raise past one reads no clock")).unwrap();
        w.lowering(2, || panic!("a lowering to one reads no clock")).unwrap();
        w.lowering(1, || 17).unwrap();
        assert_eq!(w.take(), (0, 7));
    }

    #[test]
    fn a_pass_ends_the_preemption_off_window_and_starts_the_next() {
        let w = open_at(1);
        w.raised(1, || 10).unwrap();
        w.scheduled(|| 20).unwrap();
        w.lowering(1, || 23).unwrap();
        assert_eq!(w.take(), (0, 10), "the window is the pass's, not the whole raise");
    }

    #[test]
    fn the_idle_halt_inside_a_pass_is_no_window() {
        let w = open_at(1);
        w.raised(1, || 10).unwrap();
        w.halting(|| 14).unwrap();
        w.woken(|| 1_000).unwrap();
        w.lowering(1, || 1_003).unwrap();
        assert_eq!(w.take(), (0, 4), "the wait is not counted, either side of it is");
    }

    #[test]
    fn a_whole_set_opens_or_closes_only_across_zero() {
        let w = open_at(1);
        w.set(0, 2, || 10).unwrap();
        w.set(2, 3, || panic!("a set between two raised counts reads no clock")).unwrap();
        w.set(3, 0, || 16).unwrap();
        assert_eq!(w.take(), (0, 6));
    }

    #[test]
    fn a_start_counts_from_how_the_cpu_stands() {
        let w = Windows::new();
        w.start(1, 5);
        w.unmasking(|| 9).unwrap();
        w.lowering(1, || 12).unwrap();
        assert_eq!(w.take(), (4, 7));
    }

    #[test]
    fn every_transition_nothing_reported_before_is_named() {
        let w = open_at(1);
        assert_eq!(w.unmasking(|| 2), Err(Unseen::AMask));
        assert_eq!(w.found_masked(), Err(Unseen::AMask));
        w.masked(|| 3).unwrap();
        w.found_masked().unwrap();
        assert_eq!(w.masked(|| 4), Err(Unseen::AnUnmask));

        let w = open_at(1);
        assert_eq!(w.lowering(1, || 2), Err(Unseen::ARaise));
        assert_eq!(w.scheduled(|| 2), Err(Unseen::ARaise));
        assert_eq!(w.halting(|| 2), Err(Unseen::ARaise));
        assert_eq!(w.set(1, 0, || 2), Err(Unseen::ARaise));
        w.raised(1, || 3).unwrap();
        assert_eq!(w.raised(1, || 4), Err(Unseen::ALowering));
        assert_eq!(w.woken(|| 4), Err(Unseen::ALowering));
        assert_eq!(w.set(0, 1, || 4), Err(Unseen::ALowering));
    }

    #[test]
    fn an_untracked_cpu_records_and_refuses_nothing() {
        let w = Windows::new();
        w.masked(|| 1).unwrap();
        w.masked(|| 2).unwrap();
        w.unmasking(|| 3).unwrap();
        w.raised(1, || 4).unwrap();
        w.lowering(1, || 5).unwrap();
        w.lowering(1, || 6).unwrap();
        w.scheduled(|| 7).unwrap();
        assert_eq!(w.take(), (0, 0));

        let w = open_at(1);
        w.masked(|| 2).unwrap();
        w.stop();
        w.masked(|| 3).unwrap();
        assert_eq!(w.take(), (0, 0), "a stopped CPU is untracked again");
    }
}
