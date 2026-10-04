//! Host-fast regression for what `SYS_LOG_READ` takes from a caller's cursor.
//!
//! `kernel/src/log/read.rs` is the walk the syscall copies a caller's cursor
//! into, compiled here over real shards. The cursor crossed the syscall
//! boundary, so its loss count is never read and a position no shard has
//! issued is refused.
//!
//! `--no-default-features`, for [`log_zeroed_init`]'s reason: the shards are the
//! real ones, built the way the kernel builds an AP's.
//!
//! [`log_zeroed_init`]: ../log_zeroed_init.rs

#![cfg(not(feature = "loom"))]

use std::alloc::{alloc_zeroed, Layout};

use kernel_loom::arch::IrqGuard;
use kernel_loom::log_read::{drain_ordered, Cursor, RecordSink};
use kernel_loom::log_shard::{Shard, FIRST_SEQ, SHARD_RECORDS};
use toyos_abi::log::{LogCursor, LogRecord, MAX_LOG_SHARDS};

/// A shard for the life of the test binary, which a walk names as `'static`: zeroed, as the kernel allocates an AP's.
fn shard() -> &'static Shard {
    // SAFETY: a `Shard` is not zero-sized; the block is never freed.
    let ptr = unsafe { alloc_zeroed(Layout::new::<Shard>()) }.cast::<Shard>();
    assert!(!ptr.is_null());
    // SAFETY: fresh, zeroed, aligned and never freed; zeroed is an empty shard.
    unsafe { &*ptr }
}

/// Commits `count` records to `shard`, each stamped with its own number.
fn emit(shard: &Shard, count: u64) {
    let guard = IrqGuard::close();
    for _ in 0..count {
        // SAFETY: this thread is the shard's only producer, and each number is
        // committed once under the guard it was reserved with.
        unsafe {
            let seq = shard.reserve(&guard);
            let mut record = LogRecord::EMPTY;
            record.seq = seq;
            record.at_ns = seq;
            shard.commit(seq, &record, &guard);
        }
    }
}

/// A sink that takes nothing, so a walk only positions its cursor.
struct Full;

impl RecordSink for Full {
    fn put(&mut self, _: &LogRecord) -> bool {
        false
    }
}

/// A cursor at every shard's start claiming `u64::MAX` records already lost,
/// behind a shard that has overwritten some: the answer is the overwritten
/// count, not the claim plus it.
#[test]
fn a_cursors_loss_claim_is_not_read() {
    const OVERWRITTEN: u64 = 3;
    let lapped = shard();
    emit(lapped, SHARD_RECORDS as u64 + OVERWRITTEN);
    let mut shards = [None; MAX_LOG_SHARDS];
    shards[0] = Some(lapped);

    let mut cursor = LogCursor { lost: u64::MAX, ..LogCursor::new() };
    let mut walk = Cursor::from_reader(&cursor, &shards).expect("a cursor at every shard's start is taken");
    drain_ordered(&shards, &mut walk, &mut Full);
    walk.write_into(&mut cursor);
    assert_eq!(cursor.lost, OVERWRITTEN);
}

/// A position past the number a shard issues next is refused, and one at it
/// taken: a published shard's head, and an unpublished shard's first number.
#[test]
fn a_cursor_ahead_of_a_shard_is_refused() {
    const RECORDS: u64 = 5;
    let published = shard();
    emit(published, RECORDS);
    let mut shards = [None; MAX_LOG_SHARDS];
    shards[0] = Some(published);

    let taken = |shard: usize, next: u64| {
        let mut cursor = LogCursor::new();
        cursor.next[shard] = next;
        Cursor::from_reader(&cursor, &shards).is_some()
    };
    let head = FIRST_SEQ + RECORDS;
    assert!(taken(0, head), "a cursor caught up with a published shard");
    assert!(!taken(0, head + 1), "a cursor past a published shard's head");
    assert!(taken(1, FIRST_SEQ), "a cursor at an unpublished shard's first number");
    assert!(!taken(1, FIRST_SEQ + 1), "a cursor past an unpublished shard's first number");
}
