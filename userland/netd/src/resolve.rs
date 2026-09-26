//! The machine's one resolver: a name's IPv4 addresses, asked of the servers
//! the lease named. std's `lookup_host` and libc's `getaddrinfo` both reach it
//! through `toyos::net::dns_lookup`. Every decision is `toyos_dns`'s. This
//! module owns the sockets, the clock, the query IDs and the clients waiting
//! for answers.
//!
//! **Each query leaves from a socket of its own**, bound to a port drawn from
//! the kernel's random source, and carries an ID drawn from the same source
//! (RFC 5452 §9.2). An off-path sender has to guess both to be read at all,
//! and then has to be the server the query went to. A socket of its own is
//! also what keeps one query from waiting on another: smoltcp keeps a datagram
//! at the head of its socket's queue while its server's link address is
//! unresolved or no route leads to it, and a query queued behind it on the
//! same socket would never leave.
//!
//! **A lookup's wait is a wake of netd's loop** ([`Resolver::wake_in`]), and a
//! reply is a frame, which the NIC's interrupt already wakes it for.

use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use smoltcp::iface::{SocketHandle, SocketSet};
use smoltcp::socket::udp;
use smoltcp::wire::{IpAddress, IpEndpoint, IpListenEndpoint, Ipv4Address, DHCP_MAX_DNS_SERVER_COUNT};
use toyos_dns::{Asked, Failure, Lookup, Name, Step};

/// Lookups in flight at once. Policy: each holds a socket and its buffers per
/// query waiting for its answer until it ends, and a client may start any
/// number of them.
pub const MAX_LOOKUPS: usize = 16;

/// Datagrams, and bytes, a query's socket holds between two passes: two of the
/// largest an unfragmented Ethernet frame carries. A reply to a query sent
/// without EDNS is at most 512 bytes (RFC 1035 §4.2.1), and a larger one still
/// reaches the reader, which judges it.
const RECEIVE_PACKETS: usize = 2;
const RECEIVE_BUFFER: usize = RECEIVE_PACKETS * (1500 - 20 - 8);

/// A query is at most a header, a 255-byte name and four bytes, and its socket
/// sends nothing else.
const QUERY_BYTES: usize = 12 + 255 + 4;

/// The ports a query's socket is bound in: IANA's dynamic range (RFC 6335
/// §6).
const EPHEMERAL: std::ops::RangeInclusive<u16> = 49152..=65535;

/// Why a lookup was not started.
#[derive(Debug)]
pub enum Refused {
    /// The lease named no server, or there is no lease.
    NoServer,
    /// [`MAX_LOOKUPS`] are in flight, or every port in [`EPHEMERAL`] is bound.
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
    /// Every port in [`EPHEMERAL`] is bound, so the next query had none to
    /// leave from.
    NoPort,
}

struct Pending<C> {
    client: C,
    name: Name,
    lookup: Lookup,
    /// The socket each query waiting for its answer left from, and the newest
    /// query's, which may not have left yet.
    queries: Vec<(Asked, SocketHandle)>,
}

/// The lookups in flight, and the servers a new one asks.
pub struct Resolver<C, D> {
    servers: Vec<[u8; 4]>,
    lookups: Vec<Pending<C>>,
    /// The origin of every lookup's clock.
    born: Instant,
    /// Every query's ID and port.
    draw: D,
}

/// A value the kernel's random source drew. netd ends by name if the source
/// refuses: an ID or a port anyone can predict is a forged answer's way in.
pub fn random_u16() -> u16 {
    let mut bytes = [0u8; 2];
    toyos_abi::syscall::random(&mut bytes)
        .unwrap_or_else(|e| panic!("netd: the kernel's random source refused a query's ID or port: {e:?}"));
    u16::from_le_bytes(bytes)
}

