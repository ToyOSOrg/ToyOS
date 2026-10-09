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

use toyos::net::NetError;
use toyos_inspect::{Value, NET};
use toyos_logstream::program_line;

#[path = "../outbound_said.rs"]
#[allow(dead_code, reason = "the judges read with the half this job does not call")]
mod said;
#[path = "../served_log.rs"]
mod served_log;

use said::{Anchor, Connect, Driver, Frames, Line, Link, Lookup, Neighbour, Word};

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
            // Measurement branch only: the card's ring counts after the
            // anchors' connects, numbers alone, on a line the rows do not read.
            let ring = ask();
            let count = |key: &str| match ring.get(key) {
                Some(Value::U64(n)) => n.to_string(),
                _ => "none".to_string(),
            };
            println!(
                "ring: transmit.full={} transmit.wake_armed={} transmit.wake_taken={} descriptors.sent={} wire.sent={} descriptors.stranded={}",
                count("net.transmit.full"),
                count("net.transmit.wake_armed"),
                count("net.transmit.wake_taken"),
                count("net.descriptors.sent"),
                count("net.wire.sent"),
                count("net.descriptors.stranded"),
            );
        }
    }
    say(Line::Done);
    // Measurement branch only: netstack's word on its name, which comes
    // 750 ms to 1 s after the lease (RFC 6762 §8.1's delay, three probes and
    // the wait after them), on a line the rows do not read. The boot holds
    // for it, at most 15 s: past the line above, a panic here moves no row.
    if word == Word::Lease {
        let name = std::panic::catch_unwind(|| {
            let mut said = "none";
            served_log::Log::open().until("netstack's word on its name", Duration::from_secs(15), |line| {
                let Some(line) = program_line(line).filter(|line| line.tag == NETSTACK) else { return false };
                said = if line.text.starts_with("netstack: mDNS: no host answered for ") {
                    "claimed"
                } else if line.text.starts_with("netstack: mDNS: another host answered for ") {
                    "lost"
                } else {
                    return false;
                };
                true
            });
            said
        });
        println!("name: {}", name.unwrap_or("none within 15 s"));
    }
}
