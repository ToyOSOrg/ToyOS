//! Listeners: a passive open of [tcp]'s and the pipe its owner reads its wakes from. The queues
//! are [tcp]'s: handshakes in progress, and connections that finished theirs and wait, oldest
//! first. A connection that waits is nobody's stream yet; an accept makes the oldest one a
//! stream on the pipes the accept hands over.
//!
//! **The owner holds one unspent wake for each connection that waits and that there is a place
//! for, and an accept spends one whatever it answers.** The owner reads a wake and then accepts,
//! once; the node writes what is missing whenever a pass ends ([`Node::bridge`]), from what
//! [tcp] holds and what `places` has left at that moment, so no wake depends on the pass that
//! saw a connection arrive. A wake left over by a connection its peer reset before the accept
//! stands for the next one, and until then its accept is answered [`AcceptRefused::Nothing`].
//! The node cannot tell an accept that is on its way from one that will never be sent: an owner
//! that reads a wake and sends no accept is owed one wake fewer from then on.
//!
//! **A listener lives until its owner lets go**: [`Node::close_listener`], or a wake its pipe
//! refuses, for whatever reason. [tcp] then resets every connection that still waits, so each
//! peer learns at once, and the port is free.
//!
//! **A stream starts with the options its connection has**, and those are its listener's as
//! they were when its SYN arrived: [tcp] hands them over then (LS-10), the node reads them
//! back at the accept, and an option set on the listener afterwards reaches the connections
//! that begin afterwards. A host does the same with `TCP_NODELAY` set on a listening socket
//! (`tests/host.rs` asks the host the tests run on).

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use core::net::Ipv4Addr;

use toyos_net_shard::ListenError;
use toyos_net_tcp::{Endpoint, Error, Options};
use toyos_net_wire::{Instant, Port};

use crate::streams::{Pipes, StreamId, WriteRefusal};
use crate::Node;

/// The write end of the pipe a listener's owner reads its wakes from. Dropped, the owner reads
/// the end.
pub trait Wake {
    /// Writes one wake.
    fn wake(&mut self) -> Result<(), WriteRefusal>;
}

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
    /// The port is another listener's at that address, or no drawn port was free.
    InUse,
}

/// The answer to an accept that took a connection: its stream, established.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Accepted {
    pub id: StreamId,
    pub remote: Endpoint,
    pub local: Port,
}

/// Why an accept took no connection. Whatever waits still waits, and the pipe ends are dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcceptRefused {
    /// The id names no listener.
    NoListener,
    /// The request carried no pipes.
    NoPipes,
    /// The node holds all it has places for (`places`), whatever waits.
    Full,
    /// No connection waits.
    Nothing,
}

struct Listener {
    bound: toyos_net_tcp::ListenerId,
    owner: Box<dyn Wake>,
    /// Wakes written that no accept has spent.
    unspent: usize,
    /// The options last written to [tcp].
    options: Options,
}

#[derive(Default)]
pub(crate) struct Listeners {
    live: BTreeMap<ListenerId, Listener>,
    /// The next listener's id: none is used twice.
    next: u64,
    refused: VecDeque<(ListenerId, WriteRefusal)>,
}

impl Node {
    /// A passive open at `addr`, 0.0.0.0 meaning every address the interface holds or comes to
    /// hold, on `port` or on a port chosen from `draw`'s candidates. Answers the listener and
    /// its port. Refused, nothing was made and `owner` is dropped.
    pub fn listen(&mut self, addr: Ipv4Addr, port: Option<Port>, owner: Box<dyn Wake>, mut draw: impl FnMut() -> u32) -> Result<(ListenerId, Port), ListenRefused> {
        if self.room() == 0 {
            return Err(ListenRefused::Full);
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
        self.listeners.live.insert(id, Listener { bound, owner, unspent: 0, options: Options::default() });
        Ok((id, port))
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

    pub fn listener_nodelay(&self, id: ListenerId) -> Option<bool> {
        self.listeners.live.get(&id).map(|listener| listener.options.nodelay)
    }

    /// The owner's accept: the oldest connection waiting at `id` becomes a stream on `pipes`,
    /// and what it already received moves at once.
    pub fn accept(&mut self, now: Instant, id: ListenerId, pipes: Option<Pipes>) -> Result<Accepted, AcceptRefused> {
        let Some(listener) = self.listeners.live.get_mut(&id) else { return Err(AcceptRefused::NoListener) };
        listener.unspent = listener.unspent.saturating_sub(1);
        let bound = listener.bound;
        let answer = self.take(bound, pipes);
        self.bridge(now);
        answer
    }

    fn take(&mut self, bound: toyos_net_tcp::ListenerId, pipes: Option<Pipes>) -> Result<Accepted, AcceptRefused> {
        let Some(pipes) = pipes else { return Err(AcceptRefused::NoPipes) };
        if self.room() == 0 {
            return Err(AcceptRefused::Full);
        }
        let Some((conn, tuple, options)) = self.stack.tcp_accept(bound) else { return Err(AcceptRefused::Nothing) };
        // The peer's address is what `streams` counts a stream its client can see no more by.
        let id = self.streams.accepted(conn, tuple.remote.addr, options, pipes);
        Ok(Accepted { id, remote: tuple.remote, local: tuple.local.port })
    }

    /// The owner lets go of the listener: netstack calls it for a close, and when the kernel
    /// says nobody holds the other end of the wake pipe. `false` is an id that names no listener.
    pub fn close_listener(&mut self, now: Instant, id: ListenerId) -> bool {
        let closed = self.end_listener(now, id);
        if closed {
            self.wake_owners(now);
        }
        closed
    }

    fn end_listener(&mut self, now: Instant, id: ListenerId) -> bool {
        let Some(listener) = self.listeners.live.remove(&id) else { return false };
        self.stack.tcp_close_listener(now, listener.bound);
        true
    }

    /// Writes every owner the wakes it is owed, and ends a listener whose pipe refuses one. One
    /// lookup in [tcp] a listener: nothing of [tcp]'s is moved or offered for sending.
    pub(crate) fn wake_owners(&mut self, now: Instant) {
        // A listener ended here gives its place back, which another's owner may be owed a wake
        // for.
        while self.wake_each(now) {}
    }

    /// One round over the listeners, up to the first whose pipe refuses a wake; `true` ended it.
    fn wake_each(&mut self, now: Instant) -> bool {
        let room = self.room();
        let stack = &mut self.stack;
        let refused = self.listeners.live.iter_mut().find_map(|(id, listener)| {
            let owed = stack.tcp_ready(listener.bound).min(room);
            while listener.unspent < owed {
                if let Err(refusal) = listener.owner.wake() {
                    return Some((*id, refusal));
                }
                listener.unspent = listener.unspent.saturating_add(1);
            }
            None
        });
        let Some((id, refusal)) = refused else { return false };
        self.end_listener(now, id);
        self.listeners.refused.push_back((id, refusal));
        true
    }

    /// The listeners the node holds.
    pub fn listeners(&self) -> usize {
        self.listeners.live.len()
    }

    /// The listeners ended since the last call because their owner's pipe refused a wake, and
    /// what it answered.
    pub fn drain_refused_listeners(&mut self) -> impl Iterator<Item = (ListenerId, WriteRefusal)> + '_ {
        self.listeners.refused.drain(..)
    }
}
