//! The machine's `<host>.local` name on the node. No scenario ids: a query's reading and an
//! answer's bytes are `toyos-mdns`'s own tests; these are the node's part, the group, the port,
//! the clock, the lease and where an answer is sent. What is not ours: `etherparse` reads every
//! frame the node emits (`lan::outside`), and every message here, asked or expected, is written
//! out from RFC 1035 §4.1's layout and RFC 6762's rules, by the section each test names, and
//! never by `toyos-mdns`.

mod common;
mod lan;

use std::net::Ipv4Addr;
use std::time::Duration;

use common::{sum, terms, A, MAC_B, MAC_R, R};
use lan::{udp, Lan, Seen, Udp, B, OFF_LINK};
use toyos_mdns::Host;
use toyos_net_node::{Counter, Refused};
use toyos_net_wire::Port;

/// RFC 6762 §3: the group, and the port every responder listens on.
const GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
const MDNS: u16 = 5_353;
/// RFC 1112 §6.4: 01:00:5e and the group's low 23 bits.
const GROUP_MAC: [u8; 6] = [0x01, 0x00, 0x5e, 0x00, 0x00, 0xfb];
/// `toyos.local`, as RFC 1035 §3.1 spells a name: each label behind its length, then the root.
const NAME: &[u8] = b"\x05toyos\x05local\x00";
const TYPE_A: u16 = 1;
const CLASS_IN: u16 = 1;
/// RFC 6762 §5.4: the top bit of a question's class asks for a unicast response.
const UNICAST_RESPONSE: u16 = 0x8000;
/// RFC 6762 §10.2: the top bit of a record's class is cache-flush.
const CACHE_FLUSH: u16 = 0x8000;

/// A node told its name, its link down.
fn named() -> Lan {
    let mut lan = Lan::new();
    lan.node.answer_as(lan.now, Host::new("toyos").unwrap()).unwrap();
    lan
}

/// [`named`], its lease held, both announcements out and two quiet seconds after them: a query
/// from here is answered at once (RFC 6762 §6).
fn settled() -> Lan {
    let mut lan = named();
    lan.lease(3_600);
    assert!(lan.run_until(Duration::from_secs(2), |lan| lan.datagrams().len() == 2), "both announcements");
    let quiet = lan.now.after(Duration::from_secs(2));
    lan.fire(quiet);
    assert_eq!(lan.datagrams().len(), 2);
    lan
}

/// A query (RFC 1035 §4.1.1, §4.1.2): the header with one question, then QNAME, QTYPE, QCLASS.
fn query(id: u16, labels: &[&str], qtype: u16, qclass: u16) -> Vec<u8> {
    let mut query = [&id.to_be_bytes()[..], &[0, 0, 0, 1, 0, 0, 0, 0, 0, 0]].concat();
    for label in labels {
        query.push(u8::try_from(label.len()).unwrap());
        query.extend_from_slice(label.as_bytes());
    }
    query.push(0);
    query.extend_from_slice(&qtype.to_be_bytes());
    query.extend_from_slice(&qclass.to_be_bytes());
    query
}

/// The query for this machine's address.
fn ours(id: u16, qclass: u16) -> Vec<u8> {
    query(id, &["toyos", "local"], TYPE_A, qclass)
}

/// `payload` to the group's port in a frame to the group's link address.
fn to_group(from: [u8; 6], source: (Ipv4Addr, u16), payload: &[u8]) -> Vec<u8> {
    udp(GROUP_MAC, from, source, (GROUP, MDNS), payload)
}

/// The address record (RFC 1035 §4.1.3): NAME, TYPE, CLASS, TTL, RDLENGTH 4, the address.
fn record(class: u16, ttl: u32) -> Vec<u8> {
    [NAME, &TYPE_A.to_be_bytes(), &class.to_be_bytes(), &ttl.to_be_bytes(), &[0, 4], &A.octets()].concat()
}

/// The response RFC 6762 gives a multicast query, and unasked as an announcement: ID 0 and QR
/// and AA alone (§18.1 to §18.4), no question (§6), one answer with cache-flush (§10.2) and the
/// 120 s of a record naming a host (§10).
fn response() -> Vec<u8> {
    [&[0, 0, 0x84, 0x00, 0, 0, 0, 1, 0, 0, 0, 0][..], &record(CLASS_IN | CACHE_FLUSH, 120)].concat()
}

/// The response RFC 6762 §6.7 gives a legacy resolver: its ID, its question, and a record
/// without cache-flush that lives at most ten seconds.
fn legacy_response(id: u16) -> Vec<u8> {
    let question = [NAME, &TYPE_A.to_be_bytes(), &CLASS_IN.to_be_bytes()].concat();
    [&id.to_be_bytes()[..], &[0x84, 0x00, 0, 1, 0, 1, 0, 0, 0, 0], &question, &record(CLASS_IN, 10)].concat()
}

/// A response to the group.
fn multicast(payload: Vec<u8>) -> Udp {
    Udp { to: GROUP_MAC, source: A, source_port: MDNS, destination: GROUP, port: MDNS, ttl: 255, payload }
}

