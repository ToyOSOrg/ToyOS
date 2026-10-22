//! The transport's two ends on two CPUs, under loom: what no host test that
//! runs both ends on one thread can reach.
//!
//! - **Publication**: a consumer that sees a tail sees every word of the
//!   entries below it. `transport-publish-relaxed` takes the edge away.
//! - **A hostile peer** leaves the honest end with entries, nothing, or a
//!   named violation, and never more entries than the ring holds.
//!   `transport-no-clamp` believes the peer.
//! - **A session's three waits**, each over `session`'s own sequences: a
//!   reader and a writer asleep at once, a poll beside a blocking call of one
//!   direction, and a source registered while its direction is ready. Each
//!   case names the controls that red it.
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

// ---- a session: the client's sequences and the server's rings ----
//
// The models below drive `session`'s own code: the client's `read`, `write`,
// `watch`, `unwatch` and `register` and the server's `publish`, `release`,
// `finish`, `fail` and `rung`. A doorbell is a kernel pipe as the client waits
// on it: a ring writes a byte and posts its watch, a blocking wait and a drain
// take every byte there, and a watch is answered at once if the pipe holds a
// byte when it is registered, and otherwise by whether it holds one when its
// submitter looks after a post, armed again if it does not
// (`kernel/src/inbox/polls.rs`, `deliver`). loom fails a run that ends with a
// thread waiting. Shared by `Rc`, as the stream models above are.

use loom::sync::atomic::AtomicBool;
use loom::sync::{Condvar, Mutex};
use toyos::transport::session::{
    self, Bells, Doorbell, Layout, Lock, Mode, Polled, Receiving, Refused, Rings, Sending, Server, Took, Waited, Way,
};

/// Every session model explores its interleavings with at most this many
/// preemptions unless `LOOM_MAX_PREEMPTIONS` names another bound, as the
/// kernel's own models bound theirs.
const PREEMPTIONS: usize = 5;

fn bounded(model: impl Fn() + Sync + Send + 'static) {
    let mut builder = loom::model::Builder::new();
    if builder.preemption_bound.is_none() {
        builder.preemption_bound = Some(PREEMPTIONS);
    }
    builder.check(model);
}

const BYTES: u32 = 2;
const SESSION_WORDS: usize = 10;
const SESSION: Layout<BYTES, BYTES, SESSION_WORDS> = Layout::new::<8, 9>(
    stream::Place::new::<0, 1, 2, 3, 0>(),
    stream::Place::new::<4, 5, 6, 7, { BYTES as usize }>(),
);
/// A failure the server stores in the receive ring's `end`.
const RESET: u32 = 2;

type Rx = Receiving<BYTES, SESSION_WORDS>;
type Tx = Sending<BYTES, SESSION_WORDS>;

/// A one-byte doorbell pipe.
struct Pipe {
    level: Mutex<Level>,
    posted: Condvar,
}

struct Level {
    bytes: u32,
    posts: u32,
}

impl Pipe {
    fn new() -> Self {
        Self { level: Mutex::new(Level { bytes: 0, posts: 0 }), posted: Condvar::new() }
    }

