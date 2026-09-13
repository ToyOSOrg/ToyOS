//! Every decision the machine's stop makes, and none of its effects.
//!
//! The shutdown path's two claims — every filesystem is synced, and
//! `Rebooting.` is the last thing that happened — are claims about a machine,
//! and they are true only of a machine that has stopped. Stopping it is two
//! questions: *which* thread may still run, and *when* is it over. Both are
//! here; `kernel/src/quiesce.rs` marks the threads and spends the time.
//!
//! **The log is the one carve-out, and it is a stage rather than an
//! exemption.** The last word has to reach a file, and the only writer of that
//! file is a userland process, so a stop that took every process before the
//! last word was written would deadlock on the one process the last word has
//! to reach. So it goes in two: everything but that process, then that process
//! too.

#![no_std]
#![forbid(unsafe_code)]

use core::fmt;

/// How far the stop has gone.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stage {
    /// Every userland thread but the one process the log's durability is owed
    /// to. `keep` is that process; there is no value meaning "none", because a
    /// machine whose log nobody writes is one [`Stage::All`] describes.
    ExceptLog { keep: u32 },
    /// That process too. Nothing in userland runs again.
    All,
}

impl Stage {
    /// Whether a thread of `pid` must stop at its next safe point.
    ///
    /// **`caller` is never stopped**: it is the thread running the stop, and
    /// it has the rest of the shutdown to perform. Its own process is not
    /// exempt — a sibling thread of the process that asked for the reboot
    /// stops like any other, because what the reset must outlast is one
    /// thread's remaining work and not one program's.
    pub fn must_stop(self, pid: u32, tid: u32, caller: (u32, u32)) -> bool {
        if (pid, tid) == caller {
            return false;
        }
        match self {
            Stage::ExceptLog { keep } => pid != keep,
            Stage::All => true,
        }
    }
}

/// What one sweep of the machine's userland threads found.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Sweep {
    /// Threads that must stop and have: banded at a safe point, or parked and
    /// marked so a wake can only band them.
    pub stopped: u32,
    /// Threads that must stop and are still running or runnable. The stop is
    /// not over while this is non-zero.
    pub running: u32,
}

impl Sweep {
    pub fn total(self) -> u32 {
        self.stopped + self.running
    }
}

/// What the caller does next.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Progress {
    /// Every thread that had to stop has. Nothing in userland can enter the
    /// kernel again, so no record written from here on has a userland author.
    Done,
    /// Sweep again.
    Waiting,
    /// The budget is spent and some thread never reached a safe point. **A
    /// record and not a hang**: a machine nobody can turn off is worse than a
    /// reset that lands inside somebody's syscall, which is the state this
    /// whole path is an improvement on rather than a guarantee against.
    Expired,
}

impl Progress {
    /// **`Done` outranks `Expired`.** A sweep that completed the stop on the
    /// very tick the budget ran out stopped the machine, and reporting that as
    /// an expiry would put a degradation in the log that did not happen.
    pub fn of(sweep: Sweep, elapsed_ns: u64, budget_ns: u64) -> Progress {
        if sweep.running == 0 {
            return Progress::Done;
        }
        if elapsed_ns >= budget_ns {
            return Progress::Expired;
        }
        Progress::Waiting
    }
}

/// What the stop did, as the one line it writes beside the boot's other
/// closing censuses.
///
/// Every field is counted rather than derived: `sweeps` is what the poll cost
/// and `elapsed_ms` is what it took, so a park that got slower is visible as a
/// number and not as a boot that feels different.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Record {
    pub sweep: Sweep,
    pub elapsed_ms: u64,
    pub sweeps: u32,
    pub cpus: u32,
    /// Block-device operations still open when the stop ended — the one number
    /// here the stop does not produce itself, and so the one that can disagree
    /// with it. Zero is a machine that really stopped before the shutdown
    /// claimed its filesystems were synced.
    pub in_flight: u32,
}

