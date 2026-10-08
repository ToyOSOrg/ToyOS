//! Netstack's decisions, pure: one interface of ToyOS's own stack (`toyos-net-shard`) and the
//! DHCP client (`toyos-dhcp`) that addresses it. Frames and time come in; frames leave only inside
//! [`Node::transmit`], against the credit the device offers, so there is no frame without a
//! transmit slot. Nothing here reads a clock, draws randomness or does I/O: the caller hands in
//! the time, every draw and the shard's secrets.
//!
//! The node carries out what the client asks and tells it what became of its address. What the
//! interface holds is `lease`'s to write and nobody else's: see that module for the three rules
//! its types keep.
//!
//! A client's datagram sockets are `datagram`'s and the machine's `<host>.local` name is `name`'s:
//! both read the lease and write none of it.
//!
//! **Untrusted input.** A received frame is never read here: every byte goes through
//! `toyos-net-wire`'s parsers inside the shard, and a DHCP payload through the client's. What
//! either refuses is counted where it was refused and, where it is a log line, comes out of
//! [`Node::drain_events`].
//!
//! **Draws.** Each `draw` is handed to the client, whose order is its own: a call that starts an
//! exchange draws its transaction id first.

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
mod lease;
mod name;

use alloc::vec;
use alloc::vec::Vec;

use toyos_dhcp::{AddressRequest, Client, HostName, Lease, Output};
use toyos_net_shard::{Config, Shard};
use toyos_net_wire::Instant;

pub use datagram::{Datagram, DatagramId, Refused};
use lease::{Report, Stack, Verified};

toyos_net_wire::counters! {
    DhcpUnsent = "node.dhcp-unsent";
    AddressRefused = "node.address-refused";
    RouterRefused = "node.router-refused";
    NameUnsent = "node.name-unsent";
}

/// A line for the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// A refusal of the stack's, and how many of its rule it stands for beyond itself.
    Stack { refusal: toyos_net_shard::Refusal, suppressed: u64 },
    Dhcp(toyos_dhcp::Refusal),
}

pub struct Node {
    stack: Stack,
    client: Client,
    /// The responder for the machine's name, once [`Self::answer_as`] started it.
    name: Option<name::Name>,
    counters: Counters,
    events: Vec<Event>,
    /// Room for the largest message the client accepts.
    datagram: Vec<u8>,
}

impl Node {
    /// One interface, link down and unaddressed. The client begins here, and its first DISCOVER
    /// is not sent: an exchange starts over when the link comes up ([`Self::link`]).
    pub fn new(now: Instant, config: Config, host_name: Option<HostName>, draw: impl FnMut() -> u32) -> Result<Self, toyos_net_tcp::ConfigError> {
        let mac = config.mac;
        let stack = Stack::new(now, config)?;
        let (client, _unsent) = Client::start(now, mac, host_name, draw);
        Ok(Self {
            stack,
            client,
            name: None,
            counters: Counters::default(),
            events: Vec::new(),
            datagram: vec![0; usize::from(toyos_dhcp::limits::MAX_MESSAGE)],
        })
    }

    pub fn shard(&self) -> &Shard {
        self.stack.shard()
    }

    pub fn dhcp(&self) -> &Client {
        &self.client
    }

    pub fn counters(&self) -> &Counters {
        &self.counters
    }

    /// The lease in use: its address verified, its router installed, its resolvers the ones to
    /// ask. `None` from the moment any of them goes.
    pub fn lease(&self) -> Option<&Lease> {
        self.stack.lease()
    }

