//! Kernel side of `SYS_LOG_READ` and its readiness source, and the read `SYS_TRACE_READ` shares.
//!
//! No per-reader state in a read: a cursor is the caller's own sequence numbers, one per CPU the firmware names, copied in, refused if ahead of a ring, walked, and copied back with this read's loss; readers coexist uncoordinated. Requires [`Rights::LOG`] on a `SysCap`, not ambient.
//!
//! [`Rights::LOG`]: toyos_abi::handle::Rights::LOG

use toyos_abi::syscall::SyscallError;
use toyos_abi::UserSafe;

use crate::watch::Watch;
use crate::user_ptr::UserBytesMut;

use super::read::{drain_ordered, Cursor, RecordSink, Rings, Shards, Stream};
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
struct UserRecords<'a, 'b> {
    out: &'a mut UserBytesMut<'b>,
    written: usize,
    capacity: usize,
}

impl<R: UserSafe> RecordSink<R> for UserRecords<'_, '_> {
    fn put(&mut self, record: &R) -> bool {
        if self.written >= self.capacity {
            return false;
        }
        let bytes = toyos_abi::usersafe::bytes(record);
        self.out.write_at(self.written * bytes.len(), bytes);
        self.written += 1;
        true
    }
}

/// Copies records the positions `next` have not seen into `out`, oldest
/// first, and answers how many and the walk; never blocks.
pub fn read(next: &[u64], out: &mut UserBytesMut, capacity: usize) -> Result<(usize, Cursor), SyscallError> {
    read_rings(&Shards, next, out, capacity)
}

/// The same over any `rings`, one position in `next` per ring.
pub fn read_rings<const W: usize, const N: usize>(
    rings: &(impl Rings<W, N> + ?Sized),
    next: &[u64],
    out: &mut UserBytesMut,
    capacity: usize,
) -> Result<(usize, Cursor), SyscallError>
where
    Ring<W, N>: Stream<Record: UserSafe>,
{
    if capacity == 0 || next.len() != rings.count() {
        return Err(SyscallError::InvalidArgument);
    }
    let mut walk = Cursor::from_reader(next, rings).ok_or(SyscallError::InvalidArgument)?;
    let mut sink = UserRecords { out, written: 0, capacity };
    drain_ordered(rings, &mut walk, &mut sink);
    Ok((sink.written, walk))
}
