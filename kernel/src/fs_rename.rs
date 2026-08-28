//! The one order a replacing rename runs in, shared by every mount adapter: the
//! source is validated before the destination is disturbed, `old == new` is a
//! no-op, and the displaced destination is freed only once the move commits.
//! [`replace_rename`] is the sole sequencer, and [`Committed`] — which only
//! [`ReplaceRename::commit`] mints — is what [`ReplaceRename::release`] consumes,
//! so freeing a destination before the move commits fails to compile.

use toyos_abi::syscall::SyscallError;

/// Proof the move committed, carrying whatever the displaced destination left
/// for [`ReplaceRename::release`] to free. Only a successful `commit` mints one.
#[must_use]
pub(crate) struct Committed<T>(T);

impl<T> Committed<T> {
    pub(crate) fn new(displaced: T) -> Self {
        Committed(displaced)
    }

    pub(crate) fn into_displaced(self) -> T {
        self.0
    }
}

/// A mount adapter's three rename phases, run in order by [`replace_rename`].
pub(crate) trait ReplaceRename {
    /// What a displaced destination leaves for `release` to free.
    type Displaced;

    /// Is the source present? The move fails here, disturbing nothing, when not.
    fn source_present(&mut self, old: &str) -> Result<bool, SyscallError>;

    /// Commit the move on the backend, displacing whatever `new` named; on `Err`
    /// nothing durable moved.
    fn commit(&mut self, old: &str, new: &str)
        -> Result<Committed<Self::Displaced>, SyscallError>;

    /// Free the displaced destination and re-key the source, the move committed.
    fn release(&mut self, old: &str, new: &str, committed: Committed<Self::Displaced>);
}

/// Validate the source, treat `old == new` as POSIX's no-op success, then commit
/// and only then release the destination.
pub(crate) fn replace_rename<R: ReplaceRename>(
    adapter: &mut R,
    old: &str,
    new: &str,
) -> Result<(), SyscallError> {
    if !adapter.source_present(old)? {
        return Err(SyscallError::NotFound);
    }
    if old == new {
        return Ok(());
    }
    let committed = adapter.commit(old, new)?;
    adapter.release(old, new, committed);
    Ok(())
}
