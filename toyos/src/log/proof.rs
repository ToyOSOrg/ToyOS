//! What the ring promises, run on the host against real threads: every record
//! is read once, in its writer's order, or counted refused; a full ring or
//! lane refuses at once; and a write allocates nothing.
//!
//! The memory-ordering half — that a reader which sees a record sees all of
//! it — is `kernel-loom`'s `log_ring` model: x86's ordering hides it from
//! any test run here.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, UnsafeCell};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::vec::Vec;

use super::region::{Body, Ring, LANE_SLOTS, RING_BYTES, SHARED_SLOTS};
use super::ring::{push, push_lane, Pushed, Reader, Shared, Slots};
use super::stdio::compose;
use toyos_abi::log::Severity;

/// A ring on the heap, of any size, whose bodies are `(writer, sequence)`.
struct Heap {
    head: AtomicU64,
    tail: AtomicU64,
    refused: AtomicU64,
    slots: Vec<(AtomicU64, UnsafeCell<(u32, u64)>)>,
}

// SAFETY: the bodies are reached only as the protocol allows, which is what
// is under test; the words are atomics.
unsafe impl Sync for Heap {}

impl Heap {
    fn new(slots: usize) -> Self {
        Self {
            head: AtomicU64::new(0),
            tail: AtomicU64::new(0),
            refused: AtomicU64::new(0),
            slots: (0..slots).map(|_| (AtomicU64::new(0), UnsafeCell::new((0, 0)))).collect(),
        }
    }
}

impl Slots for Heap {
    type Word = AtomicU64;
    type Body = (u32, u64);
    fn slots(&self) -> u64 {
        self.slots.len() as u64
    }
    fn head(&self) -> &AtomicU64 {
        &self.head
    }
    fn tail(&self) -> &AtomicU64 {
        &self.tail
    }
    fn refused(&self) -> &AtomicU64 {
        &self.refused
    }
    fn write(&self, slot: usize, body: &(u32, u64)) {
        // SAFETY: the protocol's claim, which the test checks.
        unsafe { *self.slots[slot].1.get() = *body };
    }
    fn read(&self, slot: usize) -> (u32, u64) {
        // SAFETY: as for `write`.
        unsafe { *self.slots[slot].1.get() }
    }
}

impl Shared for Heap {
    fn published(&self, slot: usize) -> &AtomicU64 {
        &self.slots[slot].0
    }
}

/// **A lost or reordered record is a red here.** Four writers race a reader
/// through an eight-slot ring. Each writer's records come out in the order it
/// wrote them, none twice, and what the reader got plus what the ring counted
/// refused is exactly what the writers pushed — the writers' own answers are
/// the independent count.
#[test]
fn every_record_is_read_once_in_its_writers_order_or_counted_refused() {
    const WRITERS: u32 = 4;
    const EACH: u64 = 20_000;
    let ring = Arc::new(Heap::new(8));
    let done = Arc::new(AtomicBool::new(false));
    let refused = Arc::new(AtomicU64::new(0));
    let writers: Vec<_> = (0..WRITERS)
        .map(|w| {
            let (ring, refused) = (Arc::clone(&ring), Arc::clone(&refused));
            std::thread::spawn(move || {
                for seq in 0..EACH {
                    if push(&*ring, &(w, seq)) == Pushed::Refused {
                        refused.fetch_add(1, Ordering::Relaxed);
                    }
                }
            })
        })
        .collect();
    let reader = {
        let (ring, done) = (Arc::clone(&ring), Arc::clone(&done));
        std::thread::spawn(move || {
            let mut reader = Reader::new();
            let mut got: Vec<(u32, u64)> = Vec::new();
            let mut counted = 0u64;
            loop {
                let finished = done.load(Ordering::Acquire);
                while let Some(body) = reader.next(&*ring) {
                    got.push(body);
                }
                counted += reader.refused(&*ring);
                if finished {
                    break;
                }
                std::hint::spin_loop();
            }
            let abandoned = reader.sweep(&*ring, |body| got.push(body));
            (got, counted, abandoned)
        })
    };
    for writer in writers {
        writer.join().unwrap();
    }
    done.store(true, Ordering::Release);
    let (got, counted, abandoned) = reader.join().unwrap();

    assert_eq!(abandoned, 0, "every writer published every position it took");
    assert_eq!(counted, refused.load(Ordering::Relaxed), "the ring's count is the writers'");
    assert_eq!(got.len() as u64 + counted, WRITERS as u64 * EACH);
    let mut last = [None::<u64>; WRITERS as usize];
    for (w, seq) in got {
        let before = last[w as usize].replace(seq);
        assert!(before.is_none_or(|b| b < seq), "writer {w}: {seq} after {before:?}");
    }
    assert!(counted > 0 && counted < WRITERS as u64 * EACH, "the ring both filled and drained: {counted}");
}

