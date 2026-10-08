//! This machine's name on its link: `<host>.local` answers with the held lease's address
//! (`toyos-mdns`, RFC 6762). Every decision about a query, and every byte of an answer, is
//! `toyos_mdns::Responder`'s; this is its socket on the group's port, its membership of the
//! group, and its clock.
//!
//! - The record is the held lease's: its address and prefix are what the responder is told, and
//!   it is told there is none from the moment the lease goes. Nothing is announced or answered
//!   for an address still under probe.
//! - What the record is owed later, the second announcement (§8.3) or an answer §6 held back,
//!   is a deadline of the node's ([`Name::next_deadline`]).
//! - Every response leaves with TTL 255 (§11).
//! - An answer goes where the responder says: to the group on its port, or to the asker's own
//!   address and port. One [udp] refuses is counted and dropped: the asker asks again.
//!
//! **Untrusted input.** A query's bytes are read by the responder's parser alone. Its source
//! address and port are the wire's, and are a destination only once the responder found the
//! source on this link and [udp] accepted it as one.

use alloc::vec;
use alloc::vec::Vec;
use core::net::Ipv4Addr;

use toyos_mdns::{Asker, Host, Link, Responder, To, GROUP, PORT};
use toyos_net_udp::SocketId;
use toyos_net_wire::ipv4::{MulticastAddr, Ttl};
use toyos_net_wire::{Instant, Port};

use crate::lease::Stack;
use crate::Node;

pub(crate) struct Name {
    socket: SocketId,
    record: Responder<'static>,
    /// Room for the largest datagram [udp] delivers.
    query: Vec<u8>,
}

/// `now` on the responder's clock: whole milliseconds of the node's.
fn millis(now: Instant) -> u64 {
    u64::try_from(now.since(Instant::from_millis(0)).as_millis()).unwrap_or(u64::MAX)
}

/// Queues one message of the responder's. Returns how many [udp] refused: one or none.
fn send(stack: &mut Stack, now: Instant, socket: SocketId, to: Ipv4Addr, port: u16, message: &[u8]) -> u64 {
    u64::from(stack.send_to(now, socket, to, port, message).is_err())
}

impl Name {
    /// Binds the group's port, sets the TTL of what leaves it and joins the group. Refused,
    /// nothing was joined.
    fn new(now: Instant, stack: &mut Stack, host: Host<'static>) -> Result<Self, toyos_net_udp::Error> {
        let (Ok(ttl), Some(group)) = (Ttl::new(u8::MAX), MulticastAddr::new(Ipv4Addr::from(GROUP))) else {
            unreachable!("255 is a TTL and RFC 6762 §3's group is a group")
        };
        let (socket, _) = stack.bind(Ipv4Addr::UNSPECIFIED, Port::new(PORT), || 0)?;
        stack.set_ttl(socket, ttl, ttl)?;
        stack.join(now, group);
        Ok(Self { socket, record: Responder::new(host), query: vec![0; toyos_net_udp::limits::MAX_PAYLOAD] })
    }

    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.record.owed_at().map(Instant::from_millis)
    }

    /// Tells the responder what lease is held, sends what the record is owed now, then answers
    /// every query that arrived. Returns how many messages [udp] refused.
    pub(crate) fn pass(&mut self, now: Instant, stack: &mut Stack) -> u64 {
        let link = stack.lease().map(|lease| Link { addr: lease.address.octets(), prefix: lease.prefix_len });
        let (ms, group) = (millis(now), Ipv4Addr::from(GROUP));
        let mut unsent = 0u64;
        if let Some(record) = self.record.on(link, ms) {
            unsent = unsent.saturating_add(send(stack, now, self.socket, group, PORT, &record));
        }
        loop {
            let received = match stack.recv_from(self.socket, &mut self.query) {
                Ok(Some(received)) => received,
                Ok(None) => return unsent,
                Err(_) => unreachable!("the responder's socket is never closed and never connected"),
            };
            let Some(query) = self.query.get(..received.len) else { continue };
            // Port 0 is no port to answer at: [udp] refuses the answer.
            let port = received.source_port.map_or(0, Port::get);
            let Some(answer) = self.record.answer(query, Asker { addr: received.source.octets(), port }, ms) else { continue };
            let (to, port) = match answer.to {
                To::Group => (group, PORT),
                To::Asker => (received.source, port),
            };
            unsent = unsent.saturating_add(send(stack, now, self.socket, to, port, &answer.bytes));
        }
    }
}

impl Node {
    /// Answers for `<host>.local` from here on: the node joins the multicast DNS group and holds
    /// its port. Refused when the port is taken, by a client's socket or by an earlier call.
    pub fn answer_as(&mut self, now: Instant, host: Host<'static>) -> Result<(), toyos_net_udp::Error> {
        self.name = Some(Name::new(now, &mut self.stack, host)?);
        Ok(())
    }
}
