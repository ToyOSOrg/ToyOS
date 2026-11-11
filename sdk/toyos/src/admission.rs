//! How a server admits its clients: the one table every server keeps the
//! connections it has accepted in until each has sent a whole request.
//!
//! **A request comes only from an admitted client.** A connection enters with
//! [`Admission::admit`] and leaves [`Admission::next`] as a [`Client`] holding
//! its first request, or as the reason it was let go; a [`Client`] is the only
//! thing a request is read from, and it has no call that waits on its peer: a
//! request is taken whole or not at all ([`Client::next_request`]) and a reply is one
//! send that is refused when the peer has no room ([`Client::try_send`]).
//!
//! **What one client can do to another**: nothing past its share. A client is
//! the key its server reads off the connection's badge, which whoever minted
//! its connector wrote and no holder can change, and one key holds at most
//! [`Policy::share`] of the table's [`Policy::slots`]; its next connection is
//! refused by name ([`Refused`]) while another key's is taken. A client that
//! says nothing costs its own slot for [`Policy::limit`], one that hangs up
//! is noticed by the same read that would have taken its request, and one
//! that does not read its replies is refused the reply and dropped by its
//! server. How many keys a program can hold is its minter's to bound.
//!
//! **Every wait here is the server's own.** The table waits on nothing: the
//! server's wait wakes for a connection that became readable
//! ([`Admission::watch`], [`Admission::woke`]) and no later than the oldest
//! entry's limit ([`Admission::left`]). A connection is read before it is let
//! go on its limit, so a request that is whole is served however late the pass
//! that finds it. A full table takes no more ([`Admission::has_room`]): the
//! server stops watching its acceptor, the port's queue holds the next
//! connection, and the kernel refuses the one past that queue.

use alloc::vec::Vec;
use core::fmt;
use core::ops::{Add, Sub};
use core::time::Duration;

use toyos_abi::syscall::{SyscallError, MAX_BADGE};

use crate::ipc::{self, Batch, Connection, FrameRx, IpcPayload, ReceivedHandle, RxStep, TrySendError};
use crate::port::Acceptor;
use crate::{AsHandle, BorrowedHandle, OwnedHandle};

/// How long an accepted connection may take to send its first whole request.
///
/// The shorter of the two published defaults for a request's header: Apache
/// httpd's `RequestReadTimeout header=20-40`
/// (<https://httpd.apache.org/docs/2.4/mod/mod_reqtimeout.html#requestreadtimeout>);
/// nginx's `client_header_timeout` is 60 s
/// (<https://nginx.org/en/docs/http/ngx_http_core_module.html#client_header_timeout>).
/// It bounds the client that never sends one, and it is long enough that a
/// client on a machine whose CPUs are busy elsewhere is not that client.
pub const LIMIT: Duration = Duration::from_secs(20);

/// What a server lets its clients take of one table.
#[derive(Clone, Copy, Debug)]
pub struct Policy {
    /// Connections waiting on their first request at once.
    pub slots: usize,
    /// How many of them one key may hold.
    pub share: usize,
    /// How long one may wait.
    pub limit: Duration,
}

impl Policy {
    /// `slots` connections, a quarter of them one key's, each for [`LIMIT`].
    pub const fn quartered(slots: usize) -> Self {
        Self { slots, share: slots / 4, limit: LIMIT }
    }

    /// `slots` connections any key may hold all of, each for [`LIMIT`]: for a
    /// port whose connections carry no badge that names their holder, where
    /// a share would be every holder's at once.
    pub const fn unshared(slots: usize) -> Self {
        Self { slots, share: slots, limit: LIMIT }
    }
}

/// The badge a connection was stamped with, as a key: who its minter says
/// holds the connector. Compared whole and never decoded here.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Badge {
    bytes: [u8; MAX_BADGE],
    len: usize,
}

impl Badge {
    /// The badge of `conn`, which must have been accepted from `acceptor`;
    /// `None` for a connection through a connector nobody badged, which is
    /// one key for all of them.
    pub fn of(acceptor: &Acceptor, conn: &Connection) -> Option<Self> {
        let mut bytes = [0; MAX_BADGE];
        let len = match acceptor.badge(conn, &mut bytes) {
            Ok(badge) => badge.len(),
            Err(SyscallError::NotFound) => return None,
            Err(why) => panic!("a connection's own acceptor would not say its badge: {why:?}"),
        };
        Some(Self { bytes, len })
    }

