//! The releases a dying process's last thread owes before its end is
//! published: while a [`Releases`] is open, every deferred object whose last
//! handle that thread drops, a hook's included, is collected on the thread
//! rather than the zero-handle queue, so no other CPU's drain can take it.
//! Carried by the thread's `TaskHandle`, so a migration mid-teardown keeps it.
//!
//! [`Releases::run`] is the only way to end one and the only maker of a
//! [`Released`], which is what a process's `process::Exit` is built from.

use alloc::sync::Arc;
use alloc::vec::Vec;

use super::KObjectRef;
use crate::sched::payload::TaskHandle;

pub(crate) struct Releases {
    task: Arc<TaskHandle>,
    /// The preempt depth it was opened at: a run at any other depth holds a
    /// lock, under which no hook may run.
    depth: u32,
}

/// Every release the dying process's handles owed has run.
pub(crate) struct Released(());

/// `object` collected by the running thread's open scope, or handed back when
/// it has none. The handle is cloned out of the scheduler's peek before its
/// lock is taken: a lock's release can run a pass, which no peek may span.
pub(super) fn collect(object: KObjectRef) -> Option<KObjectRef> {
    let Some(task) = crate::sched::driver::current_handle() else { return Some(object) };
    let mut owed = task.releases().lock();
    match owed.as_mut() {
        Some(owed) => {
            owed.push(object);
            None
        }
        None => Some(object),
    }
}

impl Releases {
    /// Open the running thread's scope. Opened with no lock held.
    pub(crate) fn open() -> Self {
        let task = crate::sched::driver::current_handle()
            .expect("object: a process's releases are opened on one of its threads");
        let previous = task.releases().lock().replace(Vec::new());
        assert!(previous.is_none(), "object: a thread opened a second release scope");
        Self { task, depth: crate::preempt::count() }
    }

    /// Run every collected hook on this thread, those the hooks retire
    /// included, then close the scope.
    pub(crate) fn run(self) -> Released {
        let depth = crate::preempt::count();
        assert!(
            depth == self.depth,
            "object: a release scope run at preempt depth {depth}, opened at {}: a lock is held",
            self.depth,
        );
        loop {
            let batch = core::mem::take(
                self.task.releases().lock().as_mut().expect("object: an open scope holds its list"),
            );
            if batch.is_empty() {
                break;
            }
            for object in batch {
                object.run_zero_handles();
            }
        }
        *self.task.releases().lock() = None;
        Released(())
    }
}

impl Drop for Releases {
    fn drop(&mut self) {
        assert!(
            self.task.releases().lock().is_none(),
            "object: a release scope was dropped with its releases unrun",
        );
    }
}
