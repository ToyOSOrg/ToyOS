//! ToyOS's UDP (RFC 768, RFC 8085), pure: sockets bound to any address, one address or one peer,
//! at most one socket per port, ephemeral ports by RFC 6056 Algorithm 1, the demultiplexer, and
//! ICMP errors for connected sockets. Addressing is `toyos-net-ip`'s: a call that needs it reads
//! the [`Ip`] it is handed. The caller hands in the time, the draws, arrivals and user calls;
//! nothing here reads a clock, draws randomness or does I/O.
//!
//! **Back-pressure is a refusal.** Each socket holds at most `limits::TX_DATAGRAMS` accepted
//! datagrams: a send past it is refused and the caller keeps its datagram. What closed sockets
//! had accepted is held to `limits::CLOSED_DATAGRAMS` together and takes one turn among the
//! sockets, so closing never grows the stack or starves another socket. A datagram leaves only
//! when [`Udp::transmit`] offers it to the device and is built then; one [ip] holds for its next
//! hop has left this crate and spends no credit, so a socket's next datagram is never behind it.
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SocketId {
    index: u32,
    generation: u32,
}

/// Where a socket is bound (§U2.1): a peer only once connected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Binding {
    Any,
    Specific(Ipv4Addr),
    Connected { local: Ipv4Addr, peer: Ipv4Addr, peer_port: Port },
}

/// An error the network reported against a connected socket: its next call returns it once
/// (§U8.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SocketError {
    Refused,
    Unreachable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    NoSuchSocket,
    /// Refused by the rule its counter names.
    Refused(Counter),
    /// The socket's pending error, now cleared; the call did nothing else.
    Failed(SocketError),
}

/// A datagram `recv` delivered (§U5.6).
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
    destination: Ipv4Addr,
    port: Port,
    ttl: Ttl,
    payload: Vec<u8>,
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
    tx_bytes: usize,
    active: bool,
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

/// Whose datagram leaves next: a socket's, or one a closed socket had accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Turn {
    Socket(u32),
    Closed,
}

#[derive(Debug, Default)]
pub struct Udp {
    slots: Vec<Slot>,
    free: Vec<u32>,
    ports: BTreeMap<Port, u32>,
    /// One turn per socket with datagrams queued, and one for `closed` while it holds any.
    turns: VecDeque<Turn>,
    /// Datagrams closed sockets had accepted: they still leave (§U9 (3)), at most
    /// `limits::CLOSED_DATAGRAMS` of them.
    closed: VecDeque<(Port, Queued)>,
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

    /// RFC 6056 Algorithm 1 (§U2.4): the first candidate is uniform over the range, then each
    /// next port in turn, until a free one or every port was tried.
    fn ephemeral(&self, draw: u32) -> Option<Port> {
        let offset = u16::try_from(draw.checked_rem(u32::from(limits::EPHEMERAL_COUNT)).unwrap_or(0)).unwrap_or(0);
        (0..limits::EPHEMERAL_COUNT)
            .map(|n| limits::EPHEMERAL_FIRST.wrapping_add(offset.wrapping_add(n).checked_rem(limits::EPHEMERAL_COUNT).unwrap_or(0)))
            .filter_map(Port::new)
            .find(|p| !self.ports.contains_key(p))
    }

    /// Binds a socket to `addr`, 0.0.0.0 meaning any, and `port`, or an ephemeral one when
    /// `None`, which spends the one draw (§U2).
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
            tx_bytes: 0,
            active: false,
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

    /// Only netstack's own sockets: POSIX's `SO_BROADCAST` (§U4.3).
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
    /// one the acquisition exception delivers to (§U4.4 (3), §U5.3).
    pub fn set_acquisition(&mut self, id: SocketId) -> Result<(), Error> {
        self.socket(id).map(|s| s.acquisition = true)
    }

    /// The destination classes of §U4.2 and §U3 (2) that no send or connect may name.
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
    /// queued from anyone else are dropped (§U3).
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

