//! The machine's stop: once it has begun, no thread but its caller returns
//! to Ring 3.
//!
//! The kernel's last sequence takes hardware back from the programs it was
//! handed to, settles what they left in flight and cuts power, and no program
//! can vouch that nothing runs in Ring 3 across it; so the gate is here.
//! **The stop waits for no thread.** A thread inside a syscall, or parked in
//! one and woken later, runs on to the boundary, and what it logs may follow
//! the last word; what had to finish first is the caller's to establish
//! before it asks.
//!
//! [`stops_this_thread`] is read by `scheduler::leave_user_if_due`, which
//! every return to Ring 3 passes: the syscall gate, every interrupt and trap
//! epilogue, and a task's first dispatch. A thread standing there holds no
//! kernel lock, and is banded where no pick serves it
//! (`SchedPass::dispose_stop`).
//!
//! **Tasks stop; CPUs do not.** Every CPU keeps taking interrupts and
//! scheduler passes; freezing one inside a pass would strand whatever lock
//! the thread on it held.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering::AcqRel, Ordering::Acquire, Ordering::Release};

use crate::arch::percpu;

/// No thread: the stop has not begun.
const NOBODY: u64 = u64::MAX;

/// The thread running the stop, which never stops itself, as
/// [`thread_word`] spells it; [`NOBODY`] until [`stop`] begins.
static CALLER: AtomicU64 = AtomicU64::new(NOBODY);

fn thread_word(pid: u32, tid: u32) -> u64 {
    u64::from(pid) << 32 | u64::from(tid)
}

/// Whether the machine's stop names the running thread, for the one Ring 3
/// boundary that ranks this against the kill mark. A sibling thread of the
/// caller's process stops like any other.
pub fn stops_this_thread() -> bool {
    let caller = CALLER.load(Acquire);
    if caller == NOBODY {
        return false;
    }
    let (Some(pid), Some(tid)) = (percpu::current_pid(), percpu::current_tid()) else {
        return false;
    };
    thread_word(pid.raw(), tid.raw()) != caller
}

/// Whether this boot's stop has begun; once it has, it never ends.
pub fn begun() -> bool {
    CALLER.load(Acquire) != NOBODY
}

/// Whether this thread is the one shutdown this boot gets: a second caller
/// stops with the rest.
pub fn claim_the_shutdown() -> bool {
    // One exchange: a read and a write that can come apart let two callers in.
    !CLAIMED.swap(true, AcqRel)
}

static CLAIMED: AtomicBool = AtomicBool::new(false);

/// This boot's stop has begun and every other CPU has been through Ring 0
/// since: no thread but the caller is in Ring 3, and none returns to it.
pub struct Stopping(());

/// Begin the stop. Returns once every other CPU has begun a scheduler pass:
/// whatever it returns to Ring 3 after that pass reads the stop begun.
#[must_use]
pub fn stop() -> Stopping {
    // Refused by name rather than defaulted: a caller with no task identity is
    // not a reboot syscall.
    let pid = percpu::current_pid().expect("quiesce::stop: the caller holds no process");
    let tid = percpu::current_tid().expect("quiesce::stop: the caller holds no thread");
    CALLER.store(thread_word(pid.raw(), tid.raw()), Release);
    crate::sched::driver::pass_on_every_other_cpu();
    Stopping(())
}
