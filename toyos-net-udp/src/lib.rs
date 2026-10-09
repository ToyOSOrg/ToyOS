//! ToyOS's UDP (RFC 768, RFC 8085), pure: sockets bound to any address, one address or one peer,
//! at most one socket per port, ephemeral ports by RFC 6056 Algorithm 1, the demultiplexer, and
//! ICMP errors for connected sockets. Addressing is `toyos-net-ip`'s: a call that needs it reads
//! the [`Ip`] it is handed. The caller hands in the time, the draws, arrivals and user calls;
//! nothing here reads a clock, draws randomness or does I/O.
//!
//! **Back-pressure is a refusal.** Each socket holds at most `limits::TX_DATAGRAMS` accepted
//! datagrams: a send past it is refused and the caller keeps its datagram. What closed sockets
//! had accepted is held to `limits::CLOSED_DATAGRAMS` together and is one [`Sender`] beside the
//! sockets, so closing never starves another socket. The caller's round over senders decides
//! whose datagram leaves next: a datagram leaves only when [`Udp::serve`] offers it to the caller
//! and the caller takes it, and is built then. What [`Udp::drain_eligible`] and
//! [`Udp::drain_gone`] report is held until the caller drains it, a socket closed while offered
//! included.
//!
//! **A datagram waits for its next hop where it was accepted.** One the caller answers
//! [`Offer::Waits`] stays in its sender's queue, under that queue's bound, and is passed over
//! until [`Udp::wake`] names the hop: the sender's datagrams to other hops leave meanwhile, and
//! those to one hop leave in the order they were accepted. One with no way out is dropped,
//! counted, and reported to a connected socket; nothing is kept for another try.
//!
//! **Refusals are values.** Every refusal is a named [`Counter`] and the [`Error`] the call
//! returns; one of legacy or insecure input is also a [`Refusal`] naming the socket and the peer.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    forbid(
        clippy::arithmetic_side_effects,
        clippy::indexing_slicing,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::as_conversions
    )
)]

extern crate alloc;

use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;
use core::net::Ipv4Addr;

use toyos_net_ip::{Arrival, Cast, ErrorClass, Flow, Ip, Source, Transport, TransportError, UdpOut};
use toyos_net_wire::addr::is_martian;
use toyos_net_wire::ethernet::MacClass;
use toyos_net_wire::ipv4::Ttl;
use toyos_net_wire::udp::{UdpBuilder, UdpChecksum, UdpDatagram};
use toyos_net_wire::{Instant, Port};

toyos_net_wire::counters! {
    BindAddressNotLocal = "udp.bind-address-not-local";
    PortInUse = "udp.port-in-use";
    NoEphemeralPort = "udp.no-ephemeral-port";
    ConnectUnspecified = "udp.connect-unspecified";
    ConnectGroup = "udp.connect-group";
    ConnectPortZero = "udp.connect-port-zero";
    SendLoopback = "udp.send-loopback";
    SendToSelf = "udp.send-to-self";
    SendInvalidDestination = "udp.send-invalid-destination";
    SendPortZero = "udp.send-port-zero";
    SendUnspecifiedDestination = "udp.send-unspecified-destination", logged;
    BroadcastNotPermitted = "udp.broadcast-not-permitted", logged;
    ConnectedDestinationMismatch = "udp.connected-destination-mismatch";
    NotConnected = "udp.not-connected";
    NoSourceAddress = "udp.no-source-address";
    NoRoute = "udp.no-route";
    SourceAddressNotAssigned = "udp.source-address-not-assigned";
    SourceNotPermitted = "udp.source-not-permitted";
    ExceedsMtu = "udp.exceeds-mtu";
    TxQueueFull = "udp.tx-queue-full";
    Rx = "udp.rx";
    RxDelivered = "udp.rx-delivered";
    RxNoSocket = "udp.rx-no-socket";
    RxNoSocketGroup = "udp.rx-no-socket-group";
    RxQueueFull = "udp.rx-queue-full";
    RxNoChecksum = "udp.rx-no-checksum";
    RxSrcPortZero = "udp.rx-src-port-zero";
    RxTruncated = "udp.rx-truncated";
    RxDroppedOnConnect = "udp.rx-dropped-on-connect";
    RxDiscardedOnClose = "udp.rx-discarded-on-close";
    TxDiscardedOnClose = "udp.tx-discarded-on-close";
    Tx = "udp.tx";
    TxUnreachable = "udp.tx-unreachable";
    IcmpErrorDelivered = "udp.icmp-error-delivered";
    IcmpErrorSoft = "udp.icmp-error-soft";
    IcmpErrorUnconnected = "udp.icmp-error-unconnected";
    IcmpErrorNoSocket = "udp.icmp-error-no-socket";
    EventOverflow = "udp.event-overflow";
}

