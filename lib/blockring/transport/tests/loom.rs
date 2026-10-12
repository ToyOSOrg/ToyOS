//! The transport's two ends on two CPUs, under loom: what no host test that
//! runs both ends on one thread can reach.
//!
//! - **Publication**: a consumer that sees a tail sees every word of the
//!   entries below it. `publish-relaxed` takes the edge away.
//! - **A hostile peer** leaves the honest end with entries, nothing, or a
//!   named violation, and never more entries than the ring holds. `no-clamp`
//!   believes the peer.
//!
//!   cargo test -p toyos-transport --features <control> --test loom

use core::sync::atomic::Ordering;
use std::rc::Rc;

use loom::cell::Cell;
use loom::sync::atomic::AtomicU32;
use loom::sync::{Arc, Notify};
use toyos_transport::{stream, Consumer, Place, Producer, Untrusted, Violation, Word};

/// A loom atomic as a region word: the trait is this crate's and the type is
/// loom's, so the two meet through a wrapper.
struct Shared(AtomicU32);

impl Word for Shared {
    fn load(&self, order: Ordering) -> u32 {
        self.0.load(order)
    }
    fn store(&self, value: u32, order: Ordering) {
        self.0.store(value, order)
    }
    fn swap(&self, value: u32, order: Ordering) -> u32 {
        self.0.swap(value, order)
    }
}

const E: usize = 2;
const D: u32 = 2;
const WORDS: usize = 2 + E * D as usize;
const PLACE: Place<E, D, WORDS> = Place::new::<0, 1, 2>();

fn page() -> Arc<[Shared; WORDS]> {
    Arc::new(core::array::from_fn(|_| Shared(AtomicU32::new(0))))
}

fn ends(page: &[Shared; WORDS]) -> (Producer<E, D, WORDS>, Consumer<E, D, WORDS>) {
    (Producer::new(page, PLACE), Consumer::new(page, PLACE))
}

fn entry(n: u32) -> [u32; E] {
    [100 + n, 7 * n + 3]
}

fn plain(words: [Untrusted<u32>; E]) -> [u32; E] {
    words.map(|w| w.at_most(u32::MAX.into()).unwrap() as u32)
}

/// Two entries published one at a time, read by the other end as they
/// arrive: each is whole, in order, and exactly what was written.
#[test]
fn a_published_entry_is_read_whole() {
    loom::model(|| {
        let page = page();
        let (mut tx, mut rx) = ends(&page);
        let consumer_page = Arc::clone(&page);
        let consumer = loom::thread::spawn(move || {
            let mut read = Vec::new();
            while read.len() < 2 {
                match rx.pop(&consumer_page).expect("the producer keeps the protocol") {
                    Some(words) => read.push(plain(words)),
                    None => loom::thread::yield_now(),
                }
            }
            rx.release(&consumer_page);
            read
        });
        for n in 0..2 {
            assert!(tx.push(&page, entry(n)).unwrap());
            tx.publish(&page);
        }
        let read = consumer.join().expect("the consumer thread");
        assert_eq!(read, [entry(0), entry(1)], "an entry was read before its words");
    });
}

/// A producer that stores a tail past the ring, one behind what was released,
/// and garbage into the entries and into the consumer's own head: every pop is
/// an entry, nothing, or [`Violation::TailPastDepth`], and no more than the
/// ring's depth of entries is taken without a release.
#[test]
fn a_hostile_producer_yields_entries_or_a_violation() {
    loom::model(|| {
        let page = page();
        let (_, mut rx) = ends(&page);
        let hostile_page = Arc::clone(&page);
        let hostile = loom::thread::spawn(move || {
            for (at, value) in [(1, D + 1), (3, 9), (0, 5), (1, u32::MAX), (2, 7)] {
                hostile_page[at].store(value, Ordering::Release);
            }
        });
        let mut taken = 0;
        for _ in 0..D + 2 {
            match rx.pop(&page) {
                Ok(Some(_)) => taken += 1,
                Ok(None) => {}
                Err(violation) => {
                    assert_eq!(violation, Violation::TailPastDepth);
                    break;
                }
            }
        }
        hostile.join().unwrap();
        assert!(taken <= D, "took {taken} entries from a ring of {D} without releasing one");
    });
}

/// A consumer that stores a head past what was published, one wrapped far
/// behind, and garbage into the producer's own tail and the entries: every
/// push is room, a full ring, or [`Violation::HeadPastTail`], and no more
/// than the ring's depth is ever pushed ahead of a head that was never moved.
#[test]
fn a_hostile_consumer_yields_room_or_a_violation() {
    loom::model(|| {
        let page = page();
        let (mut tx, _) = ends(&page);
        let hostile_page = Arc::clone(&page);
        let hostile = loom::thread::spawn(move || {
            for (at, value) in [(0, 5), (1, 9), (0, u32::MAX), (2, 1), (4, 6)] {
                hostile_page[at].store(value, Ordering::Release);
            }
        });
        let mut pushed = 0;
        for n in 0..D + 2 {
            match tx.push(&page, entry(n)) {
                Ok(true) => pushed += 1,
                Ok(false) => {}
                Err(violation) => {
                    assert_eq!(violation, Violation::HeadPastTail);
                    break;
                }
            }
            tx.publish(&page);
        }
        hostile.join().unwrap();
        assert!(pushed <= D, "pushed {pushed} into a ring of {D} nobody released");
    });
}

