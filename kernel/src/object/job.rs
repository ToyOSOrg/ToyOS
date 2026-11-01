//! A handle to a job: the owner a process's threads, and its children's unless
//! a spawn names another, share CPU time under (`kernel::sched::job`), and the
//! account their memory is counted to (`kernel::frames::Ledger`). A spawn
//! that endows one under `JOB_LABEL` with `WRITE` puts its child in it.
//!
//! The job lives while a thread of it runs, a handle to it is held or an
//! object charged to it lives, and not past all three: its launcher ending
//! takes nothing from it.
//!
//! **A floor lasts as long as the object it was set through**: a `SysCap`
//! holding `FLOOR` sets one through this object, and its last handle's
//! going lets the floor go. A readable poll answers once memory the job was
//! refused is free for it (`pmm::ready`).

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use kernel::sched::job::{Job, JobKey};
use toyos_abi::handle::Rights;
use toyos_abi::syscall::{AccountInfo, SyscallError};

use crate::mm::charge::Exhausted;
use crate::mm::{pmm, PAGE_2M, PAGE_SIZE};

use super::{KObjectVariant, ObjectCore};

pub struct JobObject {
    /// Visible to `object`, where `kobject!` generates this type's `core()`.
    pub(super) core: ObjectCore,
    job: Arc<Job>,
    /// Whether the job's floor was set through this object, which then holds it.
    floor: AtomicBool,
}

impl JobObject {
    /// What a handle the kernel mints to a job carries: `WRITE` to place a
    /// child in it or set its floor, `READ` to read its account, `WAIT` to
    /// wait for memory it was refused, `DUP` and `TRANSFER` to hand that on.
    pub const RIGHTS: Rights =
        Rights::WRITE.union(Rights::READ).union(Rights::WAIT).union(Rights::DUP).union(Rights::TRANSFER);

    /// Charged the job besides, which a job a thread runs in can outlive.
    pub fn new(job: Arc<Job>) -> Result<Arc<Self>, Exhausted> {
        Ok(Arc::new(Self { core: Self::new_core(crate::mm::charge::arc::<Job>())?, job, floor: AtomicBool::new(false) }))
    }

    pub fn job(&self) -> &Arc<Job> {
        &self.job
    }

    /// Hold `bytes` back for the job for as long as this object lives:
    /// `InvalidArgument` for none, `AlreadyExists` where the job has a
    /// floor, and `ResourceExhausted` where what is free cannot hold it.
    pub fn set_floor(&self, bytes: u64) -> Result<(), SyscallError> {
        if bytes == 0 {
            return Err(SyscallError::InvalidArgument);
        }
        pmm::hold_floor(&self.job, bytes)?;
        self.floor.store(true, Ordering::Release);
        Ok(())
    }

    /// What the job's account holds.
    pub fn account(&self) -> AccountInfo {
        let u = self.job.memory().usage();
        let pages = |n: u64| n.saturating_mul(PAGE_SIZE);
        AccountInfo {
            floor: pages(u.floor),
            committed: pages(u.committed),
            kernel: u.heap.saturating_add(pages(u.held)).saturating_add(u.runs.saturating_mul(PAGE_2M)),
            peak: pages(u.peak),
            wanted: pages(u.wanted),
            refusals: u.refusals,
        }
    }

    /// Whether memory the job was refused is free for it now.
    pub fn ready(&self) -> bool {
        pmm::ready(&self.job)
    }
}

impl Drop for JobObject {
    fn drop(&mut self) {
        if *self.floor.get_mut() {
            pmm::release_floor(&self.job);
        }
    }
}

/// A job no thread has joined yet.
pub fn new_job() -> Arc<Job> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    Arc::new(Job::new(JobKey(NEXT.fetch_add(1, Ordering::Relaxed))))
}