pub mod limits {
    pub const RX_DATAGRAMS: usize = 16;
    pub const RX_BYTES: usize = 65_536;
    pub const TX_DATAGRAMS: usize = 16;
    pub const TX_BYTES: usize = 65_536;
    /// Datagrams closed sockets had accepted and that have not left: one socket's queue, whose
    /// bytes MAX_PAYLOAD bounds.
    pub const CLOSED_DATAGRAMS: usize = TX_DATAGRAMS;
    /// IANA's dynamic range, 49152 to 65535 (RFC 6335 §6).
    pub const EPHEMERAL_FIRST: u16 = 49_152;
    pub const EPHEMERAL_COUNT: u16 = 16_384;
    /// The interface MTU less the IPv4 and UDP headers: nothing is fragmented (DF on every datagram).
    pub const MAX_PAYLOAD: usize = toyos_net_ip::MTU.saturating_sub(28);
    /// Refusals held for the shell until it drains them.
    pub const EVENTS: usize = 1_024;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SocketId {
    index: u32,
    generation: u32,
}

/// Where a socket is bound: a peer only once connected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Binding {
    Any,
    Specific(Ipv4Addr),
    Connected { local: Ipv4Addr, peer: Ipv4Addr, peer_port: Port },
}

/// An error against a connected socket's datagrams: its next call returns it once. Two are what
/// an ICMP message said, which anyone who knows the socket's addresses and ports can send; one
/// is [ip]'s own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SocketError {
    /// An ICMP error said the peer refuses the protocol or the port.
    Refused,
    /// An ICMP error said the way to the peer is administratively prohibited.
    Prohibited,
    /// A datagram the socket had accepted found no way out, no route or no link address for its
    /// next hop, or [ip] refused it as it left: it is gone.
    NextHopFailed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    NoSuchSocket,
    /// Refused by the rule its counter names.
    Refused(Counter),
    /// The socket's pending error, now cleared; the call did nothing else.
    Failed(SocketError),
}

/// A datagram `recv` delivered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Received {
    /// Bytes written: the payload, cut to the buffer.
    pub len: usize,
    pub source: Ipv4Addr,
    /// Absent when the peer sent port 0: it cannot be answered.
    pub source_port: Option<Port>,
    pub destination: Ipv4Addr,
    pub ttl: u8,
    pub link_broadcast: bool,
    pub at: Instant,
}

/// What [`Udp::receive`] did with an arrival.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// A socket took it, or dropped it for a full queue: the receiver exists.
    Delivered,
    /// No socket accepts this unicast datagram: [ip] may answer with a port unreachable.
    NoSocket,
    /// No socket, and nothing is owed: a group, broadcast or acquisition datagram.
    Ignored,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub rule: Counter,
    /// The socket's address, 0.0.0.0 while bound to any, and its port.
    pub local: (Ipv4Addr, Port),
    /// The destination the call asked for.
    pub peer: (Ipv4Addr, u16),
}

#[derive(Debug)]
struct Stored {
    payload: Vec<u8>,
    source: Ipv4Addr,
    source_port: Option<Port>,
    destination: Ipv4Addr,
    ttl: u8,
    link_broadcast: bool,
    at: Instant,
}

/// A datagram accepted for sending, with the addressing it was accepted with.
#[derive(Debug)]
struct Queued {
    source: Ipv4Addr,
    /// The port of the socket that accepted it.
    source_port: Port,
    destination: Ipv4Addr,
    port: Port,
    ttl: Ttl,
    /// The socket's broadcast permission at the call: what [ip] may do with the datagram when it
    /// leaves is decided by this, not by what the socket holds then.
    broadcast: bool,
    payload: Vec<u8>,
    /// The next hop whose link address it waits for: not offered until [`Udp::wake`] names it.
    waits: Option<Ipv4Addr>,
}

impl Queued {
    fn out(&self) -> UdpOut<'_> {
        UdpOut {
            source: self.source,
            destination: self.destination,
            ttl: self.ttl,
            broadcast: self.broadcast,
            datagram: UdpBuilder { source: self.source_port, destination: self.port, data: &self.payload },
        }
    }

    fn flow(&self) -> Flow {
        Flow { source: self.source, source_port: self.source_port, destination: self.destination, destination_port: self.port }
    }
}

