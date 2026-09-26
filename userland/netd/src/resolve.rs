//! The machine's one resolver: a name's IPv4 addresses, asked of the servers
//! the lease named. std's `lookup_host` and libc's `getaddrinfo` both reach it
//! through `toyos::net::dns_lookup`. Every decision is `toyos_dns`'s. This
//! module owns the sockets, the clock, the query IDs and the clients waiting
//! for answers.
//!
//! **Each query leaves from a socket of its own**, on a port the stack draws
//! from the kernel's random source (RFC 6056), and carries an ID drawn from
//! the same source (RFC 5452 §9.2). An off-path sender has to guess both to be
//! read at all. A query the stack could not send — no route to its server — is
//! a query nobody answers, and the lookup's own wait moves on from it.
//!
//! **A lookup's wait is a wake of netd's loop** ([`Resolver::wake_in`]), and a
//! reply is a frame, which the NIC's interrupt already wakes it for.

use std::time::{Duration, Instant};

use net_types::ip::{Ipv4, Ipv4Addr};
use net_types::{SpecifiedAddr, ZonedAddr};
use netstack3_core::device::WeakDeviceId;
use netstack3_core::udp::{UdpRemotePort, UdpSocketId};
use packet::Buf;
use toyos_dns::{Asked, Failure, Lookup, Name, Step};
pub use toyos_dns::MAX_LOOKUPS;

use crate::net::Net;
use crate::stack::{Bindings, Inbox};

/// A UDP socket of the stack's.
pub type UdpId = UdpSocketId<Ipv4, WeakDeviceId<Bindings>, Bindings>;

/// Why a lookup was not started.
#[derive(Debug)]
pub enum Refused {
    /// The lease named no server, or there is no lease.
    NoServer,
    /// [`MAX_LOOKUPS`] are in flight, or the stack has no port left.
    Full,
}

impl Refused {
    /// What the client is answered.
    pub fn code(&self) -> u32 {
        match self {
            // This machine is on no network that answers names, which clears
            // when a lease lands.
            Self::NoServer => toyos::net::ERR_NOT_CONNECTED,
            Self::Full => toyos::net::ERR_RESOURCE_EXHAUSTED,
        }
    }
}

/// How a lookup ended without an address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ended {
    /// What the servers' answers, or their silence, came to.
    Failed(Failure),
    /// The stack had no port left for the next query to leave from.
    NoPort,
}

struct Pending<C> {
    client: C,
    name: Name,
    lookup: Lookup,
    /// The socket each query still read left from.
    queries: Vec<(Asked, UdpId)>,
}

/// The lookups in flight, and the servers a new one asks.
pub struct Resolver<C, D> {
    servers: Vec<[u8; 4]>,
    lookups: Vec<Pending<C>>,
    /// The origin of every lookup's clock.
    born: Instant,
    /// Every query's ID.
    draw: D,
}

/// A value the kernel's random source drew. netd ends by name if the source
/// refuses: an ID anyone can predict is a forged answer's way in.
pub fn random_u16() -> u16 {
    let mut bytes = [0u8; 2];
    toyos_abi::syscall::random(&mut bytes)
        .unwrap_or_else(|e| panic!("netd: the kernel's random source refused a query's ID: {e:?}"));
    u16::from_le_bytes(bytes)
}

impl<C, D: FnMut() -> u16> Resolver<C, D> {
    /// A resolver whose clock starts at `born`, drawing every query's ID from
    /// `draw`.
    pub fn new(born: Instant, draw: D) -> Self {
        Self { servers: Vec::new(), lookups: Vec::new(), born, draw }
    }

    /// The servers the lease named, or none. A lookup already in flight keeps
    /// the servers it started with.
    ///
    /// **An address no query can be sent to is not kept.** The lease is the
    /// network's word: an unspecified, broadcast or multicast server would have
    /// every query to it refused, so it is named and left out.
    pub fn set_servers(&mut self, servers: &[[u8; 4]]) {
        self.servers.clear();
        for &server in servers {
            let ip = std::net::Ipv4Addr::from(server);
            if ip.is_unspecified() || ip.is_broadcast() || ip.is_multicast() {
                crate::say!("netd: the lease names {ip} as a resolver, which no query can reach; not asking it");
                continue;
            }
            self.servers.push(server);
        }
    }

    /// Start looking up `name` for `client`.
    pub fn start(&mut self, client: C, name: Name, net: &mut Net, now: Instant) -> Result<(), (C, Refused)> {
        if self.servers.is_empty() {
            return Err((client, Refused::NoServer));
        }
        if self.lookups.len() == MAX_LOOKUPS {
            return Err((client, Refused::Full));
        }
        let (lookup, step) = Lookup::start(name.clone(), &self.servers, self.ms(now), (self.draw)())
            .expect("the server list was checked not empty");
        let mut pending = Pending { client, name, lookup, queries: Vec::new() };
        match act(&mut pending, step, net) {
            None => {
                self.lookups.push(pending);
                Ok(())
            }
            Some(Err(Ended::NoPort)) => Err((pending.client, Refused::Full)),
            Some(other) => panic!("netd: a lookup's first step is a query, not {other:?}"),
        }
    }

