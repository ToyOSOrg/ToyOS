//! The resolver on the node, against a far end played here frame by frame: what is judged is
//! what leaves the node, not what it queued. No scenario ids: the reader's rules are
//! `toyos-dns`'s own tests; these are the node's sockets, clock and draws under it.
//!
//! What is not ours: `etherparse` reads every frame the node emits (`lan::outside`); every DNS
//! message here, a reply's and the query the node is expected to emit, is spelled by hand from
//! RFC 1035 §4.1's layouts and never by `toyos-dns`; each test names the RFC its expectation is
//! read from.

mod common;
mod lan;

use std::net::Ipv4Addr;
use std::time::Duration;

use common::{arp, from_server, message, message_of, terms, xid, A, ACK, DNS, MAC, MAC_B, NAK, R};
use lan::{udp, Lan, Seen, Udp, B};
use toyos_dns::{Failure, Name, MAX_LOOKUPS, ROUNDS, WAIT_MS};
use toyos_net_node::{Counter, Ended, LookupId, NotStarted, Refused, Resolved};
use toyos_net_udp::limits::{EPHEMERAL_COUNT, EPHEMERAL_FIRST};
use toyos_net_udp::Counter as Rule;
use toyos_net_wire::{Instant, Port};

const ANY: Ipv4Addr = Ipv4Addr::UNSPECIFIED;
/// On the link, answering ARP and every query its zone holds an answer for.
const ANSWERS: Ipv4Addr = DNS;
const MAC_S: [u8; 6] = [2, 0, 0, 0, 0, 0x53];
/// On the link, and nothing there answers ARP: a LAN's resolver that is down.
const SILENT: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 52);
/// Off the link, asked of a lease that names no router.
const UNROUTED: Ipv4Addr = Ipv4Addr::new(198, 51, 100, 53);
const ADDRESS: [u8; 4] = [203, 0, 113, 7];
/// The address every reply that must not be read carries.
const FORGED: [u8; 4] = [203, 0, 113, 66];
/// RFC 1035 §4.1.1: a response (QR), recursion desired and available, RCODE 0.
const RESPONSE: u16 = 0x8180;
/// RFC 1035 §3.2.2.
const TYPE_A: u16 = 1;
const TYPE_CNAME: u16 = 5;
/// No lookup has ended.
const NONE: [Resolved; 0] = [];

fn name(text: &str) -> Name {
    Name::parse(text).expect("a host name")
}

/// The draw of a call that draws nothing.
fn undrawn() -> u32 {
    panic!("this call draws nothing")
}

/// These draws in order, and no more.
fn sequence<const N: usize>(draws: [u32; N]) -> impl FnMut() -> u32 {
    let mut rest = draws.into_iter();
    move || rest.next().expect("the call draws no more than it was given")
}

fn counter(draws: &mut u32) -> impl FnMut() -> u32 + '_ {
    move || {
        *draws += 1;
        *draws
    }
}

