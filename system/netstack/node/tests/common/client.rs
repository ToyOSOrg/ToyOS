//! A client's end of a stream: `toyos_transport::session`'s own client sequences over a region
//! on the heap, laid out as `toyos::net::stream` lays one out, and the session the node is lent.
//! No call waits: a doorbell is a count, the client's ring of netstack a flag the test reads to
//! say which streams a pass is owed.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};

use toyos::net::stream::{Words, LAYOUT, NODELAY, RECEIVE_BYTES, RECEIVE_RING, REGION_BYTES, SEND_BYTES, SEND_RING};
use toyos_net_node::Session;
use toyos_net_tcp::Lent;
use toyos_transport::session::{self, Bells, Doorbell, Lock, Mode, Receiving, Refused, Rings, Sending, Took, Waited};

/// The region and its doorbells, which the client and the node share.
pub struct Region {
    words: Box<Words>,
    bytes: RefCell<Vec<u8>>,
    /// netstack's rings of the client's connection and bell, not yet drained.
    connection: Cell<u32>,
    bell: Cell<u32>,
    /// Every ring of netstack's, of either doorbell.
    rung: Cell<u32>,
    /// The client rang netstack since the test last asked.
    rang: Cell<bool>,
    /// The node lets go of the session.
    dropped: Cell<bool>,
}

struct Held<T>(RefCell<T>);

impl<T> Lock<T> for Held<T> {
    fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        f(&mut self.0.borrow_mut())
    }
}

impl Bells for Region {
    type Until = ();

    fn drain(&self, bell: Doorbell) -> Took {
        let count = match bell {
            Doorbell::Connection => &self.connection,
            Doorbell::Bell => &self.bell,
        };
        if count.replace(0) > 0 { Took::Rung } else { Took::Nothing }
    }

    fn wait(&self, _: Doorbell, _: Option<()>) -> Waited {
        unreachable!("a test's client never waits")
    }

    fn ring(&self) {
        self.rang.set(true);
    }
}

pub struct Client {
    pub region: Rc<Region>,
    rx: Held<Receiving<RECEIVE_BYTES, { toyos::net::stream::WORDS }>>,
    tx: Held<Sending<SEND_BYTES, { toyos::net::stream::WORDS }>>,
}

/// One ring of the region, as [tcp]'s storage.
#[derive(Debug)]
struct Ring {
    region: Rc<Region>,
    base: usize,
    len: usize,
}

impl std::fmt::Debug for Region {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Region")
    }
}

impl Lent for Ring {
    fn len(&self) -> usize {
        self.len
    }

    fn put(&mut self, at: usize, data: &[u8]) {
        assert!(at + data.len() <= self.len, "a put past the ring");
        self.region.bytes.borrow_mut()[self.base + at..][..data.len()].copy_from_slice(data);
    }

    fn take(&self, at: usize, out: &mut [u8]) {
        assert!(at + out.len() <= self.len, "a take past the ring");
        out.copy_from_slice(&self.region.bytes.borrow()[self.base + at..][..out.len()]);
    }
}

/// What the node is lent.
struct Lend(Rc<Region>);

impl Session for Lend {
    fn words(&self) -> &Words {
        &self.0.words
    }

    fn receive(&self) -> Box<dyn Lent> {
        Box::new(Ring { region: self.0.clone(), base: RECEIVE_RING, len: RECEIVE_BYTES as usize })
    }

    fn send(&self) -> Box<dyn Lent> {
        Box::new(Ring { region: self.0.clone(), base: SEND_RING, len: SEND_BYTES as usize })
    }

    fn ring(&self, rings: Rings) {
        let region = &self.0;
        for (rung, count) in [(rings.connection, &region.connection), (rings.bell, &region.bell)] {
            if rung {
                count.set(count.get() + 1);
                region.rung.set(region.rung.get() + 1);
            }
        }
    }
}

impl Drop for Lend {
    fn drop(&mut self) {
        self.0.dropped.set(true);
    }
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Client")
    }
}