    /// Log lines since the last call.
    pub fn drain_events(&mut self) -> impl Iterator<Item = Event> + '_ {
        self.events.drain(..).chain(self.client.drain_refusals().map(Event::Dhcp))
    }

    // ---- the device ----

    /// A frame the device received.
    pub fn receive(&mut self, now: Instant, frame: &[u8], mut draw: impl FnMut() -> u32) {
        self.stack.receive(now, frame);
        self.settle(now, &mut draw);
    }

    /// A transmit opportunity with room for `credit` frames, each handed to `sink` as it is built.
    /// Returns how many left.
    pub fn transmit(&mut self, now: Instant, credit: usize, sink: impl FnMut(&[u8])) -> usize {
        self.stack.transmit(now, credit, sink)
    }

    /// The link came up or went down; the caller reports a change, not a state. Down, a held
    /// lease stays; up, the client verifies a lease it holds and otherwise starts over.
    pub fn link(&mut self, now: Instant, up: bool, mut draw: impl FnMut() -> u32) {
        self.stack.link(now, up);
        if up {
            let out = self.client.link_up(now, &mut draw);
            self.carry_out(now, out, None, &mut draw);
        }
        self.settle(now, &mut draw);
    }

    // ---- the clock ----

    pub fn next_deadline(&self) -> Option<Instant> {
        let name = self.name.as_ref().and_then(name::Name::next_deadline);
        self.stack.next_deadline().into_iter().chain(self.client.next_deadline()).chain(name).min()
    }

    /// Every deadline at or before `now`; the frames they make due wait for [`Self::transmit`].
    pub fn fire(&mut self, now: Instant, mut draw: impl FnMut() -> u32) {
        self.stack.fire(now);
        self.settle(now, &mut draw);
        let out = self.client.timer(now, &mut draw);
        self.carry_out(now, out, None, &mut draw);
        self.settle(now, &mut draw);
    }

    /// Hands the client what the shard reported and what reached its socket, and carries out
    /// what it answers, until neither has more; then the name is served, against the lease as
    /// that left it.
    fn settle(&mut self, now: Instant, draw: &mut impl FnMut() -> u32) {
        loop {
            let (out, verified) = if let Some(report) = self.stack.report() {
                match report {
                    Report::Refused { refusal, suppressed } => {
                        self.events.push(Event::Stack { refusal, suppressed });
                        continue;
                    }
                    Report::Verified(verified) => (self.client.verified(now, &mut *draw), Some(verified)),
                    Report::Conflict(mac) => (self.client.conflict(now, mac, &mut *draw), None),
                    Report::NotVerified => (self.client.not_verified(now, &mut *draw), None),
                }
            } else if let Some((payload, from)) = self.stack.recv(&mut self.datagram) {
                (self.client.receive(now, payload, from, &mut *draw), None)
            } else {
                break;
            };
            self.carry_out(now, out, verified, draw);
        }
        if let Some(name) = &mut self.name {
            let unsent = name.pass(now, &mut self.stack);
            self.counters.add(Counter::NameUnsent, unsent);
        }
    }

    /// What one call of the client's asked for: the lease first, so a message leaves from the
    /// state its call left behind.
    fn carry_out(&mut self, now: Instant, out: Output, verified: Option<Verified>, draw: &mut impl FnMut() -> u32) {
        use toyos_dhcp::Config::{Configured, Deconfigured, Extended, Reconfigured};
        let installed = match (out.config, verified) {
            (Some(Configured(lease)), Some(verified)) => Some(Ok(self.stack.hold(now, verified, lease))),
            (Some(Configured(lease)), None) => unreachable!("the client configured {} unverified", lease.address),
            (Some(Extended(lease) | Reconfigured(lease)), _) => Some(self.stack.renew(now, lease)),
            // Verified, and the client has no use for it: its lease ran out under the probe.
            (Some(Deconfigured(_)), _) | (None, Some(_)) => {
                self.stack.release(now);
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
                if self.stack.probe(now, address, prefix_len).is_err() {
                    self.counters.add(Counter::AddressRefused, 1);
                    // An address [ip] will not probe is one the client never hears about again:
                    // its exchange starts over.
                    transmit = self.client.not_verified(now, &mut *draw).transmit;
                }
            }
            // Only of a link reported up twice: one that drops under a probe is `NotVerified` first.
            Some(AddressRequest::Cancel(_)) => self.stack.release(now),
            None => {}
        }
        if let Some(message) = transmit {
            if self.stack.send(now, &message).is_err() {
                self.counters.add(Counter::DhcpUnsent, 1);
            }
        }
    }
}