/// RFC 1035 §3.1: each label behind its length octet, then the root's zero.
fn labels(name: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for label in name.split('.') {
        out.push(u8::try_from(label.len()).unwrap());
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    out
}

/// RFC 1035 §4.1.1 and §4.1.2: a standard query, recursion desired, for `name`'s A records in IN.
fn question(id: u16, name: &str) -> Vec<u8> {
    let mut out = id.to_be_bytes().to_vec();
    out.extend_from_slice(&[0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0]);
    out.extend_from_slice(&labels(name));
    out.extend_from_slice(&[0, 1, 0, 1]);
    out
}

/// The dotted name a query asks, read by hand from octet 12 on.
fn asked_name(query: &[u8]) -> String {
    let mut found = Vec::new();
    let mut at = 12;
    while query[at] != 0 {
        let len = usize::from(query[at]);
        found.push(std::str::from_utf8(&query[at + 1..at + 1 + len]).unwrap().to_string());
        at += 1 + len;
    }
    found.join(".")
}

/// A reply to `query` (RFC 1035 §4.1): §4.1.1's header with the query's id, `flags`, one question
/// and one answer a record; §4.1.2's question as the query spelled it; then §4.1.3's records, each
/// owned by a §4.1.4 pointer to the question's name at octet 12, in class IN with a TTL of 60.
fn reply(query: &[u8], flags: u16, records: &[(u16, Vec<u8>)]) -> Vec<u8> {
    let mut out = query[..2].to_vec();
    out.extend_from_slice(&flags.to_be_bytes());
    out.extend_from_slice(&[0, 1]);
    out.extend_from_slice(&u16::try_from(records.len()).unwrap().to_be_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&query[12..]);
    for (rtype, data) in records {
        out.extend_from_slice(&[0xc0, 12]);
        out.extend_from_slice(&rtype.to_be_bytes());
        out.extend_from_slice(&[0, 1]);
        out.extend_from_slice(&60u32.to_be_bytes());
        out.extend_from_slice(&u16::try_from(data.len()).unwrap().to_be_bytes());
        out.extend_from_slice(data);
    }
    out
}

/// A reply to `query` that gives `address`.
fn gives(query: &[u8], address: [u8; 4]) -> Vec<u8> {
    reply(query, RESPONSE, &[(TYPE_A, address.to_vec())])
}

/// What [`ANSWERS`] says for a name.
enum Says {
    Address([u8; 4]),
    /// A CNAME to this name and nothing else, which the resolver asks again at its end.
    Alias(&'static str),
}

/// A query that reached the wire.
#[derive(Clone, Debug)]
struct Query {
    at: Instant,
    udp: Udp,
    id: u16,
    name: String,
}

struct Net {
    lan: Lan,
    /// When the lease was held: the origin of every time a test names.
    born: Instant,
    /// How many of `lan.sent` the far end has seen.
    seen: usize,
    /// [`ANSWERS`]'s zone: a name, the time before which its answer is held back, and what it
    /// says.
    zone: Vec<(&'static str, u64, Says)>,
    /// Frames [`ANSWERS`] has sent and the wire has not yet delivered, with the time they arrive.
    held: Vec<(Instant, Vec<u8>)>,
    /// Every query that left, in order.
    queried: Vec<Query>,
    ended: Vec<Resolved>,
    /// The last draw [`Self::resolve`] handed out: its ids are 0x8000 and up, and those the node
    /// draws for itself from `lan`'s count are below it.
    draws: u32,
}

impl Net {
    /// The node holding a day's lease of `A`/24 that names `servers`, through `router`, at a whole
    /// millisecond of its clock.
    fn leased(servers: &[Ipv4Addr], router: Option<Ipv4Addr>) -> Self {
        let mut options = terms(86_400, router);
        options.retain(|(code, _)| *code != 6);
        options.push((6, servers.iter().flat_map(Ipv4Addr::octets).collect()));
        let mut lan = Lan::new();
        lan.lease_on(&options);
        assert_eq!(lan.node.lease().expect("held").dns, servers, "the premise: the lease names these resolvers");
        // `toyos-dns` keeps whole milliseconds and conflict detection's waits are drawn finer: the
        // origin is the next whole one, so a wait counted from it ends on the millisecond named.
        let whole = u64::try_from(lan.now.since(Instant::from_millis(0)).as_millis()).unwrap() + 1;
        lan.fire(Instant::from_millis(whole));
        Self {
            born: lan.now,
            seen: lan.sent.len(),
            lan,
            zone: Vec::new(),
            held: Vec::new(),
            queried: Vec::new(),
            ended: Vec::new(),
            draws: 0x7c00_8000,
        }
    }

    fn at(&self, ms: u64) -> Instant {
        self.born.after(Duration::from_millis(ms))
    }

    fn ms_of(&self, at: Instant) -> u64 {
        u64::try_from(at.since(self.born).as_millis()).unwrap()
    }

    /// Milliseconds since the lease was held.
    fn ms(&self) -> u64 {
        self.ms_of(self.lan.now)
    }

    fn resolve(&mut self, text: &str) -> Result<LookupId, NotStarted> {
        self.lan.node.resolve(self.lan.now, name(text), counter(&mut self.draws))
    }

    /// `message` from the resolver's port 53 to the node's `to`.
    fn resolver_says(&mut self, to: u16, message: &[u8]) {
        self.lan.deliver(&udp(MAC, MAC_S, (ANSWERS, 53), (A, to), message));
    }

    /// Whether a socket holds `port`, by binding it: a port is one socket's.
    fn port_held(&mut self, port: u16) -> bool {
        match self.lan.node.udp_bind(ANY, Port::new(port), undrawn) {
            Ok((id, _)) => {
                self.lan.node.udp_close(self.lan.now, id).unwrap();
                false
            }
            Err(Refused::AddrInUse) => true,
            Err(other) => panic!("binding port {port}: {other:?}"),
        }
    }

    /// What the far end does with one frame of the node's.
    fn far_end(&mut self, at: Instant, seen: &Seen) {
        match seen {
            Seen::Arp { request: true, target, .. } if *target == ANSWERS => self.lan.deliver(&arp(MAC, false, MAC_S, ANSWERS, A)),
            Seen::Udp(udp) if udp.port == 53 => {
                assert_eq!((udp.to, udp.destination), (MAC_S, ANSWERS), "a query left for a server the wire cannot reach");
                let query = Query { at, udp: udp.clone(), id: u16::from_be_bytes([udp.payload[0], udp.payload[1]]), name: asked_name(&udp.payload) };
                if let Some((_, not_before, says)) = self.zone.iter().find(|(name, ..)| *name == query.name) {
                    let record = match says {
                        Says::Address(address) => (TYPE_A, address.to_vec()),
                        Says::Alias(target) => (TYPE_CNAME, labels(target)),
                    };
                    let message = reply(&udp.payload, RESPONSE, &[record]);
                    let due = self.at(*not_before).max(self.lan.now);
                    self.held.push((due, lan::udp(MAC, MAC_S, (ANSWERS, 53), (A, udp.source_port), &message)));
                }
                self.queried.push(query);
            }
            _ => {}
        }
    }

    /// The frames due now delivered, every frame the node then has to send sent, and the far
    /// end's turn, until both are silent. The clock does not move.
    fn pass(&mut self) {
        for _ in 0..10_000 {
            let now = self.lan.now;
            let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut self.held).into_iter().partition(|(at, _)| *at <= now);
            self.held = later;
            for (_, frame) in &due {
                self.lan.deliver(frame);
            }
            self.lan.pump();
            let fresh = self.lan.sent[self.seen..].to_vec();
            self.seen = self.lan.sent.len();
            for (at, seen) in &fresh {
                self.far_end(*at, seen);
            }
            if due.is_empty() && fresh.is_empty() {
                return;
            }
        }
        panic!("the node and its far end never fell silent at {} ms", self.ms());
    }

    /// When the node is next woken: its own next deadline, or the next frame on the wire.
    fn next_wake(&self) -> Instant {
        let frame = self.held.iter().map(|(at, _)| *at).min();
        self.lan.node.next_deadline().into_iter().chain(frame).min().expect("a held lease has a deadline")
    }

    /// Passes at each of the node's wakes up to `ms`, and at `ms`.
    fn until(&mut self, ms: u64) {
        let end = self.at(ms);
        for _ in 0..10_000 {
            self.pass();
            if self.lan.now >= end {
                return;
            }
            self.lan.fire(self.next_wake().min(end));
        }
        panic!("10,000 wakes and the clock stands at {} ms: a deadline that never passes", self.ms());
    }

    /// Passes at each of the node's wakes until lookup `id` has ended, or the next wake is past
    /// `until_ms`. Nothing but the node's own deadline carries a lookup's wait.
    fn run(&mut self, id: LookupId, until_ms: u64) -> Option<Result<Vec<[u8; 4]>, Ended>> {
        let end = self.at(until_ms);
        for _ in 0..10_000 {
            self.pass();
            self.ended.extend(self.lan.node.take_resolved());
            if let Some(found) = self.ended.iter().position(|resolved| resolved.id == id) {
                return Some(self.ended.remove(found).result);
            }
            let wake = self.next_wake();
            if wake > end {
                return None;
            }
            self.lan.fire(wake);
        }
        panic!("10,000 wakes and the clock stands at {} ms: a deadline that never passes", self.ms());
    }
}

// RFC 1035 §4.1.1: ID, then RD alone among the flags, QDCOUNT 1 and the other counts 0. §4.1.2:
// the name as labels, QTYPE A (1), QCLASS IN (1). §4.2.1: to the server's port 53. RFC 5452 §9.2:
// the id and the source port are each drawn. RFC 6056 §3.3.1: the port is the draw's offset into
// 49152 to 65535.
#[test]
fn a_query_is_rfc_1035s_octets_from_a_port_and_with_an_id_of_its_own_draws() {
    let mut net = Net::leased(&[ANSWERS], Some(R));
    net.lan.node.resolve(net.lan.now, name("www.example"), sequence([0xabcd_1234, 7])).expect("two draws: the id, then the port");
    net.pass();
    #[rustfmt::skip]
    let payload = vec![
        0x12, 0x34, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0,
        3, b'w', b'w', b'w', 7, b'e', b'x', b'a', b'm', b'p', b'l', b'e', 0,
        0, 1, 0, 1,
    ];
    let sent: Vec<&Udp> = net.queried.iter().map(|query| &query.udp).collect();
    assert_eq!(sent, [&Udp { to: MAC_S, source: A, source_port: 49_152 + 7, destination: ANSWERS, port: 53, ttl: 64, payload }]);
}

// RFC 5452 §9.1: a reply is accepted only from the address the query was sent to. The query's
// socket is connected to it, so another host's datagram finds no socket (RFC 1122 §4.1.3.1).
#[test]
fn a_reply_is_read_only_from_the_resolver_its_query_went_to() {
    let mut net = Net::leased(&[ANSWERS], Some(R));
    let id = net.resolve("www.example").unwrap();
    net.pass();
    let asked = net.queried[0].clone();
    let nobodys = net.lan.counted(Rule::RxNoSocket);
    net.lan.deliver(&udp(MAC, MAC_B, (B, 53), (A, asked.udp.source_port), &gives(&asked.udp.payload, FORGED)));
    assert_eq!(net.lan.node.take_resolved(), NONE, "another host's port 53 answered, with the query's id and question");
    assert_eq!(net.lan.counted(Rule::RxNoSocket), nobodys + 1, "and its datagram reached no socket");
    net.resolver_says(asked.udp.source_port, &gives(&asked.udp.payload, ADDRESS));
    assert_eq!(net.lan.node.take_resolved(), [Resolved { id, result: Ok(vec![ADDRESS]) }]);
}

// RFC 5452 §9.1: and only from the port it was sent to, 53.
#[test]
fn a_reply_is_read_only_from_port_53() {
    let mut net = Net::leased(&[ANSWERS], Some(R));
    let id = net.resolve("www.example").unwrap();
    net.pass();
    let asked = net.queried[0].clone();
    let reached = net.lan.counted(Rule::Rx);
    net.lan.deliver(&udp(MAC, MAC_S, (ANSWERS, 5_353), (A, asked.udp.source_port), &gives(&asked.udp.payload, FORGED)));
    assert_eq!(net.lan.counted(Rule::Rx), reached + 1, "the premise: the datagram reached [udp]");
    assert_eq!(net.lan.node.take_resolved(), NONE, "the resolver's port 5353 answered");
    net.resolver_says(asked.udp.source_port, &gives(&asked.udp.payload, ADDRESS));
    assert_eq!(net.lan.node.take_resolved(), [Resolved { id, result: Ok(vec![ADDRESS]) }]);
}

// RFC 768: a source port of zero is no port, and so not port 53.
#[test]
fn a_reply_from_port_0_is_not_read() {
    let mut net = Net::leased(&[ANSWERS], Some(R));
    let id = net.resolve("www.example").unwrap();
    net.pass();
    let asked = net.queried[0].clone();
    let portless = net.lan.counted(Rule::RxSrcPortZero);
    net.lan.deliver(&udp(MAC, MAC_S, (ANSWERS, 0), (A, asked.udp.source_port), &gives(&asked.udp.payload, FORGED)));
    assert_eq!(net.lan.counted(Rule::RxSrcPortZero), portless + 1, "the premise: the datagram reached [udp], which read it as from port 0");
    assert_eq!(net.lan.node.take_resolved(), NONE, "the resolver's port 0 answered");
    net.resolver_says(asked.udp.source_port, &gives(&asked.udp.payload, ADDRESS));
    assert_eq!(net.lan.node.take_resolved(), [Resolved { id, result: Ok(vec![ADDRESS]) }]);
}

// RFC 5452 §9.2: the port a query left from is half of what a forger must guess, so a reply is
// read for the query whose port it reached. A reply to the first query, id and all, on the second
// query's port is no reply; on its own port it is one still, late as it is.
#[test]
fn a_reply_on_another_querys_port_is_not_read() {
    let mut net = Net::leased(&[ANSWERS], Some(R));
    let id = net.resolve("www.example").unwrap();
    net.until(WAIT_MS);
    let (first, second) = (net.queried[0].clone(), net.queried[1].clone());
    assert_eq!(net.queried.len(), 2, "the premise: a second query left when the first one's wait ended");
    assert!(first.id != second.id && first.udp.source_port != second.udp.source_port, "the premise: {first:?} {second:?}");
    net.resolver_says(second.udp.source_port, &gives(&first.udp.payload, FORGED));
    assert_eq!(net.lan.node.take_resolved(), NONE, "the first query's reply was read on the second query's port");
    net.resolver_says(first.udp.source_port, &gives(&first.udp.payload, ADDRESS));
    assert_eq!(net.lan.node.take_resolved(), [Resolved { id, result: Ok(vec![ADDRESS]) }]);
}

// RFC 1035 §4.1.1 (the id, QR), §4.1.2 (the question), §4.1.4 (a pointer names a prior
// occurrence): a message that is not the answer to the query ends nothing, whatever it is made
// of, and the node reads the next one. RFC 5452 §9.1: the id and the question must match.
#[test]
fn nothing_but_its_answer_ends_a_lookup() {
    let mut net = Net::leased(&[ANSWERS], Some(R));
    let id = net.resolve("www.example").unwrap();
    net.pass();
    let asked = net.queried[0].clone();
    let forged = gives(&asked.udp.payload, FORGED);
    let after_question = asked.udp.payload.len();

    let mut hostile: Vec<(String, Vec<u8>)> = Vec::new();
    let mut another_id = forged.clone();
    another_id[1] ^= 1;
    hostile.push(("another id".into(), another_id));
    hostile.push(("another question".into(), gives(&question(asked.id, "other.example"), FORGED)));
    hostile.push(("a query, not a response".into(), reply(&asked.udp.payload, 0x0100, &[(TYPE_A, FORGED.to_vec())])));
    // The question's name is a pointer to itself.
    let mut looped = forged[..12].to_vec();
    looped.extend_from_slice(&[0xc0, 12, 0, 1, 0, 1]);
    looped.extend_from_slice(&forged[after_question..]);
    hostile.push(("a question that points at itself".into(), looped));
    // The answer's owner is a pointer to itself, and one to an octet past the message's end.
    for target in [after_question, 0x3fff] {
        let mut pointed = forged.clone();
        pointed[after_question..after_question + 2].copy_from_slice(&(0xc000 | u16::try_from(target).unwrap()).to_be_bytes());
        hostile.push((format!("an owner that points to octet {target}"), pointed));
    }
    for len in 0..forged.len() {
        hostile.push((format!("the first {len} octets of an answer"), forged[..len].to_vec()));
    }
    // The largest datagram a frame carries: the query's id, a response, and pointers to its end.
    let mut largest = forged[..12].to_vec();
    largest.resize(1_472, 0xc0);
    hostile.push(("1,472 octets of pointers".into(), largest));

    for (what, message) in &hostile {
        net.resolver_says(asked.udp.source_port, message);
        assert_eq!(net.lan.node.take_resolved(), NONE, "{what} ended the lookup");
    }
    net.resolver_says(asked.udp.source_port, &gives(&asked.udp.payload, ADDRESS));
    assert_eq!(net.lan.node.take_resolved(), [Resolved { id, result: Ok(vec![ADDRESS]) }], "the answer, after all of them");
}

// RFC 1035 §4.2.1: a resolver that does not answer is given up on after a wait and the next is
// asked. No route leads to the first: [udp] refuses its query's socket that peer (`udp.no-route`),
// and the query is counted, holds no port, and is waited out like one nobody answered.
#[test]
fn a_query_udp_refuses_is_counted_holds_no_port_and_is_waited_out() {
    let mut net = Net::leased(&[UNROUTED, ANSWERS], None);
    net.zone.push(("www.example", 0, Says::Address(ADDRESS)));
    let refused = net.lan.counted(Rule::NoRoute);
    let id = net.lan.node.resolve(net.lan.now, name("www.example"), sequence([0x8001, 9])).expect("a lookup whose first query cannot leave");
    assert_eq!(net.lan.counted(Rule::NoRoute), refused + 1, "the premise: [udp] refused the query for want of a route");
    assert_eq!(net.lan.node.counters().get(Counter::QueryUnsent), 1);
    assert!(!net.port_held(49_152 + 9), "the refused query's socket");
    let ended = net.run(id, 25_000);
    assert_eq!(ended, Some(Ok(vec![ADDRESS])), "the second resolver heard {:?}", net.queried);
    assert_eq!(net.ms(), WAIT_MS);
    assert_eq!(net.lan.node.counters().get(Counter::QueryUnsent), 1);
}

// RFC 1034 §5.3.3: an alias with no address is asked again at its end, every query afresh. The
// first resolver answers only after six queries have left, three of them to it, and with an alias
// alone: the queries for the old name are let go, so the two other answers to them, arriving
// with the first, find no socket (RFC 1122 §4.1.3.1), and the alias's address ends the lookup.
#[test]
fn an_alias_answered_late_restarts_the_lookup_and_lets_the_old_names_queries_go() {
    let mut net = Net::leased(&[ANSWERS, SILENT], Some(R));
    net.zone.push(("www.example", 10_500, Says::Alias("cdn.example")));
    net.zone.push(("cdn.example", 0, Says::Address(ADDRESS)));
    let nobodys = net.lan.counted(Rule::RxNoSocket);
    let id = net.resolve("www.example").unwrap();
    let ended = net.run(id, 40_000);
    assert_eq!(ended, Some(Ok(vec![ADDRESS])), "the resolver heard {:?}", net.queried);
    assert_eq!(net.ms(), 10_500);
    let names: Vec<&str> = net.queried.iter().map(|query| query.name.as_str()).collect();
    assert_eq!(names, ["www.example", "www.example", "www.example", "cdn.example"]);
    assert_eq!(net.lan.counted(Rule::RxNoSocket), nobodys + 2, "the old name's other two answers");
}

// `toyos_dns::MAX_LOOKUPS` are held and one more is refused as `toyos::net`'s
// ERR_RESOURCE_EXHAUSTED; an ended lookup holds its place until its answer is taken.
#[test]
fn the_lookup_past_the_cap_is_refused_until_an_answer_is_taken() {
    let mut net = Net::leased(&[ANSWERS], Some(R));
    net.zone.push(("one.example", 1_000, Says::Address(ADDRESS)));
    let first = net.resolve("one.example").unwrap();
    net.pass();
    for held in 1..MAX_LOOKUPS {
        net.resolve("www.example").unwrap_or_else(|why| panic!("lookup {held} was refused: {why:?}"));
    }
    assert_eq!(net.lan.node.resolve(net.lan.now, name("www.example"), undrawn), Err(NotStarted::ResourceExhausted));
    net.until(1_000);
    assert_eq!(net.lan.node.resolve(net.lan.now, name("www.example"), undrawn), Err(NotStarted::ResourceExhausted), "an answer nobody took holds its place");
    assert_eq!(net.lan.node.take_resolved(), [Resolved { id: first, result: Ok(vec![ADDRESS]) }]);
    net.resolve("www.example").expect("a taken answer's place is another's");
}

// A lookup is asked of the held lease's resolvers and of nothing else: with none to ask it is
// refused as `toyos::net`'s ERR_NOT_CONNECTED, and draws nothing.
#[test]
fn a_lookup_with_no_resolver_to_ask_is_refused_as_not_connected() {
    let mut lan = Lan::new();
    assert_eq!(lan.node.resolve(lan.now, name("www.example"), undrawn), Err(NotStarted::NotConnected), "before a lease");
    let mut options = terms(3_600, Some(R));
    options.retain(|(code, _)| *code != 6);
    lan.lease_on(&options);
    assert_eq!(lan.node.resolve(lan.now, name("www.example"), undrawn), Err(NotStarted::NotConnected), "a lease that names no resolver");

    let mut lan = Lan::new();
    lan.lease(600);
    lan.node.resolve(lan.now, name("www.example"), sequence([1, 2])).expect("the premise: a held lease's resolver is asked");
    assert!(lan.run_until(Duration::from_secs(700), |lan| lan.node.lease().is_none()), "the lease runs out");
    assert_eq!(lan.node.resolve(lan.now, name("www.example"), undrawn), Err(NotStarted::NotConnected), "after the lease");
}

// A lookup nobody waits for is let go at once: the ports its queries left from are free, and its
// place takes another lookup.
#[test]
fn a_lookup_let_go_frees_its_ports_and_its_place_at_once() {
    let mut net = Net::leased(&[ANSWERS], Some(R));
    let names: Vec<String> = (0..MAX_LOOKUPS).map(|n| format!("n{n}.example")).collect();
    let mut ids = Vec::new();
    for asked in &names {
        ids.push(net.resolve(asked).unwrap());
        net.pass();
    }
    net.until(WAIT_MS);
    assert_eq!(net.lan.node.take_resolved(), NONE, "nothing answers, so nothing has ended");
    assert_eq!(net.queried.len(), 2 * MAX_LOOKUPS, "two queries of each left, neither answered");
    let ports = |net: &Net, asked: &str| -> Vec<u16> { net.queried.iter().filter(|query| query.name == asked).map(|query| query.udp.source_port).collect() };
    let (gone, stays) = (ports(&net, &names[3]), ports(&net, &names[4]));
    assert_eq!((gone.len(), stays.len()), (2, 2));
    assert_eq!(net.resolve("www.example"), Err(NotStarted::ResourceExhausted), "the premise: every place is held");

    net.lan.node.let_go(net.lan.now, ids[3]);
    for port in gone {
        assert!(!net.port_held(port), "port {port} of the lookup let go");
    }
    for port in stays {
        assert!(net.port_held(port), "port {port} of a lookup still waited for");
    }
    net.resolve("www.example").expect("its place takes another lookup");
}

// And an answer nobody took goes with it: the id is nobody's from then on.
#[test]
fn a_lookup_let_go_after_it_ended_takes_its_answer_with_it() {
    let mut net = Net::leased(&[ANSWERS], Some(R));
    net.zone.push(("www.example", 0, Says::Address(ADDRESS)));
    let id = net.resolve("www.example").unwrap();
    net.pass();
    let left = net.queried[0].udp.source_port;
    assert!(!net.port_held(left), "the premise: the lookup was answered and ended, its port closed");
    net.lan.node.let_go(net.lan.now, id);
    assert_eq!(net.lan.node.take_resolved(), NONE);
}

// RFC 1035 §4.2.1 leaves a late answer readable: a query's port is held while its answer would
// still be read, past its own wait, and every port is free once the lookup has ended.
#[test]
fn a_query_holds_its_port_while_its_answer_is_read_and_an_ended_lookup_holds_none() {
    let mut net = Net::leased(&[ANSWERS], Some(R));
    let id = net.resolve("www.example").unwrap();
    net.until(WAIT_MS);
    assert_eq!(net.queried.len(), 2);
    let first = net.queried[0].udp.source_port;
    assert!(net.port_held(first), "the first query's answer is still read after its wait");
    net.zone.push(("www.example", 0, Says::Address(ADDRESS)));
    assert_eq!(net.run(id, 5 * WAIT_MS), Some(Ok(vec![ADDRESS])), "the third query's answer");
    assert_eq!(net.queried.len(), 3);
    let left: Vec<u16> = net.queried.iter().map(|query| query.udp.source_port).collect();
    for port in left {
        assert!(!net.port_held(port), "port {port} of an ended lookup");
    }
}

// RFC 6335 §6: the dynamic ports are 49152 to 65535. With every one held a lookup is refused as
// `toyos::net`'s ERR_RESOURCE_EXHAUSTED, and one in flight ends the moment its next query has no
// port to leave from, by that name.
#[test]
fn a_query_with_no_port_to_leave_from_ends_its_lookup_by_name() {
    let mut net = Net::leased(&[ANSWERS], Some(R));
    let last = u32::from(EPHEMERAL_COUNT) - 1;
    for offset in 0..last {
        net.lan.node.udp_bind(ANY, None, || offset).unwrap();
    }
    let id = net.lan.node.resolve(net.lan.now, name("www.example"), sequence([0x8001, last])).expect("the one port left");
    assert_eq!(net.lan.node.resolve(net.lan.now, name("other.example"), sequence([0x8002, 0])), Err(NotStarted::ResourceExhausted));
    assert_eq!(net.run(id, 10 * WAIT_MS), Some(Err(Ended::NoPort)));
    assert_eq!(net.ms(), WAIT_MS);
    let left: Vec<u16> = net.queried.iter().map(|query| query.udp.source_port).collect();
    assert_eq!(left, [EPHEMERAL_FIRST + (EPHEMERAL_COUNT - 1)]);
}

// RFC 4861 §7.2.2: [ip] holds a bounded queue for a next hop it is resolving, and keeps the
// newest. Lookups started together at a resolver whose link address is not yet known hand [ip]
// one query each: those past its queue never leave, and each is asked again when its wait ends.
// The track records this against the node (`issues/toyos-has-its-own-network-stack.md`).
#[test]
fn a_burst_at_a_resolver_not_yet_resolved_leaves_ips_queue_of_queries_and_the_rest_a_wait_later() {
    let mut net = Net::leased(&[ANSWERS], Some(R));
    let names: Vec<String> = (0..MAX_LOOKUPS).map(|n| format!("n{n}.example")).collect();
    for asked in &names {
        net.resolve(asked).unwrap();
    }
    net.pass();
    let queue = toyos_net_ip::limits::nud::PENDING_PER_NEIGHBOUR;
    let mut left: Vec<&str> = net.queried.iter().map(|query| query.name.as_str()).collect();
    let mut newest: Vec<&str> = names[MAX_LOOKUPS - queue..].iter().map(String::as_str).collect();
    left.sort_unstable();
    newest.sort_unstable();
    assert_eq!(left, newest, "the newest {queue} queries left once the resolver answered ARP");
    net.until(WAIT_MS);
    assert_eq!(net.queried.len(), queue + MAX_LOOKUPS, "and every lookup's second query, a wait later");
}

// RFC 1035 §4.2.1: each resolver is asked ROUNDS times, WAIT_MS apart. A wait is a deadline of
// the node's: a resolver that is reached and never answers is asked again the moment each wait
// ends, and the lookup ends timed out the moment its last one does, with nothing but the node's
// own next deadline to carry it there.
#[test]
fn a_resolver_that_never_answers_is_asked_at_each_waits_end() {
    let mut net = Net::leased(&[ANSWERS], Some(R));
    let id = net.resolve("www.example").unwrap();
    let ended = net.run(id, 10 * WAIT_MS);
    assert_eq!(ended, Some(Err(Ended::Failed(Failure::TimedOut))));
    let rounds = u64::try_from(ROUNDS).unwrap();
    assert_eq!(net.ms(), rounds * WAIT_MS, "the lookup ended late");
    let left: Vec<u64> = net.queried.iter().map(|query| net.ms_of(query.at)).collect();
    assert_eq!(left, (0..rounds).map(|round| round * WAIT_MS).collect::<Vec<_>>());
}

// A lookup's wait is its own: the node's deadline is the soonest of them, so a second lookup
// started half a wait behind the first pushes neither's schedule onto the other.
#[test]
fn a_lookup_is_not_carried_by_a_later_ones_schedule() {
    let mut net = Net::leased(&[ANSWERS], Some(R));
    let first = net.resolve("www.example").unwrap();
    net.until(WAIT_MS / 2);
    let second = net.resolve("other.example").unwrap();
    let rounds = u64::try_from(ROUNDS).unwrap();
    assert_eq!(net.run(first, 10 * WAIT_MS), Some(Err(Ended::Failed(Failure::TimedOut))));
    assert_eq!(net.ms(), rounds * WAIT_MS, "the first lookup waited on the second's schedule");
    assert_eq!(net.run(second, 10 * WAIT_MS), Some(Err(Ended::Failed(Failure::TimedOut))));
    assert_eq!(net.ms(), WAIT_MS / 2 + rounds * WAIT_MS, "the second lookup waited on the first's schedule");
}

// RFC 5452 §9.1: a reply is matched on the address it was sent to as well, which is the one its
// query left from. The limited broadcast, the subnet's broadcast and a group the node has joined
// each reach [udp] at the query's port, which is the premise, and none is the query's answer.
#[test]
fn a_reply_is_read_only_at_the_address_its_query_left_from() {
    let mut net = Net::leased(&[ANSWERS], Some(R));
    net.lan.node.answer_as(net.lan.now, toyos_mdns::Host::new("toyos").unwrap()).unwrap();
    let id = net.resolve("www.example").unwrap();
    net.pass();
    let asked = net.queried[0].clone();
    let group = (Ipv4Addr::new(224, 0, 0, 251), [0x01, 0x00, 0x5e, 0x00, 0x00, 0xfb]);
    for (destination, mac) in [(Ipv4Addr::BROADCAST, common::BROADCAST), (Ipv4Addr::new(192, 0, 2, 255), common::BROADCAST), group] {
        let reached = net.lan.counted(Rule::Rx);
        net.lan.deliver(&udp(mac, MAC_S, (ANSWERS, 53), (destination, asked.udp.source_port), &gives(&asked.udp.payload, FORGED)));
        assert_eq!(net.lan.counted(Rule::Rx), reached + 1, "the premise: the datagram to {destination} reached [udp]");
        assert_eq!(net.lan.node.take_resolved(), NONE, "a reply sent to {destination} was read");
    }
    net.resolver_says(asked.udp.source_port, &gives(&asked.udp.payload, ADDRESS));
    assert_eq!(net.lan.node.take_resolved(), [Resolved { id, result: Ok(vec![ADDRESS]) }]);
}

// `toyos-dns/src/tests.rs` holds this reply as `REAL_GOOGLE`: what a public resolver answered for
// `dns.google` to a query with id 0x1001, 60 octets, recorded and not written here. Its two
// addresses come out of the node in the reply's order.
#[test]
fn a_recorded_reply_of_a_public_resolver_answers_the_nodes_query() {
    #[rustfmt::skip]
    const RECORDED: [u8; 60] = [
        0x10, 0x01, 0x81, 0x80, 0x00, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x03, 0x64, 0x6e, 0x73, 0x06, 0x67,
        0x6f, 0x6f, 0x67, 0x6c, 0x65, 0x00, 0x00, 0x01, 0x00, 0x01, 0xc0, 0x0c, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00,
        0x02, 0xcf, 0x00, 0x04, 0x08, 0x08, 0x04, 0x04, 0xc0, 0x0c, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x02, 0xcf,
        0x00, 0x04, 0x08, 0x08, 0x08, 0x08,
    ];
    let mut net = Net::leased(&[ANSWERS], Some(R));
    let id = net.lan.node.resolve(net.lan.now, name("dns.google"), sequence([0x1001, 3])).unwrap();
    net.pass();
    let asked = net.queried[0].clone();
    assert_eq!((asked.id, &asked.udp.payload[12..]), (0x1001, &RECORDED[12..28]), "the premise: the node's query carries the reply's id and question");
    net.resolver_says(asked.udp.source_port, &RECORDED);
    assert_eq!(net.lan.node.take_resolved(), [Resolved { id, result: Ok(vec![[8, 8, 4, 4], [8, 8, 8, 8]]) }]);
}

// RFC 1122 §3.3.1.1 and §4.1.3.3: a datagram whose next hop never answers ARP is reported to the
// socket it left from. Each of a lookup's queries to such a resolver is counted as the network
// reported it, and waited out like one nobody answered.
#[test]
fn a_query_the_network_reports_unreachable_is_counted_and_waited_out() {
    let mut net = Net::leased(&[SILENT], Some(R));
    let id = net.resolve("www.example").unwrap();
    let rounds = u64::try_from(ROUNDS).unwrap();
    assert_eq!(net.run(id, 10 * WAIT_MS), Some(Err(Ended::Failed(Failure::TimedOut))));
    assert_eq!(net.ms(), rounds * WAIT_MS);
    assert!(net.queried.is_empty(), "the premise: no query reached the wire: {:?}", net.queried);
    assert_eq!(net.lan.node.counters().get(Counter::QueryFailed), rounds);
    assert_eq!(net.lan.node.counters().get(Counter::QueryUnsent), 0);
}

/// A lookup in flight as the lease's first renewal leaves: the net, the lookup and the renewal's
/// transaction id.
fn renewing_under_a_lookup() -> (Net, LookupId, u32) {
    let mut net = Net::leased(&[ANSWERS], Some(R));
    let sent = net.lan.udp().iter().filter(|udp| udp.port == 67).count();
    assert!(net.lan.run_until(Duration::from_secs(50_000), |lan| lan.udp().iter().filter(|udp| udp.port == 67).count() > sent), "the renewal leaves");
    let renewal = xid(&net.lan.udp().iter().rfind(|udp| udp.port == 67).expect("a renewal").payload);
    net.seen = net.lan.sent.len();
    let id = net.resolve("www.example").unwrap();
    net.pass();
    assert_eq!(net.queried.len(), 1, "the premise: the lookup's first query left");
    (net, id, renewal)
}

/// The options of a day's lease through the router that names `resolver`.
fn naming(resolver: Ipv4Addr) -> Vec<(u8, Vec<u8>)> {
    let mut options = terms(86_400, Some(R));
    options.retain(|(code, _)| *code != 6);
    options.push((6, resolver.octets().to_vec()));
    options
}

// The resolvers are the held lease's and come and go with it. A lookup asks the ones its lease
// named: a renewal that names the same ones leaves it be, and its answer still ends it.
#[test]
fn a_lookup_outlives_a_renewal_that_names_the_same_resolvers() {
    let (mut net, id, renewal) = renewing_under_a_lookup();
    net.lan.deliver(&from_server(MAC, A, &message_of(ACK, renewal, &naming(ANSWERS))));
    assert_eq!(net.lan.node.take_resolved(), NONE);
    let asked = net.queried[0].clone();
    net.resolver_says(asked.udp.source_port, &gives(&asked.udp.payload, ADDRESS));
    assert_eq!(net.lan.node.take_resolved(), [Resolved { id, result: Ok(vec![ADDRESS]) }]);
}

// A renewal that names another resolver ends the lookup by name, at once: no further query goes
// to an address the held lease does not name, its answer is not read, and its port is free.
#[test]
fn a_lookup_ends_when_its_leases_resolvers_change() {
    let (mut net, id, renewal) = renewing_under_a_lookup();
    net.lan.deliver(&from_server(MAC, A, &message_of(ACK, renewal, &naming(SILENT))));
    assert_eq!(net.lan.node.lease().expect("held").dns, [SILENT], "the premise: the renewed lease names another resolver");
    assert_eq!(net.lan.node.take_resolved(), [Resolved { id, result: Err(Ended::LeaseChanged) }]);
    let left = net.queried[0].udp.source_port;
    assert!(!net.port_held(left), "port {left} of the ended lookup");
}

// RFC 2131 §3.1: a DHCPNAK takes the lease, and the lookup under it ends by the same name.
#[test]
fn a_lookup_ends_when_its_lease_is_lost() {
    let (mut net, id, renewal) = renewing_under_a_lookup();
    net.lan.deliver(&from_server(common::BROADCAST, Ipv4Addr::BROADCAST, &message(NAK, renewal, Ipv4Addr::UNSPECIFIED, &[(54, R.octets().to_vec())])));
    assert_eq!(net.lan.node.lease(), None, "the premise: the lease is gone");
    assert_eq!(net.lan.node.take_resolved(), [Resolved { id, result: Err(Ended::LeaseChanged) }]);
}
