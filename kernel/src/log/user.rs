//! Kernel side of `SYS_LOG_READ` and its readiness source.
//!
//! No per-reader state in a read: a cursor is the caller's own sequence numbers and loss count, copied in, walked, and copied back; readers coexist uncoordinated. Requires [`Rights::LOG`] on a `SysCap`, not ambient. [`LOG_HOLDERS`] is the one table here, and it is the shutdown's, not a reader's.

use alloc::vec::Vec;

use toyos_abi::log::{LogCursor, LogRecord, RECORD_BYTES};
use toyos_abi::syscall::SyscallError;

use crate::inbox::InboxId;
use crate::sync::Lock;
use crate::user_ptr::UserBytesMut;

use super::read::{drain_ordered, Cursor, RecordSink};

static INBOX_WATCHERS: Lock<Vec<InboxId>> = Lock::new(Vec::new());

pub fn add_inbox_watcher(id: InboxId) {
    let mut w = INBOX_WATCHERS.lock();
    if !w.contains(&id) {
        w.push(id);
    }
}

pub fn remove_inbox_watcher(id: InboxId) {
    INBOX_WATCHERS.lock().retain(|&x| x != id);
}

pub fn inbox_watchers() -> Vec<InboxId> {
    INBOX_WATCHERS.lock().clone()
}

/// Tell every ring watching the log that records have moved.
// Posted only by `klogd` after a drain batch — `emit` runs under sync.rs/IRQ/scheduler locks and may not lock.
// Edge, not level: whether a caller has unread records is a property of its cursor, which the kernel does not hold.
pub fn post_readiness() {
    crate::inbox::Source::Log.wake();
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
    // `durable` travels caller-to-kernel; left as the caller wrote it so a reader that republishes the same cursor need not resend it.
    publish_durable(cursor.durable);
    Ok(written)
}

// Zero means nothing durable yet; `fetch_max` keeps it monotone.
static DURABLE_NS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

// Clamped to the newest committed record: an untrusted `durable` can only shorten its own wait, never claim a record that has not landed.
fn publish_durable(claimed: u64) {
    if claimed == 0 {
        return;
    }
    let clamped = claimed.min(super::read::newest_committed_at_ns());
    DURABLE_NS.fetch_max(clamped, core::sync::atomic::Ordering::Relaxed);
}

/// Holders one boot carves out of the first stage of its stop: a fixed set of
/// words, because the table is read with preemption off inside the block
/// layer.
const MAX_LOG_HOLDERS: usize = 8;

/// An empty slot: the pid `percpu::current_pid` cannot represent, because it
/// spells idle with it; zero is `/system/bin/init`.
const NO_PID: u32 = u32::MAX;

/// Every process that has read the log with [`Rights::LOG`] on this boot, recorded
/// where that capability is checked and nowhere a caller's own words reach;
/// the shutdown's first stage leaves exactly these running. A slot is never
/// given back: no pid is issued twice, so a dead holder's names nothing that runs.
///
/// [`Rights::LOG`]: toyos_abi::handle::Rights::LOG
static LOG_HOLDERS: [core::sync::atomic::AtomicU32; MAX_LOG_HOLDERS] =
    [const { core::sync::atomic::AtomicU32::new(NO_PID) }; MAX_LOG_HOLDERS];

/// Said once: the refusal below is reached on every read a full table sees.
static REFUSED_A_HOLDER: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Record a holder. A surplus one is refused by name rather than carried, and
/// stops in the first stage like any other process.
pub fn note_log_holder(pid: u32) {
    assert!(pid != NO_PID, "log: a holder with no pid");
    for slot in &LOG_HOLDERS {
        // A load first: the steady state is the one holder that reads on every
        // loop finding itself in slot 0, and that costs no RMW.
        let held = slot.load(core::sync::atomic::Ordering::Relaxed);
        if held == pid {
            return;
        }
        if held != NO_PID {
            continue;
        }
        match slot.compare_exchange(
            NO_PID,
            pid,
            core::sync::atomic::Ordering::Relaxed,
            core::sync::atomic::Ordering::Relaxed,
        ) {
            Ok(_) => return,
            // A sibling thread's first read took the slot for this same process.
            Err(now) if now == pid => return,
            Err(_) => {}
        }
    }
    if !REFUSED_A_HOLDER.swap(true, core::sync::atomic::Ordering::Relaxed) {
        crate::alert!(
            "log: pid {pid} is past the {MAX_LOG_HOLDERS} holders of the log capability this \
             machine carves out of a shutdown's first stage; it stops in that stage with \
             everything else"
        );
    }
}

/// Whether the shutdown's first stage leaves `pid` running.
pub fn holds_the_log(pid: u32) -> bool {
    assert!(pid != NO_PID, "log: asked whether no pid holds the log");
    LOG_HOLDERS
        .iter()
        .any(|slot| slot.load(core::sync::atomic::Ordering::Relaxed) == pid)
}

/// Newest record `/system/bin/logd` has `fsync`ed to the device, or 0 if none yet.
pub fn durable_ns() -> u64 {
    DURABLE_NS.load(core::sync::atomic::Ordering::Relaxed)
}
