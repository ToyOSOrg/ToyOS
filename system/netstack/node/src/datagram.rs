//! A client's datagram sockets on [udp]: bind, send, receive and close. A socket is named by a
//! [`DatagramId`], which only [`Node::udp_bind`] makes, so no call here names the DHCP client's
//! socket or the responder's; their ports are taken like any other.
//!
//! **Refusals.** [udp] refuses by rule and counts each under its own name. A client is answered
//! in one of the pipe ABI's words ([`Refused`]): the shell writes the word's code and decides
//! nothing. A destination, a port or a length is the client's number and is refused, never
//! trusted; a datagram off the wire reaches a socket only through [udp]'s demultiplexer.
//!
//! **A socket has holders** (`holders`), each told on its own bell that a datagram waits, all
//! receiving from the one queue: a datagram goes to the one holder whose receive takes it. The
//! socket lives until its last holder leaves ([`Node::datagram_departed`]).
//!
//! **A socket holds a place** (`places`) from its bind until then, and each holder past its
//! first one more: what it makes the node hold is its two queues, `limits::RX_DATAGRAMS`
//! received and `limits::TX_DATAGRAMS` accepted. A bind with no place left is answered
//! [`Refused::ResourceExhausted`] with nothing made.
//!
//! **A send to a broadcast address needs the socket's permission** ([`Node::udp_set_broadcast`],
//! POSIX's `SO_BROADCAST`), which a socket is bound without: one sent without it is answered
//! [`Refused::PermissionDenied`]. The permission a datagram was accepted under goes with it: one
//! accepted for a host leaves in no link broadcast, whatever prefix a server has renewed the
//! address with since, and is dropped, counted `ip.broadcast-not-permitted` and logged instead.
//!
//! **A datagram waits for its next hop in its own socket**, among the `limits::TX_DATAGRAMS` it
//! has accepted: while [ip] asks for a link address the socket's datagrams to other next hops
//! leave, and a send past the queue is answered [`Refused::ResourceExhausted`] whatever its
//! destination. One whose next hop [ip] gives up, or that has no way out when its turn comes, is
//! dropped and counted `udp.tx-unreachable`. An unconnected socket's client is told nothing; a
//! connected one's next call answers [`Refused::Unreachable`] once, as it answers an ICMP error
//! from its peer ([`Node::udp_connect`]).
//!
//! A refusal [udp] logs is in [`Node::drain_events`] when the send that met it returns, and one
//! [ip] logs as a datagram leaves when that [`Node::transmit`] returns; no other call of a
//! client's meets one.

use core::net::Ipv4Addr;

use alloc::boxed::Box;

use toyos_net_udp::{Counter, Error, SocketError, SocketId};
use toyos_net_wire::{Instant, Port};

use crate::holders::{Bell, HolderId, Holders};
use crate::streams::ExportRefused;
use crate::{Node, UdpPeer};

/// One client socket. An id outlives its socket only as a refusal: [udp] never hands a closed
/// socket's id to another.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DatagramId(SocketId);

/// Why a call was refused, in the pipe ABI's words (`toyos::net`'s `ERR_*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The port is another socket's, or no ephemeral port is free.
    AddrInUse,
    /// The id names no socket, or the holder none of its, or the machine has no address or route
    /// to send from.
    NotConnected,
    /// The call names what no datagram of this socket may carry: an address that is not this
    /// machine's to bind, a destination nothing is sent to, port 0, more than one frame holds.
    InvalidInput,
    /// The call was well formed and the socket lacks the permission it needs: a datagram to a
    /// broadcast address.
    PermissionDenied,
    /// The socket holds all it may until some of it has left, or the node holds all it has
    /// places for: the same call succeeds later.
    ResourceExhausted,
    /// An ICMP error said a connected socket's peer refuses the port: the socket's pending
    /// error, answered once.
    ConnectionRefused,
    /// A connected socket's datagram found no way to its peer: an ICMP error said the way is
    /// prohibited, or [ip] gave the next hop up. The socket's pending error, answered once.
    Unreachable,
}

