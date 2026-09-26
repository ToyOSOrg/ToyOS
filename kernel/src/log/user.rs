//! Kernel side of `SYS_LOG_READ` and its readiness source.
//!
//! No per-reader state in a read: a cursor is the caller's own sequence numbers and loss count, copied in, walked, and copied back; readers coexist uncoordinated. Requires [`Rights::LOG`] on a `SysCap`, not ambient.
//!
//! [`Rights::LOG`]: toyos_abi::handle::Rights::LOG

use toyos_abi::log::{LogCursor, LogRecord, RECORD_BYTES};
use toyos_abi::syscall::SyscallError;

use crate::watch::Watch;
use crate::user_ptr::UserBytesMut;

use super::read::{drain_ordered, Cursor, RecordSink};

/// What a log poll waits on: edge-triggered, since a reader's position is its own cursor and the kernel holds none.
pub static WATCH: Watch = Watch::new();

/// Tell every ring watching the log that records have moved.
// Posted only by `klogd` after a drain batch — `emit` runs under sync.rs/IRQ/scheduler locks and may not lock.
// Edge, not level: whether a caller has unread records is a property of its cursor, which the kernel does not hold.
pub fn post_readiness() {
    WATCH.post();
}

// Fixed `RECORD_BYTES` stride, never packed: the caller indexes by shift, so the kernel does no length arithmetic.
struct UserRecords<'a, 'b> {
    out: &'a mut UserBytesMut<'b>,
    written: usize,
    capacity: usize,
}

impl RecordSink for UserRecords<'_, '_> {
    fn put(&mut self, record: &LogRecord) -> bool {
        if self.written >= self.capacity {
            return false;
        }
        self.out.write_at(self.written * RECORD_BYTES, record.as_bytes());
        self.written += 1;
        true
    }
}

/// Copies records `cursor` has not seen into `out`, oldest first; never blocks.
pub fn read(
    cursor: &mut LogCursor,
    out: &mut UserBytesMut,
    capacity: usize,
) -> Result<usize, SyscallError> {
    // Started on first read, not at boot: an unread storm has already spent itself before a cursor exists to notice it.
    #[cfg(feature = "boot-actuators")]
    if crate::actuator::log_storm() {
        super::storm::start_once();
    }
    // Armed here too, once: one thread serves both injection windows; `log::nested` picks the target from whichever actuators are armed.
    #[cfg(feature = "boot-actuators")]
    if crate::actuator::log_nested_emit() || crate::actuator::log_nested_reserve() {
        super::nested::start_once();
    }

    let shards = super::shard_count();
    // Refused, not truncated: a capacity below one record per shard cannot hold what a single call may have to merge.
    if capacity == 0 || capacity < shards as usize {
        return Err(SyscallError::InvalidArgument);
    }

    let mut walk = Cursor::from_reader(cursor);
    let mut sink = UserRecords { out, written: 0, capacity };
    drain_ordered(&mut walk, &mut sink);
    let written = sink.written;

    walk.write_into(cursor);
    // Written unconditionally, so a caller starting from a zeroed cursor learns the shard count from the first reply.
    cursor.shards = shards;
    Ok(written)
}
