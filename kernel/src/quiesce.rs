//! Stopping this machine's userland, so the shutdown's claims about it are
//! claims about a machine that has stopped.
//!
//! Every decision here is [`toyos_quiesce`]'s; this module marks threads and
//! spends the time.
//!
//! # Where the stop is taken, and why there
//!
//! [`stop_here_if_due`] is called from `kernel_exit_to_user_check`, the one
//! function every return to Ring 3 in this kernel passes through — the syscall
//! gate, every device interrupt, the timer, the TLB shootdown IPI, the general
//! trap epilogue and a task's first dispatch. A thread standing there holds no
//! kernel lock, has nothing in the block layer and nothing in flight on any
//! controller; it is between two userland instructions, and every record this
//! kernel writes with a userland author is written from inside a syscall.
//!
//! # What keeps running
//!
//! **Tasks stop; CPUs do not.** Every CPU keeps `IF` set, keeps taking its
//! LAPIC timer and every device interrupt, and keeps taking scheduler passes —
//! it simply has no userland left to dispatch. That is what the USB stop below
//! the boot's last word needs, and what the kernel threads that carry the log
//! to its volume need. Freezing CPUs inside a pass instead would strand
//! whatever lock the thread on that CPU was holding, and `sync_all` is the
//! first thing that would wait on it.
//!
//! Kernel threads are exempt by identity, not by accident: `klogd`, `iod` and
//! `usbd` are in the process table like anything else, and
//! [`crate::sched::kthread::is_kernel_task`] is what tells them apart.
//!
//! Lock order: [`process::PROCESS_TABLE`], then the log's reader set.

use core::sync::atomic::{AtomicU32, AtomicU8, Ordering::Acquire, Ordering::Relaxed, Ordering::Release};

use toyos_quiesce::{Progress, Record, Stage, Sweep, Thread};

use crate::arch::percpu;
use crate::process;
use crate::scheduler::TaskId;
use crate::time::{Budget, Cadence, Duration};

/// How long the machine gets to stop.
///
/// The sum of the two things a thread that must stop can be doing: running in
/// Ring 3, which ends at the next timer tick — one `QUANTUM_NS`, and sooner
/// than that in practice because [`stop`] kicks every CPU — or inside a
/// syscall, whose longest uninterruptible stretch is one block-layer
/// operation. A thread between block-layer retries is parked, and a parked
/// thread is marked rather than waited for.
const PARK: Budget = Budget::of(
    Duration::from_nanos(toyos_sched::fair::QUANTUM_NS + crate::block::OPERATION.nanos()),
    "the reset lands wherever the threads that never reached a safe point are, and \
     the record names how many",
);

/// How long the caller yields between two sweeps.
///
/// A sweep takes the machine-wide process table, and a thread finishing the
/// teardown this loop waits for needs that same lock: without a cadence the
/// wait contends with what it is waiting for. One quantum, because that is
/// what a thread in Ring 3 needs to reach its safe point.
const SWEEP: Cadence = Cadence::every(
    Duration::from_nanos(toyos_sched::fair::QUANTUM_NS),
    "the process table is taken again and every thread that must stop re-examined",
);

const RUNNING: u8 = 0;
const EXCEPT_LOG: u8 = 1;
const ALL: u8 = 2;

/// Which stage this machine is in. **The one word every other read here hangs
/// off**: it is stored last with `Release` and loaded first with `Acquire`, so
/// a gate that sees a stage sees the caller that goes with it.
static STAGE: AtomicU8 = AtomicU8::new(RUNNING);

/// The thread running the stop, which never stops itself.
static CALLER_PID: AtomicU32 = AtomicU32::new(0);
static CALLER_TID: AtomicU32 = AtomicU32::new(0);

fn stage() -> Option<Stage> {
    match STAGE.load(Acquire) {
        EXCEPT_LOG => Some(Stage::ExceptLog),
        ALL => Some(Stage::All),
        _ => None,
    }
}

fn caller() -> (u32, u32) {
    (CALLER_PID.load(Relaxed), CALLER_TID.load(Relaxed))
}

/// The running thread's last return to Ring 3, if this machine is stopping.
///
/// Called from `kernel_exit_to_user_check` beside
/// [`crate::scheduler::exit_if_killed`]; see the module header for why there
/// and nowhere else.
pub fn stop_here_if_due() {
    let Some(stage) = stage() else { return };
    let (Some(pid), Some(tid)) = (percpu::current_pid(), percpu::current_tid()) else {
        return;
    };
    // A kernel thread reaches this boundary on its first dispatch and has no
    // Ring 3 to be stopped from; `klogd` and `iod` are also what carries the
    // log to its volume while userland is being stopped around them.
    if crate::sched::kthread::is_kernel_task(TaskId(pid, tid)) {
        return;
    }
    let thread = Thread {
        pid: pid.raw(),
        tid: tid.raw(),
        keeps_the_log: crate::log::user::keeps_the_log(pid.raw()),
    };
    if !stage.must_stop(thread, caller()) {
        return;
    }
    crate::scheduler::stop_current()
}