    /// `bytes` as a badge, for one that reached its server in a frame;
    /// `None` for more than a badge carries.
    pub fn new(bytes: &[u8]) -> Option<Self> {
        let mut badge = Self { bytes: [0; MAX_BADGE], len: bytes.len() };
        badge.bytes.get_mut(..bytes.len())?.copy_from_slice(bytes);
        Some(badge)
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

impl fmt::Debug for Badge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match core::str::from_utf8(self.bytes()) {
            Ok(text) if text.chars().all(|c| c.is_ascii_graphic()) => write!(f, "{text}"),
            _ => write!(f, "{:02x?}", self.bytes()),
        }
    }
}

/// A connection as the table reads and answers it: [`Connection`], and a
/// test's client that misbehaves.
pub trait Line {
    /// The next whole frame if one is queued, never waiting.
    fn pump<const MAX: usize>(&self, rx: &mut FrameRx<MAX>) -> RxStep;

    /// One frame sent whole or refused, never waiting; `handles` are consumed
    /// either way.
    fn try_frame(
        &self,
        msg_type: u32,
        payload: &[u8],
        handles: impl IntoIterator<Item = OwnedHandle>,
    ) -> Result<(), TrySendError>;
}

impl Line for Connection {
    fn pump<const MAX: usize>(&self, rx: &mut FrameRx<MAX>) -> RxStep {
        rx.pump(self)
    }

    fn try_frame(
        &self,
        msg_type: u32,
        payload: &[u8],
        handles: impl IntoIterator<Item = OwnedHandle>,
    ) -> Result<(), TrySendError> {
        self.try_send_bytes_with(msg_type, payload, handles)
    }
}

/// An admitted client: its connection, the key and context it was admitted
/// under, and the request in hand.
pub struct Client<K, C, const MAX: usize, L = Connection> {
    conn: L,
    rx: FrameRx<MAX>,
    key: K,
    /// What its server said of it at [`Admission::admit`].
    pub context: C,
    msg_type: u32,
    payload_len: usize,
}

impl<K, C, const MAX: usize, L: Line> Client<K, C, MAX, L> {
    /// Whose share it spends.
    pub fn key(&self) -> &K {
        &self.key
    }

    /// The type of the request in hand.
    pub fn msg_type(&self) -> u32 {
        self.msg_type
    }

    /// The payload of the request in hand.
    pub fn payload(&self) -> &[u8] {
        self.rx.payload(self.payload_len)
    }

    /// The handles the request in hand carried, now the caller's.
    pub fn take_batch(&mut self) -> Batch {
        self.rx.take_batch()
    }

    /// The `N` handles the request in hand carried, each with the kind the
    /// kernel says it is; `None`, with them closed, when it carried another
    /// number.
    pub fn take_handles<const N: usize>(&mut self) -> Option<[ReceivedHandle; N]> {
        self.rx.take_handles()
    }

    /// Its next request if one is queued, never waiting, and at most one, so
    /// a client that always has another cannot hold the others off. A
    /// [`RxStep::Frame`] is in hand from here.
    pub fn next_request(&mut self) -> RxStep {
        let step = self.conn.pump(&mut self.rx);
        if let RxStep::Frame { msg_type, payload_len, .. } = step {
            (self.msg_type, self.payload_len) = (msg_type, payload_len);
        }
        step
    }

    /// A reply, sent whole now or refused: a client that will not take one
    /// has not read the last, and is its server's to drop.
    pub fn try_send<T: IpcPayload>(&self, msg_type: u32, payload: &T) -> Result<(), TrySendError> {
        self.conn.try_frame(msg_type, ipc::as_bytes(payload), None)
    }

    /// [`try_send`](Self::try_send) for a byte payload.
    pub fn try_send_bytes(&self, msg_type: u32, data: &[u8]) -> Result<(), TrySendError> {
        self.conn.try_frame(msg_type, data, None)
    }

    /// [`try_send_bytes`](Self::try_send_bytes) with handles, each consumed, sent or not.
    pub fn try_send_bytes_with(
        &self,
        msg_type: u32,
        data: &[u8],
        handles: impl IntoIterator<Item = OwnedHandle>,
    ) -> Result<(), TrySendError> {
        self.conn.try_frame(msg_type, data, handles)
    }

    /// The connection, for a server that hands it on or keeps it past this
    /// request; whatever the frame carried and nobody took is closed.
    pub fn into_connection(self) -> L {
        self.conn
    }

