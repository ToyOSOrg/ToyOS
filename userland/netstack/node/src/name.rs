//! This machine's name on its link: `<host>.local` answers with the held lease's address
//! (`toyos-mdns`, RFC 6762). Every decision about a message, and every byte of a probe or an
//! answer, is `toyos_mdns::Responder`'s; this is its socket on the group's port, its membership
//! of the group, its clock and its draws.
//!
//! - The record is the held lease's on a link that is up: its address and prefix are what the
//!   responder is told, and it is told it has no link from the moment the lease goes or the link
//!   does. Nothing is probed for, announced or answered for an address still under probe, or on a
//!   link that is down.
//! - A link that comes back under a held lease is a link after none: the lease outlives the link,
//!   so no new address would say the name is in doubt (§8). The responder probes for it again.
//! - Every message that has arrived is handed over before the responder is asked what it owes: a
//!   conflicting response received as a probing ends takes the name before it is claimed (§8.1).
//! - What the name is owed later, a probe, a lost name's next among them, an announcement (§8.3)
//!   or an answer §6 held back, is a deadline of the node's ([`Name::next_deadline`]).
//! - Each probing the responder starts spends one draw, the delay before its first probe (§8.1).
//! - What became of the name, claimed or lost, is a line of [`Node::drain_events`].
//! - Every message leaves with TTL 255 (§11).
//! - An answer goes where the responder says: to the group on its port, or to the asker's own
//!   address and port. One [udp] refuses is counted and dropped: the asker asks again.
//!
//! **Untrusted input.** A message's bytes are read by the responder's parser alone. Its source
//! address and port are the wire's, and are a destination only once the responder took the
//! source for one on this link, which is the lease's prefix or 169.254/16, and [udp] accepted it
//! as one.

use alloc::vec;
use alloc::vec::Vec;
use core::net::Ipv4Addr;

use toyos_mdns::{Host, Link, Responder, Source, To, GROUP, PORT};
use toyos_net_udp::SocketId;
use toyos_net_wire::ipv4::{MulticastAddr, Ttl};
use toyos_net_wire::{Instant, Port};

use crate::lease::Stack;
use crate::{Event, Node};

pub(crate) struct Name {
    socket: SocketId,
    record: Responder<'static>,
    /// Room for the largest datagram [udp] delivers.
    message: Vec<u8>,
}

/// `now` on the clock `toyos-mdns` and `toyos-dns` keep: whole milliseconds of the node's.
pub(crate) fn millis(now: Instant) -> u64 {
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
        Ok(Self { socket, record: Responder::new(host), message: vec![0; toyos_net_udp::limits::MAX_PAYLOAD] })
    }

    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.record.owed_at().map(Instant::from_millis)
    }

    /// Tells the responder what lease is held and whether the link is `up`, hands it every
    /// message that arrived and sends what each is answered, then sends what the name is owed
    /// now. What became of the name goes to `events`. Returns how many messages [udp] refused.
    fn pass(&mut self, now: Instant, stack: &mut Stack, up: bool, events: &mut Vec<Event>, draw: &mut impl FnMut() -> u32) -> u64 {
        let link = stack.lease().filter(|_| up).map(|lease| Link { addr: lease.address.octets(), prefix: lease.prefix_len });
        let (ms, group) = (millis(now), Ipv4Addr::from(GROUP));
        let mut unsent = 0u64;
        self.record.on(link, ms);
        loop {
            let received = match stack.recv_from(self.socket, &mut self.message) {
                Ok(Some(received)) => received,
                Ok(None) => break,
                Err(_) => unreachable!("the responder's socket is never closed and never connected"),
            };
            let Some(message) = self.message.get(..received.len) else { continue };
            // Port 0 is no port to answer at: [udp] refuses the answer.
            let port = received.source_port.map_or(0, Port::get);
            let (answer, event) = self.record.heard(message, Source { addr: received.source.octets(), port }, ms);
            events.extend(event.map(Event::Name));
            let Some(answer) = answer else { continue };
            let (to, port) = match answer.to {
                To::Group => (group, PORT),
                To::Asker => (received.source, port),
            };
            unsent = unsent.saturating_add(send(stack, now, self.socket, to, port, &answer.bytes));
        }
        let (owed, event) = self.record.owed(ms, draw);
        events.extend(event.map(Event::Name));
        if let Some(owed) = owed {
            unsent = unsent.saturating_add(send(stack, now, self.socket, group, PORT, &owed));
        }
        unsent
    }
}

impl Node {
    /// Answers for `<host>.local` from here on, once it is claimed: the node joins the multicast
    /// DNS group and holds its port, and a lease already held on a link that is up is probed on
    /// by this call, which then spends one draw. Refused when the port is taken, by a client's
    /// socket or by an earlier call.
    pub fn answer_as(&mut self, now: Instant, host: Host<'static>, mut draw: impl FnMut() -> u32) -> Result<(), toyos_net_udp::Error> {
        self.name = Some(Name::new(now, &mut self.stack, host)?);
        self.serve_name(now, &mut draw);
        Ok(())
    }

    /// The name's pass, if it has been started, and the count of what [udp] refused it.
    pub(crate) fn serve_name(&mut self, now: Instant, draw: &mut impl FnMut() -> u32) {
        if let Some(name) = &mut self.name {
            let unsent = name.pass(now, &mut self.stack, self.up, &mut self.events, draw);
            self.counters.add(crate::Counter::NameUnsent, unsent);
        }
    }
}
