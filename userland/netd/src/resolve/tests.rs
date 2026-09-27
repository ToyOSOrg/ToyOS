//! The resolver on a wire: netd's own stack, Netstack3's core on a clock these
//! tests move, its far end played here frame by frame, so what is judged is
//! what leaves the device, not what the resolver queued.

use super::*;
use std::num::NonZeroU16;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::net::Address;
use crate::stack::Clock;

const OUR_MAC: [u8; 6] = [0x02, 0, 0, 0, 0, 0x01];
const OURS: [u8; 4] = [10, 0, 0, 2];
/// On the link, and nothing there answers ARP: a LAN's resolver that is down.
const SILENT: [u8; 4] = [10, 0, 0, 53];
/// On the link, answering ARP and every query it is given an answer for.
const ANSWERS: [u8; 4] = [10, 0, 0, 54];
const ANSWERS_MAC: [u8; 6] = [0x02, 0, 0, 0, 0, 0x54];
/// Off the link, and the device holds no route there.
const UNROUTED: [u8; 4] = [192, 0, 2, 53];
const ADDRESS: [u8; 4] = [192, 0, 2, 7];
/// The core's UDP ephemeral range (RFC 6335 §6).
const EPHEMERAL: std::ops::RangeInclusive<u16> = 49152..=65535;

/// A draw an off-path sender could predict, which is all a test needs.
fn counter() -> Draw {
    let mut x: u32 = 0x9e37_79b9;
    Box::new(move || {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        x as u16
    })
}

/// The host's randomness, for the stack's own draws.
fn host_random(dest: &mut [u8]) {
    use std::hash::{BuildHasher, Hasher};
    for chunk in dest.chunks_mut(8) {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u64(0);
        chunk.copy_from_slice(&hasher.finish().to_le_bytes()[..chunk.len()]);
    }
}

type Draw = Box<dyn FnMut() -> u16>;

