//! A holder of a stream: `toyos::transport::session`'s own client sequences over a region on the
//! heap, laid out as `toyos::net::stream` lays one out, and the session the node is lent. A
//! region may have several holders, each with doorbells of its own, which every ring of the
//! node's reaches. No call waits: a doorbell is a count, a holder's ring of netstack a flag the
//! test reads to say which streams a pass is owed.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};

use toyos::net::stream::{Words, LAYOUT, NODELAY, RECEIVE_BYTES, RECEIVE_INITIAL, RECEIVE_RING, REGION_BYTES, SEND_BYTES, SEND_INITIAL, SEND_RING};
use toyos_net_node::{Memory, Session};
use toyos::transport::session::{self, Bells, Doorbell, Holder, HungUp, Lock, Mode, Receiving, Refused, Rings, Sending, Took, Waited};

/// The region and its holders' doorbells, which the holders and the node share.
pub struct Region {
    words: Box<Words>,
    bytes: RefCell<Vec<u8>>,
    /// Each commit's range, the client's and the node's.
    committed: RefCell<Vec<(usize, usize)>>,
    /// Whether the client's and the node's commits make memory: one that makes none leaves
    /// every page the other's to commit, so a test shows the other's commits alone suffice.
    commits: Cell<(bool, bool)>,
    holders: RefCell<Vec<Rc<Doorbells>>>,
    /// A holder rang netstack since the test last asked.
    rang: Cell<bool>,
    /// The node lets go of the session.
    dropped: Cell<bool>,
}

/// One holder's doorbells: netstack's rings of each, not yet drained, and every ring of either.
#[derive(Default)]
struct Doorbells {
    receive: Cell<u32>,
    send: Cell<u32>,
    rung: Cell<u32>,
}

/// A holder's doorbells and its ring of netstack.
struct Ringing {
    region: Rc<Region>,
    bells: Rc<Doorbells>,
}

struct Held<T>(RefCell<T>);

impl<T> Lock<T> for Held<T> {
    fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        f(&mut self.0.borrow_mut())
    }
}

impl Bells for Ringing {
    type Until = ();

    fn drain(&self, bell: Doorbell) -> Took {
        let count = match bell {
            Doorbell::Receive => &self.bells.receive,
            Doorbell::Send => &self.bells.send,
        };
        if count.replace(0) > 0 { Took::Rung } else { Took::Nothing }
    }

    fn wait(&self, _: Doorbell, _: Option<()>) -> Waited {
        unreachable!("a test's holder never waits")
    }

    fn wait_gate(&self, _: Doorbell, _: Option<()>, _: u16) -> Waited {
        unreachable!("a test's client never waits")
    }

    fn ring(&self) -> Result<(), HungUp> {
        self.region.rang.set(true);
        Ok(())
    }
}

pub struct Client {
    pub region: Rc<Region>,
    ringing: Ringing,
    rx: Held<Receiving<RECEIVE_BYTES, { toyos::net::stream::WORDS }>>,
    tx: Held<Sending<SEND_BYTES, { toyos::net::stream::WORDS }>>,
}

impl std::fmt::Debug for Region {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Region")
    }
}

impl Memory for Region {
    fn put(&self, at: usize, data: &[u8]) {
        assert!(self.reaches(at, data.len()), "a put of {} bytes at {at} past what was committed", data.len());
        self.bytes.borrow_mut()[at..][..data.len()].copy_from_slice(data);
    }

    fn take(&self, at: usize, out: &mut [u8]) {
        assert!(self.reaches(at, out.len()), "a take of {} bytes at {at} past what was committed", out.len());
        out.copy_from_slice(&self.bytes.borrow()[at..][..out.len()]);
    }

    fn commit(&self, at: usize, len: usize) -> bool {
        if self.commits.get().1 {
            self.committed.borrow_mut().push((at, at + len));
        }
        true
    }
}

impl Region {
    /// The client's commit of `[at, at + len)`.
    fn client_commit(&self, at: usize, len: usize) -> bool {
        if self.commits.get().0 {
            self.committed.borrow_mut().push((at, at + len));
        }
        true
    }

    /// From here on the client's commits make no memory.
    pub fn client_commits_nothing(&self) {
        self.commits.set((false, self.commits.get().1));
    }

    /// From here on the node's commits make no memory.
    pub fn netstack_commits_nothing(&self) {
        self.commits.set((self.commits.get().0, false));
    }

    /// Whether `[at, at + len)` lies in what a commit made memory, as a fault would ask.
    fn reaches(&self, at: usize, len: usize) -> bool {
        self.committed.borrow().iter().any(|&(start, end)| start <= at && at + len <= end)
    }

    /// The most of the ring at `ring` committed from its start.
    pub fn committed(&self, ring: usize) -> usize {
        self.committed.borrow().iter().filter(|&&(start, _)| start == ring).map(|&(_, end)| end - ring).max().unwrap_or(0)
    }
}

