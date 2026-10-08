//! The interface's address, gateway and resolvers as one state, and its one writer.
//!
//! The state is `Unaddressed`, `Probing` an address a server acknowledged, or `Held`: a lease
//! whose address [ip] verified. The shard is private to this module, so nothing else in the crate
//! reaches `add_address`, `remove_address` or `set_gateways`:
//!
//! - the gateway is written only with a held lease, and [ip] withdraws it with the address, so no
//!   route outlives its address;
//! - a lease is held only against a [`Verified`], which only [`Stack::report`] makes, from [ip]'s
//!   report for the address under probe, so no address is in use before conflict detection ends;
//! - the resolvers are the held lease's, so they come and go with it.
//!
//! [ip] reports on no address but the one this module added, and one report at a time: each is
//! read against the state as it stands.

use alloc::collections::VecDeque;
use core::net::Ipv4Addr;

use toyos_dhcp::{Lease, Transmission};
use toyos_net_shard::{Config, Event, Refusal, Shard};
use toyos_net_udp::SocketId;
use toyos_net_wire::ethernet::MacAddr;
use toyos_net_wire::Instant;

/// The port a DHCP server listens on (RFC 2131 §4.1).
const SERVER_PORT: u16 = 67;

#[derive(Debug)]
enum State {
    Unaddressed,
    /// Tentative in [ip]: nothing sends from it and it answers nothing.
    Probing(Ipv4Addr),
    Held(Lease),
}

/// Proof that [ip] finished conflict detection for the address under probe.
pub(crate) struct Verified(Ipv4Addr);

/// What the shard said, one event at a time.
pub(crate) enum Report {
    Refused { refusal: Refusal, suppressed: u64 },
    Verified(Verified),
    /// Another host holds the address, at this MAC: [ip] removed it, and the state is
    /// `Unaddressed`.
    Conflict(MacAddr),
    /// The link went down under the probe: [ip] removed the address, and the state is
    /// `Unaddressed`.
    NotVerified,
}

pub(crate) struct Stack {
    shard: Shard,
    /// The DHCP client's socket.
    socket: SocketId,
    state: State,
    inbox: VecDeque<Event>,
}

/// [ip] refuses nothing about the one interface and the one address this module gave it.
fn agreed<T>(answer: Result<T, toyos_net_ip::Counter>) -> T {
    match answer {
        Ok(value) => value,
        Err(refusal) => unreachable!("[ip] refused its own interface or address: {}", refusal.name()),
    }
}

impl Stack {
    /// One interface, link down, unaddressed.
    pub(crate) fn new(now: Instant, config: Config) -> Result<Self, toyos_net_tcp::ConfigError> {
        let mut shard = Shard::new(now, config)?;
        let Ok(socket) = shard.acquisition() else { unreachable!("a new shard has no socket on port 68") };
        Ok(Self { shard, socket, state: State::Unaddressed, inbox: VecDeque::new() })
    }

    pub(crate) fn shard(&self) -> &Shard {
        &self.shard
    }

    pub(crate) fn lease(&self) -> Option<&Lease> {
        match &self.state {
            State::Held(lease) => Some(lease),
            State::Unaddressed | State::Probing(_) => None,
        }
    }

    fn address(&self) -> Option<Ipv4Addr> {
        match &self.state {
            State::Unaddressed => None,
            State::Probing(address) => Some(*address),
            State::Held(lease) => Some(lease.address),
        }
    }

    // ---- the device and the clock ----

    pub(crate) fn receive(&mut self, now: Instant, frame: &[u8]) {
        self.shard.receive(now, frame);
    }

