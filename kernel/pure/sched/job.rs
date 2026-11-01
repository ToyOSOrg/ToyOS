//! A job: the owner every ordinary thread's CPU time is shared out by. A
//! process's threads and its helpers' threads hold the same job, so a helper
//! buys no entitlement: each CPU weighs the group of the job's threads it
//! holds by that group's share of the job's runnable threads.
//!
//! The job counts its runnable ordinary threads on every CPU; each CPU reads
//! the count when it decides, so a change on one CPU reaches the job's groups
//! elsewhere at their own CPUs' next decisions.
//!
//! **A job is also the account its memory is counted to** ([`Ledger`]):
//! every page its threads commit, and the kernel memory its objects keep.

use crate::frames::Ledger;
use crate::sched::sync::{AtomicU32, Ordering};

/// Monotonic and never reused: a run queue keys the job's group by it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct JobKey(pub u64);

/// Every job's weight. Independent jobs share equally, whoever started them.
pub const JOB_WEIGHT: u64 = 1 << 16;

pub struct Job {
    key: JobKey,
    /// Runnable ordinary threads of this job, on every CPU.
    runnable: AtomicU32,
    memory: Ledger,
}

/// Every charge, page and run of a job holds it, so the last of it going
/// is its account at nothing: anything left is a give-back that missed it.
impl Drop for Job {
    fn drop(&mut self) {
        let usage = self.memory.usage();
        assert!(
            self.memory.is_empty() && usage.floor == 0 && usage.entitled == 0,
            "job {:?} ended holding {usage:?}",
            self.key
        );
    }
}

impl Job {
    pub fn new(key: JobKey) -> Self {
        Self { key, runnable: AtomicU32::new(0), memory: Ledger::new() }
    }

    pub fn memory(&self) -> &Ledger {
        &self.memory
    }

    pub fn key(&self) -> JobKey {
        self.key
    }

    pub fn runnable(&self) -> u32 {
        self.runnable.load(Ordering::Relaxed)
    }

    pub(crate) fn enter(&self) {
        self.runnable.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn leave(&self) {
        let before = self.runnable.fetch_sub(1, Ordering::Relaxed);
        assert!(before > 0, "job {:?}: a thread left that never entered", self.key);
    }

    /// The weight of a group holding `here` of the job's runnable threads.
    ///
    /// Read on the CPU holding them, whose own entries the count already
    /// includes: a count below `here` is a bookkeeping bug.
    pub fn group_weight(&self, here: u32) -> u64 {
        let all = self.runnable();
        assert!(here <= all, "job {:?}: {here} threads queued here of {all} runnable", self.key);
        (JOB_WEIGHT * u64::from(here) / u64::from(all)).max(1)
    }

    /// What one of its runnable threads weighs wherever it is queued, which a
    /// move from one CPU to another shifts between them.
    pub fn thread_weight(&self) -> u64 {
        (JOB_WEIGHT / u64::from(self.runnable().max(1))).max(1)
    }
}
