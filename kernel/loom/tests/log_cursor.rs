//! Host-fast regression for what `SYS_LOG_READ` takes from a caller's cursor.
//!
//! `kernel/src/log/read.rs` is the walk the syscall copies a caller's cursor
//! into, compiled here over real shards. The cursor crossed the syscall
//! boundary, so its loss count is never read and a position no shard has
//! issued is refused. A cursor holds a position per CPU the firmware names, so
//! a shard is read whether or not its CPU came online.
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
use toyos_abi::log::LogRecord;

/// A shard for the life of the test binary, which a walk names as `'static`: zeroed, as the kernel allocates an AP's.
fn shard() -> &'static Shard {
    // SAFETY: a `Shard` is not zero-sized; the block is never freed.
    let ptr = unsafe { alloc_zeroed(Layout::new::<Shard>()) }.cast::<Shard>();
    assert!(!ptr.is_null());
    // SAFETY: fresh, zeroed, aligned and never freed; zeroed is an empty shard.
    unsafe { &*ptr }
}

/// Commits a record to `shard` for each stamp in `stamps`, which rise.
fn emit(shard: &Shard, stamps: impl IntoIterator<Item = u64>) {
    let guard = IrqGuard::close();
    for at_ns in stamps {
        // SAFETY: this thread is the shard's only producer, and each number is
        // committed once under the guard it was reserved with.
        unsafe {
            let seq = shard.reserve(&guard);
            let mut record = LogRecord::EMPTY;
            record.seq = seq;
            record.at_ns = at_ns;
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

/// A sink that takes at most `room` records, as a reader's buffer does.
struct Room {
    room: usize,
    took: Vec<u64>,
}

impl RecordSink for Room {
    fn put(&mut self, record: &LogRecord) -> bool {
        if self.took.len() == self.room {
            return false;
        }
        self.took.push(record.at_ns);
        true
    }
}

/// A cursor at every shard's start, behind a shard that has overwritten some:
/// the loss is the overwritten count, and the caller's own claim is not read.
#[test]
fn a_cursors_loss_claim_is_not_read() {
    const OVERWRITTEN: u64 = 3;
    let lapped = shard();
    emit(lapped, 0..SHARD_RECORDS as u64 + OVERWRITTEN);
    let shards = [Some(lapped), None];
    let mut walk = Cursor::from_reader(&[0, 0], &shards[..]).expect("a cursor at every shard's start is taken");
    drain_ordered(&shards[..], &mut walk, &mut Full);
    assert_eq!(walk.lost(), OVERWRITTEN);
}

/// A position past the number a shard issues next is refused, and one at it
/// taken: a published shard's head, and an unpublished shard's first number.
#[test]
fn a_cursor_ahead_of_a_shard_is_refused() {
    const RECORDS: u64 = 5;
    let published = shard();
    emit(published, 0..RECORDS);
    let shards = [Some(published), None];
    let taken = |shard: usize, next: u64| {
        let mut positions = [0; 2];
        positions[shard] = next;
        Cursor::from_reader(&positions, &shards[..]).is_some()
    };
    let head = FIRST_SEQ + RECORDS;
    assert!(taken(0, head), "a cursor caught up with a published shard");
    assert!(!taken(0, head + 1), "a cursor past a published shard's head");
    assert!(taken(1, FIRST_SEQ), "a cursor at an unpublished shard's first number");
    assert!(!taken(1, FIRST_SEQ + 1), "a cursor past an unpublished shard's first number");
}

/// **A CPU that started, logged and never came online is read.** A machine of
/// four CPUs whose cpu1 never started and whose cpu2 published its shard and
/// logged before it failed to echo: the roster counts one CPU, and a cursor of
/// four positions reads cpu2's records merged among cpu0's, past cpu1's hole.
#[test]
fn an_offline_cpus_shard_is_read_beside_the_online_ones() {
    let (boot, failed) = (shard(), shard());
    emit(boot, [10, 30, 50]);
    emit(failed, [20, 40]);
    let shards = [Some(boot), None, Some(failed), None];
    let mut walk = Cursor::from_reader(&[0; 4], &shards[..]).expect("a fresh cursor is taken");
    let mut out = Room { room: usize::MAX, took: Vec::new() };
    assert_eq!(drain_ordered(&shards[..], &mut walk, &mut out), 5);
    assert_eq!(out.took, [10, 20, 30, 40, 50]);
    assert_eq!(walk.next(), [FIRST_SEQ + 3, 0, FIRST_SEQ + 2, 0], "the holes were never opened");
}

/// **A buffer of one record reads every shard, oldest first, a read at a
/// time**: a read returns the oldest record not yet taken whatever the number
/// of shards, so no buffer has to hold one per CPU.
#[test]
fn reads_of_one_record_take_the_merge_in_order() {
    let shards: Vec<Option<&'static Shard>> = (0..3u64)
        .map(|cpu| {
            let shard = shard();
            emit(shard, (0..4).map(move |i| i * 3 + cpu));
            Some(shard)
        })
        .collect();
    let mut positions = vec![0; shards.len()];
    let mut took = Vec::new();
    loop {
        let mut walk = Cursor::from_reader(&positions, &shards[..]).expect("its own position is taken");
        let mut out = Room { room: 1, took: Vec::new() };
        if drain_ordered(&shards[..], &mut walk, &mut out) == 0 {
            break;
        }
        took.extend(out.took);
        positions = walk.next().to_vec();
    }
    assert_eq!(took, (0..12).collect::<Vec<u64>>());
}