    pub(crate) fn transmit(&mut self, now: Instant, credit: usize, sink: impl FnMut(&[u8])) -> usize {
        self.shard.transmit(now, credit, sink)
    }

    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.shard.next_deadline()
    }

    pub(crate) fn fire(&mut self, now: Instant) {
        self.shard.fire(now);
    }

    pub(crate) fn link(&mut self, now: Instant, up: bool) {
        agreed(if up { self.shard.link_up(now) } else { self.shard.link_down(now) });
    }

    // ---- the DHCP client's socket ----

    /// Queues a message of the client's; a refusal is [udp]'s, by the rule it counted.
    pub(crate) fn send(&mut self, now: Instant, message: &Transmission) -> Result<(), toyos_net_udp::Error> {
        self.shard.send_from(now, self.socket, message.source, message.destination, SERVER_PORT, &message.payload)
    }

    /// The oldest datagram that reached the client's socket, in `out`, and the address it came
    /// from.
    pub(crate) fn recv<'a>(&mut self, out: &'a mut [u8]) -> Option<(&'a [u8], Ipv4Addr)> {
        let Ok(received) = self.shard.recv_from(self.socket, out) else {
            unreachable!("the client's socket is never closed and never connected")
        };
        let received = received?;
        Some((out.get(..received.len)?, received.source))
    }

    // ---- the lease ----

    /// The shard's next event, read against the state.
    pub(crate) fn report(&mut self) -> Option<Report> {
        if self.inbox.is_empty() {
            self.inbox.extend(self.shard.drain_events());
        }
        Some(match self.inbox.pop_front()? {
            Event::Refused { refusal, suppressed } => Report::Refused { refusal, suppressed },
            Event::Verified(addr) => match self.state {
                State::Probing(address) if address == addr => Report::Verified(Verified(addr)),
                _ => unreachable!("[ip] verified {addr}, which is not under probe"),
            },
            Event::Conflict { addr, mac } | Event::Lost { addr, mac } => {
                self.taken(addr);
                Report::Conflict(mac)
            }
            Event::NotVerified(addr) => {
                self.taken(addr);
                Report::NotVerified
            }
        })
    }

    /// [ip] removed `addr` itself, its gateways with it.
    fn taken(&mut self, addr: Ipv4Addr) {
        if self.address() != Some(addr) {
            unreachable!("[ip] removed {addr}, which the lease does not name");
        }
        self.state = State::Unaddressed;
    }

    /// Nothing is held: the address goes, and [ip] withdraws the gateway with it.
    pub(crate) fn release(&mut self, now: Instant) {
        if let Some(address) = self.address() {
            agreed(self.shard.remove_address(now, address));
        }
        self.state = State::Unaddressed;
    }

    /// A server acknowledged `address`: [ip] probes it, and nothing uses it yet. Refused, the
    /// state is `Unaddressed`.
    pub(crate) fn probe(&mut self, now: Instant, address: Ipv4Addr, prefix_len: u8) -> Result<(), toyos_net_ip::Counter> {
        self.release(now);
        self.shard.add_address(now, address, prefix_len)?;
        self.state = State::Probing(address);
        Ok(())
    }

    /// The verified address's lease is in use from here: [ip] holds the address at the prefix
    /// `probe` gave it, and the router is written with the lease. Returns whether [ip] refused the
    /// router.
    pub(crate) fn hold(&mut self, now: Instant, verified: Verified, mut lease: Lease) -> bool {
        if lease.address != verified.0 {
            unreachable!("the client configured {}, having probed {}", lease.address, verified.0);
        }
        let refused = self.route(now, &mut lease);
        self.state = State::Held(lease);
        refused
    }

    /// The held address acknowledged again, with new times or a new prefix, router or resolvers.
    /// Returns whether [ip] refused its router. A prefix [ip] refuses changes nothing: the lease
    /// stays as it was held.
    pub(crate) fn renew(&mut self, now: Instant, mut lease: Lease) -> Result<bool, toyos_net_ip::Counter> {
        if !matches!(&self.state, State::Held(held) if held.address == lease.address) {
            unreachable!("the client renewed {}, which is not held", lease.address);
        }
        self.shard.add_address(now, lease.address, lease.prefix_len)?;
        let refused = self.route(now, &mut lease);
        self.state = State::Held(lease);
        Ok(refused)
    }

    /// Writes `lease`'s router, which [ip] takes only on a usable prefix. One [ip] refuses is no
    /// router: the lease is left without one, and so is [ip]. Returns whether it was refused.
    fn route(&mut self, now: Instant, lease: &mut Lease) -> bool {
        let refused = self.shard.set_gateways(now, lease.router.as_slice()).is_err();
        if refused {
            // [ip] kept the list it had.
            agreed(self.shard.set_gateways(now, &[]));
            lease.router = None;
        }
        refused
    }
}