/// Whether a UDP socket in `socket_set` is bound to `port`, whatever its
/// address. smoltcp hands a datagram to the first socket that accepts it, so a
/// second socket on a port would receive nothing its first did not refuse.
pub fn udp_port_taken(socket_set: &SocketSet<'_>, port: u16) -> bool {
    socket_set.iter().any(|(_, socket)| match socket {
        smoltcp::socket::Socket::Udp(s) => s.endpoint().port == port,
        _ => false,
    })
}

impl<C, D: FnMut() -> u16> Resolver<C, D> {
    /// A resolver whose clock starts at `born`, drawing every query's ID and
    /// port from `draw`.
    pub fn new(born: Instant, draw: D) -> Self {
        Self { servers: Vec::new(), lookups: Vec::new(), born, draw }
    }

    /// The servers the lease named, or none. A lookup already in flight keeps
    /// the servers it started with.
    ///
    /// **An address no query can be sent to is not kept.** The lease is the
    /// network's word: an unspecified, broadcast or multicast server would have
    /// every query to it refused by the socket, so it is named and left out.
    pub fn set_servers(&mut self, servers: &[Ipv4Address]) {
        assert!(
            servers.len() <= DHCP_MAX_DNS_SERVER_COUNT,
            "netd: a lease carries at most {DHCP_MAX_DNS_SERVER_COUNT} resolvers, and this one {}",
            servers.len()
        );
        self.servers.clear();
        for server in servers {
            if server.is_unspecified() || server.is_broadcast() || server.is_multicast() {
                crate::say!("netd: the lease names {server} as a resolver, which no query can reach; not asking it");
                continue;
            }
            self.servers.push(server.octets());
        }
    }