/// The same promise for a lane, whose one writer races its reader.
#[test]
fn a_lane_gives_its_reader_every_record_in_order_or_counts_it_refused() {
    const EACH: u64 = 50_000;
    let lane = Arc::new(Heap::new(4));
    let writer = {
        let lane = Arc::clone(&lane);
        std::thread::spawn(move || (0..EACH).filter(|&seq| push_lane(&*lane, &(0, seq)) == Pushed::Refused).count())
    };
    let mut reader = Reader::new();
    let mut got = Vec::new();
    let mut counted = 0;
    loop {
        let finished = writer.is_finished();
        while let Some((_, seq)) = reader.next_lane(&*lane) {
            got.push(seq);
        }
        counted += reader.refused(&*lane);
        if finished {
            break;
        }
    }
    let refused = writer.join().unwrap() as u64;
    assert_eq!(counted, refused);
    assert_eq!(got.len() as u64 + counted, EACH);
    assert!(got.windows(2).all(|w| w[0] < w[1]), "a lane reordered its records");
}

/// Runs `write` on a thread of its own and fails if it has not come back
/// within a bound no write should get near: a write that waits is a red.
fn never_waits<T: Send + 'static>(write: impl FnOnce() -> T + Send + 'static) -> T {
    let writing = std::thread::spawn(write);
    let started = Instant::now();
    while !writing.is_finished() {
        assert!(started.elapsed() < Duration::from_secs(10), "a write into a full ring is waiting");
        std::thread::yield_now();
    }
    writing.join().unwrap()
}

/// **A write never waits for the reader.** With nobody reading, a ring and a
/// lane each take exactly their slots and refuse every write after, at once.
#[test]
fn a_full_ring_or_lane_refuses_at_once_and_never_waits() {
    let ring = Arc::new(Heap::new(16));
    let answers = {
        let ring = Arc::clone(&ring);
        never_waits(move || (0..48u64).map(|seq| push(&*ring, &(0, seq))).collect::<Vec<_>>())
    };
    assert!(answers[..16].iter().all(|a| *a == Pushed::Written));
    assert!(answers[16..].iter().all(|a| *a == Pushed::Refused));
    let mut reader = Reader::new();
    let kept: Vec<u64> = std::iter::from_fn(|| reader.next(&*ring)).map(|(_, seq)| seq).collect();
    assert_eq!(kept, (0..16).collect::<Vec<_>>(), "the ring kept the oldest, in order");
    assert_eq!(reader.refused(&*ring), 32);

    let lane = Arc::new(Heap::new(16));
    let answers = {
        let lane = Arc::clone(&lane);
        never_waits(move || (0..48u64).map(|seq| push_lane(&*lane, &(0, seq))).collect::<Vec<_>>())
    };
    assert!(answers[..16].iter().all(|a| *a == Pushed::Written));
    assert!(answers[16..].iter().all(|a| *a == Pushed::Refused));
    let mut reader = Reader::new();
    assert_eq!(std::iter::from_fn(|| reader.next_lane(&*lane)).count(), 16);
    assert_eq!(reader.refused(&*lane), 32);
}

/// A writer's position that never lands holds the reader there, and the sweep
/// at its writers' end counts it abandoned and reads on past it.
#[test]
fn a_position_that_never_lands_is_counted_by_the_sweep() {
    let ring = Heap::new(8);
    assert_eq!(push(&ring, &(0, 0)), Pushed::Written);
    ring.head.fetch_add(1, Ordering::Relaxed); // a writer that took a position and died
    assert_eq!(push(&ring, &(0, 2)), Pushed::Written);
    let mut reader = Reader::new();
    assert_eq!(reader.next(&ring), Some((0, 0)));
    assert_eq!(reader.next(&ring), None, "the reader stops at the hole");
    let mut after = Vec::new();
    assert_eq!(reader.sweep(&ring, |body| after.push(body)), 1);
    assert_eq!(after, std::vec![(0, 2)]);
}

