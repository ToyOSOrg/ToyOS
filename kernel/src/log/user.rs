//! Kernel side of `SYS_LOG_READ` and its readiness source, and the read `SYS_TRACE_READ` shares.
//!
//! No per-reader state in a read: a cursor is the caller's own sequence numbers, copied in, refused if ahead of a ring, walked, and copied back with this read's loss; readers coexist uncoordinated. Requires [`Rights::LOG`] on a `SysCap`, not ambient.
//!
//! [`Rights::LOG`]: toyos_abi::handle::Rights::LOG

use toyos_abi::log::{LogCursor, LogRecord};
use toyos_abi::syscall::SyscallError;

use crate::watch::Watch;
use crate::user_ptr::UserBytesMut;

use super::read::{drain_ordered, Cursor, RecordSink, Rings, Stream};
use super::shard::Ring;

/// What a log poll waits on: edge-triggered, since a reader's position is its own cursor and the kernel holds none.
pub static WATCH: Watch = Watch::new();

/// Tell every ring watching the log that records have moved.
// Posted only by `klogd` after a drain batch — `emit` runs under sync.rs/IRQ/scheduler locks and may not lock.
// Edge, not level: whether a caller has unread records is a property of its cursor, which the kernel does not hold.
pub fn post_readiness() {
    WATCH.post();
}

// Fixed stride, never packed: the caller indexes by shift, so the kernel does no length arithmetic.
struct UserRecords<'a, 'b, R> {
    out: &'a mut UserBytesMut<'b>,
    written: usize,
    capacity: usize,
    bytes: fn(&R) -> &[u8],
}

impl<R> RecordSink<R> for UserRecords<'_, '_, R> {
    fn put(&mut self, record: &R) -> bool {
        if self.written >= self.capacity {
            return false;
        }
        let bytes = (self.bytes)(record);
        self.out.write_at(self.written * bytes.len(), bytes);
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
    read_rings(&super::shards(), cursor, out, capacity, LogRecord::as_bytes)
}

/// Copies records `cursor` has not seen in `rings` into `out`, oldest first,
/// each as `bytes` writes it; never blocks.
pub fn read_rings<const W: usize, const N: usize>(
    rings: &Rings<W, N>,
    cursor: &mut LogCursor,
    out: &mut UserBytesMut,
    capacity: usize,
    bytes: fn(&<Ring<W, N> as Stream>::Record) -> &[u8],
) -> Result<usize, SyscallError>
where
    Ring<W, N>: Stream,
{
    let shards = rings.iter().flatten().count() as u32;
    // Refused, not truncated: a capacity below one record per ring cannot hold what a single call may have to merge.
    if capacity == 0 || capacity < shards as usize {
        return Err(SyscallError::InvalidArgument);
    }

    let Some(mut walk) = Cursor::from_reader(cursor, rings) else {
        return Err(SyscallError::InvalidArgument);
    };
    let mut sink = UserRecords { out, written: 0, capacity, bytes };
    drain_ordered(rings, &mut walk, &mut sink);
    let written = sink.written;

    walk.write_into(cursor);
    // Written unconditionally, so a caller starting from a zeroed cursor learns the ring count from the first reply.
    cursor.shards = shards;
    Ok(written)
}
