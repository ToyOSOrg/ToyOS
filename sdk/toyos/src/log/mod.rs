//! The machine's log from userland: a program's own lines, and the kernel's
//! records for the programs endowed to read them.
//!
//! **A program's lines are records in a ring of its own** ([`region`],
//! [`ring`]): its stdout and stderr, and what it says with [`say!`](crate::say),
//! [`warn!`](crate::warn) and [`error!`](crate::error), each stamped with its
//! time, severity and thread as it is written ([`stdio`]). Whose lines they are is decided where the ring was made:
//! the supervisor names each ring to `/system/bin/logkeeper`, and nothing a
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

use toyos_abi::log::{CursorHead, LogRecord, CURSOR_HEAD_WORDS};
use toyos_abi::syscall::{self, SyscallError};

/// The record and its shape, re-exported so a reader names them through the
/// SDK. Userland does not depend on `toyos-abi` directly, and a program that
/// had to would be one this module had forgotten to finish.
pub use toyos_abi::log::{MAX_RECORD_MESSAGE, RECORD_BYTES};
/// `LogRecord` is what a read fills, so a caller has to be able to size a
/// buffer of them.
pub use toyos_abi::log::LogRecord as Record;

use crate::syscap::SysCap;
use crate::AsHandle;

/// How many `u64`s a cursor over the machine's log takes: a head and a
/// position per shard, a shard per CPU the firmware names. Asked of the
/// kernel, which answers it to a cursor of no positions.
pub fn cursor_words(cap: &SysCap) -> Result<usize, SyscallError> {
    words_for(|cursor| syscall::log_read(cap.as_handle().raw(), cursor, &mut [LogRecord::EMPTY]))
}

/// [`cursor_words`] for a read of either kind.
pub(crate) fn words_for(read: impl FnOnce(&mut [u64]) -> Result<usize, SyscallError>) -> Result<usize, SyscallError> {
    let mut head = [0; CURSOR_HEAD_WORDS];
    match read(&mut head) {
        // A cursor of no positions on a machine of shards is refused, and still answered the count.
        Err(SyscallError::InvalidArgument) | Ok(_) => {}
        Err(e) => return Err(e),
    }
    Ok(CURSOR_HEAD_WORDS + CursorHead::from_words(head).shards as usize)
}

/// A cursor's head, out of its words.
pub(crate) fn head_of(words: &[u64]) -> CursorHead {
    let mut head = [0; CURSOR_HEAD_WORDS];
    head.copy_from_slice(&words[..CURSOR_HEAD_WORDS]);
    CursorHead::from_words(head)
}

/// One reader's position in the machine's log, in words the reader owns:
/// [`cursor_words`] of them, zeroed.
///
/// A fresh tail starts at the oldest record every shard still holds, which is
/// the whole boot on a machine that has not logged 512 records on any CPU yet —
/// so `logkeeper` starting late still writes this boot's log from its first
/// line.
pub struct LogTail<W> {
    words: W,
    /// Every read's loss, summed: the kernel answers each read's alone.
    lost: u64,
}

impl<W: AsRef<[u64]> + AsMut<[u64]>> LogTail<W> {
    pub fn new(words: W) -> Self {
        Self { words, lost: 0 }
    }

    /// Records this cursor never saw because a producer overwrote them.
    pub fn lost(&self) -> u64 {
        self.lost
    }

    /// Shards the machine has, once a read has answered. Zero before that.
    pub fn shards(&self) -> u32 {
        head_of(self.words.as_ref()).shards
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
        let count = syscall::log_read(cap.as_handle().raw(), self.words.as_mut(), out)?;
        self.lost += head_of(self.words.as_ref()).lost;
        Ok(&out[..count])
    }
}
