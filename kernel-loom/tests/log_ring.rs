//! Loom: a program's log ring — the shared ring's publication and slot reuse,
//! and a lane's.
//!
//! **x86 gives every load acquire and every store release semantics**, so a
//! dropped `Release` on a publication, or a reader that tells writers a slot
//! is free before its copy is ordered, is invisible to every guest and host
//! test in the tree. This is the instrument that sees them. The bodies live
//! in loom cells, so a reader copying a body whose write is not ordered before
//! it is loom's own `Causality violation`, not an assertion of ours.
//!
//! The negative control is a cargo feature:
//!
//! ```text
//! cargo test --manifest-path kernel-loom/Cargo.toml --features log-ring-publish-relaxed \
//!   --test log_ring
//! ```
//!
//! makes both publishing stores `Relaxed`, and this file must red.

#![cfg(feature = "loom")]

use kernel_loom::log_ring::{push, push_lane, Pushed, Reader, Shared, Slots, Word};
use loom::cell::UnsafeCell;
use loom::sync::atomic::{AtomicU64, Ordering};
use loom::sync::Arc;

/// Loom's atomic as the ring's word.
struct W(AtomicU64);

impl Word for W {
    fn load(&self, order: Ordering) -> u64 {
        self.0.load(order)
    }
    fn store(&self, value: u64, order: Ordering) {
        self.0.store(value, order)
    }
    fn fetch_add(&self, value: u64, order: Ordering) -> u64 {
        self.0.fetch_add(value, order)
    }
    fn compare_exchange_weak(&self, current: u64, new: u64, success: Ordering, failure: Ordering) -> Result<u64, u64> {
        self.0.compare_exchange_weak(current, new, success, failure)
    }
}

/// A ring whose bodies are two words, so a body mixed from two writes is
/// visible as well as a body read unordered.
struct Ring {
    head: W,
    tail: W,
    refused: W,
    published: Vec<W>,
    bodies: Vec<UnsafeCell<(u64, u64)>>,
}

impl Ring {
    fn new(slots: usize) -> Self {
        Self {
            head: W(AtomicU64::new(0)),
            tail: W(AtomicU64::new(0)),
            refused: W(AtomicU64::new(0)),
            published: (0..slots).map(|_| W(AtomicU64::new(0))).collect(),
            bodies: (0..slots).map(|_| UnsafeCell::new((0, 0))).collect(),
        }
    }
}

impl Slots for Ring {
    type Word = W;
    type Body = (u64, u64);
    fn slots(&self) -> u64 {
        self.bodies.len() as u64
    }
    fn head(&self) -> &W {
        &self.head
    }
    fn tail(&self) -> &W {
        &self.tail
    }
    fn refused(&self) -> &W {
        &self.refused
    }
    fn write(&self, slot: usize, body: &(u64, u64)) {
        // SAFETY: the protocol's exclusivity, which loom checks.
        self.bodies[slot].with_mut(|at| unsafe { *at = *body });
    }
    fn read(&self, slot: usize) -> (u64, u64) {
        // SAFETY: as for `write`.
        self.bodies[slot].with(|at| unsafe { *at })
    }
}

impl Shared for Ring {
    fn published(&self, slot: usize) -> &W {
        &self.published[slot]
    }
}

/// A body whose two words both come from `value`, so a torn one fails an
/// equality.
fn body(value: u64) -> (u64, u64) {
    (value, value * 7 + 1)
}

fn assert_whole(got: (u64, u64)) {
    assert_eq!(got.1, got.0 * 7 + 1, "torn: {got:?} is two records' words");
}

/// **Publication, with two writers.** Two writers race for one-slot-each
/// positions and a reader races both: whatever the reader finds published it
/// finds whole, and after both writers are done it has each exactly once.
#[test]
fn a_published_record_is_whole_and_read_once() {
    loom::model(|| {
        let ring = Arc::new(Ring::new(2));
        let writers: Vec<_> = [1u64, 2]
            .into_iter()
            .map(|value| {
                let ring = ring.clone();
                loom::thread::spawn(move || assert_eq!(push(&*ring, &body(value)), Pushed::Written))
            })
            .collect();
        let mut reader = Reader::new();
        let mut got = Vec::new();
        if let Some(read) = reader.next(&*ring) {
            assert_whole(read);
            got.push(read.0);
        }
        for writer in writers {
            writer.join().unwrap();
        }
        while let Some(read) = reader.next(&*ring) {
            assert_whole(read);
            got.push(read.0);
        }
        got.sort();
        assert_eq!(got, [1, 2]);
    });
}

/// **Reuse.** One slot, a writer putting two records through it and a reader
/// taking them: the second write may begin only after the reader's copy of
/// the first, which is the edge the reader's `tail` store and the writer's
/// load of it carry. A refused write is counted, never lost silently.
#[test]
fn a_slot_is_reused_only_after_its_record_was_read() {
    loom::model(|| {
        let ring = Arc::new(Ring::new(1));
        let writer = {
            let ring = ring.clone();
            loom::thread::spawn(move || {
                let first = push(&*ring, &body(1));
                let second = push(&*ring, &body(2));
                (first, second)
            })
        };
        let mut reader = Reader::new();
        let mut got = Vec::new();
        for _ in 0..2 {
            if let Some(read) = reader.next(&*ring) {
                assert_whole(read);
                got.push(read.0);
            }
        }
        let (first, second) = writer.join().unwrap();
        assert_eq!(first, Pushed::Written, "an empty ring refused");
        while let Some(read) = reader.next(&*ring) {
            assert_whole(read);
            got.push(read.0);
        }
        let refused = reader.refused(&*ring);
        match second {
            Pushed::Written => assert_eq!((got.as_slice(), refused), (&[1, 2][..], 0)),
            Pushed::Refused => assert_eq!((got.as_slice(), refused), (&[1][..], 1)),
        }
    });
}

/// **A lane**: one writer, two records through a one-slot lane, a racing
/// reader. The same two promises, on the lane's `head` as its publication.
#[test]
fn a_lane_publishes_whole_and_reuses_only_after_a_read() {
    loom::model(|| {
        let lane = Arc::new(Ring::new(1));
        // The reader on a thread of its own and the writer on this one: loom
        // explores which store a load observes only among stores already
        // made, so the writer has to be able to run first.
        let reading = {
            let lane = lane.clone();
            loom::thread::spawn(move || {
                let mut reader = Reader::new();
                let mut got = Vec::new();
                for _ in 0..2 {
                    if let Some(read) = reader.next_lane(&*lane) {
                        assert_whole(read);
                        got.push(read.0);
                    }
                }
                (reader, got)
            })
        };
        let first = push_lane(&*lane, &body(1));
        let second = push_lane(&*lane, &body(2));
        let (mut reader, mut got) = reading.join().unwrap();
        assert_eq!(first, Pushed::Written);
        while let Some(read) = reader.next_lane(&*lane) {
            assert_whole(read);
            got.push(read.0);
        }
        let refused = reader.refused(&*lane);
        match second {
            Pushed::Written => assert_eq!((got.as_slice(), refused), (&[1, 2][..], 0)),
            Pushed::Refused => assert_eq!((got.as_slice(), refused), (&[1][..], 1)),
        }
    });
}