/// What the node is lent.
struct Lend(Rc<Region>);

impl Session for Lend {
    fn words(&self) -> &Words {
        &self.0.words
    }

    fn memory(&self) -> Rc<dyn Memory> {
        self.0.clone()
    }

    fn ring(&self, rings: Rings) {
        for bells in self.0.holders.borrow().iter() {
            for (rung, count) in [(rings.receive, &bells.receive), (rings.send, &bells.send)] {
                if rung {
                    count.set(count.get() + 1);
                    bells.rung.set(bells.rung.get() + 1);
                }
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
    /// A client with a fresh region, its first holder, and the session a connect or an accept
    /// lends the node.
    pub fn new() -> (Rc<Client>, Box<dyn Session>) {
        let region = Rc::new(Region {
            words: Box::new(std::array::from_fn(|_| AtomicU32::new(0))),
            bytes: RefCell::new(vec![0; REGION_BYTES]),
            committed: RefCell::new(vec![(0, RECEIVE_RING + RECEIVE_INITIAL as usize), (SEND_RING, SEND_RING + SEND_INITIAL as usize)]),
            commits: Cell::new((true, true)),
            holders: RefCell::new(Vec::new()),
            rang: Cell::new(false),
            dropped: Cell::new(false),
        });
        let halves = session::client(&region.words, LAYOUT, SEND_INITIAL);
        (Self::holding(&region, halves), Box::new(Lend(region)))
    }

    /// Another holder of this client's region, numbered `number`, as an export makes one.
    pub fn holder(&self, number: u16) -> Rc<Client> {
        let halves = session::holder(LAYOUT, Holder::new(number).expect("a holder's number is not 0"));
        Self::holding(&self.region, halves)
    }

    fn holding(
        region: &Rc<Region>,
        (rx, tx): (Receiving<RECEIVE_BYTES, { toyos::net::stream::WORDS }>, Sending<SEND_BYTES, { toyos::net::stream::WORDS }>),
    ) -> Rc<Client> {
        let bells = Rc::new(Doorbells::default());
        region.holders.borrow_mut().push(bells.clone());
        let ringing = Ringing { region: region.clone(), bells };
        Rc::new(Client { region: region.clone(), ringing, rx: Held(RefCell::new(rx)), tx: Held(RefCell::new(tx)) })
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
        let reach = |cap: u32| region.client_commit(RECEIVE_RING, cap as usize);
        let n = session::read(&region.words, &self.rx, &self.ringing, Mode::Nonblocking, u32::try_from(max).unwrap(), reach, |span, at| {
            assert!(region.reaches(span.offset, span.len), "a read of {span:?} past what was committed");
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
        let reach = |cap: u32| region.client_commit(SEND_RING, cap as usize);
        let n = session::write(&region.words, &self.tx, &self.ringing, Mode::Nonblocking, u32::try_from(data.len()).unwrap(), reach, |span, at| {
            assert!(region.reaches(span.offset, span.len), "a write of {span:?} past what was committed");
            region.bytes.borrow_mut()[span.offset..][..span.len].copy_from_slice(&data[at..at + span.len]);
        })?;
        Ok(n as usize)
    }

    pub fn shutdown(&self) {
        session::shutdown(&self.region.words, &self.tx, &self.ringing);
    }

    /// Nagle's algorithm off or on, and netstack rung to read it.
    pub fn set_nodelay(&self, nodelay: bool) {
        self.region.words[NODELAY].store(u32::from(nodelay), Ordering::Relaxed);
        LAYOUT.ring(&self.region.words, &self.ringing);
    }

    /// The source registered for both ways.
    pub fn register(&self) {
        session::register(&self.region.words, Some(&self.rx), Some(&self.tx), &self.ringing);
    }

    /// Whether the client rang netstack since the last call.
    pub fn rang(&self) -> bool {
        self.region.rang.replace(false)
    }

    /// netstack's rings of either of this holder's doorbells so far.
    pub fn rung(&self) -> u32 {
        self.ringing.bells.rung.get()
    }

    /// netstack's rings of this holder's send bell, which a writer waits on, not yet drained.
    pub fn bell(&self) -> u32 {
        self.ringing.bells.send.get()
    }

    /// netstack's rings of this holder's receive bell, which a reader waits on, not yet
    /// drained.
    pub fn receive_bell(&self) -> u32 {
        self.ringing.bells.receive.get()
    }

    /// The node let go of the session.
    pub fn dropped(&self) -> bool {
        self.region.dropped.get()
    }

    /// Rings netstack as the client's own calls do, for a word written past them.
    pub fn region_ring(&self) {
        LAYOUT.ring(&self.region.words, &self.ringing);
    }

    /// The region's word `at`, which a hostile client may write.
    pub fn word(&self, at: usize) -> &AtomicU32 {
        &self.region.words[at]
    }
}
