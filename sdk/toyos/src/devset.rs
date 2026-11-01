//! The device set: every PCI function the kernel hands out, as a snapshot
//! read whole and refused at this boundary when it is anything else
//! ([`toyos_abi::devset::decode`]). Minted from a [`crate::syscap::SysCap`]
//! carrying `DEVICE`; its handle can be neither duplicated nor handed on.
//!
//! **Readable while the set differs from the last read through the handle**,
//! so a reader waits on it with a poller and reads after every wake; a
//! snapshot whose header names a settle time is read again by then.

use toyos_abi::devset::{self, Malformed, Snapshot};
use toyos_abi::inventory::RawRecord;
use toyos_abi::syscall::{self, SyscallError};

use crate::{AsHandle, OwnedHandle};

pub struct DeviceSet(pub(crate) OwnedHandle);

/// How many times [`DeviceSet::read`] asks again after the set grew between
/// counting it and reading it.
const ROUNDS: usize = 4;

/// Why the set was not read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unread {
    Count(SyscallError),
    Read(SyscallError),
    /// The set grew between the count and the read on every round.
    Grew,
    Malformed(Malformed),
}

impl core::fmt::Display for Unread {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Count(e) => write!(f, "the device set would not count: {e:?}"),
            Self::Read(e) => write!(f, "the device set would not read: {e:?}"),
            Self::Grew => write!(f, "the device set changed on each of {ROUNDS} reads"),
            Self::Malformed(why) => write!(f, "the device set's snapshot is refused: {why}"),
        }
    }
}

impl DeviceSet {
    /// The snapshot, read whole into what `buffer(n)` answers and decoded,
    /// handed to `read`.
    pub fn read<B, R>(&self, mut buffer: impl FnMut(usize) -> B, read: impl FnOnce(Snapshot<'_>) -> R) -> Result<R, Unread>
    where
        B: AsMut<[RawRecord]>,
    {
        let handle = self.0.raw();
        for _ in 0..ROUNDS {
            let count = syscall::device_set_read(handle, &mut []).map_err(Unread::Count)?;
            let mut raw = buffer(count);
            let raw = raw.as_mut();
            assert_eq!(raw.len(), count, "`buffer({count})` answered {} records", raw.len());
            match syscall::device_set_read(handle, raw) {
                Ok(n) => return devset::decode(&raw[..n]).map(read).map_err(Unread::Malformed),
                Err(SyscallError::ResourceExhausted) => continue,
                Err(e) => return Err(Unread::Read(e)),
            }
        }
        Err(Unread::Grew)
    }
}

impl AsHandle for DeviceSet {
    fn as_handle(&self) -> crate::BorrowedHandle<'_> {
        self.0.as_handle()
    }
}
