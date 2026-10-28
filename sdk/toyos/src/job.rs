//! A job: the owner a process's threads, and its children's unless a spawn
//! names another, share CPU time under. A spawn endows one under
//! [`JOB_LABEL`] to put its child in it; [`own`] is this process's own, and
//! [`SysCap::new_job`](crate::syscap::SysCap::new_job) makes an independent
//! one.
//!
//! **A job is also the account its memory is counted to** ([`Job::account`]),
//! and a readable poll on its handle answers once memory it was refused is
//! free for it again: what a launcher waits on before it tries again.

use toyos_abi::syscall::{self, AccountInfo, SyscallError};

pub use toyos_abi::syscall::JOB_LABEL;

use crate::{AsHandle, OwnedHandle, RawHandle};

pub struct Job(pub(crate) OwnedHandle);

/// A handle to the job this process runs in.
pub fn own() -> Result<Job, SyscallError> {
    // SAFETY: the kernel installed this handle for this call and no other.
    syscall::job_own().map(|raw| Job(unsafe { OwnedHandle::from_raw(raw) }))
}

impl Job {
    /// A second handle to the same job, for a spawn to move.
    pub fn duplicate(&self) -> Result<Job, SyscallError> {
        self.0.try_clone().map(Job)
    }

    /// What its account holds.
    pub fn account(&self) -> Result<AccountInfo, SyscallError> {
        let mut info = AccountInfo::default();
        syscall::account_info(self.0.raw(), &mut info)?;
        Ok(info)
    }

    /// Give up ownership, for a spawn that moves it.
    pub fn into_raw(self) -> RawHandle {
        self.0.into_raw()
    }
}

impl AsHandle for Job {
    fn as_handle(&self) -> crate::BorrowedHandle<'_> {
        self.0.as_handle()
    }
}