#[derive(Debug)]
struct Socket {
    port: Port,
    binding: Binding,
    /// Bound to any before its first connect: a reconnect selects its address again.
    from_any: bool,
    rx: VecDeque<Stored>,
    rx_bytes: usize,
    rx_full: u64,
    tx: VecDeque<Queued>,
    /// In `eligible` or the caller's round.
    offered: bool,
    pending: Option<SocketError>,
    ttl: Ttl,
    multicast_ttl: Ttl,
    broadcast: bool,
    acquisition: bool,
}

#[derive(Debug)]
struct Slot {
    generation: u32,
    socket: Option<Socket>,
}

/// A flow of the caller's round: one socket's datagrams, or those closed sockets had accepted,
/// together.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Sender {
    Socket(SocketId),
    Closed,
}

/// What the caller did with a datagram [`Udp::serve`] offered it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Offer {
    /// It left [udp]: in a frame, or refused by [ip], which counted it and told its flow.
    Taken,
    /// The link address of this next hop is being asked for: the datagram stays with its sender
    /// until [`Udp::wake`] names the hop.
    Waits(Ipv4Addr),
    /// It has no way out: no route, or a next hop [ip] has given up on or has no room for.
    Unreachable,
}

/// What [`Udp::serve`] handed the caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Served {
    /// A datagram, and another may be offered behind it.
    More,
    /// The last datagram the sender may be offered: it leaves the round, and is offered again
    /// once it has another or one of its own is woken.
    Last,
    /// Nothing: the sender has no datagram that waits for no next hop, or names a socket since
    /// closed.
    Nothing,
}

#[derive(Debug, Default)]
pub struct Udp {
    slots: Vec<Slot>,
    free: Vec<u32>,
    ports: BTreeMap<Port, u32>,
    /// Senders offered to the caller's round since it last drained them.
    eligible: Vec<Sender>,
    /// Sockets closed while offered to the caller's round, since it last drained them.
    gone: Vec<Sender>,
    /// `closed` is in `eligible` or the caller's round.
    closed_offered: bool,
    /// Datagrams closed sockets had accepted: they still leave, at most
    /// `limits::CLOSED_DATAGRAMS` of them.
    closed: VecDeque<Queued>,
    counters: Counters,
    refusals: Vec<Refusal>,
}

fn route_refusal(refusal: toyos_net_ip::Counter) -> Counter {
    match refusal {
        toyos_net_ip::Counter::RouteNoSourceAddress => Counter::NoSourceAddress,
        _ => Counter::NoRoute,
    }
}

impl Udp {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn counters(&self) -> &Counters {
        &self.counters
    }