    /// Queues `payload` for `destination:port` (§U4). Accepted means queued, not sent.
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
    /// assigned address (§U4.4 (3)).
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
        let bytes = socket.tx_bytes.saturating_add(payload.len());
        if socket.tx.len() >= limits::TX_DATAGRAMS || bytes > limits::TX_BYTES {
            return self.refuse(Counter::TxQueueFull, me, peer);
        }
        let ttl = if destination.is_multicast() { multicast_ttl } else { ttl };
        socket.tx.push_back(Queued { source, destination, port, ttl, payload: payload.to_vec() });
        socket.tx_bytes = bytes;
        if !core::mem::replace(&mut socket.active, true) {
            self.turns.push_back(Turn::Socket(id.index));
        }
        Ok(())
    }

    /// The oldest datagram, cut to `out` (the rest discarded, U-7), or `None` when none waits.
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
        if socket.active {
            self.turns.retain(|t| *t != Turn::Socket(id.index));
        }
        self.counters.add(Counter::RxDiscardedOnClose, u64::try_from(socket.rx.len()).unwrap_or(u64::MAX));
        let room = limits::CLOSED_DATAGRAMS.saturating_sub(self.closed.len());
        let discarded = socket.tx.len().saturating_sub(room);
        self.counters.add(Counter::TxDiscardedOnClose, u64::try_from(discarded).unwrap_or(u64::MAX));
        let turn = self.closed.is_empty();
        self.closed.extend(socket.tx.into_iter().take(room).map(|q| (socket.port, q)));
        if turn && !self.closed.is_empty() {
            self.turns.push_back(Turn::Closed);
        }
        Ok(())
    }

    /// A datagram [ip] admitted (§U5).
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

    /// An ICMP error [ip] attributed to a UDP datagram of ours (§U8): only the connected socket
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
            ErrorClass::Prohibited => SocketError::Unreachable,
            ErrorClass::PathMtu | ErrorClass::Soft => return self.count(Counter::IcmpErrorSoft),
        };
        socket.pending = Some(pending);
        self.count(Counter::IcmpErrorDelivered);
    }

    /// [ip] could not reach the next hop of a datagram this crate handed it (§U8.3 (4)).
    pub fn unreachable(&mut self, flow: &Flow) {
        if let Some(socket) = self.connected(flow) {
            socket.pending = Some(SocketError::Unreachable);
        }
    }

    /// A transmit opportunity with room for `credit` frames: one datagram per turn, each socket
    /// with datagrams queued taking one and closed sockets' datagrams together one more, each built
    /// as `sink` takes it. `sink` answers whether it spent a frame; one [ip] holds for its next hop
    /// spends none. Returns the frames spent.
    pub fn transmit(&mut self, credit: usize, mut sink: impl FnMut(&UdpOut<'_>) -> bool) -> usize {
        let mut spent = 0usize;
        while spent < credit {
            let Some(turn) = self.turns.pop_front() else { break };
            let (port, queued) = match turn {
                Turn::Closed => {
                    let Some(closed) = self.closed.pop_front() else { continue };
                    if !self.closed.is_empty() {
                        self.turns.push_back(Turn::Closed);
                    }
                    closed
                }
                Turn::Socket(index) => {
                    let Some(socket) = self.at(index) else { continue };
                    let Some(queued) = socket.tx.pop_front() else {
                        socket.active = false;
                        continue;
                    };
                    socket.tx_bytes = socket.tx_bytes.saturating_sub(queued.payload.len());
                    let port = socket.port;
                    if socket.tx.is_empty() {
                        socket.active = false;
                    } else {
                        self.turns.push_back(turn);
                    }
                    (port, queued)
                }
            };
            self.count(Counter::Tx);
            let out = UdpOut {
                source: queued.source,
                destination: queued.destination,
                ttl: queued.ttl,
                datagram: UdpBuilder { source: port, destination: queued.port, data: &queued.payload },
            };
            if sink(&out) {
                spent = spent.saturating_add(1);
            }
        }
        spent
    }
}