/// Stop every userland thread `stage` names, and answer with what it took.
///
/// Returns when the machine is stopped or when [`PARK`] is spent, never
/// otherwise: an expiry is a line in the record and a reset that lands where
/// it lands, because a machine nobody can turn off is worse than one whose
/// last word overlapped somebody's syscall.
#[must_use]
pub fn stop(stage: Stage) -> Record {
    // Refused by name rather than defaulted: a caller with no task identity is
    // not a reboot syscall, and `(0, 0)` would exempt whichever thread holds
    // those ids instead.
    let caller = (
        percpu::current_pid().expect("quiesce::stop: the caller holds no process").raw(),
        percpu::current_tid().expect("quiesce::stop: the caller holds no thread").raw(),
    );
    CALLER_PID.store(caller.0, Relaxed);
    CALLER_TID.store(caller.1, Relaxed);
    // Last, and `Release`: a gate that sees this stage sees the caller it must
    // not stop.
    STAGE.store(
        match stage {
            Stage::ExceptLog => EXCEPT_LOG,
            Stage::All => ALL,
        },
        Release,
    );

    // Kicked, and not left to arrive on their own: a CPU halted in the idle
    // path has stopped its own timer, and a thread spinning in Ring 3 would
    // otherwise hold its CPU until the quantum it is in runs out. The kick is
    // the timer vector, whose return to Ring 3 is the gate.
    crate::arch::apic::kick_all_but_self();
    let cpus = crate::arch::smp::cpu_count();

    let began = crate::clock::nanos_since_boot();
    let mut sweeps = 0;
    loop {
        let swept = sweep(stage, caller);
        sweeps += 1;
        let elapsed = crate::clock::nanos_since_boot().saturating_sub(began);
        match Progress::of(swept, elapsed, PARK.nanos()) {
            Progress::Waiting => between_sweeps(),
            Progress::Done | Progress::Expired => {
                // Read here and not by the caller: the question is what was
                // open at the moment the stop ended, and every line between
                // here and the record's own would open more.
                let (in_flight, begun) = crate::block::userland_operations();
                return Record {
                    sweep: swept,
                    elapsed_ms: elapsed / 1_000_000,
                    sweeps,
                    cpus,
                    in_flight,
                    begun,
                };
            }
        }
    }
}

/// Yielded and never parked: at `--smp 1` the caller's CPU is the only one the
/// threads it waits for can reach their safe points on.
fn between_sweeps() {
    let until = crate::clock::nanos_since_boot().saturating_add(SWEEP.nanos());
    while crate::clock::nanos_since_boot() < until {
        crate::scheduler::yield_now();
    }
}

/// Mark every parked thread `stage` names and count the rest.
///
/// The table lock is held for the walk and given up before the cadence above:
/// a thread on another CPU finishing its own teardown takes this same lock.
fn sweep(stage: Stage, caller: (u32, u32)) -> Sweep {
    let mut out = Sweep::default();
    let guard = process::PROCESS_TABLE.lock();
    let Some(table) = guard.as_ref() else { return out };
    for (_, proc) in table.iter() {
        let pid = proc.pid();
        let keeps_the_log = crate::log::user::keeps_the_log(pid.raw());
        for (tid, thread) in proc.threads().iter() {
            let who = Thread { pid: pid.raw(), tid: tid.raw(), keeps_the_log };
            if !stage.must_stop(who, caller) {
                continue;
            }
            // A zombie has already written its `exit:` record and holds no
            // task; there is nothing left of it to stop.
            if matches!(thread.state(), process::ThreadLocation::Zombie(_)) {
                continue;
            }
            let Some(sched) = thread.sched() else { continue };
            if crate::sched::kthread::is_kernel_task(TaskId(pid, tid)) {
                continue;
            }
            // `stop_if_blocked` refuses a running thread, which is the whole
            // of the difference: that one has to reach its own safe point, and
            // until it does it is what this sweep is waiting for.
            if sched.shared.stop_pending() || sched.shared.stop_if_blocked() {
                out.stopped += 1;
            } else {
                out.running += 1;
            }
        }
    }
    out
}
