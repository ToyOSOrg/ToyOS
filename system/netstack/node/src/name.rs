//! This machine's name on each of its links: `<host>.local` answers with that link's held
//! lease's address (`toyos-mdns`, RFC 6762). Every decision about a message, and every byte of a
//! probe or an answer, is one link's `toyos_mdns::Responder`'s; this is their one socket on the
//! group's port, their membership of the group on each link, their clock and their draws.
//!
//! - **One name, a responder per link.** A message is its arrival link's responder's alone, and
//!   what a responder sends leaves by its link alone. A name lost on one link is that link's: the
//!   responder there probes for the same name again later (§8.1), and the others keep it.
//! - **A message from one of this machine's own addresses never reaches a responder**: [ip]
//!   refuses a datagram whose source any of its interfaces holds (`ip.own-source`), so on one LAN
//!   two links share, as a wired and a wireless link bridged (§14), this machine hears its own
//!   messages from neither and never conflicts with itself. The one exception is a message that
//!   crosses the bridge as its sending link loses the address it came from: [ip] no longer holds
//!   that source, a millisecond's race, and the other link's responder may read it as another
//!   host's.
//! - The record is the link's held lease's on a link that is up: its address and prefix are what the
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

use alloc::collections::BTreeMap;
use alloc::vec;
use alloc::vec::Vec;
use core::net::Ipv4Addr;

use toyos_mdns::{Host, Link, Responder, Source, To, GROUP, PORT};
use toyos_net_shard::IfIndex;
use toyos_net_udp::SocketId;
use toyos_net_wire::ipv4::{MulticastAddr, Ttl};
use toyos_net_wire::{Instant, Port};

use crate::lease::Stack;
use crate::{Event, Node};

pub(crate) struct Name {
    host: Host<'static>,
    socket: SocketId,
    /// Each link's record, by the link.
    records: BTreeMap<IfIndex, Responder<'static>>,
    /// Room for the largest datagram [udp] delivers.
    message: Vec<u8>,
}

/// `now` on the clock `toyos-mdns` and `toyos-dns` keep: whole milliseconds of the node's.
pub(crate) fn millis(now: Instant) -> u64 {
    u64::try_from(now.since(Instant::from_millis(0)).as_millis()).unwrap_or(u64::MAX)
}

fn group() -> MulticastAddr {
    let Some(group) = MulticastAddr::new(Ipv4Addr::from(GROUP)) else { unreachable!("RFC 6762 §3's group is a group") };
    group
}

impl Name {
    /// Binds the group's port and sets the TTL of what leaves it, then joins the group on every
    /// link. Refused, nothing was joined.
    fn new(now: Instant, stack: &mut Stack, host: Host<'static>) -> Result<Self, toyos_net_udp::Error> {
        let Ok(ttl) = Ttl::new(u8::MAX) else { unreachable!("255 is a TTL") };
        let (socket, _) = stack.bind(Ipv4Addr::UNSPECIFIED, Port::new(PORT), || 0)?;
        stack.set_ttl(socket, ttl, ttl)?;
        let mut name = Self { host, socket, records: BTreeMap::new(), message: vec![0; toyos_net_udp::limits::MAX_PAYLOAD] };
        let links: Vec<IfIndex> = stack.links().collect();
        for link in links {
            name.add_link(now, stack, link);
        }
        Ok(name)
    }

    /// Joins the group on `link`, whose record begins with no address.
    pub(crate) fn add_link(&mut self, now: Instant, stack: &mut Stack, link: IfIndex) {
        stack.join(now, link, group());
        self.records.insert(link, Responder::new(self.host));
    }

    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.records.values().filter_map(Responder::owed_at).min().map(Instant::from_millis)
    }

    /// Tells each link's responder what lease is held there and whether the link is up, hands
    /// each message that arrived to its link's responder and sends what it is answered by that
    /// link, then sends what each link's name is owed now. What became of a name goes to
    /// `events`. Returns how many messages [udp] refused.
    fn pass(&mut self, now: Instant, stack: &mut Stack, events: &mut Vec<Event>, draw: &mut impl FnMut() -> u32) -> u64 {
        let (ms, group) = (millis(now), Ipv4Addr::from(GROUP));
        let mut unsent = 0u64;
        for (&link, record) in &mut self.records {
            let on = stack.lease(link).filter(|_| stack.up(link)).map(|lease| Link { addr: lease.address.octets(), prefix: lease.prefix_len });
            record.on(on, ms);
        }
        let socket = self.socket;
        let send = |stack: &mut Stack, link: IfIndex, to: Ipv4Addr, port: u16, message: &[u8]| u64::from(stack.send_via(now, socket, link, to, port, message).is_err());
        loop {
            let received = match stack.recv_from(self.socket, &mut self.message) {
                Ok(Some(received)) => received,
                Ok(None) => break,
                Err(_) => unreachable!("the responders' socket is never closed and never connected"),
            };
            let (Some(message), Some(record)) = (self.message.get(..received.len), self.records.get_mut(&received.iface)) else { continue };
            // Port 0 is no port to answer at: [udp] refuses the answer.
            let port = received.source_port.map_or(0, Port::get);
            let (answer, event) = record.heard(message, Source { addr: received.source.octets(), port }, ms);
            events.extend(event.map(|event| Event::Name { link: received.iface, event }));
            let Some(answer) = answer else { continue };
            let (to, port) = match answer.to {
                To::Group => (group, PORT),
                To::Asker => (received.source, port),
            };
            unsent = unsent.saturating_add(send(stack, received.iface, to, port, &answer.bytes));
        }
        for (&link, record) in &mut self.records {
            let (owed, event) = record.owed(ms, &mut *draw);
            events.extend(event.map(|event| Event::Name { link, event }));
            if let Some(owed) = owed {
                unsent = unsent.saturating_add(send(stack, link, group, PORT, &owed));
            }
        }
        unsent
    }
}

impl Node {
    /// Answers for `<host>.local` from here on, on every link, once it is claimed there: the node
    /// joins the multicast DNS group on each and holds its port, and a lease already held on a
    /// link that is up is probed on by this call, which then spends one draw for each. Refused
    /// when the port is taken, by a client's socket or by an earlier call.
    pub fn answer_as(&mut self, now: Instant, host: Host<'static>, mut draw: impl FnMut() -> u32) -> Result<(), toyos_net_udp::Error> {
        self.name = Some(Name::new(now, &mut self.stack, host)?);
        self.serve_name(now, &mut draw);
        Ok(())
    }

    /// The names' pass, if they have been started, and the count of what [udp] refused them.
    pub(crate) fn serve_name(&mut self, now: Instant, draw: &mut impl FnMut() -> u32) {
        if let Some(name) = &mut self.name {
            let unsent = name.pass(now, &mut self.stack, &mut self.events, draw);
            self.counters.add(crate::Counter::NameUnsent, unsent);
        }
    }
}
