//! The transport's two ends on two CPUs, under loom: what no host test that
//! runs both ends on one thread can reach.
//!
//! - **Publication**: a consumer that sees a tail sees every word of the
//!   entries below it. `transport-publish-relaxed` takes the edge away.
//! - **Growth**: a ring doubled while it holds unread bytes reads each byte as
//!   written. `transport-grow-room-now` and `transport-size-before-tail` red it.
//! - **A hostile peer** leaves the honest end with entries, nothing, or a
//!   named violation, and never more entries than the ring holds.
//!   `transport-no-clamp` believes the peer.
//! - **A session's three waits**, each over `session`'s own sequences: a
//!   reader and a writer asleep at once, a poll beside a blocking call of one
//!   direction, and a source registered while its direction is ready. Each
//!   case names the controls that red it.
//! - **An end with two holders**: whole calls in gate order, bytes
//!   read once, a gate's waiter woken by the release or the owner's
//!   departure, a shutdown no holder writes past, and looks without the gate
//!   that lose no wake, with one holder killed at every point of its call.
//!
//!   cargo test -p toyos --features <control> --test transport_loom

use core::sync::atomic::Ordering;
use std::rc::Rc;

use loom::cell::Cell;
use loom::sync::atomic::AtomicU32;
use loom::sync::{Arc, Notify};
use toyos::transport::{stream, Consumer, Place, Producer, Violation, Word};

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
    fn compare_exchange(&self, current: u32, new: u32, success: Ordering, failure: Ordering) -> Result<u32, u32> {
        self.0.compare_exchange(current, new, success, failure)
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
                    Some(words) => read.push(words),
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
    page: [Shared; 5],
    bytes: [Cell<u32>; RING as usize],
    to_consumer: Notify,
    to_producer: Notify,
}

const RING: u32 = 1;
const STREAM: stream::Place<RING, 5> = stream::Place::new::<0, 1, 2, 3, 4, 0>();
type StreamEnds = (stream::Producer<RING, 5>, stream::Consumer<RING, 5>);

fn stream() -> (Rc<Stream>, StreamEnds) {
    let s = Rc::new(Stream {
        page: core::array::from_fn(|_| Shared(AtomicU32::new(0))),
        bytes: core::array::from_fn(|_| Cell::new(0)),
        to_consumer: Notify::new(),
        to_producer: Notify::new(),
    });
    let ends = (stream::Producer::new(&s.page, STREAM, RING), stream::Consumer::new(&s.page, STREAM));
    (s, ends)
}

