//! Finishes the teardowns kills hand it.
//!
//! A kill posts its victim's retires and returns, because the victim may be
//! killing the killer, and a killer that waited for its victim would wait on a
//! thread that is waiting on it. This thread does the waiting instead: the
//! only threads it waits on are killed ones, whose release waits on nothing it
//! does. It takes whichever owed kill is released first, so a slow victim holds
//! up no other kill's release; the teardowns themselves run one at a time.

use alloc::vec::Vec;

use toyos_sched::task::WaitClass;

use crate::process::{self, Killed};
use crate::sched::kthread::{self, OnPanic, OnStop};
use crate::sched::payload::ANY_RELEASED;
use crate::scheduler::{Parkable, RETIRE_GIVE_UP};
use crate::sync::Lock;
use crate::time::{Deadline, Instant};
use crate::watch::{self, Watch};

const NAME: &str = "reaper";

/// Every owed kill with the instant it was owed, oldest first.
static OWED: Lock<Vec<(Instant, Killed)>> = Lock::new(Vec::new());

/// What an idle reaper waits on.
static WORK: Watch = Watch::new();

/// Spawns the reaper; call once, from `kernel_main`.
pub fn start() {
    // Halts: a dead reaper leaves every later kill unpublished and nothing to say so.
    kthread::spawn(NAME, body, 0, OnPanic::Halt, OnStop::Stops);
}

/// Hand a claimed kill, its retires already posted, to the reaper.
pub fn owe(killed: Killed) {
    OWED.lock().push((crate::clock::now(), killed));
    // Both: an idle reaper waits on `WORK`, a busy one on `ANY_RELEASED`, and
    // this kill's threads may all be released already.
    WORK.post();
    ANY_RELEASED.post();
}

extern "C" fn body(_arg: u64) -> ! {
    let parkable = Parkable::at_entry();
    loop {
        process::finish_kill(next_released(&parkable));
    }
}

/// The oldest owed kill whose every thread is released, once there is one.
fn next_released(parkable: &Parkable) -> Killed {
    loop {
        let idle = OWED.lock().is_empty();
        let armed = watch::arm(if idle { &WORK } else { &ANY_RELEASED }, 0, WaitClass::Other)
            .expect("the reaper is a task");
        let oldest = {
            let mut owed = OWED.lock();
            if let Some(at) = owed.iter().position(|(_, killed)| killed.released()) {
                return owed.remove(at).1;
            }
            // Armed on the watch that no longer wakes this state.
            if owed.is_empty() != idle {
                continue;
            }
            owed.first().map(|(since, killed)| (*since, killed.pid()))
        };
        let deadline = match oldest {
            None => Deadline::never(),
            Some((since, pid)) => {
                let give_up = Deadline::at(since + RETIRE_GIVE_UP.duration());
                assert!(
                    !give_up.reached(crate::clock::now()),
                    "reaper: pid {pid} not released after {}",
                    RETIRE_GIVE_UP.duration(),
                );
                give_up
            }
        };
        watch::wait_uncancellable(parkable, &armed, deadline);
    }
}
