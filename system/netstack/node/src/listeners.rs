//! Listeners: a passive open of [tcp]'s and its holders (`holders`), each told on its own bell
//! that a connection waits. The queues are [tcp]'s: handshakes in progress, and connections that
//! finished theirs and wait, oldest first. A connection that waits is nobody's stream yet, its
//! text held in [tcp]'s own storage; an accept makes the oldest one a stream on the region the
//! accept lends, and what it held moves there at its offsets, text past a hole included, the FIN
//! staying [tcp]'s state.
//!
//! **Every holder takes from the one queue.** A holder is told while a connection waits that
//! there is a place for, once until it asks again ([`Node::accept`]); the node tells whenever a
//! pass ends ([`Node::bridge`]), from what [tcp] holds and what `places` has left at that moment,
//! so no ring depends on the pass that saw a connection arrive. Two holders told of one
//! connection both ask; one takes it and the other is answered [`AcceptRefused::Nothing`].
//!
//! **A port is one listener's**, whatever address either listen named: [tcp] hands a SYN to
//! the listener that named its address before one at every address, so a second listen on a
//! held port would take the first's connections, and a listen carries no word for its program
//! by which the node could allow that to the first's holders alone.
//!
//! **A listener lives until its last holder leaves** ([`Node::listener_departed`]); each holder
//! past the first takes a place ([`Node::export_listener`]). [tcp] then resets every connection
//! that still waits, so each peer learns at once, and the port is free. Nothing the wire does
//! ends one: a listener whose address the machine loses stands, with its port and its place, and
//! is handed the next SYN once the address is the machine's again.
//!
//! **A stream starts with the options its connection has**, and those are its listener's as
//! they were when its SYN arrived: [tcp] hands them over then (LS-10), the node reads them
//! back at the accept and answers them with the stream, and an option set on the listener
//! afterwards reaches the connections that begin afterwards. A listener holds what its listen
//! named before any SYN can reach its port: the node takes no frame between the passive open
//! and the options. A host does the same with `TCP_NODELAY` set on a listening socket
//! (`tests/host.rs` asks the host the tests run on).

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use core::net::Ipv4Addr;

use toyos_net_shard::ListenError;
use toyos_net_tcp::{Endpoint, Error, Options};
use toyos_net_wire::{Instant, Port};

use crate::holders::{Bell, HolderId, Holders};
use crate::streams::{ExportRefused, Session, StreamId};
use crate::Node;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ListenerId(u64);

/// Why a listen made no listener.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListenRefused {
    /// The node holds all it has places for (`places`).
    Full,
    /// The address named is not this machine's to bind: what `Node::udp_bind` refuses by the
    /// same rule.
    NotLocal,
    /// The port is a listener's, whatever address either named, or no drawn port was free.
    InUse,
}

/// The answer to an accept that took a connection: its stream, established.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Accepted {
    pub id: StreamId,
    pub remote: Endpoint,
    pub local: Endpoint,
    /// Whether Nagle's algorithm is off for the stream as it is handed over: what
    /// [`Node::nodelay`] answers for as long as the node holds the stream, which may be no
    /// longer than this accept's own pass.
    pub nodelay: bool,
}

/// Why an accept took no connection. Whatever waits still waits, and no session was made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcceptRefused {
    /// The id names no listener, or the holder none of its.
    NoListener,
    /// The connection that waits found no region to become a stream on.
    NoRegion,
    /// The node holds all it has places for (`places`), whatever waits.
    Full,
    /// No connection waits.
    Nothing,
}

struct Listener {
    bound: toyos_net_tcp::ListenerId,
    port: Port,
    holders: Holders,
    /// The options last written to [tcp].
    options: Options,
}

#[derive(Default)]
pub(crate) struct Listeners {
    live: BTreeMap<ListenerId, Listener>,
    /// The next listener's id: none is used twice.
    next: u64,
}

impl Listeners {
    /// The holders past each listener's first, which take places of their own.
    pub(crate) fn more_holders(&self) -> usize {
        self.live.values().map(|listener| listener.holders.more()).fold(0, usize::saturating_add)
    }
}

impl Node {
    /// A passive open at `addr`, 0.0.0.0 meaning every address the interface holds or comes to
    /// hold, on `port` or on a port chosen from `draw`'s candidates, with `nodelay` as
    /// [`Node::set_listener_nodelay`] would set it, for every connection of the listener's
    /// from its first. Answers the listener and its port; `bell` is its first holder's,
    /// [`HolderId::FIRST`]. Refused, nothing was made and `bell` is dropped.
    pub fn listen(&mut self, addr: Ipv4Addr, port: Option<Port>, nodelay: bool, bell: Box<dyn Bell>, mut draw: impl FnMut() -> u32) -> Result<(ListenerId, Port), ListenRefused> {
        if self.room() == 0 {
            return Err(ListenRefused::Full);
        }
        // A drawn port is one [tcp] finds no listener on at any address.
        if port.is_some_and(|port| self.listeners.live.values().any(|listener| listener.port == port)) {
            return Err(ListenRefused::InUse);
        }
        let candidate = || {
            let [low, high, ..] = draw().to_le_bytes();
            u16::from_le_bytes([low, high])
        };
        let (bound, port) = match self.stack.tcp_listen(addr, port, candidate) {
            Ok(listening) => listening,
            Err(ListenError::NotLocal) => return Err(ListenRefused::NotLocal),
            Err(ListenError::Tcp(Error::AddrInUse)) => return Err(ListenRefused::InUse),
            Err(ListenError::Tcp(refusal)) => unreachable!("[tcp] refused a passive open for more than its port: {refusal:?}"),
        };
        let id = ListenerId(self.listeners.next);
        self.listeners.next = self.listeners.next.saturating_add(1);
        let options = Options { nodelay, ..Options::default() };
        self.stack.tcp_set_listener_options(bound, options);
        self.listeners.live.insert(id, Listener { bound, port, holders: Holders::new(bell), options });
        Ok((id, port))
    }