    /// `SYS_WRITE` of one byte: the watch's post wakes every waiter.
    fn ring(&self) {
        let mut level = self.level.lock().unwrap();
        level.bytes += 1;
        level.posts += 1;
        self.posted.notify_all();
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

/// One session: its words and bytes, the client's two halves, the client's two
/// doorbells and the server's one.
struct Session {
    page: [Shared; SESSION_WORDS],
    bytes: [Cell<u32>; 2 * BYTES as usize],
    rx: Held<Rx>,
    tx: Held<Tx>,
    connection: Pipe,
    bell: Pipe,
    server: Pipe,
    /// The client is done: the server's loop ends at its next ring.
    quit: AtomicBool,
}

/// The client's doorbells, which never hang up and take no deadline.
impl Bells for Session {
    type Until = ();

    fn drain(&self, bell: Doorbell) -> Took {
        if self.pipe(bell).drain() { Took::Rung } else { Took::Nothing }
    }

    fn wait(&self, bell: Doorbell, _: Option<()>) -> Waited {
        self.pipe(bell).wait();
        Waited::Rung
    }

    fn ring(&self) {
        self.server.ring();
    }
}

impl Session {
    fn new() -> (Rc<Self>, Server<BYTES, BYTES, SESSION_WORDS>) {
        let page: [Shared; SESSION_WORDS] = core::array::from_fn(|_| Shared(AtomicU32::new(0)));
        let (rx, tx) = session::client(&page, SESSION);
        let server = Server::new(&page, SESSION);
        let s = Session {
            page,
            bytes: core::array::from_fn(|_| Cell::new(0)),
            rx: Held(Mutex::new(rx)),
            tx: Held(Mutex::new(tx)),
            connection: Pipe::new(),
            bell: Pipe::new(),
            server: Pipe::new(),
            quit: AtomicBool::new(false),
        };
        (Rc::new(s), server)
    }

    fn pipe(&self, bell: Doorbell) -> &Pipe {
        match bell {
            Doorbell::Connection => &self.connection,
            Doorbell::Bell => &self.bell,
        }
    }

    /// The server's rings.
    fn rings(&self, rings: Rings) {
        if rings.connection {
            self.connection.ring();
        }
        if rings.bell {
            self.bell.ring();
        }
    }

    /// The client reads one byte, waiting or not.
    fn read(&self, mode: Mode<()>) -> Result<u32, Refused> {
        session::read(&self.page, &self.rx, self, mode, 1, |span, _| {
            for at in span.offset..span.offset + span.len {
                self.bytes[at].get();
            }
        })
    }

    /// The client writes one byte, waiting or not.
    fn write(&self, mode: Mode<()>) -> Result<u32, Refused> {
        session::write(&self.page, &self.tx, self, mode, 1, |span, _| {
            for at in span.offset..span.offset + span.len {
                self.bytes[at].set(0x5A);
            }
        })
    }

    /// The client fills its send ring, before the server's thread runs.
    fn fill(&self) {
        for _ in 0..BYTES {
            assert_eq!(self.write(Mode::Nonblocking), Ok(1));
        }
    }

    /// The server publishes `n` bytes into an empty receive ring, which it
    /// copies into first.
    fn publish(&self, server: &mut Server<BYTES, BYTES, SESSION_WORDS>, n: u32) {
        for at in 0..n as usize {
            self.bytes[at].set(0xA5);
        }
        self.rings(server.publish(&self.page, n).expect("the client keeps the protocol"));
    }

    /// The server takes back `n` bytes of the send ring.
    fn release(&self, server: &mut Server<BYTES, BYTES, SESSION_WORDS>, n: u32) {
        assert_eq!(server.sent(&self.page), Ok(BYTES), "the client filled its send ring");
        self.rings(server.release(&self.page, n).expect("the client keeps the protocol"));
    }

    /// The server's loop: each ring of the client's answered by
    /// [`Server::rung`], until the client is done.
    fn serve(&self, mut server: Server<BYTES, BYTES, SESSION_WORDS>) {
        loop {
            self.server.wait();
            self.rings(server.rung(&self.page).expect("the client keeps the protocol"));
            if self.quit.load(Ordering::Acquire) {
                return;
            }
        }
    }

    /// The client is done, and says so to the server's loop.
    fn done(&self) {
        self.quit.store(true, Ordering::Release);
        self.server.ring();
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
        let (s, mut server) = Session::new();
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
        let (s, mut server) = Session::new();
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
                s.serve(server);
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
        loop {
            let polled = if left == Left::Room {
                session::watch::<_, _, _, Tx, SESSION_WORDS>(&s.page, &s.tx, &*s)
            } else {
                session::watch::<_, _, _, Rx, SESSION_WORDS>(&s.page, &s.rx, &*s)
            };
            let Polled::Watching(watching) = polled.expect("the server keeps the protocol") else { break };
            let way = if left == Left::Room { Way::Send } else { Way::Receive };
            s.pipe(session::doorbell(way)).watch();
            let ready = if left == Left::Room {
                session::unwatch::<_, _, _, Tx, SESSION_WORDS>(&s.page, &s.tx, &*s, watching)
            } else {
                session::unwatch::<_, _, _, Rx, SESSION_WORDS>(&s.page, &s.rx, &*s, watching)
            };
            if ready.expect("the server keeps the protocol") {
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
        let (s, mut server) = Session::new();
        if ready == Ready::Reset || (ready == Ready::Room && racing) {
            s.fill();
        }
        let make_ready = move |s: &Session, server: &mut Server<BYTES, BYTES, SESSION_WORDS>| match ready {
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
                s.serve(server);
            })
        };
        let send = matches!(ready, Ready::Room | Ready::Reset);
        let registered = if send {
            session::register(&s.page, None::<&Held<Rx>>, Some(&s.tx), &*s)
        } else {
            session::register(&s.page, Some(&s.rx), None::<&Held<Tx>>, &*s)
        };
        registered.expect("the server keeps the protocol");
        let way = if send { Way::Send } else { Way::Receive };
        loop {
            s.pipe(session::doorbell(way)).watch();
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
