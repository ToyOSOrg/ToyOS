//! Stopping this machine's userland, so the shutdown's claims about it are
//! claims about a machine that has stopped.
//!
//! Every decision here is [`toyos_quiesce`]'s; this module marks threads and
//! spends the time.
//!
//! # Where the stop is taken, and why there
//!
//! [`stops_this_thread`] is read by `scheduler::leave_ring3_if_due`, called
//! from `kernel_exit_to_user_check` — the one function every return to Ring 3
//! in this kernel passes through: the syscall gate, every device interrupt, the
//! timer, the TLB shootdown IPI, the general trap epilogue and a task's first
//! dispatch. A thread standing there holds no kernel lock, has nothing in the
//! block layer and nothing in flight on any controller; it is between two
//! userland instructions, and every record this kernel writes with a userland
//! author is written from inside a syscall.
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
//! # What the stop waits on
//!
//! [`PROGRESS`], posted by [`note_progress`] from the three transitions that
//! turn a thread a sweep counted as running into one it does not: the pass
//! that bands it at its safe point, the pass that parks it, and its exit. Each
//! posts after its own transition, so the sweep it wakes finds it done; a
//! thread still running when the budget is spent is the record's shortfall.
//!
//! Lock order: [`process::PROCESS_TABLE`] alone.

use core::sync::atomic::{
    AtomicU32, AtomicU8, Ordering::Acquire, Ordering::Relaxed, Ordering::Release,
};

use toyos_quiesce::{Record, Stage, Sweep, Thread, ThreadId};
use toyos_sched::task::WaitClass;

use crate::arch::percpu;
use crate::completion::{self, Outcome, Subject, Token, Watch};
use crate::process;
use crate::scheduler::TaskId;
use crate::time::{Budget, Deadline, Duration};

mod claim;

/// How long the machine gets to stop.
///
/// **A budget, and not a bound the kernel can prove.** One `QUANTUM_NS` is
/// what a thread running in Ring 3 needs to reach the boundary, and one
/// `block::OPERATION` is the longest a thread lies inside the block layer
/// without parking — but one syscall may open several operations in a row,
/// parking between them inside a `block::OpenUpdate` this stop waits out, and
/// `block::DEADMAN` is what bounds that sequence. A thread can therefore
/// outlast this, which is why its expiry is a clause in the record.
const PARK: Budget = Budget::of(
    Duration::from_nanos(toyos_sched::fair::QUANTUM_NS + crate::block::OPERATION.nanos()),
    "the reset lands wherever the threads that never reached a safe point are, and \
     the record names how many",
);

const RUNNING: u8 = 0;
const EXCEPT_LOG: u8 = 1;
const ALL: u8 = 2;

/// Which stage this machine is in. **The one word every other read here hangs
/// off**: it is stored last with `Release` and loaded first with `Acquire`, so
/// a gate that sees a stage sees the caller that goes with it.
static STAGE: AtomicU8 = AtomicU8::new(RUNNING);

/// The thread running the stop, which never stops itself; no thread at all
/// until [`stop`] stores one, spelt as `percpu` spells idle.
static CALLER_PID: AtomicU32 = AtomicU32::new(u32::MAX);
static CALLER_TID: AtomicU32 = AtomicU32::new(u32::MAX);

fn stage() -> Option<Stage> {
    match STAGE.load(Acquire) {
        EXCEPT_LOG => Some(Stage::ExceptLog),
        ALL => Some(Stage::All),
        _ => None,
    }
}

fn caller() -> ThreadId {
    ThreadId { pid: CALLER_PID.load(Relaxed), tid: CALLER_TID.load(Relaxed) }
}

/// Whether the machine's stop names the running thread, for the one Ring 3
/// boundary that ranks this against the kill mark.
pub fn stops_this_thread() -> bool {
    let Some(stage) = stage() else { return false };
    let (Some(pid), Some(tid)) = (percpu::current_pid(), percpu::current_tid()) else {
        return false;
    };
    // A kernel thread reaches this boundary on its first dispatch and has no
    // Ring 3 to be stopped from; `klogd` and `iod` are also what carries the
    // log to its volume while userland is being stopped around them.
    if crate::sched::kthread::is_kernel_task(TaskId(pid, tid)) {
        return false;
    }
    let thread = Thread {
        id: ThreadId { pid: pid.raw(), tid: tid.raw() },
        holds_the_log: crate::log::user::holds_the_log(pid.raw()),
    };
    stage.must_stop(thread, caller())
}

/// Whether this thread is the one shutdown this boot gets: a second caller's
/// sweep would band the first where it parks inside its own sync.
pub fn claim_the_shutdown() -> bool {
    CLAIMED.take()
}