    /// Refusals to log since the last call.
    pub fn drain_refusals(&mut self) -> alloc::vec::Drain<'_, Refusal> {
        self.refusals.drain(..)
    }

    pub fn socket_count(&self) -> usize {
        self.ports.len()
    }

    fn count(&mut self, counter: Counter) {
        self.counters.add(counter, 1);
    }

    fn refuse<T>(&mut self, rule: Counter, local: (Ipv4Addr, Port), peer: (Ipv4Addr, u16)) -> Result<T, Error> {
        self.count(rule);
        if rule.logged() {
            if self.refusals.len() >= limits::EVENTS {
                self.count(Counter::EventOverflow);
            } else {
                self.refusals.push(Refusal { rule, local, peer });
            }
        }
        Err(Error::Refused(rule))
    }

    fn socket(&mut self, id: SocketId) -> Result<&mut Socket, Error> {
        let slot = usize::try_from(id.index).ok().and_then(|i| self.slots.get_mut(i)).ok_or(Error::NoSuchSocket)?;
        slot.socket.as_mut().filter(|_| slot.generation == id.generation).ok_or(Error::NoSuchSocket)
    }

    fn at(&mut self, index: u32) -> Option<&mut Socket> {
        self.slots.get_mut(usize::try_from(index).ok()?)?.socket.as_mut()
    }

    /// RFC 6056 Algorithm 1: the first candidate is uniform over the range, then each
    /// next port in turn, until a free one or every port was tried.
    fn ephemeral(&self, draw: u32) -> Option<Port> {
        let offset = u16::try_from(draw.checked_rem(u32::from(limits::EPHEMERAL_COUNT)).unwrap_or(0)).unwrap_or(0);
        (0..limits::EPHEMERAL_COUNT)
            .map(|n| limits::EPHEMERAL_FIRST.wrapping_add(offset.wrapping_add(n).checked_rem(limits::EPHEMERAL_COUNT).unwrap_or(0)))
            .filter_map(Port::new)
            .find(|p| !self.ports.contains_key(p))
    }

    /// Binds a socket to `addr`, 0.0.0.0 meaning any, and `port`, or an ephemeral one when
    /// `None`, which spends the one draw.
    pub fn bind(&mut self, ip: &Ip, addr: Ipv4Addr, port: Option<Port>, draw: impl FnOnce() -> u32) -> Result<SocketId, Error> {
        let binding = if addr.is_unspecified() {
            Binding::Any
        } else if ip.is_assigned(addr) {
            Binding::Specific(addr)
        } else {
            self.count(Counter::BindAddressNotLocal);
            return Err(Error::Refused(Counter::BindAddressNotLocal));
        };
        let port = match port {
            Some(port) if self.ports.contains_key(&port) => {
                self.count(Counter::PortInUse);
                return Err(Error::Refused(Counter::PortInUse));
            }
            Some(port) => port,
            None => match self.ephemeral(draw()) {
                Some(port) => port,
                None => {
                    self.count(Counter::NoEphemeralPort);
                    return Err(Error::Refused(Counter::NoEphemeralPort));
                }
            },
        };
        let socket = Socket {
            port,
            binding,
            from_any: binding == Binding::Any,
            rx: VecDeque::new(),
            rx_bytes: 0,
            rx_full: 0,
            tx: VecDeque::new(),
            offered: false,
            pending: None,
            ttl: Ttl::DEFAULT,
            multicast_ttl: Ttl::LINK,
            broadcast: false,
            acquisition: false,
        };
        let index = match self.free.pop() {
            Some(index) => index,
            None => {
                self.slots.push(Slot { generation: 0, socket: None });
                u32::try_from(self.slots.len().saturating_sub(1)).unwrap_or(u32::MAX)
            }
        };
        let slot = self.slots.get_mut(usize::try_from(index).unwrap_or(usize::MAX)).ok_or(Error::NoSuchSocket)?;
        slot.socket = Some(socket);
        let generation = slot.generation;
        self.ports.insert(port, index);
        Ok(SocketId { index, generation })
    }

    pub fn binding(&mut self, id: SocketId) -> Result<(Binding, Port), Error> {
        self.socket(id).map(|s| (s.binding, s.port))
    }

    /// Datagrams this socket's full queue dropped.
    pub fn dropped(&mut self, id: SocketId) -> Result<u64, Error> {
        self.socket(id).map(|s| s.rx_full)
    }

    /// POSIX's `SO_BROADCAST`: without it a send to a broadcast address is refused, and a datagram
    /// accepted without it is refused by [ip] if a broadcast is where it would leave.
    pub fn set_broadcast(&mut self, id: SocketId, permitted: bool) -> Result<(), Error> {
        self.socket(id).map(|s| s.broadcast = permitted)
    }

    pub fn set_ttl(&mut self, id: SocketId, unicast: Ttl, multicast: Ttl) -> Result<(), Error> {
        self.socket(id).map(|s| {
            s.ttl = unicast;
            s.multicast_ttl = multicast;
        })
    }

    /// Marks netstack's DHCP client's socket: the only one that may send from 0.0.0.0, and the only
    /// one the acquisition exception delivers to.
    pub fn set_acquisition(&mut self, id: SocketId) -> Result<(), Error> {
        self.socket(id).map(|s| s.acquisition = true)
    }

    /// The destinations no send or connect may name.
    fn unusable(ip: &Ip, addr: Ipv4Addr) -> Option<Counter> {
        if addr.is_loopback() {
            Some(Counter::SendLoopback)
        } else if ip.is_local(addr) {
            Some(Counter::SendToSelf)
        } else if is_martian(addr) {
            Some(Counter::SendInvalidDestination)
        } else {
            None
        }
    }

    /// Connects to one unicast peer; the local address is fixed now, and datagrams already
    /// queued from anyone else are dropped.
    pub fn connect(&mut self, ip: &mut Ip, id: SocketId, peer: Ipv4Addr, peer_port: u16) -> Result<(), Error> {
        let socket = self.socket(id)?;
        let (binding, from_any, local_port) = (socket.binding, socket.from_any, socket.port);
        let local = match binding {
            Binding::Specific(a) | Binding::Connected { local: a, .. } => a,
            Binding::Any => Ipv4Addr::UNSPECIFIED,
        };
        let refusal = if peer.is_unspecified() {
            Some(Counter::ConnectUnspecified)
        } else if peer.is_broadcast() || peer.is_multicast() || ip.is_directed_broadcast(peer) {
            Some(Counter::ConnectGroup)
        } else {
            Self::unusable(ip, peer)
        };
        let peer_port = match (refusal, Port::new(peer_port)) {
            (None, Some(port)) => port,
            (Some(rule), _) => return self.refuse(rule, (local, local_port), (peer, peer_port)),
            (None, None) => return self.refuse(Counter::ConnectPortZero, (local, local_port), (peer, peer_port)),
        };
        let source = if from_any { Source::Any } else { Source::Bound(local) };
        let local = match ip.route(peer, source, None) {
            Ok(route) => route.source,
            Err(refusal) => return self.refuse(route_refusal(refusal), (local, local_port), (peer, peer_port.get())),
        };
        let socket = self.socket(id)?;
        socket.binding = Binding::Connected { local, peer, peer_port };
        let before = socket.rx.len();
        socket.rx.retain(|d| d.source == peer && d.source_port == Some(peer_port));
        socket.rx_bytes = socket.rx.iter().map(|d| d.payload.len()).sum();
        let dropped = before.saturating_sub(socket.rx.len());
        self.counters.add(Counter::RxDroppedOnConnect, u64::try_from(dropped).unwrap_or(u64::MAX));
        Ok(())
    }

    /// Queues `payload` for `destination:port`. Accepted means queued, not sent.
    pub fn send_to(&mut self, ip: &mut Ip, id: SocketId, destination: Ipv4Addr, port: u16, payload: &[u8]) -> Result<(), Error> {
        self.submit(ip, id, None, destination, port, payload)
    }

    /// Queues `payload` for the connected peer.
    pub fn send(&mut self, ip: &mut Ip, id: SocketId, payload: &[u8]) -> Result<(), Error> {
        let socket = self.socket(id)?;
        match socket.binding {
            Binding::Connected { peer, peer_port, .. } => self.submit(ip, id, None, peer, peer_port.get(), payload),
            Binding::Any | Binding::Specific(_) => {
                self.count(Counter::NotConnected);
                Err(Error::Refused(Counter::NotConnected))
            }
        }
    }

    /// The acquisition socket's send, naming its source: 0.0.0.0 to the limited broadcast, or an
    /// assigned address.
    pub fn send_from(&mut self, ip: &mut Ip, id: SocketId, source: Ipv4Addr, destination: Ipv4Addr, port: u16, payload: &[u8]) -> Result<(), Error> {
        self.submit(ip, id, Some(source), destination, port, payload)
    }

    fn submit(&mut self, ip: &mut Ip, id: SocketId, named: Option<Ipv4Addr>, destination: Ipv4Addr, port: u16, payload: &[u8]) -> Result<(), Error> {
        let socket = self.socket(id)?;
        if let Some(error) = socket.pending.take() {
            return Err(Error::Failed(error));
        }
        let (binding, local_port, broadcast_permitted, acquisition) = (socket.binding, socket.port, socket.broadcast, socket.acquisition);
        let (ttl, multicast_ttl) = (socket.ttl, socket.multicast_ttl);
        let local = match binding {
            Binding::Specific(a) | Binding::Connected { local: a, .. } => a,
            Binding::Any => Ipv4Addr::UNSPECIFIED,
        };
        let me = (local, local_port);
        let peer = (destination, port);
        if let Binding::Connected { peer: p, peer_port: pp, .. } = binding {
            if (p, pp.get()) != peer {
                return self.refuse(Counter::ConnectedDestinationMismatch, me, peer);
            }
        }
        let Some(port) = Port::new(port) else { return self.refuse(Counter::SendPortZero, me, peer) };
        let broadcast = destination.is_broadcast() || ip.is_directed_broadcast(destination);
        let refusal = if destination.is_unspecified() {
            Some(Counter::SendUnspecifiedDestination)
        } else if let Some(rule) = Self::unusable(ip, destination) {
            Some(rule)
        } else if broadcast && !broadcast_permitted {
            Some(Counter::BroadcastNotPermitted)
        } else if payload.len() > limits::MAX_PAYLOAD {
            Some(Counter::ExceedsMtu)
        } else {
            None
        };
        if let Some(rule) = refusal {
            return self.refuse(rule, me, peer);
        }
        let (source, lookup) = match (named, binding) {
            (Some(_), _) if !acquisition => return self.refuse(Counter::SourceNotPermitted, me, peer),
            (Some(named), _) if named.is_unspecified() && !destination.is_broadcast() => {
                return self.refuse(Counter::SourceNotPermitted, me, peer);
            }
            (Some(named), _) if named.is_unspecified() => (named, Source::Unspecified),
            (Some(named), _) | (None, Binding::Specific(named) | Binding::Connected { local: named, .. }) => {
                if !ip.is_assigned(named) {
                    return self.refuse(Counter::SourceAddressNotAssigned, me, peer);
                }
                (named, Source::Bound(named))
            }
            (None, Binding::Any) => (Ipv4Addr::UNSPECIFIED, Source::Any),
        };
        let source = match ip.route(destination, lookup, None) {
            Ok(route) if lookup == Source::Any => route.source,
            Ok(_) => source,
            Err(refusal) => return self.refuse(route_refusal(refusal), me, peer),
        };
        let socket = self.socket(id)?;
        let held: usize = socket.tx.iter().map(|queued| queued.payload.len()).sum();
        if socket.tx.len() >= limits::TX_DATAGRAMS || held.saturating_add(payload.len()) > limits::TX_BYTES {
            return self.refuse(Counter::TxQueueFull, me, peer);
        }
        let ttl = if destination.is_multicast() { multicast_ttl } else { ttl };
        socket.tx.push_back(Queued { source, source_port: local_port, destination, port, ttl, broadcast: broadcast_permitted, payload: payload.to_vec(), waits: None });
        if !core::mem::replace(&mut socket.offered, true) {
            self.eligible.push(Sender::Socket(id));
        }
        Ok(())
    }

    /// The oldest datagram, cut to `out` (the rest discarded), or `None` when none waits.
    pub fn recv(&mut self, id: SocketId, out: &mut [u8]) -> Result<Option<Received>, Error> {
        let socket = self.socket(id)?;
        if let Some(error) = socket.pending.take() {
            return Err(Error::Failed(error));
        }
        let Some(d) = socket.rx.pop_front() else { return Ok(None) };
        socket.rx_bytes = socket.rx_bytes.saturating_sub(d.payload.len());
        let len = out.len().min(d.payload.len());
        if let (Some(to), Some(from)) = (out.get_mut(..len), d.payload.get(..len)) {
            to.copy_from_slice(from);
        }
        if len < d.payload.len() {
            self.count(Counter::RxTruncated);
        }
        Ok(Some(Received {
            len,
            source: d.source,
            source_port: d.source_port,
            destination: d.destination,
            ttl: d.ttl,
            link_broadcast: d.link_broadcast,
            at: d.at,
        }))
    }

    /// Releases the port at once: received datagrams are discarded, and accepted ones still leave
    /// while fewer than `limits::CLOSED_DATAGRAMS` wait; the rest are refused
    /// `udp.tx-discarded-on-close`, since nobody is left to refuse them to.
    pub fn close(&mut self, id: SocketId) -> Result<(), Error> {
        self.socket(id)?;
        let Some(slot) = usize::try_from(id.index).ok().and_then(|i| self.slots.get_mut(i)) else { return Err(Error::NoSuchSocket) };
        let Some(socket) = slot.socket.take() else { return Err(Error::NoSuchSocket) };
        slot.generation = slot.generation.wrapping_add(1);
        self.free.push(id.index);
        self.ports.remove(&socket.port);
        self.counters.add(Counter::RxDiscardedOnClose, u64::try_from(socket.rx.len()).unwrap_or(u64::MAX));
        let room = limits::CLOSED_DATAGRAMS.saturating_sub(self.closed.len());
        let discarded = socket.tx.len().saturating_sub(room);
        self.counters.add(Counter::TxDiscardedOnClose, u64::try_from(discarded).unwrap_or(u64::MAX));
        if socket.offered {
            self.gone.push(Sender::Socket(id));
        }
        self.closed.extend(socket.tx.into_iter().take(room));
        if !self.closed.is_empty() && !core::mem::replace(&mut self.closed_offered, true) {
            self.eligible.push(Sender::Closed);
        }
        Ok(())
    }

    /// A datagram [ip] admitted.
    pub fn receive(&mut self, now: Instant, arrival: &Arrival<'_>, datagram: &UdpDatagram<'_>) -> Verdict {
        self.count(Counter::Rx);
        if datagram.checksum() == UdpChecksum::Absent {
            self.count(Counter::RxNoChecksum);
        }
        if datagram.source_port().is_none() {
            self.count(Counter::RxSrcPortZero);
        }
        let (source, destination) = (arrival.packet.source(), arrival.packet.destination());
        let source_port = datagram.source_port();
        let accepts = |s: &Socket| match (arrival.cast, s.binding) {
            (Cast::Acquisition, _) => s.acquisition,
            (Cast::Unicast, Binding::Any) => true,
            (Cast::Unicast, Binding::Specific(local)) => destination == local,
            (Cast::Unicast, Binding::Connected { local, peer, peer_port }) => destination == local && source == peer && source_port == Some(peer_port),
            (Cast::LimitedBroadcast | Cast::SubnetBroadcast | Cast::Multicast(_), binding) => binding == Binding::Any,
        };
        let index = self.ports.get(&datagram.destination_port()).copied();
        let Some(socket) = index.and_then(|i| self.at(i)).filter(|s| accepts(s)) else {
            return match arrival.cast {
                Cast::Unicast => {
                    self.count(Counter::RxNoSocket);
                    Verdict::NoSocket
                }
                Cast::Acquisition => {
                    self.count(Counter::RxNoSocket);
                    Verdict::Ignored
                }
                Cast::LimitedBroadcast | Cast::SubnetBroadcast | Cast::Multicast(_) => {
                    self.count(Counter::RxNoSocketGroup);
                    Verdict::Ignored
                }
            };
        };
        let payload = datagram.payload();
        let bytes = socket.rx_bytes.saturating_add(payload.len());
        if socket.rx.len() >= limits::RX_DATAGRAMS || bytes > limits::RX_BYTES {
            socket.rx_full = socket.rx_full.saturating_add(1);
            self.count(Counter::RxQueueFull);
            return Verdict::Delivered;
        }
        socket.rx_bytes = bytes;
        socket.rx.push_back(Stored {
            payload: payload.to_vec(),
            source,
            source_port,
            destination,
            ttl: arrival.packet.ttl(),
            link_broadcast: arrival.link == MacClass::Broadcast,
            at: now,
        });
        self.count(Counter::RxDelivered);
        Verdict::Delivered
    }

    fn connected(&mut self, flow: &Flow) -> Option<&mut Socket> {
        let index = *self.ports.get(&flow.source_port)?;
        self.at(index).filter(|s| {
            s.binding == Binding::Connected { local: flow.source, peer: flow.destination, peer_port: flow.destination_port }
        })
    }

    /// An ICMP error [ip] attributed to a UDP datagram of ours: only the connected socket
    /// whose whole 4-tuple it quotes hears it.
    pub fn icmp_error(&mut self, error: &TransportError) {
        if error.transport != Transport::Udp {
            return;
        }
        let flow = error.flow;
        let class = error.kind.class();
        let holder = self.ports.get(&flow.source_port).copied().and_then(|i| self.at(i)).map(|s| s.binding);
        let Some(socket) = self.connected(&flow) else {
            let counter = match holder {
                Some(Binding::Any | Binding::Specific(_)) => Counter::IcmpErrorUnconnected,
                Some(Binding::Connected { .. }) | None => Counter::IcmpErrorNoSocket,
            };
            return self.count(counter);
        };
        let pending = match class {
            ErrorClass::Refused => SocketError::Refused,
            ErrorClass::Prohibited => SocketError::Prohibited,
            ErrorClass::PathMtu | ErrorClass::Soft => return self.count(Counter::IcmpErrorSoft),
        };
        socket.pending = Some(pending);
        self.count(Counter::IcmpErrorDelivered);
    }

    /// A datagram of `flow` this crate had accepted will not leave: [ip] refused it as it left,
    /// or it found no way out.
    pub fn unreachable(&mut self, flow: &Flow) {
        if let Some(socket) = self.connected(flow) {
            socket.pending = Some(SocketError::NextHopFailed);
        }
    }

    /// Senders offered to the caller's round since the last call, each once, in the order they
    /// became eligible, less the sockets closed since: one is offered again only after
    /// [`Self::serve`] answered [`Served::Last`] or [`Served::Nothing`] for it.
    pub fn drain_eligible(&mut self) -> impl Iterator<Item = Sender> + '_ {
        let Self { eligible, slots, .. } = self;
        eligible.drain(..).filter(|sender| match sender {
            Sender::Socket(id) => slots.get(usize::try_from(id.index).unwrap_or(usize::MAX)).is_some_and(|s| s.generation == id.generation && s.socket.is_some()),
            Sender::Closed => true,
        })
    }

    /// Sockets closed while offered to the caller since the last call, each once: the caller
    /// takes each out of its round, where one it never drained is not.
    pub fn drain_gone(&mut self) -> alloc::vec::Drain<'_, Sender> {
        self.gone.drain(..)
    }

    /// Offers `sender`'s datagrams in the order it accepted them, those waiting for a next hop
    /// aside, until `offer` takes one: each is built as `offer` reads it. One `offer` answers
    /// [`Offer::Waits`] for stays and waits; one it answers [`Offer::Unreachable`] for is dropped
    /// and counted, and the connected socket it left from is told.
    pub fn serve(&mut self, sender: Sender, mut offer: impl FnMut(&UdpOut<'_>) -> Offer) -> Served {
        let (queue, offered) = match sender {
            Sender::Closed => (&mut self.closed, &mut self.closed_offered),
            Sender::Socket(id) => {
                let Ok(socket) = self.socket(id) else { return Served::Nothing };
                (&mut socket.tx, &mut socket.offered)
            }
        };
        let mut taken = false;
        let mut lost = Vec::new();
        queue.retain_mut(|queued| {
            if taken || queued.waits.is_some() {
                return true;
            }
            match offer(&queued.out()) {
                Offer::Taken => taken = true,
                Offer::Waits(next_hop) => queued.waits = Some(next_hop),
                Offer::Unreachable => lost.push(queued.flow()),
            }
            queued.waits.is_some()
        });
        let more = taken && queue.iter().any(|queued| queued.waits.is_none());
        *offered = more;
        self.counters.add(Counter::Tx, u64::from(taken));
        self.lose(&lost);
        match (taken, more) {
            (true, true) => Served::More,
            (true, false) => Served::Last,
            (false, _) => Served::Nothing,
        }
    }

    /// Datagrams this crate had accepted, each dropped for want of a way out.
    fn lose(&mut self, lost: &[Flow]) {
        self.counters.add(Counter::TxUnreachable, u64::try_from(lost.len()).unwrap_or(u64::MAX));
        for flow in lost {
            self.unreachable(flow);
        }
    }

    /// [ip] gave `next_hop` up: every datagram that waited for it is dropped and counted, and
    /// the connected socket it left from is told.
    pub fn fail(&mut self, next_hop: Ipv4Addr) {
        let mut lost = Vec::new();
        let queues = self.slots.iter_mut().filter_map(|slot| slot.socket.as_mut()).map(|socket| &mut socket.tx).chain([&mut self.closed]);
        for queue in queues {
            queue.retain(|queued| {
                let waited = queued.waits == Some(next_hop);
                if waited {
                    lost.push(queued.flow());
                }
                !waited
            });
        }
        self.lose(&lost);
    }

    /// `next_hop`'s link address is known: the datagrams that wait for it are offered again.
    pub fn wake(&mut self, next_hop: Ipv4Addr) {
        self.rouse(|waits| waits == next_hop);
    }

    /// The routes changed: every waiting datagram's next hop is asked for again.
    pub fn wake_all(&mut self) {
        self.rouse(|_| true);
    }

    /// Ends the wait of every datagram whose next hop `woken` names, and offers its sender.
    fn rouse(&mut self, woken: impl Fn(Ipv4Addr) -> bool) {
        let Self { slots, closed, closed_offered, eligible, .. } = self;
        let mut rouse = |queue: &mut VecDeque<Queued>, offered: &mut bool, sender: Sender| {
            let mut any = false;
            for queued in queue.iter_mut().filter(|queued| queued.waits.is_some_and(&woken)) {
                queued.waits = None;
                any = true;
            }
            if any && !core::mem::replace(offered, true) {
                eligible.push(sender);
            }
        };
        for (index, slot) in slots.iter_mut().enumerate() {
            let (Some(socket), Ok(index)) = (&mut slot.socket, u32::try_from(index)) else { continue };
            rouse(&mut socket.tx, &mut socket.offered, Sender::Socket(SocketId { index, generation: slot.generation }));
        }
        rouse(closed, closed_offered, Sender::Closed);
    }
}
