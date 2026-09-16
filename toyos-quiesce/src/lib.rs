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
//!
//! [`Record`] is written by the kernel and read back off a stick by
//! `src/metal.rs` and by the harness, so its wire form is rendered and parsed
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

/// What the stop knows about one thread.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Thread {
    pub id: ThreadId,
    /// Whether this thread's process holds the machine's log capability.
    pub holds_the_log: bool,
}

/// How far the stop has gone.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stage {
    /// Every userland thread but those of a process holding the log
    /// capability.
    ExceptLog,
    /// Those too. Nothing in userland runs again.
    All,
}

impl Stage {
    /// Whether `thread` must stop at its next safe point.
    ///
    /// **`caller` is never stopped**: it is the thread running the stop, and
    /// it has the rest of the shutdown to perform. Its own process is not
    /// exempt — a sibling thread of the process that asked for the reboot
    /// stops like any other, because what the reset must outlast is one
    /// thread's remaining work and not one program's.
    pub fn must_stop(self, thread: Thread, caller: ThreadId) -> bool {
        if thread.id == caller {
            return false;
        }
        match self {
            Stage::ExceptLog => !thread.holds_the_log,
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

/// What a line of kernel log carrying a [`Record`] begins with.
pub const STOPPED: &str = "stop: ";

const OF: &str = " of ";
const THREADS: &str = " userland thread(s) stopped across ";
const CPUS: &str = " cpu(s) in ";
const MS: &str = " ms over ";
const SWEEPS: &str = " sweep(s), ";
const OPEN: &str = " userland block operation(s) still open";
const SHORTFALL: &str = "; this reset lands wherever the other ";
const ARE: &str = " are";

/// What the stop did, as the one line it writes beside the boot's other
/// closing censuses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Record {
    pub sweep: Sweep,
    pub elapsed_ms: u64,
    pub sweeps: u32,
    pub cpus: u32,
    /// Block-device operations still open on a thread the stop stops: an
    /// operation lasts only while its opener is inside the device, so a thread
    /// that has stopped holds none.
    pub in_flight: u32,
    /// How many such operations this boot began. Zero says the counter behind
    /// [`Self::in_flight`] never counted at all, which no boot that wrote a
    /// file can honestly report.
    pub begun: u64,
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
        let (elapsed_ms, rest) = rest.split_once(MS)?;
        let (sweeps, rest) = rest.split_once(SWEEPS)?;
        let (in_flight, rest) = rest.split_once(OF)?;
        let (begun, rest) = rest.split_once(OPEN)?;

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
            sweeps: sweeps.parse().ok()?,
            cpus: cpus.parse().ok()?,
            in_flight: in_flight.parse().ok()?,
            begun: begun.parse().ok()?,
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
            "{STOPPED}{}{OF}{}{THREADS}{}{CPUS}{}{MS}{}{SWEEPS}{}{OF}{}{OPEN}",
            self.sweep.stopped,
            self.sweep.total(),
            self.cpus,
            self.elapsed_ms,
            self.sweeps,
            self.in_flight,
            self.begun,
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
    const LOGD: u32 = 2;

    fn id(pid: u32, tid: u32) -> ThreadId {
        ThreadId { pid, tid }
    }

    fn thread(pid: u32, tid: u32) -> Thread {
        Thread { id: id(pid, tid), holds_the_log: false }
    }

    fn log_thread(pid: u32, tid: u32) -> Thread {
        Thread { id: id(pid, tid), holds_the_log: true }
    }

    #[test]
    fn the_caller_is_never_stopped_and_its_siblings_always_are() {
        for stage in [Stage::ExceptLog, Stage::All] {
            assert!(!stage.must_stop(thread(10, 0), CALLER), "{stage:?}");
            assert!(
                stage.must_stop(thread(10, 1), CALLER),
                "a sibling thread of the caller's process is not the caller: {stage:?}",
            );
        }
    }

    #[test]
    fn the_log_is_carved_out_of_the_first_stage_and_not_the_second() {
        assert!(!Stage::ExceptLog.must_stop(log_thread(LOGD, 0), CALLER));
        assert!(!Stage::ExceptLog.must_stop(log_thread(LOGD, 3), CALLER), "every thread of it");
        assert!(Stage::ExceptLog.must_stop(thread(LOGD + 1, 0), CALLER));
        assert!(Stage::All.must_stop(log_thread(LOGD, 0), CALLER), "and then it too");
    }

    /// **A process the capability was never moved into is not the carve-out**,
    /// however much of the log it has seen: what ends the shutdown's wait is
    /// making a record durable, and only a holder can do it.
    #[test]
    fn a_process_without_the_capability_stops_in_the_first_stage() {
        assert!(Stage::ExceptLog.must_stop(thread(LOGD + 4, 0), CALLER));
        assert!(!Stage::ExceptLog.must_stop(log_thread(LOGD, 0), CALLER));
    }

    /// The caller could itself be a process the log is owed to — `logd` may
    /// hold `Rights::POWER` on some boot config — and neither rule may cancel
    /// the other.
    #[test]
    fn the_caller_being_the_log_writer_stops_neither_rule_working() {
        assert!(!Stage::ExceptLog.must_stop(log_thread(LOGD, 0), id(LOGD, 0)));
        assert!(
            !Stage::ExceptLog.must_stop(log_thread(LOGD, 7), id(LOGD, 0)),
            "a sibling of the caller is still carved out while the stage names it",
        );
        assert!(Stage::All.must_stop(log_thread(LOGD, 7), id(LOGD, 0)), "and not after");
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
        sweeps: 3,
        cpus: 8,
        in_flight: 0,
        begun: 4812,
    };

    fn short() -> Record {
        Record { sweep: Sweep { stopped: 4, running: 2 }, in_flight: 1, ..WHOLE }
    }

    #[test]
    fn the_record_names_the_shortfall_only_when_there_is_one() {
        assert_eq!(
            alloc::format!("{WHOLE}"),
            "stop: 6 of 6 userland thread(s) stopped across 8 cpu(s) in 11 ms over 3 sweep(s), \
             0 of 4812 userland block operation(s) still open",
        );
        assert_eq!(
            alloc::format!("{}", short()),
            "stop: 4 of 6 userland thread(s) stopped across 8 cpu(s) in 11 ms over 3 sweep(s), \
             1 of 4812 userland block operation(s) still open; this reset lands wherever the \
             other 2 are",
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
            let line = alloc::format!("[2026-09-14 20:38:12 7.955 cpu4] {record}\r");
            assert_eq!(Record::parse(&line), Some(record), "{line}");
        }
    }

    #[test]
    fn a_line_that_is_not_a_record_is_refused_rather_than_half_read() {
        for line in [
            "[stamp] Rebooting.",
            "[stamp] stop: 6 of 6 userland thread(s) stopped across 8 cpu(s)",
            // The shortfall clause disagreeing with the counts it restates.
            "[stamp] stop: 4 of 6 userland thread(s) stopped across 8 cpu(s) in 11 ms over 3 \
             sweep(s), 1 of 4812 userland block operation(s) still open; this reset lands \
             wherever the other 9 are",
            // A shortfall clause on a record that claims to have stopped.
            "[stamp] stop: 6 of 6 userland thread(s) stopped across 8 cpu(s) in 11 ms over 3 \
             sweep(s), 0 of 4812 userland block operation(s) still open; this reset lands \
             wherever the other 2 are",
            // More threads stopped than there were.
            "[stamp] stop: 7 of 6 userland thread(s) stopped across 8 cpu(s) in 11 ms over 3 \
             sweep(s), 0 of 4812 userland block operation(s) still open",
        ] {
            assert_eq!(Record::parse(line), None, "{line}");
        }
    }
}