/// A response to B's `port`.
fn unicast(port: u16, payload: Vec<u8>) -> Udp {
    Udp { to: MAC_B, source: A, source_port: MDNS, destination: B, port, ttl: 255, payload }
}

/// The node's datagrams from `from` on.
fn since(lan: &Lan, from: usize) -> Vec<Udp> {
    lan.datagrams().into_iter().skip(from).map(|(_, udp)| udp.clone()).collect()
}

// RFC 9776 §4: every IGMP message has TTL 1, internetwork-control precedence (ToS 0xC0) and the
// Router Alert option (RFC 2113: 0x94, length 4, value 0). §4.2: a version 3 report is type
// 0x22, a reserved octet, the checksum, two reserved octets and the count of group records; a
// record is its type, an auxiliary length of 0, the count of sources and the group, and a join
// from nothing is CHANGE_TO_EXCLUDE_MODE, type 4, with no source. Reports go to 224.0.0.22, at
// RFC 1112 §6.4's link address for it, and §4.2.14 lets one leave from 0.0.0.0 before the
// interface has an address.
#[test]
fn the_group_is_joined_and_its_membership_reported() {
    let mut lan = named();
    lan.pump();
    assert!(lan.sent.is_empty(), "nothing leaves a link that is down");
    lan.lease(3_600);

    let mut message = vec![0x22, 0, 0, 0, 0, 0, 0, 1, 4, 0, 0, 0, 224, 0, 0, 251];
    let check = sum(&message);
    message[2..4].copy_from_slice(&check.to_be_bytes());
    let report = |source: Ipv4Addr| Seen::Igmp {
        to: [0x01, 0x00, 0x5e, 0x00, 0x00, 0x16],
        source,
        destination: Ipv4Addr::new(224, 0, 0, 22),
        ttl: 1,
        tos: 0xC0,
        options: vec![0x94, 4, 0, 0],
        message: message.clone(),
    };
    let reports: Vec<&Seen> = lan.sent.iter().map(|(_, seen)| seen).filter(|seen| matches!(seen, Seen::Igmp { .. })).collect();
    assert_eq!(reports.first(), Some(&&report(Ipv4Addr::UNSPECIFIED)), "the join is reported as the link comes up");
    assert!(reports.iter().all(|seen| **seen == report(Ipv4Addr::UNSPECIFIED) || **seen == report(A)), "{reports:?}");
}

// RFC 6762 §3 and §6: a query is sent to the group, in a frame to the group's link address
// (RFC 1112 §6.4), and the answer to one from port 5353 is multicast back. A host that has not
// joined never sees the frame.
#[test]
fn a_query_in_a_frame_to_the_groups_address_reaches_the_responder() {
    let mut lan = settled();
    let from = lan.datagrams().len();
    lan.deliver(&to_group(MAC_B, (B, MDNS), &ours(0, CLASS_IN)));
    assert_eq!(since(&lan, from), [multicast(response())]);
}

// RFC 6762 §8.3: "The Multicast DNS responder MUST send at least two unsolicited responses, one
// second apart." The node's clock is the test's, and the second is a deadline the node names.
// Nothing is announced for an address still under probe (RFC 5227 §2.1.1).
#[test]
fn a_held_lease_is_announced_at_once_and_a_second_later() {
    let mut lan = named();
    lan.acknowledge(&terms(3_600, Some(R)));
    assert!(lan.run_until(Duration::from_secs(10), |lan| lan.node.lease().is_some()), "the lease is held");
    let held = lan.now;
    assert_eq!(lan.datagrams(), [(held, &multicast(response()))], "announced as the lease is held, and not before");

    let owed = lan.node.next_deadline().expect("the second announcement is owed");
    assert!(owed <= held.after(Duration::from_secs(1)), "and is a deadline of the node's");
    assert!(lan.run_until(Duration::from_secs(2), |lan| lan.datagrams().len() == 2), "the second announcement");
    let (second, again) = lan.datagrams()[1];
    assert_eq!(again, &multicast(response()));
    // The responder's clock is whole milliseconds.
    let apart = second.since(held);
    assert!(apart <= Duration::from_secs(1) && apart > Duration::from_millis(999), "{apart:?} apart");

    let later = lan.now.after(Duration::from_secs(10));
    lan.fire(later);
    assert_eq!(lan.datagrams().len(), 2, "owed twice, sent twice");
}

// RFC 6762 §6.7: a query from a port other than 5353 is a legacy resolver's, and its answer goes
// to its own address and port. §5.4: one from port 5353 with the unicast-response bit is
// answered at its own address, with the response a multicast would carry.
#[test]
fn an_answer_goes_to_the_group_or_to_the_asker_as_the_query_asked() {
    let mut lan = settled();
    let from = lan.datagrams().len();
    lan.deliver(&to_group(MAC_B, (B, 53_000), &ours(0xBEEF, CLASS_IN)));
    lan.deliver(&to_group(MAC_B, (B, MDNS), &ours(0, CLASS_IN | UNICAST_RESPONSE)));
    lan.deliver(&to_group(MAC_B, (B, MDNS), &ours(0, CLASS_IN)));
    assert_eq!(since(&lan, from), [unicast(53_000, legacy_response(0xBEEF)), unicast(MDNS, response()), multicast(response())]);
}

