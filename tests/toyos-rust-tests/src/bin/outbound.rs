//! Whether this machine reaches its router and the internet, said in words a
//! judge reads off the stick (`tests/common/outbound.rs`): the
//! `outbound_router` and `outbound_internet` metal rows run it.
//!
//! It waits for netstack's own word on its lease, asks netstack what it holds,
//! then for each [`Anchor`] at once looks the name up and opens one connection
//! to port 443 of its first address, which it closes without writing a byte,
//! and asks netstack for the router's neighbour entry. Each fact is a line the
//! moment it is known, so a job its runner ends has said what it found.
//!
//! **It asserts nothing about the network**: a machine with no cable exits 0
//! having said so, and the rows judge. It panics only where netstack or
//! logkeeper does not answer it at all.
//!
//! **Every line is a [`Line`]**, which holds no address.
//!
//! Its time, against the runner's `toyos_tco::JOB_BOUND_MS` from boot:
//! netstack says it has no lease `toyos_tco::LEASE_BOUND_MS` after it came up,
//! so a lease is at most that late; a lookup asks at most three resolvers
//! `toyos_dns::ROUNDS` times, `toyos_dns::WAIT_MS` each; and a connect has
//! [`CONNECT_MS`]: 20 s, 18 s and 5 s, the anchors side by side.

use std::collections::BTreeMap;
use std::time::Duration;

use toyos::net::{MsgType, NetError, NetstackConn, UdpSendToRequest};
use toyos_inspect::{Value, NET};
use toyos_logstream::program_line;

#[path = "../outbound_said.rs"]
#[allow(dead_code, reason = "the judges read with the half this job does not call")]
mod said;
#[path = "../served_log.rs"]
mod served_log;

use said::{Anchor, Asked, Connect, Driver, Frames, Line, Link, Lookup, Neighbour, Word};

/// The program whose lines these are, as the supervisor names its ring.
const NETSTACK: &str = "netstack";

/// What netstack's line on a lease it took opens with, and on its bound
/// passing with none; and its whole line where it was endowed no card.
const LEASED: &str = "netstack: DHCP: lease ";
const NO_LEASE: &str = "netstack: DHCP: no lease as ";
const NO_CARD: &str = "netstack: no NIC on this machine, exiting";

/// The ceiling on netstack's word reaching this job: netstack's own bound on
/// saying it, and two of logkeeper's rounds at its write budget
/// (`userland/logkeeper/src/policy.rs`, 5 s), since a served line is one the
/// stick already holds.
const WORD_BOUND: Duration = Duration::from_millis(toyos_tco::LEASE_BOUND_MS + 10_000);

/// How long one connect has, which netstack keeps.
const CONNECT_MS: u32 = 5_000;

const HTTPS: u16 = 443;

/// The router's neighbour entry in netstack's `inspect` answer, where its
/// stack says one.
const NEIGHBOUR: &str = "net.neighbour.router";

/// The discard service (RFC 863): a datagram to it asks for no answer.
const DISCARD: u16 = 9;

/// The largest datagram one Ethernet frame carries: 1500 less the IPv4 and
/// UDP headers.
const DATAGRAM: usize = 1500 - 20 - 8;

/// The Intel driver's usable transmit descriptors (`toyos-i219`, a ring of 16),
/// and how many rings' worth the burst is.
const RING: usize = 15;
const RINGS: usize = 4;

type Snapshot = BTreeMap<String, Value>;

fn say(line: Line) {
    println!("{line}");
}

/// netstack's first word about its lease, off the served log.
fn netstack_said() -> Word {
    let mut word = None;
    served_log::Log::open().until("netstack's word on its lease", WORD_BOUND, |line| {
        let Some(said) = program_line(line).filter(|said| said.tag == NETSTACK) else { return false };
        let this = if said.text.starts_with(LEASED) {
            Some(Word::Lease)
        } else if said.text.starts_with(NO_LEASE) {
            Some(Word::NoLease)
        } else if said.text == NO_CARD {
            Some(Word::NoCard)
        } else {
            None
        };
        word = word.or(this);
        word.is_some()
    });
    word.expect("the wait ended on a word")
}

fn ask() -> Snapshot {
    inspect::ask(NET).unwrap_or_else(|why| panic!("netstack's inspect answer: {why}"))
}

/// A key netstack's answer must carry as text. Its value is in no panic: it
/// may be an address.
fn text<'a>(snapshot: &'a Snapshot, key: &str) -> &'a str {
    match snapshot.get(key) {
        Some(Value::Text(text)) => text,
        Some(_) => panic!("netstack's snapshot carries {key} as something that is not text"),
        None => panic!("netstack's snapshot carries no {key}"),
    }
}

fn frames(snapshot: &Snapshot, key: &str) -> Frames {
    match snapshot.get(key) {
        Some(Value::U64(count)) => Frames(Some(*count)),
        Some(_) => panic!("netstack's snapshot carries {key} as something that is not a count"),
        None => Frames(None),
    }
}

fn card(snapshot: &Snapshot) -> Line {
    let driver = text(snapshot, "net.driver");
    let link = text(snapshot, "net.link.state");
    Line::Card {
        driver: Driver::read(driver).unwrap_or(Driver::Other),
        link: Link::read(link).unwrap_or_else(|| panic!("netstack's snapshot carries a link state this job has no word for")),
        sent: frames(snapshot, "net.wire.sent"),
        received: frames(snapshot, "net.wire.received"),
    }
}