/// Writes `byte` into the room the producer was last shown and publishes it,
/// ringing a consumer that armed.
fn send(s: &Stream, tx: &mut stream::Producer<RING, 5>, byte: u32) {
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
fn receive(s: &Stream, rx: &mut stream::Consumer<RING, 5>) -> u32 {
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
/// `transport-publish-relaxed` takes the first edge away.
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
/// the byte published around its sleep still reaches it.
/// `transport-want-relaxed` lets it miss the tail stored before the want it
/// read cleared.
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
/// the room released around its sleep still reaches it.
/// `transport-want-relaxed` lets it miss the head stored before the want it
/// read cleared.
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

/// A ring of at most four bytes, grown from two while the producer keeps
/// writing: no byte waits for it to empty, and every byte is read as written.
/// The consumer may hold two unread bytes at offsets 0 and 1 when the producer
/// grows, so the first byte past the switch, at offset 0 again, lands on an
/// unread one, a write beside the consumer's read, where the room is counted
/// in the new capacity at once (`transport-grow-room-now`); and a consumer
/// that loads the size before the tail meets a tail past what that size holds
/// (`transport-size-before-tail`).
#[test]
fn a_ring_grown_while_it_holds_bytes_reads_each_as_written() {
    const MOST: u32 = 4;
    const GROWN: stream::Place<MOST, 5> = stream::Place::new::<0, 1, 2, 3, 4, 0>();
    bounded(|| {
        let page: Rc<[Shared; 5]> = Rc::new(core::array::from_fn(|_| Shared(AtomicU32::new(0))));
        let bytes: Rc<[Cell<u32>; MOST as usize]> = Rc::new(core::array::from_fn(|_| Cell::new(0)));
        let (mut tx, mut rx) = (stream::Producer::new(&page, GROWN, 2), stream::Consumer::new(&page, GROWN));
        let put = |tx: &mut stream::Producer<MOST, 5>, bytes: &[Cell<u32>], at: u32| {
            for span in tx.span(0, 1).expect("a byte of room") {
                (span.offset..span.offset + span.len).for_each(|i| bytes[i].set(at));
            }
            assert!(tx.advance(1));
            let _ = tx.publish(&page);
        };
        // Positions 0 and 1 go round once and are read, then 2 and 3 are
        // written and left unread.
        for at in 0..2 {
            assert_eq!(tx.room(&page), Ok(2 - at));
            put(&mut tx, &bytes[..], at);
        }
        assert_eq!(rx.ready(&page), Ok(2));
        assert!(rx.advance(2));
        let _ = rx.release(&page);
        for at in 2..4 {
            assert!(tx.room(&page).expect("the consumer keeps the protocol") > 0);
            put(&mut tx, &bytes[..], at);
        }
        let (producer_page, producer_bytes) = (Rc::clone(&page), Rc::clone(&bytes));
        let producer = loom::thread::spawn(move || {
            // The consumer may already be reading: the switch is the old
            // ring past whatever head this look finds.
            tx.room(&producer_page).expect("the consumer keeps the protocol");
            tx.grow(&producer_page);
            for at in 4..7 {
                while tx.room(&producer_page).expect("the consumer keeps the protocol") == 0 {
                    loom::thread::yield_now();
                }
                for span in tx.span(0, 1).expect("a byte of room") {
                    (span.offset..span.offset + span.len).for_each(|i| producer_bytes[i].set(at));
                }
                assert!(tx.advance(1));
                let _ = tx.publish(&producer_page);
            }
            tx.capacity()
        });
        let mut read = Vec::new();
        while read.len() < 5 {
            match rx.ready(&page).expect("the producer keeps the protocol") {
                0 => loom::thread::yield_now(),
                _ => {
                    for span in rx.span(0, 1).expect("a byte ready") {
                        read.extend((span.offset..span.offset + span.len).map(|i| bytes[i].get()));
                    }
                    assert!(rx.advance(1));
                    let _ = rx.release(&page);
                }
            }
        }
        assert_eq!(producer.join().expect("the producer thread"), MOST);
        assert_eq!(read, [2, 3, 4, 5, 6], "a byte was read other than as written");
    });
}

// ---- a session: the client's sequences and the server's rings ----
//
// The models below drive `session`'s own code: a holder's `read`, `write`,
// `shutdown`, `watch`, `unwatch` and `register` and the server's `publish`,
// `release`, `finish`, `fail`, `rung` and `departed`. A doorbell is a kernel
// pipe as a holder waits on it: a ring writes a byte and posts its watch, a
// blocking wait and a drain take every byte there, and a watch is answered at
// once if the pipe holds a byte when it is registered, and otherwise by
// whether it holds one when its submitter looks after a post, armed again if
// it does not (`kernel/src/inbox/polls.rs`, `deliver`). The server rings
// every holder's doorbell a ring names, and is told a holder left only after
// that holder's last effect, as the kernel closes a process's handles once its
// last thread is out. loom fails a run that ends with a thread waiting.
// Shared by `Rc`, as the stream models above are.

use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};

use loom::sync::atomic::AtomicBool;
use loom::sync::{Condvar, Mutex};
use toyos::transport::session::{
    self, Bells, Doorbell, Holder, HungUp, Layout, Lock, Mode, Polled, Receiving, Refused, Rings, Sending, Server, Took,
    Waited, Way,
};

/// Every session model explores its interleavings with at most this many
/// preemptions unless `LOOM_MAX_PREEMPTIONS` names another bound: each
/// mutation of `build/ci.rs`'s controls is red at two, and a holder's death
/// is a stop of its own that takes no preemption.
const PREEMPTIONS: usize = 3;

fn bounded(model: impl Fn() + Sync + Send + 'static) {
    bounded_by(PREEMPTIONS, model);
}

/// [`bounded`] at `preemptions`.
fn bounded_by(preemptions: usize, model: impl Fn() + Sync + Send + 'static) {
    let mut builder = loom::model::Builder::new();
    if builder.preemption_bound.is_none() {
        builder.preemption_bound = Some(preemptions);
    }
    builder.check(model);
}

const BYTES: u32 = 2;
const SESSION_WORDS: usize = 16;
const SESSION: Layout<BYTES, BYTES, SESSION_WORDS> = Layout::new::<8, 9, 10, 11, 12, 13>(
    stream::Place::new::<0, 1, 2, 3, 14, 0>(),
    stream::Place::new::<4, 5, 6, 7, 15, { BYTES as usize }>(),
);
/// A failure the server stores in the receive ring's `end`.
const RESET: u32 = 2;

type Rx = Receiving<BYTES, SESSION_WORDS>;
type Tx = Sending<BYTES, SESSION_WORDS>;
type End = Server<BYTES, BYTES, SESSION_WORDS>;

/// A doorbell that coalesces, as an event does.
struct Pipe {
    level: Mutex<Level>,
    posted: Condvar,
}

struct Level {
    bytes: u32,
    posts: u32,
    /// A holder rang, since the server last took it: the server's pipe alone.
    rang: bool,
}

impl Pipe {
    fn new() -> Self {
        Self { level: Mutex::new(Level { bytes: 0, posts: 0, rang: false }), posted: Condvar::new() }
    }

    /// `SYS_WRITE` of one byte: the watch's post wakes every waiter.
    fn ring(&self) {
        let mut level = self.level.lock().unwrap();
        level.bytes += 1;
        level.posts += 1;
        self.posted.notify_all();
    }

    /// A holder's ring of the server, as its rung bell is, so the server that
    /// takes it looks at the words after the holder wrote them.
    fn ring_as_holder(&self) {
        self.level.lock().unwrap().rang = true;
        self.ring();
    }

    /// [`Self::wait`], and whether a holder rang since the last: a departure
    /// alone asks no ring back of the server, as netstack's pass rings back
    /// only a stream whose client rang.
    fn wait_rang(&self) -> bool {
        let mut level = self.level.lock().unwrap();
        while level.bytes == 0 {
            level = self.posted.wait(level).unwrap();
        }
        level.bytes = 0;
        std::mem::take(&mut level.rang)
    }

    /// A nonblocking `SYS_READ` of every byte there: whether there was one.
    fn drain(&self) -> bool {
        let mut level = self.level.lock().unwrap();
        std::mem::replace(&mut level.bytes, 0) != 0
    }

    /// A blocking `SYS_READ` and the drain after it.
    fn wait(&self) {
        let mut level = self.level.lock().unwrap();
        while level.bytes == 0 {
            level = self.posted.wait(level).unwrap();
        }
        level.bytes = 0;
    }

    /// `OP_WATCH` for reading, registered and waited on.
    fn watch(&self) {
        let mut seen = {
            let level = self.level.lock().unwrap();
            if level.bytes != 0 {
                return;
            }
            level.posts
        };
        loop {
            {
                let mut level = self.level.lock().unwrap();
                while level.posts == seen {
                    level = self.posted.wait(level).unwrap();
                }
            }
            let level = self.level.lock().unwrap();
            if level.bytes != 0 {
                return;
            }
            seen = level.posts;
        }
    }
}

/// A half behind loom's mutex.
struct Held<T>(Mutex<T>);

impl<T> Lock<T> for Held<T> {
    fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        f(&mut self.0.lock().unwrap())
    }
}

