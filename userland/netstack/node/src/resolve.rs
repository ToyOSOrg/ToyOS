//! The machine's one resolver: a name's IPv4 addresses, asked of the resolvers the held lease
//! names. Every decision about a question and every byte of a reply is `toyos_dns::Lookup`'s;
//! this is its sockets on [udp], its clock and its draws.
//!
//! - **Each query leaves from a socket of its own, connected to its resolver's port 53**, on an
//!   ephemeral port [udp] picks from one draw, and carries an id that is another draw (RFC 5452
//!   §9.2): an off-path sender has to guess both to be read at all. [udp] delivers a connected
//!   socket only what its peer's address and port sent to the address the socket sends from
//!   (§9.1), so no reply is read from another source, at a broadcast or group address, or for a
//!   query other than the one whose socket it reached.
//! - **A socket lives as long as its query's answer is read**: it is closed when the lookup lets
//!   the query go, ends, or is let go itself, and its port is free from then.
//! - **A query [udp] refuses never left**: it is counted, holds no socket, and the lookup waits
//!   its wait out as for any query nobody answered. So does one the network reports back, its
//!   resolver unreachable or its port refused: counted, and waited out.
//! - **A lookup asks the resolvers of the lease it started under, and ends with that lease's
//!   word**: when the held lease names other resolvers, or none is held.
//! - **A wait is a deadline of the node's** ([`Resolver::next_deadline`]); a reply is a frame.
//! - **At most `toyos_dns::MAX_LOOKUPS` lookups are held**, an ended one until
//!   [`Node::take_resolved`] hands its answer over.
//!
//! **Untrusted input.** A reply's bytes are read by the lookup's reader alone, cut to the largest
//! datagram [udp] delivers.

use alloc::vec;
use alloc::vec::Vec;
use core::net::Ipv4Addr;

use toyos_dns::{Asked, Failure, Lookup, Name, Step, MAX_LOOKUPS, PORT};
use toyos_net_udp::{Error, SocketId};
use toyos_net_wire::Instant;

use crate::lease::Stack;
use crate::name::millis;
use crate::{Counter, Counters, Node};

/// One lookup, from [`Node::resolve`] until its answer is taken or it is let go.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LookupId(u64);

/// Why a lookup was not started, in the pipe ABI's words (`toyos::net`'s `ERR_*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotStarted {
    /// No lease is held, or the held one names no resolver.
    NotConnected,
    /// `toyos_dns::MAX_LOOKUPS` are held, or no ephemeral port is free: the same call succeeds
    /// later.
    ResourceExhausted,
}

/// How a lookup ended without an address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ended {
    /// What the resolvers' answers, or their silence, came to.
    Failed(Failure),
    /// Every ephemeral port is held, so the next query had none to leave from.
    NoPort,
    /// The lease the lookup started under went, or names other resolvers now.
    LeaseChanged,
}

/// A lookup that ended: the name's addresses, at least one, or why none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub id: LookupId,
    pub result: Result<Vec<[u8; 4]>, Ended>,
}

/// One query whose answer is still read.
#[derive(Clone, Copy)]
struct Query {
    asked: Asked,
    socket: SocketId,
    /// The resolver the socket is connected to.
    to: [u8; 4],
}

struct Asking {
    id: LookupId,
    lookup: Lookup,
    /// The resolvers of the lease the lookup started under.
    resolvers: Vec<Ipv4Addr>,
    queries: Vec<Query>,
}

pub(crate) struct Resolver {
    asking: Vec<Asking>,
    /// Ended, and not yet taken.
    ended: Vec<Resolved>,
    /// The id the next lookup gets.
    next: u64,
    /// Room for the largest datagram [udp] delivers.
    reply: Vec<u8>,
}

/// A query's id: the low half of one draw.
fn id(draw: &mut impl FnMut() -> u32) -> u16 {
    let [_, _, high, low] = draw().to_be_bytes();
    u16::from_be_bytes([high, low])
}

fn close(stack: &mut Stack, now: Instant, socket: SocketId) {
    if stack.close(now, socket).is_err() {
        unreachable!("a query's socket is closed once, by the lookup that bound it");
    }
}

/// The length of the next reply waiting at one of `queries`' sockets, now in `reply`, and the
/// query it is for. An error the network reported against a query is counted and read past.
fn waiting(queries: &[Query], stack: &mut Stack, reply: &mut [u8], counters: &mut Counters) -> Option<(Query, usize)> {
    for query in queries {
        loop {
            match stack.recv_from(query.socket, reply) {
                Ok(Some(received)) => return Some((*query, received.len)),
                Ok(None) => break,
                Err(Error::Failed(_)) => counters.add(Counter::QueryFailed, 1),
                Err(Error::NoSuchSocket | Error::Refused(_)) => unreachable!("a query's socket is open while it is listed, and no rule refuses a receive"),
            }
        }
    }
    None
}