// RFC 6762 §11: "All Multicast DNS responses (including responses sent via unicast) SHOULD be
// sent with IP TTL set to 255."
#[test]
fn every_response_leaves_with_ttl_255() {
    let mut lan = settled();
    lan.deliver(&to_group(MAC_B, (B, 53_000), &ours(1, CLASS_IN)));
    lan.deliver(&to_group(MAC_B, (B, MDNS), &ours(0, CLASS_IN)));
    let ttls: Vec<u8> = lan.datagrams().iter().map(|(_, udp)| udp.ttl).collect();
    assert_eq!(ttls, [255; 4], "two announcements, a unicast answer and a multicast one");
}

// The record is the held lease's: no answer is made up for an address under probe (RFC 5227
// §2.1.1) or one the lease no longer gives.
#[test]
fn the_name_is_answered_only_while_its_lease_is_held() {
    let asked = to_group(MAC_B, (B, 53_000), &ours(7, CLASS_IN));
    let mut lan = named();
    lan.acknowledge(&terms(600, Some(R)));
    lan.deliver(&asked);
    assert!(lan.datagrams().is_empty(), "under probe");

    assert!(lan.run_until(Duration::from_secs(10), |lan| lan.node.lease().is_some()), "the lease is held");
    let from = lan.datagrams().len();
    lan.deliver(&asked);
    assert_eq!(since(&lan, from), [unicast(53_000, legacy_response(7))], "held");

    assert!(lan.run_until(Duration::from_secs(700), |lan| lan.node.lease().is_none()), "the lease runs out");
    let from = lan.datagrams().len();
    lan.deliver(&asked);
    assert!(since(&lan, from).is_empty(), "gone");
    assert_eq!(lan.node.counters().get(Counter::NameUnsent), 0, "and no answer was made for [udp] to refuse");
}

// RFC 6762 §11: a query whose source is not on the local link is silently ignored. The link is
// the lease's prefix.
#[test]
fn a_query_from_off_the_link_is_not_answered() {
    let mut lan = settled();
    let from = lan.datagrams().len();
    for port in [MDNS, 53_000] {
        lan.deliver(&to_group(MAC_R, (OFF_LINK, port), &ours(3, CLASS_IN)));
    }
    assert!(since(&lan, from).is_empty());
    assert_eq!(lan.node.counters().get(Counter::NameUnsent), 0);
}

// RFC 768: a source port of zero is no port to reply to. The answer is [udp]'s to refuse, and
// the node counts that it did.
#[test]
fn an_answer_udp_refuses_is_counted_and_dropped() {
    let mut lan = settled();
    let from = lan.datagrams().len();
    lan.deliver(&to_group(MAC_B, (B, 0), &ours(9, CLASS_IN)));
    assert!(since(&lan, from).is_empty());
    assert_eq!(lan.node.counters().get(Counter::NameUnsent), 1);
    assert_eq!(lan.counted(toyos_net_udp::Counter::SendPortZero), 1);
}

// Nothing a peer sends is trusted to be a message: no prefix of a query is answered, and the
// whole one after them still is.
#[test]
fn no_cut_of_a_query_is_answered() {
    let mut lan = settled();
    let from = lan.datagrams().len();
    let whole = ours(0x1234, CLASS_IN);
    for cut in 0..whole.len() {
        lan.deliver(&to_group(MAC_B, (B, 53_000), &whole[..cut]));
    }
    assert!(since(&lan, from).is_empty(), "no cut is a question");
    assert_eq!(lan.node.counters().get(Counter::NameUnsent), 0);
    lan.deliver(&to_group(MAC_B, (B, 53_000), &whole));
    assert_eq!(since(&lan, from), [unicast(53_000, legacy_response(0x1234))]);
}

// The group's port is one socket's: a client cannot take the responder's, and a responder that
// finds it taken joins nothing.
#[test]
fn the_names_port_is_held_by_one_socket() {
    let mdns = Port::new(MDNS);
    let mut lan = named();
    assert_eq!(lan.node.udp_bind(Ipv4Addr::UNSPECIFIED, mdns, || 0).map(|(_, port)| port), Err(Refused::AddrInUse));
    let again = lan.node.answer_as(lan.now, Host::new("toyos").unwrap());
    assert_eq!(again, Err(toyos_net_udp::Error::Refused(toyos_net_udp::Counter::PortInUse)));

    let mut taken = Lan::new();
    taken.node.udp_bind(Ipv4Addr::UNSPECIFIED, mdns, || 0).unwrap();
    let refused = taken.node.answer_as(taken.now, Host::new("toyos").unwrap());
    assert_eq!(refused, Err(toyos_net_udp::Error::Refused(toyos_net_udp::Counter::PortInUse)));
    taken.lease(3_600);
    assert!(!taken.sent.iter().any(|(_, seen)| matches!(seen, Seen::Igmp { .. })), "no group was joined");
}
