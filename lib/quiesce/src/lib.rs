//! Every decision the machine's stop makes, and none of its effects.
//!
//! The shutdown path's two claims — every filesystem is synced, and
//! `Rebooting.` is the last thing that happened — are claims about a machine,
//! and they are true only of a machine that has stopped. Stopping it is two
//! questions: *which* thread may still run, and *when* is it over. Both are
//! here; `kernel/src/quiesce.rs` marks the threads and spends the time.
//!
//! **Nothing is carved out.** The log's writer stops with every other thread:
//! the supervisor has it flush before it asks for the stop, and what the
//! kernel says after that goes to its console and its black box, never to a
//! process it would have to keep running.
//!
//! [`Record`] is written by the kernel and read back off a stick by
//! `tests/metal` and by the harness, so its wire form is rendered and parsed
//! here and nowhere else.

#![no_std]
#![forbid(unsafe_code)]

use core::fmt;

/// One thread, as the kernel's task ids spell it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ThreadId {
    pub pid: u32,
    pub tid: u32,
}

/// Whether `thread` must stop at its next safe point.
///
/// **`caller` is never stopped**: it is the thread running the stop, and it has
/// the rest of the shutdown to perform. Its own process is not exempt — a
/// sibling thread of the process that asked for the reboot stops like any
/// other, because what the reset must outlast is one thread's remaining work
/// and not one program's.
pub fn must_stop(thread: ThreadId, caller: ThreadId) -> bool {
    thread != caller
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

    /// Whether the caller sweeps again.
    ///
    /// **A spent budget ends the wait; it never ends the reset.** What is left
    /// running when this answers `false` is what [`Record`]'s shortfall clause
    /// names, because a machine nobody can turn off is worse than a reset that
    /// lands inside somebody's syscall. A sweep that finished the stop on the
    /// very tick the budget ran out finished it: the running count is read
    /// first.
    pub fn keep_waiting(self, elapsed_ns: u64, budget_ns: u64) -> bool {
        self.running != 0 && elapsed_ns < budget_ns
    }
}

/// A policy number: how long the supervisor waits on one call into a file server that
/// is alive and has not answered — and so how long a stop request can wait
/// behind one, since the supervisor serves `power` between its loop's passes.
pub const FILES_MS: u64 = 30_000;

/// A policy number: how long the supervisor waits for `logkeeper`'s flush before a stop.
pub const FLUSH_MS: u64 = 5_000;

/// A policy number: how long the supervisor waits for every file server's sync, run
/// together after the flush, before a stop.
pub const SYNC_MS: u64 = 5_000;

/// A policy number: how long the supervisor waits for every disk driver to end once
/// told the machine stops, after the sync. Above usbd's own bound on its stop
/// (`toyos_xhci::hc::STOP_NS`), so a driver that keeps its bound is waited out.
pub const DRIVERS_MS: u64 = 3_000;

/// The endowment a disk driver is told the machine stops by: a bell whose
/// waker the supervisor closes once every file server has synced; it is
/// readable from then. The driver hands its devices back, leaves its
/// controller as firmware would find it, and ends.
pub const DRIVER_STOP: &str = "stop";

/// The thread the kernel's quiesce-last actuators hold, by the name its
/// program gives it: held until it is the one thread the stop still waits
/// on, so the transition it makes next is the stop's last.
pub const LAST_THREAD: &str = "quiesce-last";

/// What a line of kernel log carrying a [`Record`] begins with.
pub const STOPPED: &str = "stop: ";

const OF: &str = " of ";
const THREADS: &str = " userland thread(s) stopped across ";
const CPUS: &str = " cpu(s) in ";
const BUDGET: &str = " ms of a ";
const MS: &str = " ms budget over ";
const SWEEPS: &str = " sweep(s)";
const SHORTFALL: &str = "; this reset lands wherever the other ";
const ARE: &str = " are";

/// What the stop did, as the one line it writes beside the boot's other
/// closing censuses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Record {
    pub sweep: Sweep,
    pub elapsed_ms: u64,
    /// What the stop was given, declared by the kernel alone: a reader that
    /// spelt it again would pass a stop that slept out a budget the kernel had
    /// since lowered.
    pub budget_ms: u64,
    pub sweeps: u32,
    pub cpus: u32,
}

impl Record {
    /// The [`Record`] a line of kernel log carries, or `None` for a line that
    /// is not one — every boot that reset without going through the stop.
    ///
    /// **The shortfall is said twice and checked against itself**: a line
    /// whose clause disagrees with `total - stopped` is not one this kernel
    /// wrote, and is refused rather than half-read.
    pub fn parse(line: &str) -> Option<Record> {
        let (_, rest) = line.trim_end().split_once(STOPPED)?;
        let (stopped, rest) = rest.split_once(OF)?;
        let (total, rest) = rest.split_once(THREADS)?;
        let (cpus, rest) = rest.split_once(CPUS)?;
        let (elapsed_ms, rest) = rest.split_once(BUDGET)?;
        let (budget_ms, rest) = rest.split_once(MS)?;
        let (sweeps, rest) = rest.split_once(SWEEPS)?;

        let stopped: u32 = stopped.parse().ok()?;
        let running = total.parse::<u32>().ok()?.checked_sub(stopped)?;
        if running > 0 {
            let said: u32 = rest.strip_prefix(SHORTFALL)?.strip_suffix(ARE)?.parse().ok()?;
            if said != running {
                return None;
            }
        } else if !rest.is_empty() {
            return None;
        }
        Some(Record {
            sweep: Sweep { stopped, running },
            elapsed_ms: elapsed_ms.parse().ok()?,
            budget_ms: budget_ms.parse().ok()?,
            sweeps: sweeps.parse().ok()?,
            cpus: cpus.parse().ok()?,
        })
    }

