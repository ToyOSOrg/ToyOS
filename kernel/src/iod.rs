//! Drains the write-back queue and evicts the page cache; `SYS_CLOSE` does not wait on it, `SYS_FSYNC` flushes inline instead.

use toyos_sched::task::WaitClass;

use crate::completion::{self, Subject};
use crate::sched::kthread::{self, OnPanic};
use crate::scheduler;
use crate::time::Deadline;

// Relied on by name in `sched::dump`, `ps`, and crash reports.
const NAME: &str = "iod";

/// Spawns the `iod` kthread; call once, from `kernel_main`.
pub fn start() {
    // Recoverable: unlike klogd's silent loss, a killed iod's stalled write-back is visible to SYS_FSYNC and logd.
    let _ = kthread::spawn(NAME, body, 0, OnPanic::Recover);
}

extern "C" fn body(_arg: u64) -> ! {
    let parkable = scheduler::Parkable::at_entry();
    // Probes the nesting gate here: iod is the one task every boot has, idle at entry.
    #[cfg(feature = "boot-actuators")]
    if crate::actuator::sched_operation_nesting() {
        crate::sched_gate::run("iod");
    }
    // Held across the loop: a push during a drain must still find this watch armed.
    let armed = completion::arm(
        Subject::of(&crate::writeback::WORK),
        crate::writeback::TOKEN,
        WaitClass::Io,
    )
    .expect("a kernel thread is a task and can arm");
    loop {
        #[cfg(feature = "boot-actuators")]
        if crate::actuator::writeback_stall() {
            let _ = completion::wait(&parkable, &armed, Deadline::never());
            continue;
        }
        // drain_all_iod, not drain_all: this thread already holds the WORK arm; drain's backoff parks on it instead of arming a second.
        crate::writeback::drain_all_iod(&parkable, &armed);
        // No deadline: only a push should end this wait; a periodic wake would need audio sign-off.
        // Discarded: nothing retires a kernel thread, so this wait never reports Cancelled.
        let _ = completion::wait(&parkable, &armed, Deadline::never());
    }
}