    /// Read every reply that arrived and end every wait that is over; answer
    /// each lookup that ended with its client, the name it asked, and how it
    /// ended.
    pub fn pass(&mut self, net: &mut Net, now: Instant) -> Vec<(C, Name, Result<Vec<[u8; 4]>, Ended>)> {
        let now_ms = self.ms(now);
        let mut ended = Vec::new();
        let mut i = 0;
        while i < self.lookups.len() {
            let pending = &mut self.lookups[i];
            let mut done = None;
            let mut q = 0;
            while done.is_none() && q < pending.queries.len() {
                let (asked, ref id) = pending.queries[q];
                let Some(reply) = id.external_data().take() else {
                    q += 1;
                    continue;
                };
                let step = pending.lookup.on_datagram(asked, reply.from, reply.port, &reply.bytes, now_ms, (self.draw)());
                done = act(pending, step, net);
                // The step may have let this query go: its socket is read
                // again from wherever it now is, or every socket from the
                // first.
                q = pending.queries.iter().position(|(a, _)| *a == asked).unwrap_or(0);
            }
            if done.is_none() {
                let step = pending.lookup.on_time(now_ms, (self.draw)());
                done = act(pending, step, net);
            }
            match done {
                None => i += 1,
                Some(result) => {
                    let pending = self.lookups.swap_remove(i);
                    close(pending.queries, net);
                    ended.push((pending.client, pending.name, result));
                }
            }
        }
        ended
    }

    /// End every lookup whose client `gone` says has left, at once: nobody is
    /// waiting for its answer, and its sockets and its slot are another's.
    pub fn let_go(&mut self, net: &mut Net, mut gone: impl FnMut(&C) -> bool) {
        let mut i = 0;
        while i < self.lookups.len() {
            if gone(&self.lookups[i].client) {
                close(self.lookups.swap_remove(i).queries, net);
            } else {
                i += 1;
            }
        }
    }

    /// The clients waiting for an answer.
    pub fn clients(&self) -> impl Iterator<Item = &C> {
        self.lookups.iter().map(|p| &p.client)
    }

    /// How many of the stack's sockets are the resolver's.
    pub fn sockets(&self) -> usize {
        self.lookups.iter().map(|p| p.queries.len()).sum()
    }

    /// How long until the soonest lookup's wait is over, if one is in flight.
    pub fn wake_in(&self, now: Instant) -> Option<Duration> {
        let now_ms = self.ms(now);
        self.lookups.iter().map(|p| Duration::from_millis(p.lookup.due().saturating_sub(now_ms))).min()
    }

    fn ms(&self, now: Instant) -> u64 {
        now.saturating_duration_since(self.born).as_millis() as u64
    }
}

/// Carry out `step` for `pending`, let go of the socket of every query no
/// longer read, and answer the lookup's end where it ended.
fn act<C>(pending: &mut Pending<C>, step: Step, net: &mut Net) -> Option<Result<Vec<[u8; 4]>, Ended>> {
    let done = match step {
        Step::Ask { asked, to, query } => match query_socket(net) {
            Some(id) => {
                send(net, &id, to, query);
                pending.queries.push((asked, id));
                None
            }
            None => Some(Err(Ended::NoPort)),
        },
        Step::Wait => None,
        Step::Done(result) => Some(result.map_err(Ended::Failed)),
    };
    let (keep, gone): (Vec<_>, Vec<_>) =
        std::mem::take(&mut pending.queries).into_iter().partition(|(asked, _)| pending.lookup.waiting().any(|w| w == *asked));
    pending.queries = keep;
    close(gone, net);
    done
}

/// Let go of every query socket in `queries`.
fn close(queries: Vec<(Asked, UdpId)>, net: &mut Net) {
    for (_, id) in queries {
        crate::stack::removed(net.api().udp::<Ipv4>().close(id));
    }
}

/// A socket for one query, bound on every address to a port the stack draws;
/// `None` once it has none left.
fn query_socket(net: &mut Net) -> Option<UdpId> {
    let api = net.api();
    let mut udp = api.udp::<Ipv4>();
    let id = udp.create_with(Inbox::default());
    match udp.listen(&id, None, None) {
        Ok(()) => Some(id),
        Err(_) => {
            crate::stack::removed(udp.close(id));
            None
        }
    }
}

/// Send `query` to port 53 of `to`. A query the stack refuses — no route to
/// its server — is named and left to the lookup's wait, like one lost.
fn send(net: &mut Net, id: &UdpId, to: [u8; 4], query: Vec<u8>) {
    let at = SpecifiedAddr::new(Ipv4Addr::new(to)).expect("a resolver kept is a specified address");
    let port = std::num::NonZeroU16::new(toyos_dns::PORT).expect("53 is a port");
    let sent = net.api().udp::<Ipv4>().send_to(
        id,
        Some(ZonedAddr::Unzoned(at)),
        UdpRemotePort::Set(port),
        Buf::new(query, ..),
        (),
    );
    if let Err(e) = sent {
        crate::say!("netd: a query to {} could not leave: {e:?}", crate::net::show(to));
    }
}