    /// Whether every thread that had to stop did.
    pub fn stopped_the_machine(self) -> bool {
        self.sweep.running == 0
    }
}

impl fmt::Display for Record {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{STOPPED}{}{OF}{}{THREADS}{}{CPUS}{}{BUDGET}{}{MS}{}{SWEEPS}",
            self.sweep.stopped,
            self.sweep.total(),
            self.cpus,
            self.elapsed_ms,
            self.budget_ms,
            self.sweeps,
        )?;
        if self.sweep.running > 0 {
            write!(f, "{SHORTFALL}{}{ARE}", self.sweep.running)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    extern crate alloc;

    use super::*;

    const CALLER: ThreadId = ThreadId { pid: 10, tid: 0 };

    fn id(pid: u32, tid: u32) -> ThreadId {
        ThreadId { pid, tid }
    }

    #[test]
    fn the_caller_is_never_stopped_and_its_siblings_always_are() {
        assert!(!must_stop(id(10, 0), CALLER));
        assert!(must_stop(id(10, 1), CALLER), "a sibling thread of the caller's process is not the caller");
    }

    /// **The log's writer is not carved out**: `logkeeper`'s threads stop like any
    /// other process's, whatever it holds.
    #[test]
    fn every_other_thread_stops_the_log_writer_included() {
        const LOGKEEPER: u32 = 2;
        for tid in 0..4 {
            assert!(must_stop(id(LOGKEEPER, tid), CALLER));
        }
    }

    #[test]
    fn a_sweep_with_nothing_left_running_ends_the_wait_however_long_it_took() {
        let done = Sweep { stopped: 6, running: 0 };
        assert!(!done.keep_waiting(0, 2_000));
        assert!(!done.keep_waiting(2_000, 2_000));
        assert!(!done.keep_waiting(u64::MAX, 2_000));
    }

    #[test]
    fn the_budget_ends_the_wait_and_the_boundary_is_inclusive() {
        let left = Sweep { stopped: 4, running: 2 };
        assert!(left.keep_waiting(0, 2_000));
        assert!(left.keep_waiting(1_999, 2_000));
        assert!(!left.keep_waiting(2_000, 2_000));
    }

    /// A machine with no userland left at all — every process already exited —
    /// is stopped, not waiting.
    #[test]
    fn an_empty_machine_is_already_stopped() {
        assert!(!Sweep::default().keep_waiting(0, 2_000));
    }

    const WHOLE: Record = Record {
        sweep: Sweep { stopped: 6, running: 0 },
        elapsed_ms: 11,
        budget_ms: 2010,
        sweeps: 3,
        cpus: 8,
    };

    fn short() -> Record {
        Record { sweep: Sweep { stopped: 4, running: 2 }, ..WHOLE }
    }

    #[test]
    fn the_record_names_the_shortfall_only_when_there_is_one() {
        assert_eq!(
            alloc::format!("{WHOLE}"),
            "stop: 6 of 6 userland thread(s) stopped across 8 cpu(s) in 11 ms of a 2010 ms \
             budget over 3 sweep(s)",
        );
        assert_eq!(
            alloc::format!("{}", short()),
            "stop: 4 of 6 userland thread(s) stopped across 8 cpu(s) in 11 ms of a 2010 ms \
             budget over 3 sweep(s); this \
             reset lands wherever the other 2 are",
        );
        assert!(WHOLE.stopped_the_machine());
        assert!(!short().stopped_the_machine());
    }

    /// **The one declaration, checked both ways.** A reworded arm of `Display`
    /// that the readers no longer match is what this crate exists to make
    /// impossible.
    #[test]
    fn every_record_reads_back_as_itself_off_a_line_of_log() {
        for record in [WHOLE, short(), Record { sweep: Sweep::default(), ..WHOLE }] {
            let line = alloc::format!("[2026-09-14 20:38:12  7.955 cpu4 kernel] {record}\r");
            assert_eq!(Record::parse(&line), Some(record), "{line}");
        }
    }

    #[test]
    fn a_line_that_is_not_a_record_is_refused_rather_than_half_read() {
        for line in [
            "[stamp] Rebooting.",
            "[stamp] stop: 6 of 6 userland thread(s) stopped across 8 cpu(s)",
            // The shortfall clause disagreeing with the counts it restates.
            "[stamp] stop: 4 of 6 userland thread(s) stopped across 8 cpu(s) in 11 ms of a 2010 \
             ms budget over 3 sweep(s); this \
             reset lands wherever the other 9 are",
            // A shortfall clause on a record that claims to have stopped.
            "[stamp] stop: 6 of 6 userland thread(s) stopped across 8 cpu(s) in 11 ms of a 2010 \
             ms budget over 3 sweep(s); this \
             reset lands wherever the other 2 are",
            // More threads stopped than there were.
            "[stamp] stop: 7 of 6 userland thread(s) stopped across 8 cpu(s) in 11 ms of a 2010 \
             ms budget over 3 sweep(s)",
        ] {
            assert_eq!(Record::parse(line), None, "{line}");
        }
    }
}
