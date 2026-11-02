//! Netstack's decisions, pure: ToyOS's own stack (`toyos-net-shard`) over every link a driver
//! brings, each with the DHCP client (`toyos-dhcp`) that addresses it. Frames and time come in;
//! frames leave only inside [`Node::transmit`], against the credit each link offers, so there is
//! no frame without a transmit slot. Nothing here reads a clock, draws randomness or does I/O: the caller hands in
//! the time, every draw and the shard's secrets.
//!
//! The node carries out what each link's client asks and tells it what became of its address. What the
//! interface holds is `lease`'s to write and nobody else's: see that module for the three rules
//! its types keep. A client's TCP connection and the region it lent are `streams`', a passive open
//! and its owner's wakes `listeners`'.
//!
//! A client's datagram sockets are `datagram`'s, the machine's `<host>.local` name is `name`'s
//! and the lookups of other machines' names are `resolve`'s: each reads the lease and writes none
//! of it. How many streams, listeners and clients' datagram sockets the node holds is `places`';
//! the responder's socket and a lookup's stand outside it.
//!
//! **Untrusted input.** A received frame is never read here: every byte goes through
//! `toyos-net-wire`'s parsers inside the shard, and a DHCP payload through the client's. What
//! either refuses is counted where it was refused and, where it is a log line, comes out of
//! [`Node::drain_events`], at most one a rule in any 10 s with a count of the rest: a peer's
//! frames never write the log faster than that.
//!
//! **Draws.** Each `draw` is handed to the client, whose order is its own: a call that starts an
//! exchange draws its transaction id first. The name draws after it, the delay of each probing it
//! starts. The lookups in flight draw after both, and alone in [`Node::transmit`], which serves no
//! name: an id and then a port for each query they send, and nothing else.

#![no_std]
#![forbid(unsafe_code)]
#![forbid(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::as_conversions
)]

extern crate alloc;

mod datagram;
mod holders;
mod lease;
mod listeners;
mod name;
mod places;
mod resolve;
mod streams;

pub use holders::{Bell, HolderId};
pub use listeners::{AcceptRefused, Accepted, ListenRefused, ListenerId};
pub use streams::{ConnectRefused, ExportRefused, Memory, Session, StreamEvent, StreamId};

use alloc::vec;
use alloc::vec::Vec;

use toyos_dhcp::{AddressRequest, Client, HostName, Lease, Output};
pub use toyos_net_shard::{Credit, IfIndex, Rank};
use toyos_net_shard::{Config, Shard};
use toyos_net_wire::ethernet::IndividualMac;
use toyos_net_wire::Instant;

pub use datagram::{Datagram, DatagramId, Refused};
use lease::{Report, Stack, Verified};
pub use resolve::{Ended, LookupId, NotStarted, Resolved, Resolving};

toyos_net_wire::counters! {
    DhcpUnsent = "node.dhcp-unsent";
    DhcpOtherLink = "node.dhcp-other-link";
    AddressRefused = "node.address-refused";
    RouterRefused = "node.router-refused";
    NameUnsent = "node.name-unsent";
    QueryUnsent = "node.query-unsent";
    QueryFailed = "node.query-failed";
}

/// A line for the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// A refusal of the stack's, and how many of its rule it stands for beyond itself.
    Stack { refusal: toyos_net_shard::Refusal, suppressed: u64 },
    /// A refusal of a link's client's, and how many of its rule it stands for beyond itself.
    Dhcp { link: IfIndex, refusal: toyos_dhcp::Refusal, suppressed: u64 },
    /// What became of the machine's name on a link.
    Name { link: IfIndex, event: toyos_mdns::Event },
}

/// One link's DHCP client.
struct LinkClient {
    link: IfIndex,
    mac: IndividualMac,
    client: Client,
    /// Its refusals pass through it as the stack's pass through the shard's: one line a rule in
    /// any 10 s, carrying how many were not.
    log: toyos_dhcp::RefusalLog,
}

pub struct Node {
    stack: Stack,
    /// The name each link's client asks its network to record.
    host_name: Option<HostName>,
    /// Each link's DHCP client, its link's address and the log its refusals pass through, in
    /// the order the links were added.
    clients: Vec<LinkClient>,
    /// The responders for the machine's name, once [`Self::answer_as`] started them.
    name: Option<name::Name>,
    resolver: resolve::Resolver,
    counters: Counters,
    events: Vec<Event>,
    /// Room for the largest message the client accepts.
    datagram: Vec<u8>,
    streams: streams::Streams,
    listeners: listeners::Listeners,
    /// The clients' datagram sockets bound and not closed, and their holders.
    datagrams: alloc::collections::BTreeMap<DatagramId, holders::Holders>,
    /// How many streams, listeners, datagram sockets and connections [tcp] finishes alone the
    /// node holds at most.
    places: usize,
}