    /// Start looking up `name` for `client`.
    pub fn start(&mut self, client: C, name: Name, socket_set: &mut SocketSet<'_>, now: Instant) -> Result<(), (C, Refused)> {
        if self.servers.is_empty() {
            return Err((client, Refused::NoServer));
        }
        if self.lookups.len() == MAX_LOOKUPS {
            return Err((client, Refused::Full));
        }
        let (lookup, step) = Lookup::start(name.clone(), &self.servers, self.ms(now), (self.draw)())
            .expect("the server list was checked not empty");
        let mut pending = Pending { client, name, lookup, queries: Vec::new() };
        match act(&mut pending, step, socket_set, &mut self.draw) {
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
    pub fn pass(&mut self, socket_set: &mut SocketSet<'_>, now: Instant) -> Vec<(C, Name, Result<Vec<[u8; 4]>, Ended>)> {
        let now_ms = self.ms(now);
        let mut ended = Vec::new();
        let mut i = 0;
        while i < self.lookups.len() {
            let pending = &mut self.lookups[i];
            let mut done = None;
            let mut q = 0;
            while done.is_none() && q < pending.queries.len() {
                let (asked, handle) = pending.queries[q];
                let socket = socket_set.get_mut::<udp::Socket>(handle);
                let (reply, from, port) = match socket.recv() {
                    Ok((reply, meta)) => {
                        let IpAddress::Ipv4(from) = meta.endpoint.addr;
                        (reply.to_vec(), from.octets(), meta.endpoint.port)
                    }
                    Err(udp::RecvError::Exhausted) => {
                        q += 1;
                        continue;
                    }
                    Err(udp::RecvError::Truncated) => {
                        unreachable!("netd: recv hands back the whole datagram and truncates nothing")
                    }
                };
                let step = pending.lookup.on_datagram(asked, from, port, &reply, now_ms, (self.draw)());
                done = act(pending, step, socket_set, &mut self.draw);
                // The step may have let this query go: its socket is read
                // again from wherever it now is, or every socket from the
                // first.
                q = pending.queries.iter().position(|&(a, _)| a == asked).unwrap_or(0);
            }
            if done.is_none() {
                let step = pending.lookup.on_time(now_ms, (self.draw)());
                done = act(pending, step, socket_set, &mut self.draw);
            }
            match done {
                None => i += 1,
                Some(result) => {
                    let pending = self.lookups.swap_remove(i);
                    close(&pending, socket_set);
                    ended.push((pending.client, pending.name, result));
                }
            }
        }
        ended
    }

    /// End every lookup whose client `gone` says has left, at once: nobody is
    /// waiting for its answer, and its sockets and its slot are another's.
    pub fn let_go(&mut self, socket_set: &mut SocketSet<'_>, mut gone: impl FnMut(&C) -> bool) {
        let mut i = 0;
        while i < self.lookups.len() {
            if gone(&self.lookups[i].client) {
                close(&self.lookups.swap_remove(i), socket_set);
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
/// longer answered, and answer the lookup's end where it ended.
///
/// **A query whose wait ended before it left is let go with its socket.**
/// Nothing can answer it, and smoltcp would go on asking for its server's link
/// address once a second, which is its one discovery a second for every
/// neighbour: the next server's would wait behind it for as long as it lived.
fn act<C>(
    pending: &mut Pending<C>,
    step: Step,
    socket_set: &mut SocketSet<'_>,
    draw: &mut impl FnMut() -> u16,
) -> Option<Result<Vec<[u8; 4]>, Ended>> {
    let done = match step {
        Step::Ask { asked, to, query } => match free_port(socket_set, draw()) {
            Some(port) => {
                let handle = socket_set.add(query_socket(port));
                send(socket_set.get_mut::<udp::Socket>(handle), to, &query);
                pending.queries.push((asked, handle));
                None
            }
            None => Some(Err(Ended::NoPort)),
        },
        Step::Wait => None,
        Step::Done(result) => Some(result.map_err(Ended::Failed)),
    };
    let lookup = &pending.lookup;
    let newest = pending.queries.last().map(|&(asked, _)| asked);
    pending.queries.retain(|&(asked, handle)| {
        let unsent = Some(asked) != newest && socket_set.get::<udp::Socket>(handle).send_queue() > 0;
        let waiting = !unsent && lookup.waiting().any(|w| w == asked);
        if !waiting {
            socket_set.remove(handle);
        }
        waiting
    });
    done
}

/// Let go of every socket `pending` holds.
fn close<C>(pending: &Pending<C>, socket_set: &mut SocketSet<'_>) {
    for &(_, handle) in &pending.queries {
        socket_set.remove(handle);
    }
}

/// A socket for one query, bound to `port` on every address.
fn query_socket(port: u16) -> udp::Socket<'static> {
    let buffer = |packets, bytes| udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; packets], vec![0u8; bytes]);
    let mut socket = udp::Socket::new(buffer(RECEIVE_PACKETS, RECEIVE_BUFFER), buffer(1, QUERY_BYTES));
    socket
        .bind(IpListenEndpoint { addr: None, port })
        .unwrap_or_else(|e| panic!("netd: a fresh socket refused the free port {port}: {e:?}"));
    socket
}

/// Queue `query` for port 53 of `to` on its own fresh socket.
fn send(socket: &mut udp::Socket, to: [u8; 4], query: &[u8]) {
    let at = IpEndpoint::new(IpAddress::Ipv4(Ipv4Addr::from(to)), toyos_dns::PORT);
    socket
        .send_slice(query, at)
        .unwrap_or_else(|e| panic!("netd: a fresh socket's empty one-query queue refused its query to {at}: {e:?}"));
}

/// A port in [`EPHEMERAL`] no UDP socket holds, found from `drawn`.
fn free_port(socket_set: &SocketSet<'_>, drawn: u16) -> Option<u16> {
    let span = u32::from(EPHEMERAL.end() - EPHEMERAL.start()) + 1;
    let start = u32::from(drawn) % span;
    (0..span)
        .map(|k| EPHEMERAL.start() + ((start + k) % span) as u16)
        .find(|&port| !udp_port_taken(socket_set, port))
}

#[cfg(test)]
mod tests;