/// One holder's halves and its two doorbells.
struct Holding {
    rx: Held<Rx>,
    tx: Held<Tx>,
    receive: Pipe,
    send: Pipe,
}

impl Holding {
    fn new((rx, tx): (Rx, Tx)) -> Self {
        Self { rx: Held(Mutex::new(rx)), tx: Held(Mutex::new(tx)), receive: Pipe::new(), send: Pipe::new() }
    }

    fn pipe(&self, bell: Doorbell) -> &Pipe {
        match bell {
            Doorbell::Receive => &self.receive,
            Doorbell::Send => &self.send,
        }
    }
}

/// One session: its words and bytes, its holders and the server's doorbell.
struct Session {
    page: [Shared; SESSION_WORDS],
    bytes: [Cell<u32>; 2 * BYTES as usize],
    holders: Vec<Holding>,
    server: Pipe,
    /// A bit for each holder that left, as the kernel tells the server of a
    /// hang-up.
    left: AtomicU32,
    /// The test is done: the server's loop ends at its next ring.
    quit: AtomicBool,
}

/// When a holder dies: after `left` more effects another thread can see, a
/// word written, a byte copied in or a ring, it makes none, as a process the
/// kernel ended makes none. The effect it does not make unwinds its call,
/// through code whose effects are all void.
struct Fate {
    left: std::cell::Cell<u32>,
    dead: std::cell::Cell<bool>,
}

/// What a dead holder's call unwinds with.
struct Died;

impl Fate {
    fn after(effects: Option<u32>) -> Self {
        Self { left: std::cell::Cell::new(effects.unwrap_or(u32::MAX)), dead: std::cell::Cell::new(false) }
    }

    /// Whether the effect about to be made is made.
    fn lives(&self) -> bool {
        if self.dead.get() {
            return false;
        }
        let Some(left) = self.left.get().checked_sub(1) else {
            self.dead.set(true);
            resume_unwind(Box::new(Died));
        };
        self.left.set(left);
        true
    }
}

fn lives(fate: Option<&Fate>) -> bool {
    fate.is_none_or(Fate::lives)
}

/// A word as a holder that may die writes it.
struct Mortal<'a> {
    word: &'a Shared,
    fate: &'a Fate,
}

impl Word for Mortal<'_> {
    fn load(&self, order: Ordering) -> u32 {
        self.word.load(order)
    }
    fn store(&self, value: u32, order: Ordering) {
        if self.fate.lives() {
            self.word.store(value, order);
        }
    }
    fn swap(&self, value: u32, order: Ordering) -> u32 {
        if self.fate.lives() { self.word.swap(value, order) } else { 0 }
    }
    fn compare_exchange(&self, current: u32, new: u32, success: Ordering, failure: Ordering) -> Result<u32, u32> {
        if self.fate.lives() { self.word.compare_exchange(current, new, success, failure) } else { Ok(current) }
    }
}

/// A holder's doorbells and its ring of the server, and its fate where it may
/// die.
struct By<'a> {
    s: &'a Session,
    at: usize,
    fate: Option<&'a Fate>,
}

