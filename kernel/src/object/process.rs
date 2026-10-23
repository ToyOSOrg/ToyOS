//! A process, as something a handle can name.
//!
//! The end lives on the object, not a table entry: no zombie, no reap,
//! no orphan adoption. A wait after the fact reads a value; a wait before it
//! parks and is woken by the publish. An `OP_WATCH` reads the same fact at its
//! look, so a handle is readable from the publish on. A process nobody holds a
//! handle to disappears.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

use toyos_abi::end::End;
use toyos_abi::syscall::ProcessStats;

use crate::mm::charge::{self, Exhausted};
use crate::process::Pid;
use crate::watch::Watch;
use crate::sync::Lock;

use super::{KObjectVariant, ObjectCore, Released};

/// What is left of a process once it has stopped running, built only once
/// its own releases have run.
pub struct Exit {
    pub end: End,
    pub stats: ProcessStats,
    _released: Released,
}

impl Exit {
    pub(crate) fn new(end: End, stats: ProcessStats, released: Released) -> Self {
        Self { end, stats, _released: released }
    }
}

pub struct ProcessObject {
    pub(super) core: ObjectCore,
    pid: Pid,
    /// Written exactly once, by whichever teardown path owns this process.
    exit: Lock<Option<Exit>>,
    /// The same fact, without the lock, for a waiter's per-wake predicate.
    finished: AtomicBool,
    /// What `SYS_PROCESS_WAIT` arms on; holding the `Arc` across the park keeps the watch from outliving its subject.
    /// An `Arc` of its own, as a port's is, for the share `ops::read_watch` answers.
    watch: Arc<Watch>,
}

impl ProcessObject {
    pub fn new(pid: Pid) -> Result<Arc<Self>, Exhausted> {
        Ok(Arc::new(Self {
            core: Self::new_core(charge::arc::<Watch>())?,
            pid,
            exit: Lock::new(None),
            finished: AtomicBool::new(false),
            watch: Arc::new(Watch::new()),
        }))
    }

    pub fn pid(&self) -> Pid {
        self.pid
    }

    pub fn finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }

    pub fn end(&self) -> Option<End> {
        self.exit.lock().as_ref().map(|e| e.end)
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
                "pid {} published two exits ({:?} then {:?})",
                self.pid,
                slot.as_ref().map(|e| e.end),
                exit.end,
            );
            *slot = Some(exit);
        }
        self.finished.store(true, Ordering::Release);
        // Must come after the store: reap_finished polls this flag, not the lock.
        crate::scheduler::note_reapable();
        self.watch.post();
    }
}