/// A datagram [`Node::udp_recv_from`] handed over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Datagram {
    /// Bytes written: the payload, cut to the buffer.
    pub len: usize,
    pub source: Ipv4Addr,
    /// Absent when the peer sent port 0: it cannot be answered.
    pub source_port: Option<Port>,
}

/// [udp]'s refusal as the client hears it. A client's socket never names its source, so that
/// call's rule, and the counters that are no refusal, are not its to meet.
fn refused(error: Error) -> Refused {
    let rule = match error {
        Error::NoSuchSocket => return Refused::NotConnected,
        Error::Refused(rule) => rule,
        Error::Failed(SocketError::Refused) => return Refused::ConnectionRefused,
        Error::Failed(SocketError::Prohibited | SocketError::NextHopFailed) => return Refused::Unreachable,
    };
    match rule {
        Counter::PortInUse | Counter::NoEphemeralPort => Refused::AddrInUse,
        Counter::NoSourceAddress | Counter::NoRoute | Counter::SourceAddressNotAssigned | Counter::NotConnected => Refused::NotConnected,
        Counter::BindAddressNotLocal
        | Counter::SendLoopback
        | Counter::SendToSelf
        | Counter::SendInvalidDestination
        | Counter::SendPortZero
        | Counter::SendUnspecifiedDestination
        | Counter::ExceedsMtu
        | Counter::ConnectUnspecified
        | Counter::ConnectGroup
        | Counter::ConnectPortZero
        | Counter::ConnectedDestinationMismatch => Refused::InvalidInput,
        Counter::BroadcastNotPermitted => Refused::PermissionDenied,
        Counter::TxQueueFull => Refused::ResourceExhausted,
        Counter::SourceNotPermitted
        | Counter::Rx
        | Counter::RxDelivered
        | Counter::RxNoSocket
        | Counter::RxNoSocketGroup
        | Counter::RxQueueFull
        | Counter::RxNoChecksum
        | Counter::RxSrcPortZero
        | Counter::RxTruncated
        | Counter::RxDroppedOnConnect
        | Counter::RxDiscardedOnClose
        | Counter::TxDiscardedOnClose
        | Counter::Tx
        | Counter::TxUnreachable
        | Counter::IcmpErrorDelivered
        | Counter::IcmpErrorSoft
        | Counter::IcmpErrorUnconnected
        | Counter::IcmpErrorNoSocket
        | Counter::EventOverflow => unreachable!("[udp] refused a client's bind or send as {}", rule.name()),
    }
}

impl Node {
    /// Binds a socket to `addr`, 0.0.0.0 meaning every address the machine holds, and to `port`
    /// or, with none named, an ephemeral one, which spends the one draw. Returns the socket and
    /// the port it holds; `bell` is its first holder's, [`HolderId::FIRST`].
    pub fn udp_bind(&mut self, addr: Ipv4Addr, port: Option<Port>, bell: Box<dyn Bell>, draw: impl FnOnce() -> u32) -> Result<(DatagramId, Port), Refused> {
        if self.room() == 0 {
            return Err(Refused::ResourceExhausted);
        }
        let (id, port) = self.stack.bind(addr, port, draw).map_err(refused)?;
        self.datagrams.insert(DatagramId(id), Holders::new(bell));
        Ok((DatagramId(id), port))
    }

    /// A holder more of socket `id`, told on `bell` at once of a datagram that waits, which takes
    /// a place: the socket's port.
    pub fn export_datagram(&mut self, id: DatagramId, bell: Box<dyn Bell>) -> Result<(HolderId, Port), ExportRefused> {
        if self.room() == 0 {
            return Err(ExportRefused::Full);
        }
        let holders = self.datagrams.get_mut(&id).ok_or(ExportRefused::Gone)?;
        let holder = holders.add(bell);
        let Ok(port) = self.stack.port(id.0) else { unreachable!("[udp] lost a socket the node holds") };
        // Told now of what waits: no pass need come before its first look.
        self.tell_holders();
        Ok((holder, port))
    }

    /// Whether the socket's datagrams may go to a broadcast address, the limited one or the
    /// directed one of a prefix the machine holds, from here on.
    pub fn udp_set_broadcast(&mut self, id: DatagramId, permitted: bool) -> Result<(), Refused> {
        self.stack.set_broadcast(id.0, permitted).map_err(refused)
    }

