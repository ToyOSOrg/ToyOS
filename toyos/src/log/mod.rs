//! The machine's log from userland: a program's own lines, and the kernel's
//! records for the programs endowed to read them.
//!
//! **A program's lines are records in a ring of its own** ([`region`],
//! [`ring`]): its stdout and stderr, and what it says with [`say!`](crate::say),
//! [`warn!`](crate::warn) and [`error!`](crate::error), each stamped with its
//! time, severity and thread as it is written ([`stdio`]). Writing never waits,
//! allocates or makes a syscall; a full ring drops the record and its reader
//! counts the drop. Whose lines they are is decided where the ring was made:
//! `/system/bin/init` names each ring to `/system/bin/logd`, and nothing a
//! program writes can change the name.
//!
//! **The kernel's records** are read with [`LogTail`]. The kernel keeps no
//! per-reader state, so all a reader is is a cursor and a buffer. Two readers
//! see the same records and neither consumes the stream.

pub mod region;
pub mod ring;
pub mod stdio;

#[cfg(test)]
mod proof;

pub use stdio::{bind, claim_lane, say, say_lane};
pub use toyos_abi::log::Severity;

/// One line of this program's own, at [`Severity::Info`], onto its stderr:
/// a record in its ring, or a line on whatever else its stderr is.
#[macro_export]
macro_rules! say {
    ($($arg:tt)*) => {
        $crate::log::say($crate::log::Severity::Info, format_args!($($arg)*))
    };
}

/// [`say!`](crate::say) at [`Severity::Warn`].
#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => {
        $crate::log::say($crate::log::Severity::Warn, format_args!($($arg)*))
    };
}

/// [`say!`](crate::say) at [`Severity::Error`].
#[macro_export]
macro_rules! error {
    ($($arg:tt)*) => {
        $crate::log::say($crate::log::Severity::Error, format_args!($($arg)*))
    };
}

use toyos_abi::log::{LogCursor, LogRecord};
use toyos_abi::syscall::{self, SyscallError};

/// The record and its shape, re-exported so a reader names them through the
/// SDK. Userland does not depend on `toyos-abi` directly, and a program that
/// had to would be one this module had forgotten to finish.
pub use toyos_abi::log::{MAX_LOG_SHARDS, MAX_RECORD_MESSAGE, RECORD_BYTES};
/// `LogRecord` is what a read fills, so a caller has to be able to size a
/// buffer of them.
pub use toyos_abi::log::LogRecord as Record;

use crate::syscap::SysCap;
use crate::AsHandle;

/// One reader's position in the machine's log.
///
/// A fresh tail starts at the oldest record every shard still holds, which is
/// the whole boot on a machine that has not logged 512 records on any CPU yet —
/// so `/bin/logd` starting late still writes this boot's log from its first
/// line.
pub struct LogTail {
    cursor: LogCursor,
}

impl Default for LogTail {
    fn default() -> Self {
        Self::new()
    }
}

impl LogTail {
    pub const fn new() -> Self {
        Self { cursor: LogCursor::new() }
    }

    /// Records this cursor never saw because a producer overwrote them.
    ///
    /// Cumulative and exact: the kernel derives it from the two numbers that
    /// have to be right anyway, so it cannot drift from the ring the way a
    /// producer-side counter would.
    pub fn lost(&self) -> u64 {
        self.cursor.lost
    }

    /// Shards the machine has, once a read has answered. Zero before that.
    pub fn shards(&self) -> u32 {
        self.cursor.shards
    }

    /// Fill as much of `out` as there is, oldest first, merged by timestamp.
    ///
    /// Answers an empty slice when there is nothing new; **it does not block**,
    /// so a caller with nothing to do arms on a readiness source and parks.
    pub fn read<'a>(
        &mut self,
        cap: &SysCap,
        out: &'a mut [LogRecord],
    ) -> Result<&'a [LogRecord], SyscallError> {
        let count = syscall::log_read(cap.as_handle(), &mut self.cursor, out)?;
        Ok(&out[..count])
    }
}