impl Node {
    /// No link, and no place for a stream or a listener until [`Self::set_places`]. It serves
    /// from here: what needs a link is refused for want of a route until one comes.
    pub fn new(now: Instant, config: Config, host_name: Option<HostName>) -> Result<Self, toyos_net_tcp::ConfigError> {
        Ok(Self {
            stack: Stack::new(now, config)?,
            host_name,
            clients: Vec::new(),
            name: None,
            resolver: resolve::Resolver::new(),
            counters: Counters::default(),
            events: Vec::new(),
            datagram: vec![0; usize::from(toyos_dhcp::limits::MAX_MESSAGE)],
            streams: streams::Streams::default(),
            listeners: listeners::Listeners::default(),
            datagrams: alloc::collections::BTreeMap::new(),
            places: 0,
        })
    }

    /// A link, down and unaddressed, ranked against the others by `rank`: its client begins here,
    /// and its first DISCOVER is not sent: an exchange starts when the link comes up
    /// ([`Self::link`]). The machine's name, once answered, is served on it too.
    pub fn add_link(&mut self, now: Instant, mac: IndividualMac, rank: Rank, draw: impl FnMut() -> u32) -> IfIndex {
        let iface = self.stack.add_link(now, mac, rank);
        let (client, _unsent) = Client::start(now, mac, self.host_name.clone(), draw);
        self.clients.push(LinkClient { link: iface, mac, client, log: toyos_dhcp::RefusalLog::default() });
        if let Some(name) = &mut self.name {
            name.add_link(now, &mut self.stack, iface);
        }
        iface
    }

    pub fn shard(&self) -> &Shard {
        self.stack.shard()
    }

    pub fn counters(&self) -> &Counters {
        &self.counters
    }

