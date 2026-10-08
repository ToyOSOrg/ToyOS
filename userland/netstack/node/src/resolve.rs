//! The machine's one resolver: a name's IPv4 addresses, asked of the resolvers the held lease
//! names. Every decision about a question and every byte of a reply is `toyos_dns::Lookup`'s;
//! this is its sockets on [udp], its clock and its draws.
//!
//! - **Each query leaves from a socket of its own**, on an ephemeral port [udp] picks from one
//!   draw, and carries an id that is another draw (RFC 5452 §9.2): an off-path sender has to
//!   guess both to be read at all. A reply is handed to the lookup as arriving for the query
//!   whose socket it reached, and for no other.
//! - **A socket lives as long as its query's answer is read**: it is closed when the lookup lets
//!   the query go, ends, or is let go itself, and its port is free from then.
//! - **A query [udp] refuses never left**: it is counted, holds no socket, and the lookup waits
//!   its wait out as for any query nobody answered.
//! - **A wait is a deadline of the node's** ([`Resolver::next_deadline`]); a reply is a frame.
//! - **At most `toyos_dns::MAX_LOOKUPS` lookups are held**, an ended one until
//!   [`Node::take_resolved`] hands its answer over.
//!
//! **Untrusted input.** A reply's bytes are read by the lookup's reader alone, cut to the largest
//! datagram [udp] delivers. Its source address and port are the wire's: the lookup reads a reply
//! only from port 53 of the resolver its query went to.

use alloc::vec;
use alloc::vec::Vec;
use core::net::Ipv4Addr;

use toyos_dns::{Asked, Failure, Lookup, Name, Step, MAX_LOOKUPS, PORT};
use toyos_net_udp::SocketId;
use toyos_net_wire::{Instant, Port};

use crate::datagram::Refused;
use crate::lease::Stack;
use crate::name::millis;
use crate::{Counter, Counters, Node};

/// One lookup, from [`Node::resolve`] until its answer is taken or it is let go.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LookupId(u64);

/// How a lookup ended without an address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ended {
    /// What the resolvers' answers, or their silence, came to.
    Failed(Failure),
    /// Every ephemeral port is held, so the next query had none to leave from.
    NoPort,
}

/// A lookup that ended: the name's addresses, at least one, or why none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub id: LookupId,
    pub result: Result<Vec<[u8; 4]>, Ended>,
}

struct Asking {
    id: LookupId,
    lookup: Lookup,
    /// The socket of each query whose answer is still read.
    queries: Vec<(Asked, SocketId)>,
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

/// Carries out `step` for `asking`, then closes the socket of every query the lookup no longer
/// reads an answer for. Returns how the lookup ended, if it did.
fn act(asking: &mut Asking, step: Step, now: Instant, stack: &mut Stack, counters: &mut Counters, draw: &mut impl FnMut() -> u32) -> Option<Result<Vec<[u8; 4]>, Ended>> {
    let done = match step {
        // An any-address bind that names no port is refused only for want of a free one.
        Step::Ask { asked, to, query } => match stack.bind(Ipv4Addr::UNSPECIFIED, None, &mut *draw) {
            Ok((socket, _)) => {
                if stack.send_to(now, socket, Ipv4Addr::from(to), PORT, &query).is_ok() {
                    asking.queries.push((asked, socket));
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
    asking.queries.retain(|&(asked, socket)| {
        let read = lookup.waiting().any(|waiting| waiting == asked);
        if !read {
            close(stack, now, socket);
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

    fn start(&mut self, now: Instant, name: Name, stack: &mut Stack, counters: &mut Counters, draw: &mut impl FnMut() -> u32) -> Result<LookupId, Refused> {
        let servers: Vec<[u8; 4]> = stack.lease().map(|lease| lease.dns.iter().map(Ipv4Addr::octets).collect()).unwrap_or_default();
        if servers.is_empty() {
            return Err(Refused::NotConnected);
        }
        if self.asking.len().saturating_add(self.ended.len()) >= MAX_LOOKUPS {
            return Err(Refused::ResourceExhausted);
        }
        let Some((lookup, step)) = Lookup::start(name, &servers, millis(now), id(&mut *draw)) else { unreachable!("the lease names a resolver") };
        let mut asking = Asking { id: LookupId(self.next), lookup, queries: Vec::new() };
        match act(&mut asking, step, now, stack, counters, &mut *draw) {
            None => {
                self.next = self.next.wrapping_add(1);
                let started = asking.id;
                self.asking.push(asking);
                Ok(started)
            }
            Some(Err(Ended::NoPort)) => Err(Refused::ResourceExhausted),
            Some(other) => unreachable!("a lookup's first step is a query, not {other:?}"),
        }
    }

    /// Reads every reply that reached a query's socket and ends every wait that is over. A
    /// lookup that ended closes its sockets and waits to be taken.
    pub(crate) fn pass(&mut self, now: Instant, stack: &mut Stack, counters: &mut Counters, draw: &mut impl FnMut() -> u32) {
        let Self { asking: lookups, ended, reply, .. } = self;
        let ms = millis(now);
        lookups.retain_mut(|asking| {
            let mut done = None;
            let mut at = 0usize;
            while done.is_none() {
                let Some(&(asked, socket)) = asking.queries.get(at) else { break };
                let received = match stack.recv_from(socket, reply.as_mut_slice()) {
                    Ok(Some(received)) => received,
                    Ok(None) => {
                        at = at.saturating_add(1);
                        continue;
                    }
                    Err(_) => unreachable!("a query's socket is open while it is listed, and never connected"),
                };
                let Some(message) = reply.get(..received.len) else { continue };
                // Port 0 is not port 53: the lookup drops it.
                let port = received.source_port.map_or(0, Port::get);
                let step = asking.lookup.on_datagram(asked, received.source.octets(), port, message, ms, id(&mut *draw));
                done = act(asking, step, now, stack, counters, &mut *draw);
                // The step may have let this query go: its socket is read again from wherever it
                // now is, or every socket from the first.
                at = asking.queries.iter().position(|&(listed, _)| listed == asked).unwrap_or(0);
            }
            if done.is_none() {
                let step = asking.lookup.on_time(ms, id(&mut *draw));
                done = act(asking, step, now, stack, counters, &mut *draw);
            }
            let Some(result) = done else { return true };
            for &(_, socket) in &asking.queries {
                close(stack, now, socket);
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
            for &(_, socket) in &asking.queries {
                close(stack, now, socket);
            }
            false
        });
        self.ended.retain(|resolved| resolved.id != id);
    }
}

impl Node {
    /// Starts looking `name` up at the resolvers the held lease names, which spends two draws:
    /// the first query's id, then its port. Refused `NotConnected` while no lease is held or it
    /// names no resolver, and `ResourceExhausted` while `toyos_dns::MAX_LOOKUPS` are held or no
    /// ephemeral port is free. A call refused for want of a port has spent both draws; any other
    /// refused call spends none.
    pub fn resolve(&mut self, now: Instant, name: Name, mut draw: impl FnMut() -> u32) -> Result<LookupId, Refused> {
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