impl Client {
    /// A client with a fresh region, and the session a connect or an accept lends the node.
    pub fn new() -> (Rc<Client>, Box<dyn Session>) {
        let region = Rc::new(Region {
            words: Box::new(std::array::from_fn(|_| AtomicU32::new(0))),
            bytes: RefCell::new(vec![0; REGION_BYTES]),
            connection: Cell::new(0),
            bell: Cell::new(0),
            rung: Cell::new(0),
            rang: Cell::new(false),
            dropped: Cell::new(false),
        });
        let (rx, tx) = session::client(&region.words, LAYOUT);
        let client = Client { region: region.clone(), rx: Held(RefCell::new(rx)), tx: Held(RefCell::new(tx)) };
        (Rc::new(client), Box::new(Lend(region)))
    }

    /// A client that set Nagle's algorithm off before it connected.
    pub fn nodelay() -> (Rc<Client>, Box<dyn Session>) {
        let (client, session) = Self::new();
        client.region.words[NODELAY].store(1, Ordering::Relaxed);
        (client, session)
    }

    /// A read of at most `max` bytes that does not wait.
    pub fn read(&self, max: usize) -> Result<Vec<u8>, Refused> {
        let mut out = vec![0; max];
        let region = &self.region;
        let n = session::read(&region.words, &self.rx, &**region, Mode::Nonblocking, u32::try_from(max).unwrap(), |span, at| {
            out[at..at + span.len].copy_from_slice(&region.bytes.borrow()[span.offset..][..span.len]);
        })?;
        out.truncate(n as usize);
        Ok(out)
    }

    /// Everything readable now, and how the reading stopped: `WouldBlock`, or the end.
    pub fn read_all(&self) -> (Vec<u8>, Result<(), Refused>) {
        let mut all = Vec::new();
        loop {
            match self.read(1 << 20) {
                Ok(bytes) if bytes.is_empty() => return (all, Ok(())),
                Ok(bytes) => all.extend(bytes),
                Err(refused) => return (all, Err(refused)),
            }
        }
    }

    /// A write that does not wait: how much of `data` the send ring took.
    pub fn write(&self, data: &[u8]) -> Result<usize, Refused> {
        let region = &self.region;
        let n = session::write(&region.words, &self.tx, &**region, Mode::Nonblocking, u32::try_from(data.len()).unwrap(), |span, at| {
            region.bytes.borrow_mut()[span.offset..][..span.len].copy_from_slice(&data[at..at + span.len]);
        })?;
        Ok(n as usize)
    }

    pub fn shutdown(&self) {
        session::shutdown(&self.region.words, &self.tx, &*self.region);
    }

    /// Nagle's algorithm off or on, and netstack rung to read it.
    pub fn set_nodelay(&self, nodelay: bool) {
        self.region.words[NODELAY].store(u32::from(nodelay), Ordering::Relaxed);
        self.region.ring();
    }

    /// The source registered for both ways.
    pub fn register(&self) {
        session::register(&self.region.words, Some(&self.rx), Some(&self.tx), &*self.region).expect("netstack keeps the protocol");
    }

    /// Whether the client rang netstack since the last call.
    pub fn rang(&self) -> bool {
        self.region.rang.replace(false)
    }

    /// netstack's rings of either doorbell so far.
    pub fn rung(&self) -> u32 {
        self.region.rung.get()
    }

    /// netstack's rings of the bell, which a writer waits on, not yet drained.
    pub fn bell(&self) -> u32 {
        self.region.bell.get()
    }

    /// The node let go of the session.
    pub fn dropped(&self) -> bool {
        self.region.dropped.get()
    }

    /// Rings netstack as the client's own calls do, for a word written past them.
    pub fn region_ring(&self) {
        self.region.ring();
    }

    /// The region's word `at`, which a hostile client may write.
    pub fn word(&self, at: usize) -> &AtomicU32 {
        &self.region.words[at]
    }
}