/// A stream's four words, its bytes, and a doorbell each way. A byte is a loom
/// cell: a copy into or out of it that the cursors do not order after the
/// other end's last copy of it is loom's own `Causality violation`. A doorbell
/// is loom's `Notify`, which keeps one ring and may wake its waiter for
/// nothing, and loom fails a run that ends with a thread waiting on one. It is
/// held in an `Rc`, as loom's threads take no `Send` bound and run on the one
/// thread that runs the model: loom's `Arc` panics again in its drop when that
/// failure unwinds the model, which aborts the run before it says which test
/// failed (`issues/loom-arc-panics-in-its-drop-while-a-failed-model-unwinds.md`).
struct Stream {
    page: [Shared; 4],
    bytes: [Cell<u32>; RING as usize],
    to_consumer: Notify,
    to_producer: Notify,
}

const RING: u32 = 1;
const STREAM: stream::Place<RING, 4> = stream::Place::new::<0, 1, 2, 3, 0>();
type StreamEnds = (stream::Producer<RING, 4>, stream::Consumer<RING, 4>);

fn stream() -> (Rc<Stream>, StreamEnds) {
    let s = Rc::new(Stream {
        page: core::array::from_fn(|_| Shared(AtomicU32::new(0))),
        bytes: core::array::from_fn(|_| Cell::new(0)),
        to_consumer: Notify::new(),
        to_producer: Notify::new(),
    });
    let ends = (stream::Producer::new(&s.page, STREAM), stream::Consumer::new(&s.page, STREAM));
    (s, ends)
}

/// Writes `byte` into the room the producer was last shown and publishes it,
/// ringing a consumer that armed.
fn send(s: &Stream, tx: &mut stream::Producer<RING, 4>, byte: u32) {
    for span in tx.span(0, 1).expect("a byte of room") {
        for at in span.offset..span.offset + span.len {
            s.bytes[at].set(byte);
        }
    }
    assert!(tx.advance(1));
    if tx.publish(&s.page) {
        s.to_consumer.notify();
    }
}

/// Reads the byte the consumer was last shown and releases it, ringing a
/// producer that armed.
fn receive(s: &Stream, rx: &mut stream::Consumer<RING, 4>) -> u32 {
    let mut read = Vec::new();
    for span in rx.span(0, 1).expect("a byte ready") {
        read.extend((span.offset..span.offset + span.len).map(|at| s.bytes[at].get()));
    }
    assert!(rx.advance(1));
    if rx.release(&s.page) {
        s.to_producer.notify();
    }
    read[0]
}

/// A consumer that polls reads each byte as the producer wrote it, through a
/// ring that wraps under it: its copy out follows the producer's copy in, and
/// the producer's next copy into that byte follows its copy out.
/// `publish-relaxed` takes the first edge away.
#[test]
fn a_published_byte_is_read_as_written() {
    loom::model(|| {
        let (s, (mut tx, mut rx)) = stream();
        let producer_side = Rc::clone(&s);
        let producer = loom::thread::spawn(move || {
            for byte in [0x5A, 0xA5] {
                while tx.room(&producer_side.page).expect("the consumer keeps the protocol") == 0 {
                    loom::thread::yield_now();
                }
                send(&producer_side, &mut tx, byte);
            }
        });
        let mut read = Vec::new();
        while read.len() < 2 {
            match rx.ready(&s.page).expect("the producer keeps the protocol") {
                0 => loom::thread::yield_now(),
                _ => read.push(receive(&s, &mut rx)),
            }
        }
        producer.join().expect("the producer thread");
        assert_eq!(read, [0x5A, 0xA5], "a byte was read before it was written");
    });
}

/// A consumer sleeps whenever the look after arming finds the ring empty, and
/// the byte published around its sleep still reaches it. `want-relaxed` lets
/// it miss the tail stored before the want it read cleared.
#[test]
fn a_consumer_that_looked_again_is_rung_for_bytes_it_missed() {
    loom::model(|| {
        let (s, (mut tx, mut rx)) = stream();
        let producer_side = Rc::clone(&s);
        let producer = loom::thread::spawn(move || send(&producer_side, &mut tx, 0x5A));
        while rx.ready(&s.page).expect("the producer keeps the protocol") == 0 {
            if rx.arm(&s.page).expect("the producer keeps the protocol") == 0 {
                s.to_consumer.wait();
            }
        }
        assert_eq!(receive(&s, &mut rx), 0x5A);
        producer.join().expect("the producer thread");
    });
}

/// A producer sleeps whenever the look after arming finds the ring full, and
/// the room released around its sleep still reaches it. `want-relaxed` lets
/// it miss the head stored before the want it read cleared.
#[test]
fn a_producer_that_looked_again_is_rung_for_room_it_missed() {
    loom::model(|| {
        let (s, (mut tx, mut rx)) = stream();
        send(&s, &mut tx, 0x5A);
        let consumer_side = Rc::clone(&s);
        let consumer = loom::thread::spawn(move || {
            assert_eq!(rx.ready(&consumer_side.page), Ok(1), "published before the consumer started");
            receive(&consumer_side, &mut rx)
        });
        while tx.room(&s.page).expect("the consumer keeps the protocol") == 0 {
            if tx.arm(&s.page).expect("the consumer keeps the protocol") == 0 {
                s.to_producer.wait();
            }
        }
        send(&s, &mut tx, 0xA5);
        assert_eq!(consumer.join().expect("the consumer thread"), 0x5A);
    });
}