fn lease(snapshot: &Snapshot) -> Line {
    let held = match snapshot.get("net.lease.held") {
        Some(Value::Bool(held)) => *held,
        _ => panic!("netstack's snapshot does not say whether it holds a lease"),
    };
    if !held {
        return Line::Lease { held, router: false, resolver: said::Resolver::None };
    }
    let router = snapshot.contains_key("net.lease.router").then(|| text(snapshot, "net.lease.router"));
    let resolver = said::resolver(text(snapshot, "net.lease.address"), router, text(snapshot, "net.lease.dns"))
        .expect("netstack's words for its lease read as an address with its prefix, a router and resolvers");
    Line::Lease { held, router: router.is_some(), resolver }
}

/// One anchor: its name looked up, and one connection to its first address
/// opened and closed.
fn reach(anchor: Anchor) -> Line {
    let mut first = [[0u8; 4]; 1];
    // A wildcard and not every variant: one source builds against the ABI
    // before a word is added to it and after.
    let lookup = match toyos::net::dns_lookup(anchor.word(), &mut first) {
        Ok(0) => Lookup::NoAddress,
        Ok(_) => Lookup::Addresses,
        Err(NetError::TimedOut) => Lookup::Timeout,
        Err(NetError::Io) => Lookup::Failed,
        Err(NetError::NotConnected) => Lookup::NoResolver,
        Err(_) => Lookup::Refused,
    };
    let connect = match lookup {
        Lookup::Addresses => match toyos::net::tcp_connect(first[0], HTTPS, CONNECT_MS) {
            Ok(open) => {
                let id = open.socket_id;
                drop(open);
                // Unread, as std's own drop leaves it: the peer may have ended
                // the connection first, and what netstack answers then is no
                // fact about reaching it.
                let _ = toyos::net::tcp_close(id);
                Connect::Connected
            }
            Err(NetError::ConnectionRefused) => Connect::Refused,
            Err(NetError::ConnectionReset) => Connect::Reset,
            Err(NetError::TimedOut) => Connect::Timeout,
            Err(NetError::NotConnected) => Connect::NoAddress,
            Err(_) => Connect::Error,
        },
        _ => Connect::NotTried,
    };
    Line::Anchor { anchor, lookup, connect }
}

fn neighbour(snapshot: &Snapshot) -> Neighbour {
    if !snapshot.contains_key(NEIGHBOUR) {
        return Neighbour::NotAsked;
    }
    Neighbour::read(text(snapshot, NEIGHBOUR))
        .unwrap_or_else(|| panic!("netstack's snapshot carries {NEIGHBOUR} as a word this job has none for"))
}

/// A burst of [`RINGS`] × [`RING`] full-size datagrams from one socket to the
/// router's discard port, a ring's worth of requests put to netstack at once
/// so its pass finds them together; how many netstack took.
fn burst(router: [u8; 4]) -> u64 {
    let socket = toyos::net::udp_bind([0; 4], 0).unwrap_or_else(|e| panic!("a datagram socket for the burst: {e:?}"));
    let datagram = [0u8; DATAGRAM];
    let mut taken = 0;
    for _ in 0..RINGS {
        let askers: Vec<NetstackConn> = (0..RING)
            .map(|_| NetstackConn::connect().unwrap_or_else(|e| panic!("a connection to netstack for the burst: {e:?}")))
            .collect();
        for _ in 0..RING {
            assert_eq!(socket.tx.write(&datagram), Ok(DATAGRAM), "a datagram into the socket's pipe");
        }
        let request =
            UdpSendToRequest { socket_id: socket.socket_id.0, addr: router, port: DISCARD, len: DATAGRAM as u16 };
        let asked: Vec<_> =
            askers.into_iter().map(|asker| asker.request(MsgType::UdpSendTo, &request)).collect();
        // A datagram netstack's queue had no place for is refused, and counted
        // by its absence.
        taken += asked.into_iter().filter_map(|pending| pending.ok()?.response::<u32>().ok()).count() as u64;
    }
    // Unread, as the anchors' close is.
    let _ = toyos::net::udp_close(socket.socket_id);
    taken
}

fn asked(snapshot: &Snapshot, key: &str) -> Asked {
    Asked(frames(snapshot, key).0)
}

fn main() {
    let word = netstack_said();
    say(Line::Netstack(word));
    if word != Word::NoCard {
        let held = ask();
        say(card(&held));
        let lease = lease(&held);
        say(lease);
        if matches!(lease, Line::Lease { held: true, .. }) {
            std::thread::scope(|anchors| {
                for anchor in Anchor::ALL {
                    anchors.spawn(|| say(reach(*anchor)));
                }
            });
            say(Line::Gateway(neighbour(&ask())));
            if let Some(router) = held.get("net.lease.router") {
                let Value::Text(router) = router else { panic!("netstack's snapshot carries a router that is not text") };
                let router: std::net::Ipv4Addr =
                    router.parse().expect("netstack's word for its lease's router reads as an address");
                let taken = burst(router.octets());
                let after = ask();
                say(Line::Ring {
                    full: asked(&after, "net.transmit.full"),
                    wake_armed: asked(&after, "net.transmit.wake_armed"),
                    wake_taken: asked(&after, "net.transmit.wake_taken"),
                    unsent: asked(&after, "net.descriptors.unsent"),
                    taken,
                });
            }
        }
    }
    say(Line::Done);
}
