//! A process, as something a handle can name.
//!
//! The exit code lives on the object, not a table entry: no zombie, no reap,
//! no orphan adoption. A wait after the fact reads a value and an `OP_WATCH`
//! completes at once; before it, the wait parks, the watch registers, and the
//! publish answers both. A process nobody holds a handle to disappears.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

use toyos_abi::syscall::ProcessStats;

use crate::process::Pid;
use crate::watch::Watch;
use crate::sync::Lock;

use super::{KObjectVariant, ObjectCore};

/// What is left of a process once it has stopped running.
pub struct Exit {
    pub code: i32,
    pub stats: ProcessStats,
}

pub struct ProcessObject {
    pub(super) core: ObjectCore,
    pid: Pid,
    /// Written exactly once, by whichever teardown path owns this process.
    exit: Lock<Option<Exit>>,
    /// The same fact, without the lock, for a waiter's per-wake predicate.
    finished: AtomicBool,
    /// What `SYS_PROCESS_WAIT` arms on and an `OP_WATCH` registers on; an `Arc`
    /// so a poll's registration holds it with no object borrowed.
    watch: Arc<Watch>,
}

impl ProcessObject {
    pub fn new(pid: Pid) -> Arc<Self> {
        Arc::new(Self {
            core: Self::new_core(),
            pid,
            exit: Lock::new(None),
            finished: AtomicBool::new(false),
            watch: Arc::new(Watch::new()),
        })
    }

    pub fn pid(&self) -> Pid {
        self.pid
    }

    pub fn finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }

    pub fn exit_code(&self) -> Option<i32> {
        self.exit.lock().as_ref().map(|e| e.code)
    }

    /// The last accounting the process had; `None` while running — sample `ProcessData` instead.
    pub fn final_stats(&self) -> Option<ProcessStats> {
        self.exit.lock().as_ref().map(|e| e.stats)
    }

    pub fn watch(&self) -> &Arc<Watch> {
        &self.watch
    }

    /// Publish the exit and release every waiter; panics on a second call, since that would mean two teardowns claimed one process, which `claim_teardown` prevents.
    pub fn publish_exit(&self, exit: Exit) {
        {
            let mut slot = self.exit.lock();
            assert!(
                slot.is_none(),
                "pid {} published two exits ({} then {})",
                self.pid,
                slot.as_ref().map_or(0, |e| e.code),
                exit.code,
            );
            *slot = Some(exit);
        }
        self.finished.store(true, Ordering::Release);
        // Must come after the store: reap_finished polls this flag, not the lock.
        crate::scheduler::note_reapable();
        self.watch.post();
    }
}