/// A writer that scribbles `head` far ahead is swept one lap at most, and one
/// that scribbles `refused` backwards reads as nothing refused.
#[test]
fn scribbled_words_bound_the_reader_and_never_panic_it() {
    let ring = Heap::new(8);
    ring.head.store(u64::MAX - 1, Ordering::Relaxed);
    let mut reader = Reader::new();
    assert_eq!(reader.sweep(&ring, |_| {}), 8);
    ring.refused.store(5, Ordering::Relaxed);
    assert_eq!(reader.refused(&ring), 5);
    ring.refused.store(2, Ordering::Relaxed);
    assert_eq!(reader.refused(&ring), 0);
    // A `tail` scribbled past `head` is a full ring, not room.
    let full = Heap::new(8);
    full.tail.store(100, Ordering::Relaxed);
    assert_eq!(push(&full, &(0, 0)), Pushed::Refused);
}

thread_local! {
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
}

/// Counts this thread's allocations, so a test can say its region made none.
struct Counting;

// SAFETY: forwards every call to `System` unchanged, and counts.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
        // SAFETY: the caller's contract, passed on.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller's contract, passed on.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

fn allocations() -> u64 {
    ALLOCATIONS.with(Cell::get)
}

/// A real ring's region, on the heap and 8-byte aligned.
fn region() -> (Vec<u64>, Ring) {
    let mut words = std::vec![0u64; RING_BYTES / 8];
    let base = core::ptr::NonNull::new(words.as_mut_ptr() as *mut u8).unwrap();
    // SAFETY: `words` is `RING_BYTES` long, 8-aligned — the protocol's words
    // are 8 bytes — and lives as long as the returned view is used.
    let ring = unsafe { Ring::at(base) };
    ring.lay_out();
    (words, ring)
}

/// **What soundd's mix thread does to log, allocating nothing.** A line of the
/// stats line's shape composed on the stack and pushed into a claimed lane of
/// a real ring layout — its refusal path included — and the same into the
/// shared ring, is zero allocations on the writing thread.
#[test]
fn a_real_time_write_allocates_nothing() {
    let (_words, ring) = region();
    let lane = ring.claim_lane(7, 3).expect("a fresh ring has free lanes");
    let before = allocations();
    for window in 0..(LANE_SLOTS + 4) {
        compose(
            Severity::Info,
            format_args!("soundd: wakes={} completions={} underruns={} late_wakes={}", window, 2, 0, 1),
            &mut |body: &mut Body| {
                body.at_ns = window;
                let _ = lane.push(body);
            },
        );
    }
    for window in 0..(SHARED_SLOTS + 4) {
        compose(Severity::Info, format_args!("x={window}"), &mut |body: &mut Body| {
            let _ = ring.push(body);
        });
    }
    assert_eq!(allocations() - before, 0, "a write allocated");
}

/// The real layout, end to end: what one view pushes the reader reads back
/// whole, from the shared ring and from a lane.
#[test]
fn the_region_carries_a_record_whole() {
    let (_words, ring) = region();
    assert!(ring.is_laid_out());
    let stamp = |body: &mut Body| {
        body.at_ns = 42;
        body.pid = 3;
        body.tid = 9;
    };
    compose(Severity::Error, format_args!("hello {}", 7), &mut |body: &mut Body| {
        stamp(body);
        assert_eq!(ring.push(body), Pushed::Written);
    });
    let lane = ring.claim_lane(3, 9).unwrap();
    compose(Severity::Warn, format_args!("from a lane"), &mut |body: &mut Body| {
        stamp(body);
        assert_eq!(lane.push(body), Pushed::Written);
    });
    let mut reader = Reader::new();
    let body = reader.next(&ring).expect("a record was pushed");
    assert_eq!((body.text(), body.at_ns, body.pid, body.tid), (&b"hello 7"[..], 42, 3, 9));
    assert_eq!(body.severity(), Some(Severity::Error));
    assert!(!body.unended());
    assert!(reader.next(&ring).is_none());
    let mut from_lane = Reader::new();
    let body = from_lane.next_lane(&ring.lane(0)).expect("the lane holds one");
    assert_eq!((body.text(), body.severity()), (&b"from a lane"[..], Some(Severity::Warn)));
}

/// A lane is one thread's: four claims succeed and a fifth is refused.
#[test]
fn a_ring_has_four_lanes_to_claim() {
    let (_words, ring) = region();
    for tid in 0..4 {
        assert!(ring.claim_lane(1, tid).is_some());
    }
    assert!(ring.claim_lane(1, 4).is_none());
}
