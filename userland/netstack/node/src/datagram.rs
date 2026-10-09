//! A client's datagram sockets on [udp]: bind, send, receive and close. A socket is named by a
//! [`DatagramId`], which only [`Node::udp_bind`] makes, so no call here names the DHCP client's
//! socket or the responder's; their ports are taken like any other.
//!
//! **Refusals.** [udp] refuses by rule and counts each under its own name. A client is answered
//! in one of the pipe ABI's words ([`Refused`]): the shell writes the word's code and decides
//! nothing. A destination, a port or a length is the client's number and is refused, never
//! trusted; a datagram off the wire reaches a socket only through [udp]'s demultiplexer.
//!
//! **A socket holds a place** (`places`) from its bind to its close: what it makes the node hold
//! is its two queues, `limits::RX_DATAGRAMS` received and `limits::TX_DATAGRAMS` accepted. A bind
//! with no place left is answered [`Refused::ResourceExhausted`] with nothing made.
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
//! dropped and counted `udp.tx-unreachable`, and the client is told nothing: its socket is not
//! connected.
//!
//! A refusal [udp] logs is in [`Node::drain_events`] when the send that met it returns, and one
//! [ip] logs as a datagram leaves when that [`Node::transmit`] returns; no other call of a
//! client's meets one.

use core::net::Ipv4Addr;

use toyos_net_udp::{Counter, Error, SocketId};
use toyos_net_wire::{Instant, Port};

use crate::Node;

/// One client socket. An id outlives its socket only as a refusal: [udp] never hands a closed
/// socket's id to another.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DatagramId(SocketId);

/// Why a call was refused, in the pipe ABI's words (`toyos::net`'s `ERR_*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The port is another socket's, or no ephemeral port is free.
    AddrInUse,
    /// The id names no socket, or the machine has no address or route to send from.
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

/// [udp]'s refusal as the client hears it. A client's socket is bound and never connected, and
/// never names its source, so the rules of those calls, and the counters that are no refusal,
/// are not its to meet.
fn refused(error: Error) -> Refused {
    let rule = match error {
        Error::NoSuchSocket => return Refused::NotConnected,
        Error::Refused(rule) => rule,
        Error::Failed(_) => unreachable!("[udp] reported a network error against a socket that is not connected"),
    };
    match rule {
        Counter::PortInUse | Counter::NoEphemeralPort => Refused::AddrInUse,
        Counter::NoSourceAddress | Counter::NoRoute | Counter::SourceAddressNotAssigned => Refused::NotConnected,
        Counter::BindAddressNotLocal
        | Counter::SendLoopback
        | Counter::SendToSelf
        | Counter::SendInvalidDestination
        | Counter::SendPortZero
        | Counter::SendUnspecifiedDestination
        | Counter::ExceedsMtu => Refused::InvalidInput,
        Counter::BroadcastNotPermitted => Refused::PermissionDenied,
        Counter::TxQueueFull => Refused::ResourceExhausted,
        Counter::ConnectUnspecified
        | Counter::ConnectGroup
        | Counter::ConnectPortZero
        | Counter::ConnectedDestinationMismatch
        | Counter::NotConnected
        | Counter::SourceNotPermitted
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
    /// the port it holds.
    pub fn udp_bind(&mut self, addr: Ipv4Addr, port: Option<Port>, draw: impl FnOnce() -> u32) -> Result<(DatagramId, Port), Refused> {
        if self.room() == 0 {
            return Err(Refused::ResourceExhausted);
        }
        let (id, port) = self.stack.bind(addr, port, draw).map_err(refused)?;
        self.sockets = self.sockets.saturating_add(1);
        Ok((DatagramId(id), port))
    }

    /// Whether the socket's datagrams may go to a broadcast address, the limited one or the
    /// directed one of a prefix the machine holds, from here on.
    pub fn udp_set_broadcast(&mut self, id: DatagramId, permitted: bool) -> Result<(), Refused> {
        self.stack.set_broadcast(id.0, permitted).map_err(refused)
    }

    /// Queues `payload` for `destination:port`; it leaves in a later [`Node::transmit`].
    pub fn udp_send_to(&mut self, now: Instant, id: DatagramId, destination: Ipv4Addr, port: u16, payload: &[u8]) -> Result<(), Refused> {
        let sent = self.stack.send_to(now, id.0, destination, port, payload).map_err(refused);
        self.log();
        sent
    }

    /// The oldest datagram that reached the socket, in `out`, or `None` when none waits. One
    /// longer than `out` is cut to it and the rest is gone, as [udp] counts.
    pub fn udp_recv_from(&mut self, id: DatagramId, out: &mut [u8]) -> Result<Option<Datagram>, Refused> {
        let received = self.stack.recv_from(id.0, out).map_err(refused)?;
        Ok(received.map(|received| Datagram { len: received.len, source: received.source, source_port: received.source_port }))
    }

    /// Closes the socket: its port is free at once, what it received is gone, and what it had
    /// accepted still leaves, to [udp]'s bound. Its place is back.
    pub fn udp_close(&mut self, now: Instant, id: DatagramId) -> Result<(), Refused> {
        self.stack.close(now, id.0).map_err(refused)?;
        self.sockets = self.sockets.saturating_sub(1);
        self.wake_owners(now);
        Ok(())
    }
}