/// What [`ANSWERS`] says for a name.
enum Says {
    Address([u8; 4]),
    /// A CNAME to this name and nothing else, which the resolver asks again
    /// at its end.
    Alias(&'static str),
}

/// RFC 1071's sum over `bytes`, folded.
fn ones_complement(bytes: &[u8], mut sum: u32) -> u16 {
    for pair in bytes.chunks(2) {
        sum += u32::from(u16::from_be_bytes([pair[0], *pair.get(1).unwrap_or(&0)]));
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

/// An Ethernet frame from [`ANSWERS`] to this machine.
fn ethernet(ether_type: u16, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(14 + payload.len());
    frame.extend_from_slice(&OUR_MAC);
    frame.extend_from_slice(&ANSWERS_MAC);
    frame.extend_from_slice(&ether_type.to_be_bytes());
    frame.extend_from_slice(payload);
    frame
}

/// [`ANSWERS`]'s ARP reply to this machine (RFC 826).
fn arp_reply() -> Vec<u8> {
    let mut arp = vec![0, 1, 0x08, 0x00, 6, 4, 0, 2];
    arp.extend_from_slice(&ANSWERS_MAC);
    arp.extend_from_slice(&ANSWERS);
    arp.extend_from_slice(&OUR_MAC);
    arp.extend_from_slice(&OURS);
    ethernet(0x0806, &arp)
}

/// A datagram from port 53 of [`ANSWERS`] to `port` of this machine.
fn udp_from_answers(port: u16, payload: &[u8]) -> Vec<u8> {
    let udp_len = 8 + payload.len();
    let mut udp = Vec::with_capacity(udp_len);
    udp.extend_from_slice(&toyos_dns::PORT.to_be_bytes());
    udp.extend_from_slice(&port.to_be_bytes());
    udp.extend_from_slice(&(udp_len as u16).to_be_bytes());
    udp.extend_from_slice(&[0, 0]);
    udp.extend_from_slice(payload);
    let mut pseudo = Vec::new();
    pseudo.extend_from_slice(&ANSWERS);
    pseudo.extend_from_slice(&OURS);
    pseudo.extend_from_slice(&[0, 17]);
    pseudo.extend_from_slice(&(udp_len as u16).to_be_bytes());
    pseudo.extend_from_slice(&udp);
    let sum = ones_complement(&pseudo, 0);
    udp[6..8].copy_from_slice(&(if sum == 0 { 0xffff } else { sum }).to_be_bytes());
    let mut ip = vec![0x45, 0];
    ip.extend_from_slice(&((20 + udp_len) as u16).to_be_bytes());
    ip.extend_from_slice(&[0, 0, 0x40, 0, 64, 17, 0, 0]);
    ip.extend_from_slice(&ANSWERS);
    ip.extend_from_slice(&OURS);
    let sum = ones_complement(&ip, 0);
    ip[10..12].copy_from_slice(&sum.to_be_bytes());
    ip.extend_from_slice(&udp);
    ethernet(0x0800, &ip)
}

/// The dotted name a query asks, read by hand.
fn question(query: &[u8]) -> String {
    let mut labels = Vec::new();
    let mut at = 12;
    while query[at] != 0 {
        let len = usize::from(query[at]);
        labels.push(std::str::from_utf8(&query[at + 1..at + 1 + len]).unwrap().to_string());
        at += 1 + len;
    }
    labels.join(".")
}

/// **The stack is its last field**, so every socket the resolver and the
/// tests hold drops before it.
struct Wire {
    clock: Arc<AtomicU64>,
    resolver: Resolver<u32, Draw>,
    born: Instant,
    /// [`ANSWERS`]'s zone: a name, the time before which its answer is held
    /// back, and what it says.
    zone: Vec<(&'static str, u64, Says)>,
    /// Frames the far end has sent and the wire has not yet delivered, with
    /// the time they arrive.
    held: Vec<(u64, Vec<u8>)>,
    /// Every address the device asked ARP for.
    arp_asked: Vec<[u8; 4]>,
    /// Every server a query reached.
    queried: Vec<[u8; 4]>,
    /// The port every query left from.
    sources: Vec<u16>,
    ended: Vec<(u32, Name, Result<Vec<[u8; 4]>, Ended>)>,
    net: Net,
}

impl Wire {
    fn new(servers: &[[u8; 4]]) -> Self {
        let clock = Arc::new(AtomicU64::new(0));
        let mut net = Net::new(OUR_MAC, Clock::Moved(Arc::clone(&clock)), host_random);
        net.apply(Some(Address { addr: OURS, prefix: 24, router: None })).expect("a /24 address");
        let born = Instant::now();
        let mut resolver = Resolver::new(born, counter());
        resolver.set_servers(servers);
        Self {
            clock,
            resolver,
            born,
            zone: Vec::new(),
            held: Vec::new(),
            arp_asked: Vec::new(),
            queried: Vec::new(),
            sources: Vec::new(),
            ended: Vec::new(),
            net,
        }
    }

    fn now_ms(&self) -> u64 {
        self.clock.load(Ordering::Relaxed) / 1_000_000
    }

    fn set_ms(&mut self, ms: u64) {
        self.clock.store(ms * 1_000_000, Ordering::Relaxed);
    }

    fn now(&self) -> Instant {
        self.born + Duration::from_millis(self.now_ms())
    }

    fn start(&mut self, client: u32, name: &str) -> Result<(), Refused> {
        let now = self.now();
        self.resolver.start(client, Name::parse(name).unwrap(), &mut self.net, now).map_err(|(_, why)| why)
    }

    /// The stack's sockets, the resolver's and the tests' own.
    fn sockets(&mut self) -> usize {
        self.net.api().udp::<Ipv4>().collect_all_sockets().len()
    }

    /// Passes of netd's loop, each followed by the far end's turn, for as long
    /// as a frame is due now: a frame is the NIC's interrupt, which wakes the
    /// loop at once.
    fn pass(&mut self) {
        loop {
            let now_ms = self.now_ms();
            let (due, later): (Vec<_>, Vec<_>) = self.held.drain(..).partition(|(at, _)| *at <= now_ms);
            self.held = later;
            for (_, frame) in due {
                self.net.receive(&frame);
            }
            self.net.fire_timers();
            let now = self.now();
            let ended = self.resolver.pass(&mut self.net, now);
            self.ended.extend(ended);
            while let Some(frame) = self.net.bindings.pop_frame() {
                self.far_end(&frame);
            }
            if self.held.iter().all(|(at, _)| *at > now_ms) {
                return;
            }
        }
    }

    /// When netd's loop next wakes, as its `main` computes it: the soonest of
    /// the resolver's wake, the stack's own and the next frame the far end has
    /// on the wire. `None` is a loop asleep until something else wakes it.
    fn next_wake(&mut self) -> Option<u64> {
        let now = self.now();
        let resolver = self.resolver.wake_in(now).map(|d| d.as_millis() as u64);
        let stack = self.net.next_timer().map(|d| d.as_nanos().div_ceil(1_000_000) as u64);
        let frame = self.held.iter().map(|(at, _)| at - self.now_ms()).min();
        let wake = [resolver, stack, frame].into_iter().flatten().min()?;
        Some(self.now_ms() + wake.max(1))
    }

    /// Pass at each of netd's wakes up to `ms`, and at `ms`.
    fn until(&mut self, ms: u64) {
        loop {
            self.pass();
            if self.now_ms() == ms {
                return;
            }
            let at = self.next_wake().map_or(ms, |at| at.min(ms));
            self.set_ms(at);
        }
    }

    /// Pass at each of netd's wakes until `client`'s lookup has ended, or
    /// `until_ms` has come. A lookup in flight with no wake to carry it is a
    /// loop that would sleep through it, and ends the test.
    fn run(&mut self, client: u32, until_ms: u64) -> Option<Result<Vec<[u8; 4]>, Ended>> {
        loop {
            self.pass();
            if let Some(at) = self.ended.iter().position(|(c, ..)| *c == client) {
                return Some(self.ended.remove(at).2);
            }
            let at = self.next_wake().unwrap_or_else(|| {
                panic!("lookup {client} is in flight at {} ms and nothing would wake netd's loop", self.now_ms())
            });
            if at > until_ms {
                return None;
            }
            self.set_ms(at);
        }
    }

    fn far_end(&mut self, frame: &[u8]) {
        let ether_type = u16::from_be_bytes([frame[12], frame[13]]);
        match ether_type {
            0x0806 => {
                let arp = &frame[14..];
                if arp[6..8] != [0, 1] {
                    return;
                }
                let target = [arp[24], arp[25], arp[26], arp[27]];
                self.arp_asked.push(target);
                if target == ANSWERS {
                    self.held.push((self.now_ms(), arp_reply()));
                }
            }
            0x0800 => {
                let ip = &frame[14..];
                // The multicast group reports the device makes are no query.
                if ip[9] != 17 {
                    return;
                }
                let header = usize::from(ip[0] & 0x0f) * 4;
                let udp = &ip[header..];
                assert_eq!(u16::from_be_bytes([udp[2], udp[3]]), toyos_dns::PORT, "the resolver sends only to port 53");
                let to = [ip[16], ip[17], ip[18], ip[19]];
                let port = u16::from_be_bytes([udp[0], udp[1]]);
                self.queried.push(to);
                self.sources.push(port);
                assert_eq!(to, ANSWERS, "a query left for a server the wire cannot reach");
                self.answer(port, &udp[8..]);
            }
            other => panic!("the device sent a frame of type {other:#06x}"),
        }
    }

    /// [`ANSWERS`]'s reply to `query`, spelled by hand: the question echoed
    /// and one answer record behind a pointer to it.
    fn answer(&mut self, port: u16, query: &[u8]) {
        let asked = question(query);
        let Some((_, not_before, says)) = self.zone.iter().find(|(name, ..)| *name == asked) else {
            return;
        };
        let mut reply = query.to_vec();
        reply[2..4].copy_from_slice(&0x8180u16.to_be_bytes());
        reply[6..8].copy_from_slice(&1u16.to_be_bytes());
        reply.extend_from_slice(&[0xc0, 12]);
        let (rtype, data) = match says {
            Says::Address(addr) => (1u16, addr.to_vec()),
            Says::Alias(target) => (5, Name::parse(target).unwrap().wire().to_vec()),
        };
        reply.extend_from_slice(&rtype.to_be_bytes());
        reply.extend_from_slice(&1u16.to_be_bytes());
        reply.extend_from_slice(&60u32.to_be_bytes());
        reply.extend_from_slice(&(data.len() as u16).to_be_bytes());
        reply.extend_from_slice(&data);
        let at = (*not_before).max(self.now_ms());
        self.held.push((at, udp_from_answers(port, &reply)));
    }
}

/// **A query to a server whose link address never resolves holds up no other
/// query.** The first server is on the link and answers no ARP; the second is
/// asked when the first query's wait is over, and answers.
#[test]
fn a_server_that_answers_no_arp_holds_up_no_other_server() {
    let mut w = Wire::new(&[SILENT, ANSWERS]);
    w.zone.push(("www.example", 0, Says::Address(ADDRESS)));
    w.start(1, "www.example").unwrap();
    let ended = w.run(1, 25_000);
    assert!(w.arp_asked.contains(&SILENT), "the premise: the first server was asked for its link address");
    assert_eq!(ended, Some(Ok(vec![ADDRESS])), "the second server was never asked: it heard {:?}", w.queried);
    assert!(w.now_ms() < 2 * toyos_dns::WAIT_MS, "answered at {} ms", w.now_ms());
}

/// The same, for a server no route leads to: the stack refuses that query,
/// and the lookup's own wait moves on from it.
#[test]
fn a_server_with_no_route_holds_up_no_other_server() {
    let mut w = Wire::new(&[UNROUTED, ANSWERS]);
    w.zone.push(("www.example", 0, Says::Address(ADDRESS)));
    w.start(1, "www.example").unwrap();
    let ended = w.run(1, 25_000);
    assert_eq!(ended, Some(Ok(vec![ADDRESS])), "the second server was never asked: it heard {:?}", w.queried);
    assert!(w.now_ms() < 2 * toyos_dns::WAIT_MS, "answered at {} ms", w.now_ms());
}

/// **An alias answered late, while queries sit behind a missing neighbour,
/// restarts the lookup without overflowing anything.** The first server
/// answers its first query only after five more have been sent, the second
/// server's among them, and answers with an alias alone, so the lookup asks
/// again at the alias with every query afresh. The reply reaches netd from
/// the network, so nothing it arranges may end netd.
#[test]
fn an_alias_answered_while_queries_are_stuck_restarts_the_lookup() {
    let mut w = Wire::new(&[ANSWERS, SILENT]);
    w.zone.push(("www.example", 10_500, Says::Alias("cdn.example")));
    w.zone.push(("cdn.example", 0, Says::Address(ADDRESS)));
    w.start(1, "www.example").unwrap();
    let ended = w.run(1, 40_000);
    assert_eq!(ended, Some(Ok(vec![ADDRESS])), "the servers heard {:?}", w.queried);
}

/// **At most [`MAX_LOOKUPS`] are in flight**, and one past it is refused with
/// the code a client reads as "this machine is full".
#[test]
fn the_lookup_past_the_cap_is_refused_as_exhausted() {
    let mut w = Wire::new(&[SILENT]);
    for client in 0..MAX_LOOKUPS as u32 {
        w.start(client, "www.example").unwrap_or_else(|_| panic!("lookup {client} was refused"));
    }
    let refused = w.start(MAX_LOOKUPS as u32, "www.example").expect_err("one lookup past the cap");
    assert!(matches!(refused, Refused::Full));
    assert_eq!(refused.code(), toyos::net::ERR_RESOURCE_EXHAUSTED);
}

/// **A lookup whose client has left is let go at once**: every socket its
/// queries left from leaves the stack, and its slot takes another lookup.
#[test]
fn a_lookup_whose_client_left_is_let_go_at_once() {
    let mut w = Wire::new(&[ANSWERS]);
    // The server's link address first: the core holds ten frames at most
    // for a neighbour it is still asking for (`nud.rs`'s `MAX_PENDING_FRAMES`).
    w.zone.push(("warm.example", 0, Says::Address(ADDRESS)));
    w.start(100, "warm.example").unwrap();
    assert_eq!(w.run(100, toyos_dns::WAIT_MS), Some(Ok(vec![ADDRESS])));
    w.queried.clear();
    let begun = w.now_ms();
    for client in 0..MAX_LOOKUPS as u32 {
        w.start(client, "www.example").unwrap();
    }
    w.until(begun + toyos_dns::WAIT_MS);
    assert!(w.ended.is_empty(), "nothing answers, so nothing has ended");
    assert_eq!(w.queried.len(), 2 * MAX_LOOKUPS, "two queries each left, neither answered");
    assert_eq!(w.resolver.sockets(), 2 * MAX_LOOKUPS);
    assert_eq!(w.sockets(), 2 * MAX_LOOKUPS);
    w.resolver.let_go(&mut w.net, |&client| client == 3);
    assert_eq!(w.sockets(), 2 * (MAX_LOOKUPS - 1), "both of its sockets left the stack");
    assert_eq!(w.resolver.sockets(), 2 * (MAX_LOOKUPS - 1));
    assert!(w.resolver.clients().all(|&client| client != 3));
    w.start(MAX_LOOKUPS as u32, "www.example").expect("the slot it left takes another lookup");
}

/// **Each query a lookup sent holds one socket until the lookup ends**, its
/// answer read however late, and an ended lookup holds none. A query the
/// stack holds for a neighbour that never answers has left as far as its
/// socket can tell.
#[test]
fn a_lookup_holds_a_socket_per_query_that_left_and_none_once_ended() {
    let mut w = Wire::new(&[ANSWERS, SILENT]);
    w.start(1, "www.example").unwrap();
    w.until(toyos_dns::WAIT_MS - 1);
    assert_eq!(w.queried, [ANSWERS]);
    assert_eq!(w.sockets(), 1);
    w.until(toyos_dns::WAIT_MS);
    assert_eq!(w.sockets(), 2, "the first query is still answered, the second is new");
    w.until(2 * toyos_dns::WAIT_MS);
    assert_eq!(w.queried, [ANSWERS, ANSWERS], "the second query never reached its server");
    assert_eq!(w.sockets(), 3, "every query sent is still read");
    w.zone.push(("www.example", 0, Says::Address(ADDRESS)));
    assert_eq!(w.run(1, 5 * toyos_dns::WAIT_MS), Some(Ok(vec![ADDRESS])), "the fifth query, to the first server");
    assert_eq!(w.sockets(), 0, "an ended lookup holds no socket");
}

/// Bind a client's socket on every port of the core's ephemeral range but
/// `free`, which none takes.
fn clients_on_every_port_but(w: &mut Wire, free: Option<u16>) -> Vec<UdpId> {
    let api = w.net.api();
    let mut udp = api.udp::<Ipv4>();
    EPHEMERAL
        .filter(|port| Some(*port) != free)
        .map(|port| {
            let id = udp.create_with(Inbox::default());
            udp.listen(&id, None, NonZeroU16::new(port)).expect("a client binds its port");
            id
        })
        .collect()
}

/// **A query never leaves from a port a client holds**, and its answer is
/// read by the lookup rather than by a client: with every ephemeral port but
/// one held by clients, the query leaves from that one.
#[test]
fn a_query_leaves_from_no_port_a_client_holds() {
    let free = 50_000;
    let mut w = Wire::new(&[ANSWERS]);
    w.zone.push(("www.example", 0, Says::Address(ADDRESS)));
    let clients = clients_on_every_port_but(&mut w, Some(free));
    w.start(1, "www.example").unwrap();
    let ended = w.run(1, toyos_dns::WAIT_MS);
    assert_eq!(w.sources, [free], "the query left from a port a client holds");
    assert_eq!(ended, Some(Ok(vec![ADDRESS])), "the lookup did not read its own answer");
    assert!(clients.iter().all(|id| id.external_data().take().is_none()), "a client was handed the lookup's answer");
    for id in clients {
        crate::stack::removed(w.net.api().udp::<Ipv4>().close(id));
    }
}

/// **A lookup the stack has no port for is refused as this machine being
/// full**, and holds nothing.
#[test]
fn a_lookup_with_no_port_left_is_refused_as_exhausted() {
    let mut w = Wire::new(&[ANSWERS]);
    let clients = clients_on_every_port_but(&mut w, None);
    let refused = w.start(1, "www.example").expect_err("a lookup with no port");
    assert!(matches!(refused, Refused::Full));
    assert_eq!(w.resolver.sockets(), 0);
    assert_eq!(w.sockets(), clients.len(), "the refused lookup left no socket behind");
    for id in clients {
        crate::stack::removed(w.net.api().udp::<Ipv4>().close(id));
    }
}

/// **A lookup's waits are netd's wakes**: a server that is reached and never
/// answers is asked again the moment each wait ends, and the lookup ends
/// timed out the moment its last one does, with nothing but the resolver's
/// own wake to carry it there.
#[test]
fn a_server_that_never_answers_is_asked_at_each_waits_end() {
    let mut w = Wire::new(&[ANSWERS]);
    w.start(1, "www.example").unwrap();
    let ended = w.run(1, 10 * toyos_dns::WAIT_MS);
    assert_eq!(ended, Some(Err(Ended::Failed(Failure::TimedOut))));
    assert_eq!(w.now_ms(), toyos_dns::ROUNDS as u64 * toyos_dns::WAIT_MS, "the lookup ended late");
    assert_eq!(w.queried, [ANSWERS; toyos_dns::ROUNDS]);
}

/// **A lookup's wait is its own, not the latest of every lookup in flight.**
/// A second lookup started half a wait behind the first must not push the
/// first's wake back to the second's: `wake_in` names the soonest due lookup,
/// and the first here ends on its own schedule regardless of the second.
#[test]
fn a_lookup_is_not_carried_by_a_later_ones_schedule() {
    let mut w = Wire::new(&[ANSWERS]);
    w.start(1, "www.example").unwrap();
    w.until(toyos_dns::WAIT_MS / 2);
    w.start(2, "other.example").unwrap();
    let ended = w.run(1, 10 * toyos_dns::WAIT_MS);
    assert_eq!(ended, Some(Err(Ended::Failed(Failure::TimedOut))));
    assert_eq!(w.now_ms(), toyos_dns::ROUNDS as u64 * toyos_dns::WAIT_MS, "lookup 1 waited on lookup 2's schedule");
    let ended = w.run(2, 10 * toyos_dns::WAIT_MS);
    assert_eq!(ended, Some(Err(Ended::Failed(Failure::TimedOut))));
    assert_eq!(
        w.now_ms(),
        toyos_dns::WAIT_MS / 2 + toyos_dns::ROUNDS as u64 * toyos_dns::WAIT_MS,
        "lookup 2 waited on lookup 1's schedule"
    );
}