/// A holder's doorbells, which never hang up and take no deadline.
impl Bells for By<'_> {
    type Until = ();

    fn drain(&self, bell: Doorbell) -> Took {
        if self.s.holders[self.at].pipe(bell).drain() { Took::Rung } else { Took::Nothing }
    }

    fn wait(&self, bell: Doorbell, _: Option<()>) -> Waited {
        self.s.holders[self.at].pipe(bell).wait();
        Waited::Rung
    }

    fn wait_gate(&self, bell: Doorbell, until: Option<()>, _: u16) -> Waited {
        self.wait(bell, until)
    }

    fn ring(&self) -> Result<(), HungUp> {
        if lives(self.fate) {
            self.s.server.ring_as_holder();
        }
        Ok(())
    }
}

impl Session {
    /// A session whose end has `holders` holders, and its server end.
    fn new(holders: u16) -> (Rc<Self>, End) {
        let page: [Shared; SESSION_WORDS] = core::array::from_fn(|_| Shared(AtomicU32::new(0)));
        let mut all = vec![Holding::new(session::client(&page, SESSION, BYTES))];
        all.extend((2..=holders).map(|n| Holding::new(session::holder(SESSION, Holder::new(n).unwrap()))));
        let server = Server::new(&page, SESSION, BYTES);
        let s = Session {
            page,
            bytes: core::array::from_fn(|_| Cell::new(0)),
            holders: all,
            server: Pipe::new(),
            left: AtomicU32::new(0),
            quit: AtomicBool::new(false),
        };
        (Rc::new(s), server)
    }

    /// Holder `at`'s doorbells.
    fn by(&self, at: usize) -> By<'_> {
        By { s: self, at, fate: None }
    }

    /// The first holder's doorbell for `way`.
    fn pipe(&self, way: Way) -> &Pipe {
        self.holders[0].pipe(session::doorbell(way))
    }

    /// The server's rings, of every holder.
    fn rings(&self, rings: Rings) {
        for holding in &self.holders {
            if rings.receive {
                holding.receive.ring();
            }
            if rings.send {
                holding.send.ring();
            }
        }
    }

    /// A holder reads at most `max` bytes over `page`: the bytes it read.
    fn read_by<W: Word>(&self, page: &[W; SESSION_WORDS], by: &By, mode: Mode<()>, max: u32) -> Result<Vec<u32>, Refused> {
        let mut read = Vec::new();
        session::read(page, &self.holders[by.at].rx, by, mode, max, |_| true, |span, _| {
            read.extend((span.offset..span.offset + span.len).map(|at| self.bytes[at].get()));
        })?;
        Ok(read)
    }

    /// A holder writes `byte` over `page`, once at most.
    fn write_by<W: Word>(&self, page: &[W; SESSION_WORDS], by: &By, mode: Mode<()>, byte: u32) -> Result<u32, Refused> {
        session::write(page, &self.holders[by.at].tx, by, mode, 1, |_| true, |span, _| {
            for at in span.offset..span.offset + span.len {
                if lives(by.fate) {
                    self.bytes[at].set(byte);
                }
            }
        })
    }

    /// The first holder reads one byte, waiting or not.
    fn read(&self, mode: Mode<()>) -> Result<u32, Refused> {
        self.read_by(&self.page, &self.by(0), mode, 1).map(|read| read.len() as u32)
    }

    /// The first holder writes one byte, waiting or not.
    fn write(&self, mode: Mode<()>) -> Result<u32, Refused> {
        self.write_by(&self.page, &self.by(0), mode, 0x5A)
    }

    /// The first holder fills its send ring, before the server's thread runs.
    fn fill(&self) {
        for _ in 0..BYTES {
            assert_eq!(self.write(Mode::Nonblocking), Ok(1));
        }
    }

    /// The server publishes `bytes` into the receive ring from its tail at
    /// `from`, which it copies in first.
    fn publish_at(&self, server: &mut End, from: u32, bytes: &[u32]) {
        for (n, byte) in (from..).zip(bytes) {
            self.bytes[(n % BYTES) as usize].set(*byte);
        }
        self.rings(server.publish(&self.page, bytes.len() as u32).expect("the client keeps the protocol"));
    }

    /// The server publishes `n` bytes into an empty receive ring.
    fn publish(&self, server: &mut End, n: u32) {
        self.publish_at(server, 0, &vec![0xA5; n as usize]);
    }

    /// The server takes back `n` bytes of the send ring.
    fn release(&self, server: &mut End, n: u32) {
        assert_eq!(server.sent(&self.page), Ok(BYTES), "the client filled its send ring");
        self.rings(server.release(&self.page, n).expect("the client keeps the protocol"));
    }

    /// The server's loop: each holder's departure answered by
    /// [`Server::departed`] and each ring by [`Server::rung`], until the
    /// test is done.
    fn serve(&self, server: &mut End) {
        let mut departed = 0u32;
        loop {
            let rang = self.server.wait_rang();
            let left = self.left.load(Ordering::Acquire);
            for at in 0..self.holders.len() {
                let bit = 1 << at;
                if left & bit != 0 && departed & bit == 0 {
                    departed |= bit;
                    let holder = Holder::new(at as u16 + 1).unwrap();
                    self.rings(server.departed(&self.page, holder).expect("the client keeps the protocol"));
                }
            }
            self.answer_rings(server, rang);
            if self.quit.load(Ordering::Acquire) {
                return;
            }
        }
    }

    /// [`Server::rung`]'s rings, where a holder `rang`.
    fn answer_rings(&self, server: &mut End, rang: bool) {
        if rang {
            self.rings(server.rung(&self.page).expect("the client keeps the protocol"));
        }
    }

    /// Holder `at` left: its hang-up, which the kernel gives the server once
    /// the holder's last thread is out.
    fn leave(&self, at: usize) {
        self.left.fetch_or(1 << at, Ordering::Release);
        self.server.ring();
    }

    /// The test is done, and says so to the server's loop.
    fn done(&self) {
        self.quit.store(true, Ordering::Release);
        self.server.ring();
    }

    /// Runs `call` as holder `at` over a page it writes as a mortal, dying
    /// after `effects` of its effects where given, then hangs it up: what the
    /// call answered, or `None` where the holder died first.
    fn as_holder<R>(&self, at: usize, effects: Option<u32>, call: impl FnOnce(&[Mortal; SESSION_WORDS], &By) -> R) -> Option<R> {
        let fate = Fate::after(effects);
        let page: [Mortal; SESSION_WORDS] = core::array::from_fn(|n| Mortal { word: &self.page[n], fate: &fate });
        let by = By { s: self, at, fate: Some(&fate) };
        let answer = catch_unwind(AssertUnwindSafe(|| call(&page, &by)));
        self.leave(at);
        match answer {
            Ok(answer) => Some(answer),
            Err(died) if died.is::<Died>() => {
                DIED.set(true);
                None
            }
            Err(other) => resume_unwind(other),
        }
    }

    /// The bytes of the send ring the server holds, in stream order.
    fn sent(&self, server: &mut End) -> Vec<u32> {
        let sent = server.sent(&self.page).expect("the holders keep the protocol");
        (0..sent).map(|n| self.bytes[(BYTES + n % BYTES) as usize].get()).collect()
    }
}

