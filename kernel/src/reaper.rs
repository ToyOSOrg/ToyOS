//! Finishes the teardowns kills hand it.
//!
//! A kill posts its victim's retires and returns, because the victim may be
//! killing the killer, and a killer that waited for its victim would wait on a
//! thread that is waiting on it. This thread does the waiting instead: the
//! only threads it waits on are killed ones, whose release waits on nothing it
//! does. Which owed kill it takes is `toyos_proclife::teardown::next_released`;
//! the teardowns themselves run one at a time.

use alloc::vec::Vec;

use toyos_proclife::teardown::next_released;
use toyos_sched::task::WaitClass;

use crate::process::{self, Killed};
use crate::sched::kthread::{self, OnPanic, OnStop};
use crate::sched::payload::ANY_RELEASED;
use crate::scheduler::{Parkable, RETIRE_GIVE_UP};
use crate::sync::Lock;
use crate::time::{Deadline, Instant};
use crate::watch;

/// Every owed kill with the instant it was owed, oldest first.
static OWED: Lock<Vec<(Instant, Killed)>> = Lock::new(Vec::new());

/// Spawns the reaper; call once, from `kernel_main`.
pub fn start() {
    // Halts: a dead reaper leaves every later kill unpublished and nothing to say so.
    kthread::spawn("reaper", body, 0, OnPanic::Halt, OnStop::Stops);
}

/// Hand a claimed kill, its retires already posted, to the reaper.
pub fn owe(killed: Killed) {
    OWED.lock().push((crate::clock::now(), killed));
    // Its threads may all be released already, and then no release posts again.
    ANY_RELEASED.post();
}

extern "C" fn body(_arg: u64) -> ! {
    let parkable = Parkable::at_entry();
    loop {
        let armed = watch::arm(&ANY_RELEASED, 0, WaitClass::Other).expect("the reaper is a task");
        let (next, oldest) = {
            let mut owed = OWED.lock();
            let next = next_released(owed.iter().map(|(_, killed)| killed.released()));
            (next.map(|at| owed.remove(at).1), owed.first().map(|(since, killed)| (*since, killed.pid())))
        };
        match next {
            Some(killed) => {
                drop(armed);
                process::finish_kill(killed);
            }
            None => {
                // The oldest's deadline is the earliest, so it is the only one to check.
                let deadline = oldest.map_or(Deadline::never(), |(since, pid)| {
                    let give_up = Deadline::at(since + RETIRE_GIVE_UP.duration());
                    assert!(
                        !give_up.reached(crate::clock::now()),
                        "reaper: pid {pid} not released after {}",
                        RETIRE_GIVE_UP.duration(),
                    );
                    give_up
                });
                watch::wait_uncancellable(&parkable, &armed, deadline);
            }
        }
    }
}
