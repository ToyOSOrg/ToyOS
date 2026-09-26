//! The machine's one resolver: a name's IPv4 addresses, asked of the servers
//! the lease named. std's `lookup_host` and libc's `getaddrinfo` both reach it
//! through `toyos::net::dns_lookup`. Every decision is `toyos_dns`'s. This
//! module owns the sockets, the clock, the query IDs and the clients waiting
//! for answers.
//!
//! **Each lookup asks from a socket of its own**, bound to a port drawn from
//! the kernel's random source, and each of its queries carries an ID drawn from
//! the same source (RFC 5452 §9.2). An off-path sender has to guess both to be
//! read at all, and then has to be the server the query went to.
//!
//! **A lookup's wait is a wake of netd's loop** ([`Resolver::wake_in`]), and a
//! reply is a frame, which the NIC's interrupt already wakes it for.

use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use smoltcp::iface::{SocketHandle, SocketSet};
use smoltcp::socket::udp;
use smoltcp::wire::{IpAddress, IpEndpoint, IpListenEndpoint, Ipv4Address, DHCP_MAX_DNS_SERVER_COUNT};
use toyos_dns::{Failure, Lookup, Name, Step};

/// Lookups in flight at once. Policy: each holds a socket and its buffers
/// until it ends, and a client may start any number of them.
const MAX_LOOKUPS: usize = 16;

/// Bytes a lookup's socket holds between two passes. A reply to a query sent
/// without EDNS is at most 512 bytes (RFC 1035 §4.2.1); this holds several,
/// and a larger one still reaches the reader, which judges it.
const RECEIVE_BUFFER: usize = 8192;
const RECEIVE_PACKETS: usize = 8;

/// Every query one name's lookup can send. smoltcp keeps a datagram queued
/// while the gateway's link address is unresolved, so a gateway that never
/// answers leaves each of them queued. A reply is what ends a name's queries
/// and starts another's, and a reply means the queue has gone out ahead of it.
const SEND_PACKETS: usize = toyos_dns::ROUNDS * DHCP_MAX_DNS_SERVER_COUNT;
/// A query is at most a header, a 255-byte name and four bytes.
const SEND_BUFFER: usize = SEND_PACKETS * 512;

/// The ports a lookup's socket is bound in: IANA's dynamic range (RFC 6335
/// §6).
const EPHEMERAL: std::ops::RangeInclusive<u16> = 49152..=65535;

/// Why a lookup was not started.
pub enum Refused {
    /// The lease named no server, or there is no lease.
    NoServer,
    /// [`MAX_LOOKUPS`] are in flight, or every port in [`EPHEMERAL`] is bound.
    Full,
}

struct Pending<C> {
    client: C,
    name: Name,
    lookup: Lookup,
    handle: SocketHandle,
}

/// The lookups in flight, and the servers a new one asks.
pub struct Resolver<C> {
    servers: Vec<[u8; 4]>,
    lookups: Vec<Pending<C>>,
    /// The origin of every lookup's clock.
    born: Instant,
}

/// A value the kernel's random source drew.
fn random_u16() -> u16 {
    let mut bytes = [0u8; 2];
    toyos_abi::syscall::random(&mut bytes);
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

impl<C> Resolver<C> {
    pub fn new() -> Self {
        Self { servers: Vec::new(), lookups: Vec::new(), born: Instant::now() }
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
        let Some(port) = free_port(socket_set) else {
            return Err((client, Refused::Full));
        };
        let buffer = |packets, bytes| udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; packets], vec![0u8; bytes]);
        let mut socket = udp::Socket::new(buffer(RECEIVE_PACKETS, RECEIVE_BUFFER), buffer(SEND_PACKETS, SEND_BUFFER));
        socket
            .bind(IpListenEndpoint { addr: None, port })
            .unwrap_or_else(|e| panic!("netd: a fresh socket refused the free port {port}: {e:?}"));
        let handle = socket_set.add(socket);
        let (lookup, step) = Lookup::start(name.clone(), &self.servers, self.ms(now), random_u16())
            .expect("the server list was checked not empty");
        match step {
            Step::Ask { to, query } => send(socket_set.get_mut::<udp::Socket>(handle), to, &query),
            other => panic!("netd: a lookup's first step is a query, not {other:?}"),
        }
        self.lookups.push(Pending { client, name, lookup, handle });
        Ok(())
    }

    /// Read every reply that arrived and end every wait that is over; answer
    /// each lookup that ended with its client, the name it asked, and how it
    /// ended.
    pub fn pass(&mut self, socket_set: &mut SocketSet<'_>, now: Instant) -> Vec<(C, Name, Result<Vec<[u8; 4]>, Failure>)> {
        let now_ms = self.ms(now);
        let mut ended = Vec::new();
        let mut i = 0;
        while i < self.lookups.len() {
            let pending = &mut self.lookups[i];
            let socket = socket_set.get_mut::<udp::Socket>(pending.handle);
            let mut done = None;
            while done.is_none() {
                let step = match socket.recv() {
                    Ok((reply, meta)) => {
                        let IpAddress::Ipv4(from) = meta.endpoint.addr;
                        pending.lookup.on_datagram(from.octets(), meta.endpoint.port, reply, now_ms, random_u16())
                    }
                    Err(udp::RecvError::Exhausted) => break,
                    Err(udp::RecvError::Truncated) => {
                        unreachable!("netd: recv hands back the whole datagram and truncates nothing")
                    }
                };
                done = act(socket, step);
            }
            if done.is_none() {
                done = act(socket, pending.lookup.on_time(now_ms, random_u16()));
            }
            match done {
                None => i += 1,
                Some(result) => {
                    let pending = self.lookups.swap_remove(i);
                    socket_set.remove(pending.handle);
                    ended.push((pending.client, pending.name, result));
                }
            }
        }
        ended
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

/// Carry out `step` on `socket`, and answer the lookup's end where it ended.
fn act(socket: &mut udp::Socket, step: Step) -> Option<Result<Vec<[u8; 4]>, Failure>> {
    match step {
        Step::Ask { to, query } => {
            send(socket, to, &query);
            None
        }
        Step::Wait => None,
        Step::Done(result) => Some(result),
    }
}

/// Queue `query` for port 53 of `to`.
///
/// Neither refusal can happen: [`SEND_PACKETS`] holds every query a name's
/// lookup sends, and [`Resolver::set_servers`] keeps no address a datagram
/// cannot go to.
fn send(socket: &mut udp::Socket, to: [u8; 4], query: &[u8]) {
    let at = IpEndpoint::new(IpAddress::Ipv4(Ipv4Addr::from(to)), toyos_dns::PORT);
    socket.send_slice(query, at).unwrap_or_else(|e| panic!("netd: a lookup's socket refused its query to {at}: {e:?}"));
}

/// A port in [`EPHEMERAL`] no UDP socket holds, found from a random start.
fn free_port(socket_set: &SocketSet<'_>) -> Option<u16> {
    let span = u32::from(EPHEMERAL.end() - EPHEMERAL.start()) + 1;
    let start = u32::from(random_u16()) % span;
    (0..span)
        .map(|k| EPHEMERAL.start() + ((start + k) % span) as u16)
        .find(|&port| !udp_port_taken(socket_set, port))
}