/// Which the server's thread does first.
#[derive(Clone, Copy)]
enum First {
    Room,
    Data,
}

/// A reader asleep over an empty receive ring and a writer asleep over a full
/// send ring, while the server releases room and publishes data in either
/// order: each wakes, as each waits on a doorbell of its own.
/// `transport-one-doorbell` lets the reader take the writer's wake, or the
/// writer the reader's.
fn two_sleepers(first: First) {
    bounded(move || {
        let (s, mut server) = Session::new(1);
        s.fill();
        let net = {
            let s = Rc::clone(&s);
            loom::thread::spawn(move || match first {
                First::Room => {
                    s.release(&mut server, 1);
                    s.publish(&mut server, 1);
                }
                First::Data => {
                    s.publish(&mut server, 1);
                    s.release(&mut server, 1);
                }
            })
        };
        let reader = {
            let s = Rc::clone(&s);
            loom::thread::spawn(move || assert_eq!(s.read(Mode::Blocking(None)), Ok(1)))
        };
        assert_eq!(s.write(Mode::Blocking(None)), Ok(1));
        reader.join().unwrap();
        net.join().unwrap();
    });
}

#[test]
fn two_sleepers_each_wake_when_room_is_released_first() {
    two_sleepers(First::Room);
}

#[test]
fn two_sleepers_each_wake_when_data_is_published_first() {
    two_sleepers(First::Data);
}

/// What the server's thread leaves ready, once, for a blocking call to take
/// part of.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Left {
    /// Two bytes published; a blocking read takes one.
    Bytes,
    /// The receive ring's orderly end; a blocking read answers it.
    End,
    /// Two bytes of a full send ring released; a blocking write fills one.
    Room,
}

/// A blocking call and a `poll` of one direction, the server making the
/// direction ready once and then answering each ring: the poll is answered with
/// the direction ready, whichever of them took the doorbell.
/// `transport-no-pass-on` lets the blocking call take the wake the poll sleeps
/// over.
fn same_direction(left: Left) {
    bounded(move || {
        let (s, mut server) = Session::new(1);
        if left == Left::Room {
            s.fill();
        }
        let net = {
            let s = Rc::clone(&s);
            loom::thread::spawn(move || {
                match left {
                    Left::Bytes => s.publish(&mut server, 2),
                    Left::End => s.rings(server.finish(&s.page)),
                    Left::Room => s.release(&mut server, 2),
                }
                s.serve(&mut server);
            })
        };
        let blocking = {
            let s = Rc::clone(&s);
            loom::thread::spawn(move || {
                let answer = if left == Left::Room { s.write(Mode::Blocking(None)) } else { s.read(Mode::Blocking(None)) };
                let owed = if left == Left::End { 0 } else { 1 };
                assert_eq!(answer, Ok(owed));
            })
        };
        let (holding, by) = (&s.holders[0], s.by(0));
        loop {
            let polled = if left == Left::Room {
                session::watch::<_, _, _, Tx, SESSION_WORDS>(&s.page, &holding.tx, &by)
            } else {
                session::watch::<_, _, _, Rx, SESSION_WORDS>(&s.page, &holding.rx, &by)
            };
            let Polled::Watching(watching) = polled else { break };
            let way = if left == Left::Room { Way::Send } else { Way::Receive };
            s.pipe(way).watch();
            let ready = if left == Left::Room {
                session::unwatch::<_, _, _, Tx, SESSION_WORDS>(&s.page, &holding.tx, &by, watching)
            } else {
                session::unwatch::<_, _, _, Rx, SESSION_WORDS>(&s.page, &holding.rx, &by, watching)
            };
            if ready {
                break;
            }
        }
        blocking.join().unwrap();
        s.done();
        net.join().unwrap();
    });
}