    /// Every link, in the order added.
    pub fn links(&self) -> impl Iterator<Item = IfIndex> + '_ {
        self.stack.links()
    }

    /// `link`'s lease in use: its address verified, its router installed. `None` from the moment
    /// either goes.
    pub fn lease(&self, link: IfIndex) -> Option<&Lease> {
        self.stack.lease(link)
    }

    /// The link whose lease's resolvers lookups ask.
    pub fn asked(&self) -> Option<IfIndex> {
        self.stack.asked()
    }

    /// Log lines since the last call.
    pub fn drain_events(&mut self) -> impl Iterator<Item = Event> + '_ {
        self.events.drain(..)
    }

    fn client(&mut self, link: IfIndex) -> &mut Client {
        match self.clients.iter_mut().find(|c| c.link == link) {
            Some(c) => &mut c.client,
            None => unreachable!("{link:?} is no link of the node's"),
        }
    }

    // ---- the devices ----

    /// The frames `link` received since the last call, one by one and at most `budget`: `next`
    /// hands its next frame to the sink it is given and answers `false` once it had none. One
    /// pass over the streams follows the last frame, so what the batch brought reaches each
    /// client in one write and not one a frame. `true` is a batch that stopped at the budget, and
    /// another is owed without waiting for the device to say so.
    ///
    /// **The budget is the device's ring**: a device may refill it as fast as it is read, and a
    /// batch that waited for it to empty would keep the deadlines, the transmit opportunity and
    /// every stream's pass waiting with it, while a bound below what one interrupt's wait can
    /// bring would owe a second pass for every long wait.
    pub fn receive(&mut self, now: Instant, link: IfIndex, budget: usize, mut next: impl FnMut(&mut dyn FnMut(&[u8])) -> bool, mut draw: impl FnMut() -> u32) -> bool {
        let mut taken = 0usize;
        let owed = loop {
            if taken == budget {
                break true;
            }
            if !next(&mut |frame| {
                self.stack.receive(now, link, frame);
                self.settle(now, &mut draw);
            }) {
                break false;
            }
            taken = taken.saturating_add(1);
        };
        self.pass(now, false);
        owed
    }

    /// A transmit opportunity with `credit`'s room on each link, each frame handed to `sink` as
    /// it is built, with the link it leaves by. Returns how many left. A lookup's query that found
    /// no way out as its turn came is read here, and the query asked in its place leaves in this
    /// opportunity while credit lasts.
    pub fn transmit(&mut self, now: Instant, mut credit: impl Credit, mut sink: impl FnMut(IfIndex, &[u8]), mut draw: impl FnMut() -> u32) -> usize {
        let mut sent = 0usize;
        loop {
            sent = sent.saturating_add(self.stack.transmit(now, &mut credit, &mut sink));
            // A datagram [ip] refused as it left is a line of this opportunity.
            self.log(now);
            let asked = self.resolver.pass(now, &mut self.stack, &mut self.counters, &mut draw);
            if !asked || !self.stack.links().any(|link| credit.room(link)) {
                break;
            }
        }
        self.pass(now, true);
        sent
    }

    /// `link` came up or went down; the caller reports a change, not a state. Down, a held lease
    /// stays and the machine's name has no link there; up, the link's client verifies a lease it
    /// holds and otherwise starts over, and the name of a lease that stayed is probed for again.
    pub fn link(&mut self, now: Instant, link: IfIndex, up: bool, mut draw: impl FnMut() -> u32) {
        self.stack.link(now, link, up);
        if up {
            let out = self.client(link).link_up(now, &mut draw);
            self.carry_out(now, link, out, None, &mut draw);
        }
        self.settle(now, &mut draw);
    }

    // ---- the clock ----

    pub fn next_deadline(&self) -> Option<Instant> {
        let name = self.name.as_ref().and_then(name::Name::next_deadline);
        let clients = self.clients.iter().filter_map(|c| c.client.next_deadline());
        self.stack.next_deadline().into_iter().chain(clients).chain(name).chain(self.resolver.next_deadline()).chain(self.streams.next_deadline()).min()
    }

    /// Every deadline at or before `now`; the frames they make due wait for [`Self::transmit`].
    pub fn fire(&mut self, now: Instant, mut draw: impl FnMut() -> u32) {
        self.stack.fire(now);
        self.settle(now, &mut draw);
        for at in 0..self.clients.len() {
            let Some(c) = self.clients.get_mut(at) else { continue };
            let link = c.link;
            let out = c.client.timer(now, &mut draw);
            self.carry_out(now, link, out, None, &mut draw);
        }
        self.settle(now, &mut draw);
        self.pass(now, false);
    }

    /// Hands each link's client what the shard reported and what reached its socket, and carries
    /// out what it answers, until neither has more; then the name is served and the lookups are
    /// carried on, against the leases as that left them.
    fn settle(&mut self, now: Instant, draw: &mut impl FnMut() -> u32) {
        loop {
            self.log(now);
            let (link, out, verified) = if let Some((link, report)) = self.stack.report() {
                let client = self.client(link);
                match report {
                    Report::Verified(verified) => (link, client.verified(now, &mut *draw), Some(verified)),
                    Report::Conflict(mac) => (link, client.conflict(now, mac, &mut *draw), None),
                    Report::NotVerified => (link, client.not_verified(now, &mut *draw), None),
                }
            } else if let Some((payload, from, link)) = self.stack.recv(&mut self.datagram) {
                // On a LAN two of the links share, a reply for one link's client reaches the
                // other's too: it is that link's, and this one's client never hears it.
                let other = toyos_dhcp::addressed_to(payload)
                    .is_some_and(|to| self.clients.iter().any(|c| c.link != link && c.mac.get() == to));
                if other {
                    self.counters.add(Counter::DhcpOtherLink, 1);
                    continue;
                }
                let Some(c) = self.clients.iter_mut().find(|c| c.link == link) else { unreachable!("[ip] delivered from no link of the node's") };
                (link, c.client.receive(now, payload, from, &mut *draw), None)
            } else {
                break;
            };
            self.carry_out(now, link, out, verified, draw);
        }
        self.serve_name(now, draw);
        self.resolver.pass(now, &mut self.stack, &mut self.counters, draw);
    }

    /// The stack's and the clients' log lines so far, into [`Self::drain_events`]: the one way a
    /// line leaves any of them.
    fn log(&mut self, now: Instant) {
        let events = &mut self.events;
        self.stack.refusals(|refusal, suppressed| events.push(Event::Stack { refusal, suppressed }));
        for c in &mut self.clients {
            for refusal in c.client.drain_refusals() {
                if let Some(suppressed) = c.log.admit(now, refusal.rule) {
                    events.push(Event::Dhcp { link: c.link, refusal, suppressed });
                }
            }
        }
    }

    /// What one call of `link`'s client asked for: the lease first, so a message leaves from the
    /// state its call left behind.
    fn carry_out(&mut self, now: Instant, link: IfIndex, out: Output, verified: Option<Verified>, draw: &mut impl FnMut() -> u32) {
        use toyos_dhcp::Config::{Configured, Deconfigured, Extended, Reconfigured};
        let installed = match (out.config, verified) {
            (Some(Configured(lease)), Some(verified)) => Some(Ok(self.stack.hold(now, link, verified, lease))),
            (Some(Configured(lease)), None) => unreachable!("the client configured {} unverified", lease.address),
            (Some(Extended(lease) | Reconfigured(lease)), _) => Some(self.stack.renew(now, link, lease)),
            // Verified, and the client has no use for it: its lease ran out under the probe.
            (Some(Deconfigured(_)), _) | (None, Some(_)) => {
                self.stack.release(now, link);
                None
            }
            (None, None) => None,
        };
        match installed {
            Some(Ok(true)) => self.counters.add(Counter::RouterRefused, 1),
            Some(Err(_)) => self.counters.add(Counter::AddressRefused, 1),
            Some(Ok(false)) | None => {}
        }
        let mut transmit = out.transmit;
        match out.address {
            Some(AddressRequest::Probe { address, prefix_len }) => {
                if self.stack.probe(now, link, address, prefix_len).is_err() {
                    self.counters.add(Counter::AddressRefused, 1);
                    // An address [ip] will not probe is one the client never hears about again:
                    // its exchange starts over.
                    transmit = self.client(link).not_verified(now, &mut *draw).transmit;
                }
            }
            // Only of a link reported up twice: one that drops under a probe is `NotVerified` first.
            Some(AddressRequest::Cancel(_)) => self.stack.release(now, link),
            None => {}
        }
        if let Some(message) = transmit {
            if self.stack.send(now, link, &message).is_err() {
                self.counters.add(Counter::DhcpUnsent, 1);
            }
        }
    }
}