    /// [`into_connection`](Self::into_connection), with its key and context.
    pub fn into_parts(self) -> (L, K, C) {
        (self.conn, self.key, self.context)
    }
}

impl<K, C, const MAX: usize> AsHandle for Client<K, C, MAX> {
    fn as_handle(&self) -> BorrowedHandle<'_> {
        self.conn.as_handle()
    }
}

/// Why a connection was not taken into the table, and the connection, which
/// closes with this unless its server answers it first.
pub struct Refused<K, L = Connection> {
    pub conn: L,
    pub key: K,
    pub why: Full,
}

/// What a refused connection found full.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Full {
    /// Its key already holds this many, its share.
    Share(usize),
    /// The table holds this many, every slot.
    Table(usize),
}

impl fmt::Display for Full {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Share(share) => write!(f, "its holder's share of the table, {share}, already waits on a first request"),
            Self::Table(slots) => write!(f, "every one of the table's {slots} slots already waits on a first request"),
        }
    }
}

/// Why a connection left the table with no request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Left {
    /// It hung up.
    HungUp,
    /// It sent a frame its server's protocol cannot describe.
    Malformed,
    /// It sent no whole request within the limit.
    Silent(Duration),
}

impl fmt::Display for Left {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HungUp => write!(f, "it hung up before its first request"),
            Self::Malformed => write!(f, "it sent a frame this protocol cannot describe"),
            Self::Silent(limit) => write!(f, "it sent no whole request within {limit:?}"),
        }
    }
}

/// What a pass found of one connection.
pub enum Event<K, C, const MAX: usize, L = Connection> {
    /// Its first request is whole: it is a client.
    Admitted(Client<K, C, MAX, L>),
    /// It is let go unanswered; the connection closes with this.
    Left { conn: L, key: K, context: C, why: Left },
}

struct Waiting<K, C, I, const MAX: usize, L> {
    conn: L,
    rx: FrameRx<MAX>,
    key: K,
    context: C,
    deadline: I,
    /// Its number among the table's slots, which its watch's token carries.
    slot: usize,
    /// A watch on it waits in its server's poller.
    armed: bool,
    /// Its watch fired since it was last read.
    woken: bool,
}

/// The connections a server has accepted that have not yet sent a whole
/// request, keyed by `K` and each carrying its server's `C`. `I` is the
/// server's clock, an `Instant`; `MAX` the widest first request.
pub struct Admission<K, C, I, const MAX: usize, L = Connection> {
    policy: Policy,
    waiting: Vec<Waiting<K, C, I, MAX, L>>,
    /// Where [`Self::next`] reads on from within one pass.
    cursor: usize,
}

