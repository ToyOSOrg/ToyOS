//! A process, as something a handle can name.
//!
//! The exit code lives on the object, not a table entry: no zombie, no reap,
//! no orphan adoption. A wait after the fact reads a value; a wait before it
//! parks and is woken by the publish. A process nobody holds a handle to
//! disappears.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

use toyos_abi::syscall::ProcessStats;

use crate::process::Pid;
use crate::completion::{self, Outcome, Subject, Watch};
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
    /// What `SYS_PROCESS_WAIT` arms on; holding the `Arc` across the park keeps the watch from outliving its subject.
    watch: Watch,
}

impl ProcessObject {
    pub fn new(pid: Pid) -> Arc<Self> {
        Arc::new(Self {
            core: Self::new_core(),
            pid,
            exit: Lock::new(None),
            finished: AtomicBool::new(false),
            watch: Watch::new(),
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

    pub fn watch(&self) -> &Watch {
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
        completion::post(Subject::of(&self.watch), Outcome::Ready);
    }
}

/// Control for the `reopenable` row: a process the table still answers for
/// takes a fresh handle after its last one has gone. These are
/// `sys_process_open`'s own two steps with the `SysCap` demand left off, and
/// the second install is where the resurrection assert used to fire.
#[cfg(feature = "boot-actuators")]
pub(crate) fn reopen_selftest(pid: Pid) {
    use super::handle::HandleTable;
    use super::{ops, KObjectRef};
    use toyos_abi::handle::Rights;

    let Some(object) = crate::process::process_object(pid) else {
        crate::log!("process-reopen: FAIL (pid {} names no process)", pid.raw());
        return;
    };
    let mut table = HandleTable::new();
    let opened = match ops::install(&mut table, KObjectRef::Process(Arc::clone(&object))) {
        Ok(h) => h,
        Err(e) => {
            crate::log!("process-reopen: FAIL (the first install was refused: {e:?})");
            return;
        }
    };
    match table.remove(opened) {
        Ok(entry) => drop(entry),
        Err(e) => {
            crate::log!("process-reopen: FAIL (the first handle would not close: {e})");
            return;
        }
    }
    let retired = object.core().retired();
    let reopened = ops::install(&mut table, KObjectRef::Process(Arc::clone(&object)))
        .ok()
        .and_then(|h| table.get::<ProcessObject>(h, Rights::WAIT).ok())
        .is_some_and(|reached| reached.pid() == pid);
    let verdict = if reopened && !retired { "PASS" } else { "FAIL" };
    crate::log!(
        "process-reopen: {verdict} (pid={} retired={retired} reopened={reopened})",
        pid.raw(),
    );
}