    /// A holder more of listener `id`, told on `bell` at once of a connection that waits, which
    /// takes a place: the listener's port.
    pub fn export_listener(&mut self, id: ListenerId, bell: Box<dyn Bell>) -> Result<(HolderId, Port), ExportRefused> {
        if self.room() == 0 {
            return Err(ExportRefused::Full);
        }
        let listener = self.listeners.live.get_mut(&id).ok_or(ExportRefused::Gone)?;
        let made = (listener.holders.add(bell), listener.port);
        // Told now of what waits: no pass need come before its first look.
        self.tell_holders();
        Ok(made)
    }

    /// `holder` of listener `id` left; the last holder's leaving closes it. Either gives a place
    /// back.
    pub fn listener_departed(&mut self, now: Instant, id: ListenerId, holder: HolderId) {
        let Some(listener) = self.listeners.live.get_mut(&id) else { return };
        if !listener.holders.remove(holder) {
            return;
        }
        if listener.holders.is_empty() {
            self.end_listener(now, id);
        }
        self.tell_holders();
    }

    /// Nagle's algorithm off or on (RFC 9293 §3.7.4) for every connection whose SYN arrives at
    /// `id` from here on; one that began before keeps what it has. `false` is an id that names
    /// no listener.
    pub fn set_listener_nodelay(&mut self, id: ListenerId, nodelay: bool) -> bool {
        let Some(listener) = self.listeners.live.get_mut(&id) else { return false };
        listener.options.nodelay = nodelay;
        self.stack.tcp_set_listener_options(listener.bound, listener.options);
        true
    }

    /// Whether Nagle's algorithm is off for the connections that begin at `id` from here on:
    /// `None` is an id that names no listener.
    pub fn listener_nodelay(&self, id: ListenerId) -> Option<bool> {
        self.listeners.live.get(&id).map(|listener| listener.options.nodelay)
    }

    /// `holder`'s accept: the oldest connection waiting at `id` becomes a stream on the region of
    /// the session `session` makes, which is asked for only once there is a connection and a
    /// place for it, and what the connection already received moves there at once.
    pub fn accept(&mut self, now: Instant, id: ListenerId, holder: HolderId, session: impl FnOnce() -> Option<Box<dyn Session>>) -> Result<Accepted, AcceptRefused> {
        let Some(listener) = self.listeners.live.get_mut(&id).filter(|listener| listener.holders.holds(holder)) else {
            return Err(AcceptRefused::NoListener);
        };
        listener.holders.asked(holder);
        let bound = listener.bound;
        let answer = self.take(bound, session);
        self.pass(now, false);
        answer
    }

    fn take(&mut self, bound: toyos_net_tcp::ListenerId, session: impl FnOnce() -> Option<Box<dyn Session>>) -> Result<Accepted, AcceptRefused> {
        if self.room() == 0 {
            return Err(AcceptRefused::Full);
        }
        if self.stack.tcp_ready(bound) == 0 {
            return Err(AcceptRefused::Nothing);
        }
        let session = session().ok_or(AcceptRefused::NoRegion)?;
        let Some((conn, tuple, options)) = self.stack.tcp_accept(bound) else { unreachable!("[tcp] counted a connection it would not hand over") };
        let id = self.streams.accepted(&mut self.stack, conn, options, session);
        Ok(Accepted { id, remote: tuple.remote, local: tuple.local, nodelay: options.nodelay })
    }

    fn end_listener(&mut self, now: Instant, id: ListenerId) {
        if let Some(listener) = self.listeners.live.remove(&id) {
            self.stack.tcp_close_listener(now, listener.bound);
        }
    }

    /// Tells every holder of a listener or a datagram socket what it is owed: a listener's, while
    /// a connection waits that there is a place for; a datagram socket's, while a datagram
    /// waits. One lookup in [tcp] or [udp] a socket: nothing is moved or offered for sending.
    pub(crate) fn tell_holders(&mut self) {
        let room = self.room();
        for listener in self.listeners.live.values_mut() {
            listener.holders.tell(room > 0 && self.stack.tcp_ready(listener.bound) > 0);
        }
        self.tell_datagram_holders();
    }

    /// The listeners the node holds.
    pub fn listeners(&self) -> usize {
        self.listeners.live.len()
    }
}