/// Carries out `step` for `asking`, then closes the socket of every query the lookup no longer
/// reads an answer for. Returns how the lookup ended, if it did.
fn act(asking: &mut Asking, step: Step, now: Instant, stack: &mut Stack, counters: &mut Counters, draw: &mut impl FnMut() -> u32) -> Option<Result<Vec<[u8; 4]>, Ended>> {
    let done = match step {
        // An any-address bind that names no port is refused only for want of a free one.
        Step::Ask { asked, to, query } => match stack.bind(Ipv4Addr::UNSPECIFIED, None, &mut *draw) {
            Ok((socket, _)) => {
                let resolver = Ipv4Addr::from(to);
                if stack.connect(now, socket, resolver, PORT).is_ok() && stack.send_to(now, socket, resolver, PORT, &query).is_ok() {
                    asking.queries.push(Query { asked, socket, to });
                } else {
                    counters.add(Counter::QueryUnsent, 1);
                    close(stack, now, socket);
                }
                None
            }
            Err(_) => Some(Err(Ended::NoPort)),
        },
        Step::Wait => None,
        Step::Done(result) => Some(result.map_err(Ended::Failed)),
    };
    let lookup = &asking.lookup;
    asking.queries.retain(|query| {
        let read = lookup.waiting().any(|waiting| waiting == query.asked);
        if !read {
            close(stack, now, query.socket);
        }
        read
    });
    done
}

impl Resolver {
    pub(crate) fn new() -> Self {
        Self { asking: Vec::new(), ended: Vec::new(), next: 0, reply: vec![0; toyos_net_udp::limits::MAX_PAYLOAD] }
    }

    /// When the soonest lookup's wait is over.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.asking.iter().map(|asking| Instant::from_millis(asking.lookup.due())).min()
    }

    fn start(&mut self, now: Instant, name: Name, stack: &mut Stack, counters: &mut Counters, draw: &mut impl FnMut() -> u32) -> Result<LookupId, NotStarted> {
        let resolvers = stack.lease().map(|lease| lease.dns.clone()).unwrap_or_default();
        if resolvers.is_empty() {
            return Err(NotStarted::NotConnected);
        }
        if self.asking.len().saturating_add(self.ended.len()) >= MAX_LOOKUPS {
            return Err(NotStarted::ResourceExhausted);
        }
        let servers: Vec<[u8; 4]> = resolvers.iter().map(Ipv4Addr::octets).collect();
        let Some((lookup, step)) = Lookup::start(name, &servers, millis(now), || id(&mut *draw)) else { unreachable!("the lease names a resolver") };
        let mut asking = Asking { id: LookupId(self.next), lookup, resolvers, queries: Vec::new() };
        match act(&mut asking, step, now, stack, counters, &mut *draw) {
            None => {
                self.next = self.next.wrapping_add(1);
                let started = asking.id;
                self.asking.push(asking);
                Ok(started)
            }
            Some(Err(Ended::NoPort)) => Err(NotStarted::ResourceExhausted),
            Some(other) => unreachable!("a lookup's first step is a query, not {other:?}"),
        }
    }

    /// Ends every lookup whose lease went or names other resolvers, reads every reply that
    /// reached a query's socket, and ends every wait that is over. A lookup that ended closes
    /// its sockets and waits to be taken.
    pub(crate) fn pass(&mut self, now: Instant, stack: &mut Stack, counters: &mut Counters, draw: &mut impl FnMut() -> u32) {
        let Self { asking: lookups, ended, reply, .. } = self;
        let ms = millis(now);
        lookups.retain_mut(|asking| {
            let named = stack.lease().is_some_and(|lease| lease.dns == asking.resolvers);
            let mut done = if named { None } else { Some(Err(Ended::LeaseChanged)) };
            while done.is_none() {
                let Some((query, len)) = waiting(&asking.queries, stack, reply.as_mut_slice(), counters) else { break };
                let message = reply.get(..len).unwrap_or_default();
                // The socket is connected: [udp] delivered this from port 53 of the resolver the
                // query went to, or not at all.
                let step = asking.lookup.on_datagram(query.asked, query.to, PORT, message, ms, || id(&mut *draw));
                done = act(asking, step, now, stack, counters, &mut *draw);
            }
            if done.is_none() {
                let step = asking.lookup.on_time(ms, || id(&mut *draw));
                done = act(asking, step, now, stack, counters, &mut *draw);
            }
            let Some(result) = done else { return true };
            for query in &asking.queries {
                close(stack, now, query.socket);
            }
            ended.push(Resolved { id: asking.id, result });
            false
        });
    }

    fn let_go(&mut self, now: Instant, id: LookupId, stack: &mut Stack) {
        self.asking.retain(|asking| {
            if asking.id != id {
                return true;
            }
            for query in &asking.queries {
                close(stack, now, query.socket);
            }
            false
        });
        self.ended.retain(|resolved| resolved.id != id);
    }
}

impl Node {
    /// Starts looking `name` up at the resolvers the held lease names, which spends two draws:
    /// the first query's id, then its port. A call refused for want of a port has spent both;
    /// any other refused call spends none.
    pub fn resolve(&mut self, now: Instant, name: Name, mut draw: impl FnMut() -> u32) -> Result<LookupId, NotStarted> {
        self.resolver.start(now, name, &mut self.stack, &mut self.counters, &mut draw)
    }

    /// The lookups that ended since the last call, each once.
    pub fn take_resolved(&mut self) -> Vec<Resolved> {
        core::mem::take(&mut self.resolver.ended)
    }

    /// Nobody waits for `id`'s answer any more: its sockets close and its place is another's at
    /// once, and an answer not yet taken goes with it.
    pub fn let_go(&mut self, now: Instant, id: LookupId) {
        self.resolver.let_go(now, id, &mut self.stack);
    }
}
