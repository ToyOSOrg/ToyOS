//! A job: the owner a process's threads, and its children's unless a spawn
//! names another, share CPU time under. A spawn endows one under
//! [`JOB_LABEL`] to put its child in it; [`own`] is this process's own, and
//! [`SysCap::new_job`](crate::syscap::SysCap::new_job) makes an independent
//! one.

use toyos_abi::syscall::{self, SyscallError};

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
        // SAFETY: the kernel installed this handle for this call and no other.
        syscall::dup(self.0.raw()).map(|raw| Job(unsafe { OwnedHandle::from_raw(raw) }))
    }

    /// Give up ownership, for a spawn that moves it.
    pub fn into_raw(self) -> RawHandle {
        self.0.into_raw()
    }
}

impl AsHandle for Job {
    fn as_handle(&self) -> RawHandle {
        self.0.raw()
    }
}