static CLAIMED: claim::Claim = claim::Claim::new();

/// What the stop's caller parks on between two sweeps.
static PROGRESS: Watch = Watch::new();

/// Called just after the running thread made a transition that can end the
/// stop's wait on it — banded at its safe point, parked, or exited — and never
/// before one: a sweep woken first would find it still running and wait for a
/// post that has already come.
pub fn note_progress() {
    if stops_this_thread() {
        completion::post(Subject::of(&PROGRESS), Outcome::Ready);
    }
}

/// Whether the machine's stop has begun: what the `quiesce-fsync-refuse`
/// actuator refuses from.
#[cfg(feature = "boot-actuators")]
pub fn stopping() -> bool {
    stage().is_some()
}

/// Whether the running thread is the one performing the shutdown: what the
/// `quiesce-drain-refuse` actuator refuses by.
#[cfg(feature = "boot-actuators")]
pub fn runs_the_shutdown() -> bool {
    if !stopping() {
        return false;
    }
    let (Some(pid), Some(tid)) = (percpu::current_pid(), percpu::current_tid()) else {
        return false;
    };
    ThreadId { pid: pid.raw(), tid: tid.raw() } == caller()
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
    // not a reboot syscall.
    let caller = ThreadId {
        pid: percpu::current_pid().expect("quiesce::stop: the caller holds no process").raw(),
        tid: percpu::current_tid().expect("quiesce::stop: the caller holds no thread").raw(),
    };
    CALLER_PID.store(caller.pid, Relaxed);
    CALLER_TID.store(caller.tid, Relaxed);
    // Last, and `Release`: a gate that sees this stage sees the caller it must
    // not stop.
    STAGE.store(
        match stage {
            Stage::ExceptLog => EXCEPT_LOG,
            Stage::All => ALL,
        },
        Release,
    );

    // Armed before the first sweep, so a transition landing between a sweep
    // and the park after it leaves a record that park returns on at once.
    let parkable = crate::scheduler::Parkable::at_entry();
    let armed = completion::arm(Subject::of(&PROGRESS), Token::new(0), WaitClass::Other)
        .expect("quiesce::stop: the caller holds no task to park");
    // The kick is the timer vector, whose return to Ring 3 is the gate.
    crate::arch::apic::kick_all_but_self();
    let cpus = crate::arch::smp::cpu_count();

    let began = crate::clock::now();
    let deadline = Deadline::at(began + PARK.duration());
    let mut sweeps = 0;
    loop {
        let swept = sweep(stage, caller);
        sweeps += 1;
        let elapsed = (crate::clock::now() - began).nanos();
        if swept.keep_waiting(elapsed, PARK.nanos()) {
            // Uncancellable: the claim is taken, and a caller that left here
            // would leave a machine nothing else may turn off. The deadline is
            // `keep_waiting`'s own, so an expiry ends the loop at the next sweep.
            let _ = completion::wait_uncancellable(&parkable, &armed, deadline);
            continue;
        }
        // Read here and not by the caller: the question is what was open at the
        // moment the stop ended, and every line between here and the record's
        // own would open more.
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

/// Mark every parked thread `stage` names and count the rest.
///
/// The table lock is held for the walk and given up before the park: a thread
/// on another CPU finishing its own teardown takes this same lock.
fn sweep(stage: Stage, caller: ThreadId) -> Sweep {
    let mut out = Sweep::default();
    let guard = process::PROCESS_TABLE.lock();
    let Some(table) = guard.as_ref() else { return out };
    for (_, proc) in table.iter() {
        let pid = proc.pid();
        let holds_the_log = crate::log::user::holds_the_log(pid.raw());
        for (tid, thread) in proc.threads().iter() {
            let who = Thread { id: ThreadId { pid: pid.raw(), tid: tid.raw() }, holds_the_log };
            if !stage.must_stop(who, caller) {
                continue;
            }
            // A zombie has already written its `exit:` record and holds no
            // task; there is nothing left of it to stop.
            if matches!(thread.state(), process::ThreadLocation::Zombie(_)) {
                continue;
            }
            if crate::sched::kthread::is_kernel_task(TaskId(pid, tid)) {
                continue;
            }
            // Counted as running and not skipped: a thread between its table
            // insert and its task mint has no task to mark, and a sweep that
            // passed over it would report a machine stopped with a thread still
            // to be dispatched.
            let Some(sched) = thread.sched() else {
                out.running += 1;
                continue;
            };
            // `stop_if_blocked` refuses a running thread and one parked inside
            // a `block::OpenUpdate`: each has to reach its own safe point, and
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