#[test]
fn a_poll_beside_a_blocking_read_is_answered_for_the_bytes_it_left() {
    same_direction(Left::Bytes);
}

#[test]
fn a_poll_beside_a_blocking_read_is_answered_for_the_end_it_left() {
    same_direction(Left::End);
}

#[test]
fn a_poll_beside_a_blocking_write_is_answered_for_the_room_it_left() {
    same_direction(Left::Room);
}

/// What makes the registered direction ready.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ready {
    /// A byte in the receive ring.
    Bytes,
    /// The receive ring's orderly end.
    End,
    /// Room in the send ring: empty at the start where it is ready before the
    /// registration, released from full where it races it.
    Room,
    /// A reset stored while the send ring is full.
    Reset,
}

/// A source registered while its direction is ready, or racing the server
/// making it so, gets its first event: its doorbell is watched, as mio's select
/// watches it, and each event leads to the nonblocking operation, until that
/// finds the direction ready. `transport-register-no-arm`,
/// `transport-register-no-ring` and `transport-register-neither` take the
/// registration's arm or ring or both away; `transport-want-relaxed` lets a
/// racing server's clear go unseen; `transport-head-only` makes the reset no
/// readiness to the arm's look or the ring back.
fn registered(ready: Ready, racing: bool) {
    bounded(move || {
        let (s, mut server) = Session::new(1);
        if ready == Ready::Reset || (ready == Ready::Room && racing) {
            s.fill();
        }
        let make_ready = move |s: &Session, server: &mut End| match ready {
            Ready::Bytes => s.publish(server, 1),
            Ready::End => s.rings(server.finish(&s.page)),
            Ready::Room => s.release(server, 1),
            Ready::Reset => s.rings(server.fail(&s.page, RESET)),
        };
        if !racing && ready != Ready::Room {
            make_ready(&s, &mut server);
        }
        let net = {
            let s = Rc::clone(&s);
            loom::thread::spawn(move || {
                if racing {
                    make_ready(&s, &mut server);
                }
                s.serve(&mut server);
            })
        };
        let send = matches!(ready, Ready::Room | Ready::Reset);
        let (holding, by) = (&s.holders[0], s.by(0));
        if send {
            session::register(&s.page, None::<&Held<Rx>>, Some(&holding.tx), &by);
        } else {
            session::register(&s.page, Some(&holding.rx), None::<&Held<Tx>>, &by);
        }
        let way = if send { Way::Send } else { Way::Receive };
        loop {
            s.pipe(way).watch();
            let answer = if send { s.write(Mode::Nonblocking) } else { s.read(Mode::Nonblocking) };
            match answer {
                Err(Refused::WouldBlock) => {}
                Ok(_) | Err(Refused::Failed(RESET)) => break,
                Err(other) => panic!("the operation answered {other:?}"),
            }
        }
        s.done();
        net.join().unwrap();
    });
}

#[test]
fn a_source_registered_after_bytes_arrived_gets_its_first_event() {
    registered(Ready::Bytes, false);
}

#[test]
fn a_source_registered_as_bytes_arrive_gets_its_first_event() {
    registered(Ready::Bytes, true);
}

#[test]
fn a_source_registered_after_the_end_gets_its_first_event() {
    registered(Ready::End, false);
}

#[test]
fn a_source_registered_as_the_end_arrives_gets_its_first_event() {
    registered(Ready::End, true);
}

#[test]
fn a_source_registered_with_room_gets_its_first_event() {
    registered(Ready::Room, false);
}

#[test]
fn a_source_registered_as_room_is_released_gets_its_first_event() {
    registered(Ready::Room, true);
}

#[test]
fn a_source_registered_over_a_full_ring_after_a_reset_gets_its_first_event() {
    registered(Ready::Reset, false);
}

#[test]
fn a_source_registered_over_a_full_ring_as_a_reset_arrives_gets_its_first_event() {
    registered(Ready::Reset, true);
}

// ---- an end with two holders ----
//
// The second holder may be killed: it lives for a count of its effects, from
// none up to the first count no execution of the model kills it at, so every
// model below meets it dead at every point of its call, with a gate held or
// not, and its departure is answered whenever the server's loop reads it.

