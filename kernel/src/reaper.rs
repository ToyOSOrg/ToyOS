//! Finishes the teardowns kills hand it.
//!
//! A kill posts its victim's retires and returns, because the victim may be
//! killing the killer, and a killer that waited for its victim would wait on a
//! thread that is waiting on it. This thread does the waiting instead: no
//! process can kill it, and the only threads it waits on are killed ones,
//! whose release waits on nothing it does. It stops with userland
//! (`quiesce::exempt`), since the teardowns it runs are userland's.

use alloc::collections::VecDeque;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::process::{self, Killed};
use crate::sched::kthread::{self, OnPanic};
use crate::scheduler::{Parkable, TaskId};
use crate::sync::Lock;
use crate::watch::{self, Watch};

const NAME: &str = "reaper";

static OWED: Lock<VecDeque<Killed>> = Lock::new(VecDeque::new());

static WORK: Watch = Watch::new();

/// The reaper's packed `TaskId`, stored before the machine is released.
static REAPER: AtomicU64 = AtomicU64::new(u64::MAX);

/// Spawns the reaper; call once, from `kernel_main`.
pub fn start() {
    // Halts: a dead reaper leaves every later kill unpublished and nothing to say so.
    let (id, _) = kthread::spawn(NAME, body, 0, OnPanic::Halt);
    REAPER.store(id.pack(), Ordering::Release);
}

pub fn is(id: TaskId) -> bool {
    REAPER.load(Ordering::Acquire) == id.pack()
}

/// Hand a claimed kill, its retires already posted, to the reaper.
pub fn owe(killed: Killed) {
    OWED.lock().push_back(killed);
    WORK.post();
}

extern "C" fn body(_arg: u64) -> ! {
    let parkable = Parkable::at_entry();
    loop {
        watch::wait_uncancellable_until(&parkable, &WORK, 0, || !OWED.lock().is_empty());
        let killed = OWED.lock().pop_front().expect("the one consumer saw an entry");
        process::finish_kill(killed);
    }
}
