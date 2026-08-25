//! `iod`: the context deferred filesystem work runs in, because `Drop` has no
//! `Parkable`.
//!
//! The third of the kernel's three threads — `klogd` drains the console
//! (`log/console.rs`), `usbd` owns the xHCI port machine
//! (`drivers::xhci::usbd`), and this one owns the write-back queue: the flush
//! a closed file's dirty pages owe, and page-cache eviction's.
//!
//! **Why the write-back cannot stay where it is.** `OpenFileState::drop`
//! (`object/file.rs`) used to take the VFS lock and flush. Once that lock is a
//! `SleepLock` the flush needs a [`crate::scheduler::Parkable`], and a `Drop`
//! impl cannot take one — a compile-time property, which is why this thread
//! exists rather than a rule saying "don't block in `Drop`". So the flush
//! became a queue (`crate::writeback`):
//! the last close pins the file and pushes it, and this thread drains it and
//! runs the teardown the `Drop` used to. `SYS_CLOSE` never promised durability,
//! so it does not wait; `SYS_FSYNC` did, so it still flushes inline — while the
//! VFS is a spinlock a caller that asked for durability takes it directly, and
//! only once it is a sleep lock does `fsync` too have to submit here and park.
//!
//! **The drain landed with the write-back queue** (wall 4 of
//! `issues/kernel/every-wait-in-this-kernel-is-a-spin.md`); the loop below
//! calls `writeback::drain_all`, and `SYS_SHUTDOWN` calls the same function
//! before `sync_all` so a file closed but not yet drained is still made durable
//! on the way down. This chunk converts no lock: the VFS is still a spinlock and
//! this thread spins in the driver like any other.
//!
//! **One `iod`, machine-wide, is a serialisation point, and now it has a
//! number.** At the 128-core target the root `CLAUDE.md` sets, one thread
//! draining the write-back of 128 cores' closed files is a serialisation point
//! per-CPU `iod`s are the obvious escape from. With the write-back queue
//! landed the producers exist, so it was measured: a 6-thread burst that
//! closes 360 modified files on NVMe `/home` at once drove the worst
//! close-to-drained latency to **~72 ms** — the whole backlog drained serially
//! at ~200 µs a file, because each drain holds the VFS spinlock across an NVMe
//! round trip and `iod` is one thread. That is the serialisation, quantified: a
//! burst of N closes has a tail of N times the per-file drain, and per-CPU
//! `iod`s are the answer if that tail ever matters. The lock conversion
//! this chunk unblocks turns the held spinlock into a park, which shortens the
//! per-file cost but not the one-thread serialisation.
//!
//! **Its panic is recoverable.** A killed `iod` costs the machine its deferred
//! write-back — dirty pages stop reaching the device — and that is a loss
//! `SYS_FSYNC`'s own error path and `/bin/logd`'s give-up policy can both see.
//! `klogd`'s is not recoverable for the opposite reason: its loss is the one
//! nothing left alive can report.

use toyos_sched::task::WaitClass;

use crate::completion::{self, Subject};
use crate::sched::kthread::{self, OnPanic};
use crate::scheduler;
use crate::time::Deadline;

/// The name `sched::dump`, `ps` and a crash report use.
const NAME: &str = "iod";

/// Start the thread. Called once, from `kernel_main`, beside `klogd`'s.
pub fn start() {
    let _ = kthread::spawn(NAME, body, 0, OnPanic::Recover);
}

extern "C" fn body(_arg: u64) -> ! {
    let parkable = scheduler::Parkable::at_entry();
    // The task half of the operation-nesting gate, and this thread is where it
    // lives because of the sentence above: `iod` is the one context in this
    // kernel that is a task, exists on every boot, and reaches its loop with
    // nothing to do — so asking the question here displaces no work and needs
    // no thread of its own. Every establishment any other gate drives is a
    // boot phase's, which is the *other* slot (`kernel/src/sched_gate.rs`).
    // Between `at_entry` above and the arm below because it must not be inside
    // an establishment, which is what `at_entry` refuses, and it leaves none.
    #[cfg(feature = "boot-actuators")]
    if crate::actuator::sched_operation_nesting() {
        crate::sched_gate::run("iod");
    }
    // Armed once and held across the loop — the edge contract: a producer
    // that pushes while this thread is draining must find the watch still
    // armed. The subject is the write-back queue's own watch, which is where a
    // closed file's `writeback::enqueue` posts.
    let armed = completion::arm(
        Subject::of(&crate::writeback::WORK),
        crate::writeback::TOKEN,
        WaitClass::Io,
    )
    .expect("a kernel thread is a task and can arm");
    loop {
        // A test can hold a closed file's write-back pending — so it can prove a
        // re-open before the drain reads the buffered pages and not the device —
        // by parking this thread before any teardown. `SYS_SHUTDOWN`'s own drain
        // is not on this thread and is never stalled, so a stalled boot still
        // shuts down. The accessor folds to `false` in a shipping kernel.
        #[cfg(feature = "boot-actuators")]
        if crate::actuator::writeback_stall() {
            let _ = completion::wait(&parkable, &armed, Deadline::never());
            continue;
        }
        // Drain every file a close has pinned, flushing each under the VFS lock
        // with this thread's own `Parkable`. At this chunk the VFS is still a
        // spinlock, so this drive spins like any thread's disk wait — legal for a
        // dedicated housekeeping thread, and the lock conversion is a later
        // chunk that needs no change here.
        //
        // `drain_all_iod`, not `drain_all`: this thread holds the `WORK` arm
        // across the loop, and the drain's inter-attempt backoff waits on that
        // same arm rather than arming a second (which `completion::arm` refuses).
        crate::writeback::drain_all_iod(&parkable, &armed);
        // No deadline: what ends this wait is a push, and a periodic wake on a
        // machine with nothing to write back is an audio change (root
        // `CLAUDE.md`). The cancel arm is unreachable: nothing retires a kernel
        // thread.
        let _ = completion::wait(&parkable, &armed, Deadline::never());
    }
}