impl<K, C, I, const MAX: usize, L> Admission<K, C, I, MAX, L>
where
    K: PartialEq,
    I: Copy + PartialOrd + Add<Duration, Output = I> + Sub<I, Output = Duration>,
    L: Line,
{
    pub fn new(policy: Policy) -> Self {
        assert!(policy.slots > 0 && policy.share > 0, "a table nobody can enter");
        Self { policy, waiting: Vec::new(), cursor: 0 }
    }

    /// Whether another connection may wait. While it may not, the server
    /// leaves its acceptor unwatched and the port's queue holds the next.
    pub fn has_room(&self) -> bool {
        self.waiting.len() < self.policy.slots
    }

    /// Take `conn`, just accepted, to wait for its first request until
    /// `now + limit`, under `key`'s share; or refuse it.
    pub fn admit(&mut self, conn: L, key: K, context: C, now: I) -> Result<(), Refused<K, L>> {
        let Policy { slots, share, limit } = self.policy;
        let why = if self.waiting.len() >= slots {
            Some(Full::Table(slots))
        } else if self.waiting.iter().filter(|w| w.key == key).count() >= share {
            Some(Full::Share(share))
        } else {
            None
        };
        if let Some(why) = why {
            return Err(Refused { conn, key, why });
        }
        let slot = (0..slots).find(|n| self.waiting.iter().all(|w| w.slot != *n)).expect("fewer wait than there are slots");
        // Read at the next pass whether or not anything wakes for it: its
        // request may have been sent before it was accepted.
        let rx = FrameRx::new();
        self.waiting.push(Waiting { conn, rx, key, context, deadline: now + limit, slot, armed: false, woken: true });
        Ok(())
    }

    /// Hand `watch` each waiting connection no watch waits on, with its slot,
    /// below [`Policy::slots`], for the token: a watch stays in a poller
    /// until it fires, and [`Self::woke`] says it did.
    pub fn watch(&mut self, mut watch: impl FnMut(&L, u64)) {
        for w in self.waiting.iter_mut().filter(|w| !w.armed) {
            watch(&w.conn, w.slot as u64);
            w.armed = true;
        }
    }

    /// The watch on `slot` fired.
    pub fn woke(&mut self, slot: u64) {
        if let Some(w) = self.waiting.iter_mut().find(|w| w.slot as u64 == slot) {
            (w.armed, w.woken) = (false, true);
        }
    }

    /// How long the server's wait may last before the oldest entry's limit
    /// ends: what is left of it, never a fresh limit. `None` while none waits.
    pub fn left(&self, now: I) -> Option<Duration> {
        let left = |w: &Waiting<K, C, I, MAX, L>| if w.deadline > now { w.deadline - now } else { Duration::ZERO };
        self.waiting.iter().map(left).min()
    }

    /// The next connection this pass settles, until `None` ends the pass.
    /// Each one woken, or past its limit at `now`, is read once: a whole
    /// request admits it, even past its limit, and only one that has sent
    /// none by then is let go for it.
    pub fn next(&mut self, now: I) -> Option<Event<K, C, MAX, L>> {
        while let Some(w) = self.waiting.get_mut(self.cursor) {
            let due = now >= w.deadline;
            if !w.woken && !due {
                self.cursor += 1;
                continue;
            }
            w.woken = false;
            let why = match w.conn.pump(&mut w.rx) {
                RxStep::Frame { msg_type, payload_len, .. } => {
                    let Waiting { conn, rx, key, context, .. } = self.waiting.remove(self.cursor);
                    return Some(Event::Admitted(Client { conn, rx, key, context, msg_type, payload_len }));
                }
                RxStep::Idle if !due => {
                    self.cursor += 1;
                    continue;
                }
                RxStep::Idle => Left::Silent(self.policy.limit),
                RxStep::Eof => Left::HungUp,
                RxStep::Malformed => Left::Malformed,
            };
            let Waiting { conn, key, context, .. } = self.waiting.remove(self.cursor);
            return Some(Event::Left { conn, key, context, why });
        }
        self.cursor = 0;
        None
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    use super::*;

    const LIMIT: Duration = Duration::from_secs(20);
    const POLICY: Policy = Policy { slots: 8, share: 2, limit: LIMIT };
    const REQUEST: u32 = 7;

    /// What a test's client does to its end.
    enum Does {
        Sends(u32, &'static [u8]),
        SendsWhatNoProtocolDescribes,
        HangsUp,
    }

    /// A client's end as its server holds it: what the client did, in order,
    /// and whether it reads what it is sent.
    #[derive(Clone, Default)]
    struct Peer {
        did: Rc<RefCell<VecDeque<Does>>>,
        deaf: bool,
        got: Rc<RefCell<Vec<u32>>>,
        reads: Rc<RefCell<usize>>,
    }

    impl Peer {
        fn does(&self, does: Does) {
            self.did.borrow_mut().push_back(does);
        }
    }

    impl Line for Peer {
        fn pump<const MAX: usize>(&self, rx: &mut FrameRx<MAX>) -> RxStep {
            *self.reads.borrow_mut() += 1;
            match self.did.borrow_mut().pop_front() {
                None => RxStep::Idle,
                Some(Does::Sends(msg_type, payload)) => rx.load(msg_type, payload),
                Some(Does::SendsWhatNoProtocolDescribes) => RxStep::Malformed,
                Some(Does::HangsUp) => RxStep::Eof,
            }
        }

        fn try_frame(
            &self,
            msg_type: u32,
            _: &[u8],
            _: impl IntoIterator<Item = OwnedHandle>,
        ) -> Result<(), TrySendError> {
            if self.deaf {
                return Err(TrySendError::Full);
            }
            self.got.borrow_mut().push(msg_type);
            Ok(())
        }
    }

    /// A table whose clock is the time since the test began.
    type Table = Admission<&'static str, u32, Duration, 16, Peer>;

    fn at(secs: u64) -> Duration {
        Duration::from_secs(secs)
    }

    /// A connection of `key`'s taken at `now`, and the client's end of it.
    fn connect(table: &mut Table, key: &'static str, now: Duration) -> Peer {
        let peer = Peer::default();
        assert!(table.admit(peer.clone(), key, 0, now).is_ok(), "{key}'s connection was refused");
        peer
    }

    /// Every event of one pass at `now`, as `(key, what happened)`.
    fn pass(table: &mut Table, now: Duration) -> Vec<(&'static str, Result<u32, Left>)> {
        let mut seen = Vec::new();
        while let Some(event) = table.next(now) {
            seen.push(match event {
                Event::Admitted(client) => (*client.key(), Ok(client.msg_type())),
                Event::Left { key, why, .. } => (key, Err(why)),
            });
        }
        seen
    }

    #[test]
    fn a_client_that_never_sends_its_request_is_let_go_at_its_limit_and_not_before() {
        let mut table = Table::new(POLICY);
        connect(&mut table, "silent", at(0));
        assert_eq!(pass(&mut table, at(0)), []);
        assert_eq!(pass(&mut table, LIMIT.saturating_sub(Duration::from_nanos(1))), []);
        assert_eq!(pass(&mut table, LIMIT), [("silent", Err(Left::Silent(LIMIT)))]);
        assert_eq!(table.left(LIMIT), None, "still waiting after it was let go");
    }

    /// The wait is what is left of the oldest entry's limit, so no entry
    /// waits a limit and then another.
    #[test]
    fn the_wait_is_what_is_left_of_the_oldest_limit() {
        let mut table = Table::new(POLICY);
        assert_eq!(table.left(at(0)), None);
        connect(&mut table, "old", at(0));
        connect(&mut table, "new", at(5));
        assert_eq!(table.left(at(7)), Some(LIMIT.saturating_sub(at(7))));
        assert_eq!(table.left(LIMIT + at(1)), Some(Duration::ZERO));
        assert_eq!(pass(&mut table, LIMIT), [("old", Err(Left::Silent(LIMIT)))]);
        assert_eq!(table.left(LIMIT), Some(at(5)));
    }

    /// The pass that would let a connection go on its limit reads it first:
    /// a request that is whole there is served, though nothing woke for it
    /// and the pass came late.
    #[test]
    fn a_request_that_is_whole_is_served_on_a_pass_past_its_limit() {
        let mut table = Table::new(POLICY);
        let late = connect(&mut table, "late", at(0));
        assert_eq!(pass(&mut table, at(0)), []);
        late.does(Does::Sends(REQUEST, b"whole"));
        assert_eq!(pass(&mut table, LIMIT + at(30)), [("late", Ok(REQUEST))]);
    }

    /// A request sent before its connection was accepted wakes nothing after
    /// it: the first pass reads it.
    #[test]
    fn a_request_sent_before_the_accept_is_read_at_the_first_pass() {
        let mut table = Table::new(POLICY);
        let peer = Peer::default();
        peer.does(Does::Sends(REQUEST, b""));
        assert!(table.admit(peer, "early", 0, at(0)).is_ok());
        assert_eq!(pass(&mut table, at(0)), [("early", Ok(REQUEST))]);
    }

    /// A connection is read when its watch fired or its limit passed, and at
    /// no other pass: a quiet table costs its server no read.
    #[test]
    fn a_connection_is_read_only_when_it_woke_or_its_limit_passed() {
        let mut table = Table::new(POLICY);
        let peer = connect(&mut table, "quiet", at(0));
        assert_eq!(pass(&mut table, at(0)), []);
        let reads = *peer.reads.borrow();
        assert_eq!(pass(&mut table, at(1)), []);
        assert_eq!(*peer.reads.borrow(), reads, "read with nothing to say it had spoken");

        let mut slots = Vec::new();
        table.watch(|_, slot| slots.push(slot));
        assert_eq!(slots.len(), 1);
        table.watch(|_, _| panic!("watched twice with one watch waiting"));
        peer.does(Does::Sends(REQUEST, b""));
        table.woke(slots[0]);
        assert_eq!(pass(&mut table, at(1)), [("quiet", Ok(REQUEST))]);
    }

    #[test]
    fn a_frame_no_protocol_describes_and_a_hang_up_each_free_their_slot() {
        let mut table = Table::new(Policy { slots: 2, share: 2, limit: LIMIT });
        let half = connect(&mut table, "half", at(0));
        let closes = connect(&mut table, "closes", at(0));
        assert!(!table.has_room());
        assert_eq!(pass(&mut table, at(0)), []);
        half.does(Does::SendsWhatNoProtocolDescribes);
        closes.does(Does::HangsUp);
        let mut slots = Vec::new();
        table.watch(|_, slot| slots.push(slot));
        slots.into_iter().for_each(|slot| table.woke(slot));
        assert_eq!(pass(&mut table, at(1)), [("half", Err(Left::Malformed)), ("closes", Err(Left::HungUp))]);
        assert_eq!(table.left(at(1)), None, "still waiting after it left");
    }

    /// One key holds its share and no more, and what it holds refuses no
    /// other key; a slot it gives back is its own to take again.
    #[test]
    fn a_key_at_its_share_is_refused_while_another_key_is_taken() {
        let mut table = Table::new(POLICY);
        let first = connect(&mut table, "greedy", at(0));
        connect(&mut table, "greedy", at(0));
        let Err(refused) = table.admit(Peer::default(), "greedy", 0, at(0)) else { panic!("taken past its share") };
        assert_eq!((refused.key, refused.why), ("greedy", Full::Share(2)));
        connect(&mut table, "modest", at(0));

        first.does(Does::Sends(REQUEST, b""));
        assert_eq!(pass(&mut table, LIMIT.saturating_sub(at(1))), [("greedy", Ok(REQUEST))]);
        connect(&mut table, "greedy", LIMIT.saturating_sub(at(1)));
    }

    /// A full table refuses whoever asks, and says it has no room first.
    #[test]
    fn a_full_table_has_no_room_and_refuses_the_next() {
        let mut table = Table::new(Policy { slots: 3, share: 1, limit: LIMIT });
        for key in ["a", "b", "c"] {
            assert!(table.has_room());
            connect(&mut table, key, at(0));
        }
        assert!(!table.has_room());
        let Err(refused) = table.admit(Peer::default(), "d", 0, at(0)) else { panic!("taken past the table") };
        assert_eq!(refused.why, Full::Table(3));
    }

    /// Slots are numbered below the table's size and never shared by two
    /// waiting connections, so a slot is a token.
    #[test]
    fn two_waiting_connections_never_share_a_slot() {
        let mut table = Table::new(Policy { slots: 3, share: 3, limit: LIMIT });
        let a = connect(&mut table, "k", at(0));
        connect(&mut table, "k", at(0));
        a.does(Does::HangsUp);
        assert_eq!(pass(&mut table, at(0)), [("k", Err(Left::HungUp))]);
        connect(&mut table, "k", at(1));
        connect(&mut table, "k", at(1));
        let mut slots = Vec::new();
        table.watch(|_, slot| slots.push(slot));
        slots.sort_unstable();
        assert_eq!(slots, [0, 1, 2]);
    }

    /// A client that does not read is refused its reply in the one send, and
    /// the reply of the client beside it goes.
    #[test]
    fn a_client_that_reads_no_reply_is_refused_it_and_holds_nobody() {
        let mut table = Table::new(POLICY);
        let deaf = Peer { deaf: true, ..Peer::default() };
        deaf.does(Does::Sends(REQUEST, b""));
        assert!(table.admit(deaf, "deaf", 0, at(0)).is_ok());
        let hears = connect(&mut table, "hears", at(0));
        hears.does(Does::Sends(REQUEST, b""));
        let mut answered = Vec::new();
        while let Some(Event::Admitted(client)) = table.next(at(0)) {
            answered.push((*client.key(), client.try_send_bytes(9, &[]).is_ok()));
        }
        assert_eq!(answered, [("deaf", false), ("hears", true)]);
        assert_eq!(*hears.got.borrow(), [9]);
    }

    /// A client's later requests are taken one at a time, and each is the
    /// one in hand until the next.
    #[test]
    fn an_admitted_client_s_requests_come_one_at_a_time() {
        let mut table = Table::new(POLICY);
        let peer = connect(&mut table, "k", at(0));
        peer.does(Does::Sends(1, b"one"));
        peer.does(Does::Sends(2, b"second"));
        let Some(Event::Admitted(mut client)) = table.next(at(0)) else { panic!("not admitted") };
        assert_eq!((client.msg_type(), client.payload()), (1, &b"one"[..]));
        assert!(matches!(client.next_request(), RxStep::Frame { msg_type: 2, .. }));
        assert_eq!((client.msg_type(), client.payload()), (2, &b"second"[..]));
        assert!(matches!(client.next_request(), RxStep::Idle));
        assert_eq!(client.msg_type(), 2);
    }
}