    /// Whether [`Self::udp_set_broadcast`] last permitted it.
    pub fn udp_broadcast(&mut self, id: DatagramId) -> Result<bool, Refused> {
        self.stack.broadcast(id.0).map_err(refused)
    }

    /// Connects the socket to `peer:port` (POSIX's `connect`): from here its datagrams come only
    /// from there, and [`Self::udp_send`] goes there, for every holder alike. Datagrams already
    /// received from anyone else are dropped.
    pub fn udp_connect(&mut self, now: Instant, id: DatagramId, peer: Ipv4Addr, port: u16) -> Result<(), Refused> {
        let connected = self.stack.connect(now, id.0, peer, port).map_err(refused);
        self.log(now);
        connected
    }

    /// The peer [`Self::udp_connect`] named, if any, and the address the socket sends there from.
    pub fn udp_peer(&mut self, id: DatagramId) -> Result<Option<UdpPeer>, Refused> {
        self.stack.peer(id.0).map_err(refused)
    }

    /// Queues `payload` for the connected socket's peer; one not connected is refused
    /// [`Refused::NotConnected`].
    pub fn udp_send(&mut self, now: Instant, id: DatagramId, payload: &[u8]) -> Result<(), Refused> {
        let sent = self.stack.send_connected(now, id.0, payload).map_err(refused);
        self.log(now);
        sent
    }

    /// Queues `payload` for `destination:port`; it leaves in a later [`Node::transmit`].
    pub fn udp_send_to(&mut self, now: Instant, id: DatagramId, destination: Ipv4Addr, port: u16, payload: &[u8]) -> Result<(), Refused> {
        let sent = self.stack.send_to(now, id.0, destination, port, payload).map_err(refused);
        self.log(now);
        sent
    }

    /// `holder`'s receive: the oldest datagram that reached the socket, in `out`, or `None` when
    /// none waits. One longer than `out` is cut to it and the rest is gone, as [udp] counts.
    pub fn udp_recv_from(&mut self, id: DatagramId, holder: HolderId, out: &mut [u8]) -> Result<Option<Datagram>, Refused> {
        if !self.datagrams.get_mut(&id).is_some_and(|holders| holders.asked(holder)) {
            return Err(Refused::NotConnected);
        }
        // Told whatever it answers: a pending error answered leaves the datagrams behind it.
        let received = self.stack.recv_from(id.0, out);
        self.tell_datagram_holders();
        Ok(received.map_err(refused)?.map(|received| Datagram { len: received.len, source: received.source, source_port: received.source_port }))
    }

    /// `holder` of socket `id` left. The last holder's leaving closes the socket: its port is
    /// free at once, what it received is gone, and what it had accepted still leaves, to [udp]'s
    /// bound. Either gives a place back.
    pub fn datagram_departed(&mut self, now: Instant, id: DatagramId, holder: HolderId) {
        let Some(holders) = self.datagrams.get_mut(&id) else { return };
        if !holders.remove(holder) {
            return;
        }
        if holders.is_empty() {
            self.datagrams.remove(&id);
            if let Err(error) = self.stack.close(now, id.0) {
                unreachable!("[udp] refused the close of a socket the node holds: {error:?}");
            }
        }
        self.tell_holders();
    }

    /// The clients' datagram sockets the node holds.
    pub fn datagrams(&self) -> usize {
        self.datagrams.len()
    }

    /// The clients' datagram sockets the node holds, and their holders past each first.
    pub(crate) fn datagram_places(&self) -> usize {
        self.datagrams.values().map(Holders::more).fold(self.datagrams.len(), usize::saturating_add)
    }

    /// Tells each holder of a socket a datagram, or its pending error, waits for.
    pub(crate) fn tell_datagram_holders(&mut self) {
        for (id, holders) in &mut self.datagrams {
            let Ok(readable) = self.stack.readable(id.0) else { unreachable!("[udp] lost a socket the node holds") };
            holders.tell(readable);
        }
    }
}