thread_local! {
    /// Whether a holder died in any execution of the model since the last
    /// [`whenever_it_dies`] count: loom runs every execution of a model, its
    /// threads too, on the test's own thread.
    static DIED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// `model` with its holder killed after each count of its effects in turn,
/// until a count at which no execution killed it.
fn whenever_it_dies(model: impl Fn(Option<u32>)) {
    for effects in 0.. {
        DIED.set(false);
        model(Some(effects));
        if !DIED.get() {
            return;
        }
    }
}

/// Two holders write a byte each, the second killed after `dies` of its
/// effects or never: the server holds whole calls in the order the gate
/// passed, the dead holder's byte only where it published it, and the
/// survivor's call returns. `transport-departure-no-pass` leaves the survivor
/// asleep for a gate the dead holder held; `transport-departure-relaxed` lets
/// the survivor copy into the byte the dead holder copied into, unordered.
/// Where both live, the one that found the gate the other's waits:
/// `transport-gate-no-want`, `transport-contended-no-arm` and
/// `transport-want-before-arm` leave it asleep after the owner let go.
fn two_writers(dies: Option<u32>) {
    bounded(move || {
        let (s, mut server) = Session::new(2);
        let net = {
            let s = Rc::clone(&s);
            loom::thread::spawn(move || {
                s.serve(&mut server);
                server
            })
        };
        let mortal = {
            let s = Rc::clone(&s);
            loom::thread::spawn(move || s.as_holder(1, dies, |page, by| s.write_by(page, by, Mode::Blocking(None), 0xB)))
        };
        assert_eq!(s.write_by(&s.page, &s.by(0), Mode::Blocking(None), 0xA), Ok(1), "the survivor's write");
        let mortal = mortal.join().unwrap();
        s.done();
        let mut server = net.join().unwrap();
        let sent = s.sent(&mut server);
        match mortal {
            Some(answer) => {
                assert_eq!(answer, Ok(1), "the second holder's write");
                assert!(sent == [0xA, 0xB] || sent == [0xB, 0xA], "the server holds {sent:x?}");
            }
            None => assert!(sent == [0xA] || sent == [0xA, 0xB] || sent == [0xB, 0xA], "the server holds {sent:x?}"),
        }
    });
}

#[test]
fn two_holders_write_whole_calls_in_gate_order_whenever_one_dies() {
    whenever_it_dies(two_writers);
}

/// A holder that finds the gate another's is woken by the owner's
/// release.
#[test]
fn a_holder_waiting_for_the_gate_is_woken_by_its_release() {
    two_writers(None);
}

/// The owner dies after its copy into the byte, with the gate held: the
/// waiter is rung by the departure, and its copy follows the dead holder's.
#[test]
fn a_holder_waiting_for_the_gate_is_woken_by_its_owners_departure() {
    two_writers(Some(2));
}

/// Two holders read two bytes and the orderly end, the second killed
/// after `dies` of its effects or never: every byte is read once and in
/// order, the survivor reads exactly what the dead holder did not release,
/// and its reads return. `transport-departure-no-pass` leaves the survivor
/// asleep for a gate the dead holder held; `transport-no-resume` has a holder
/// read again what the other released.
fn two_readers(dies: Option<u32>) {
    bounded(move || {
        let (s, mut server) = Session::new(2);
        s.publish_at(&mut server, 0, &[0xA, 0xB]);
        s.rings(server.finish(&s.page));
        let net = {
            let s = Rc::clone(&s);
            loom::thread::spawn(move || s.serve(&mut server))
        };
        let mortal = {
            let s = Rc::clone(&s);
            loom::thread::spawn(move || s.as_holder(1, dies, |page, by| s.read_by(page, by, Mode::Blocking(None), 1)))
        };
        let mut survivor = Vec::new();
        loop {
            match s.read_by(&s.page, &s.by(0), Mode::Blocking(None), 2) {
                Ok(read) if read.is_empty() => break,
                Ok(read) => survivor.extend(read),
                Err(refused) => panic!("the survivor's read answered {refused:?}"),
            }
        }
        let mortal = mortal.join().unwrap();
        s.done();
        net.join().unwrap();
        match mortal {
            Some(Ok(took)) => assert_eq!([took, survivor].concat(), [0xA, 0xB], "each byte once, in order"),
            Some(Err(refused)) => panic!("the second holder's read answered {refused:?}"),
            None => assert!(survivor == [0xA, 0xB] || survivor == [0xB], "the survivor read {survivor:x?}"),
        }
    });
}

#[test]
fn two_holders_read_each_byte_once_whenever_one_dies() {
    whenever_it_dies(two_readers);
}

#[test]
fn two_holders_read_each_byte_once() {
    two_readers(None);
}

/// One holder shuts the sending half down and, once it returned, another
/// writes: the write answers `Shut` and the server holds nothing.
/// `transport-shut-local` has the second holder write past the first's
/// shutdown.
#[test]
fn a_write_after_another_holders_shutdown_answers_shut() {
    bounded(|| {
        let (s, mut server) = Session::new(2);
        let net = {
            let s = Rc::clone(&s);
            loom::thread::spawn(move || {
                s.serve(&mut server);
                server
            })
        };
        let shut = Rc::new(AtomicBool::new(false));
        let shutting = {
            let (s, shut) = (Rc::clone(&s), Rc::clone(&shut));
            loom::thread::spawn(move || {
                session::shutdown(&s.page, &s.holders[0].tx, &s.by(0));
                shut.store(true, Ordering::Release);
            })
        };
        while !shut.load(Ordering::Acquire) {
            loom::thread::yield_now();
        }
        assert_eq!(s.write_by(&s.page, &s.by(1), Mode::Blocking(None), 0xB), Err(Refused::Shut));
        shutting.join().unwrap();
        s.done();
        let mut server = net.join().unwrap();
        assert!(server.shut(&s.page));
        assert_eq!(s.sent(&mut server), [], "a byte past the shutdown");
    });
}

/// One holder shuts the sending half down while another, killed
/// after `dies` of its effects or never, writes: the shutdown returns, and no
/// byte is published after the `end` the server saw. A shutdown under the
/// writer's gate is the writer's to store, or its departure's.
fn shutdown_racing_a_write(dies: Option<u32>) {
    bounded(move || {
        let (s, mut server) = Session::new(2);
        let net = {
            let s = Rc::clone(&s);
            loom::thread::spawn(move || {
                let mut at_the_end = None;
                let mut departed = 0u32;
                loop {
                    let rang = s.server.wait_rang();
                    let left = s.left.load(Ordering::Acquire);
                    if left & 2 != 0 && departed == 0 {
                        departed = 2;
                        s.rings(server.departed(&s.page, Holder::new(2).unwrap()).expect("the client keeps the protocol"));
                    }
                    if server.shut(&s.page) {
                        let sent = server.sent(&s.page).expect("the client keeps the protocol");
                        assert_eq!(*at_the_end.get_or_insert(sent), sent, "a byte published past the end");
                    }
                    s.answer_rings(&mut server, rang);
                    if s.quit.load(Ordering::Acquire) {
                        return (server, at_the_end);
                    }
                }
            })
        };
        let mortal = {
            let s = Rc::clone(&s);
            loom::thread::spawn(move || s.as_holder(1, dies, |page, by| s.write_by(page, by, Mode::Blocking(None), 0xB)))
        };
        session::shutdown(&s.page, &s.holders[0].tx, &s.by(0));
        let mortal = mortal.join().unwrap();
        s.done();
        let (mut server, at_the_end) = net.join().unwrap();
        assert!(server.shut(&s.page), "the shutdown returned before its `end` was stored");
        let sent = s.sent(&mut server);
        if let Some(at_the_end) = at_the_end {
            assert_eq!(sent.len() as u32, at_the_end, "a byte published past the end");
        }
        match mortal {
            Some(Ok(1)) => assert_eq!(sent, [0xB]),
            Some(Err(Refused::Shut)) => assert_eq!(sent, []),
            Some(other) => panic!("the write answered {other:?}"),
            None => assert!(sent.is_empty() || sent == [0xB]),
        }
    });
}

#[test]
fn a_shutdown_racing_a_write_is_never_written_past() {
    shutdown_racing_a_write(None);
}

#[test]
fn a_shutdown_racing_a_write_is_never_written_past_whenever_the_writer_dies() {
    whenever_it_dies(shutdown_racing_a_write);
}

/// Two holders read until the end while the server fills the ring, takes
/// a byte of room back and publishes a third: a look without the gate, over a
/// cursor the other holder moved, can see that third byte past a head from
/// before the room was released, which overstates and loses no wake, and
/// every byte is read once. `transport-ungated-violation` ends a read over
/// such a look. The stale head is a load loom answers with an older store,
/// which takes no preemption: its control is red at none, so its bound is one.
#[test]
fn looks_without_the_gate_lose_no_wake() {
    bounded_by(1, || {
        let (s, mut server) = Session::new(2);
        s.publish_at(&mut server, 0, &[0xA, 0xB]);
        let net = {
            let s = Rc::clone(&s);
            loom::thread::spawn(move || {
                // Every ring taken here is answered, as a server answers each:
                // a release a waiting holder asked for rings it through `rung`.
                while server.want_room(&s.page).expect("the client keeps the protocol") == 0 {
                    let rang = s.server.wait_rang();
                    s.answer_rings(&mut server, rang);
                }
                s.publish_at(&mut server, 2, &[0xC]);
                s.rings(server.finish(&s.page));
                s.serve(&mut server);
            })
        };
        let reader = |at: usize| {
            let s = Rc::clone(&s);
            move || {
                let mut read = Vec::new();
                loop {
                    match s.read_by(&s.page, &s.by(at), Mode::Blocking(None), 1) {
                        Ok(bytes) if bytes.is_empty() => return read,
                        Ok(bytes) => read.extend(bytes),
                        Err(refused) => panic!("holder {at}'s read answered {refused:?}"),
                    }
                }
            }
        };
        let other = loom::thread::spawn(reader(1));
        let mine = reader(0)();
        let other = other.join().unwrap();
        s.done();
        net.join().unwrap();
        let mut all = [mine.clone(), other.clone()].concat();
        all.sort();
        assert_eq!(all, [0xA, 0xB, 0xC], "{mine:x?} and {other:x?}");
        assert!(mine.is_sorted() && other.is_sorted(), "{mine:x?} and {other:x?} out of order");
    });
}
