//! A handle to a job: the owner a process's threads, and its children's unless
//! a spawn names another, share CPU time under (`kernel::sched::job`). A spawn
//! that endows one under `JOB_LABEL` with `WRITE` puts its child in it.
//!
//! The job lives while a thread of it runs or a handle to it is held, and
//! not past both: its launcher ending takes nothing from it.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

use kernel::sched::job::{Job, JobKey};
use toyos_abi::handle::Rights;

use super::{KObjectVariant, ObjectCore};

pub struct JobObject {
    /// Visible to `object`, where `kobject!` generates this type's `core()`.
    pub(super) core: ObjectCore,
    job: Arc<Job>,
}

impl JobObject {
    /// What a handle the kernel mints to a job carries: `WRITE` to place a
    /// child in it, `DUP` and `TRANSFER` to hand that on.
    pub const RIGHTS: Rights = Rights::WRITE.union(Rights::DUP).union(Rights::TRANSFER);

    pub fn new(job: Arc<Job>) -> Arc<Self> {
        Arc::new(Self { core: Self::new_core(), job })
    }

    pub fn job(&self) -> &Arc<Job> {
        &self.job
    }
}

/// A job no thread has joined yet.
pub fn new_job() -> Arc<Job> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    Arc::new(Job::new(JobKey(NEXT.fetch_add(1, Ordering::Relaxed))))
}