impl fmt::Display for Record {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "stop: {} of {} userland thread(s) stopped across {} cpu(s) in {} ms over {} sweep(s), \
             {} block operation(s) still open",
            self.sweep.stopped,
            self.sweep.total(),
            self.cpus,
            self.elapsed_ms,
            self.sweeps,
            self.in_flight,
        )?;
        if self.sweep.running > 0 {
            write!(
                f,
                "; this reset lands wherever the other {} are",
                self.sweep.running,
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CALLER: (u32, u32) = (10, 0);
    const LOGD: u32 = 2;

    #[test]
    fn the_caller_is_never_stopped_and_its_siblings_always_are() {
        for stage in [Stage::ExceptLog { keep: LOGD }, Stage::All] {
            assert!(!stage.must_stop(10, 0, CALLER), "{stage:?}");
            assert!(
                stage.must_stop(10, 1, CALLER),
                "a sibling thread of the caller's process is not the caller: {stage:?}",
            );
        }
    }

    #[test]
    fn the_log_is_carved_out_of_the_first_stage_and_not_the_second() {
        let first = Stage::ExceptLog { keep: LOGD };
        assert!(!first.must_stop(LOGD, 0, CALLER));
        assert!(!first.must_stop(LOGD, 3, CALLER), "every thread of it");
        assert!(first.must_stop(LOGD + 1, 0, CALLER));
        assert!(Stage::All.must_stop(LOGD, 0, CALLER), "and then it too");
    }

    /// The caller could itself be the process the log is owed to — `logd` may
    /// hold `Rights::POWER` on some boot config — and neither rule may cancel
    /// the other.
    #[test]
    fn the_caller_being_the_log_writer_stops_neither_rule_working() {
        let stage = Stage::ExceptLog { keep: LOGD };
        assert!(!stage.must_stop(LOGD, 0, (LOGD, 0)));
        assert!(
            !stage.must_stop(LOGD, 7, (LOGD, 0)),
            "a sibling of the caller is still carved out while the stage names it",
        );
        assert!(Stage::All.must_stop(LOGD, 7, (LOGD, 0)), "and not after");
    }

    #[test]
    fn a_sweep_with_nothing_left_running_is_done_however_long_it_took() {
        let done = Sweep { stopped: 6, running: 0 };
        assert_eq!(Progress::of(done, 0, 2_000), Progress::Done);
        assert_eq!(Progress::of(done, 2_000, 2_000), Progress::Done);
        assert_eq!(
            Progress::of(done, u64::MAX, 2_000),
            Progress::Done,
            "a stop that completed is never reported as an expiry",
        );
    }

    #[test]
    fn the_budget_ends_the_wait_and_the_boundary_is_inclusive() {
        let left = Sweep { stopped: 4, running: 2 };
        assert_eq!(Progress::of(left, 0, 2_000), Progress::Waiting);
        assert_eq!(Progress::of(left, 1_999, 2_000), Progress::Waiting);
        assert_eq!(Progress::of(left, 2_000, 2_000), Progress::Expired);
    }

    /// A machine with no userland left at all — every process already exited —
    /// is stopped, not waiting.
    #[test]
    fn an_empty_machine_is_already_stopped() {
        assert_eq!(Progress::of(Sweep::default(), 0, 2_000), Progress::Done);
    }

    #[test]
    fn the_record_names_the_shortfall_only_when_there_is_one() {
        extern crate alloc;
        let whole = Record {
            sweep: Sweep { stopped: 6, running: 0 },
            elapsed_ms: 11,
            sweeps: 3,
            cpus: 8,
            in_flight: 0,
        };
        assert_eq!(
            alloc::format!("{whole}"),
            "stop: 6 of 6 userland thread(s) stopped across 8 cpu(s) in 11 ms over 3 sweep(s), \
             0 block operation(s) still open",
        );
        let short = Record {
            sweep: Sweep { stopped: 4, running: 2 },
            in_flight: 1,
            ..whole
        };
        assert_eq!(
            alloc::format!("{short}"),
            "stop: 4 of 6 userland thread(s) stopped across 8 cpu(s) in 11 ms over 3 sweep(s), \
             1 block operation(s) still open; this reset lands wherever the other 2 are",
        );
    }
}
