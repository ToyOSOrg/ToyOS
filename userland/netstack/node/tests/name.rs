//! The machine's `<host>.local` name on the node. No scenario ids: a message's reading, the
//! claim's rules and an answer's bytes are `toyos-mdns`'s own tests; these are the node's part,
//! the group, the port, the clock, the draws, the lease and where a message is sent. What is not
//! ours: `etherparse` reads every frame the node emits (`lan::outside`), and every message here,
//! heard or expected, is written out from RFC 1035 §4.1's layout and RFC 6762's rules, by the
//! section each test names, and never by `toyos-mdns`.

mod common;
mod lan;

use std::net::Ipv4Addr;
use std::time::Duration;

use common::{arp, sum, terms, A, BROADCAST, MAC, MAC_B, MAC_R, R};
use lan::{udp, Lan, Seen, Udp, B, OFF_LINK};
use toyos_mdns::{Event, Host, Lost};
use toyos_net_node::{Counter, Refused};
use toyos_net_wire::{Instant, Port};

/// RFC 6762 §3: the group, and the port every responder listens on.
const GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
const MDNS: u16 = 5_353;
/// RFC 1112 §6.4: 01:00:5e and the group's low 23 bits.
const GROUP_MAC: [u8; 6] = [0x01, 0x00, 0x5e, 0x00, 0x00, 0xfb];
/// `toyos.local`, as RFC 1035 §3.1 spells a name: each label behind its length, then the root.
const NAME: &[u8] = b"\x05toyos\x05local\x00";
const TYPE_A: u16 = 1;
/// RFC 6762 §8.1: a probe's question is of type "ANY" (255).
const TYPE_ANY: u16 = 255;
const CLASS_IN: u16 = 1;
/// RFC 6762 §5.4: the top bit of a question's class asks for a unicast response.
const UNICAST_RESPONSE: u16 = 0x8000;
/// RFC 6762 §10.2: the top bit of a record's class is cache-flush.
const CACHE_FLUSH: u16 = 0x8000;
/// RFC 6762 §8.1: the delay before the first probe is at most this, and the probes are this far
/// apart.
const QUARTER: Duration = Duration::from_millis(250);
const SECOND: Duration = Duration::from_secs(1);

/// The draw of a call that starts no probing.
fn undrawn() -> u32 {
    panic!("no probing starts here, so no delay is drawn")
}

/// A node told its name, its link down.
fn named() -> Lan {
    let mut lan = Lan::new();
    lan.node.answer_as(lan.now, Host::new("toyos").unwrap(), undrawn).unwrap();
    lan
}

/// The node's datagrams until there are `count` of them, which is at most `limit` away.
fn until(lan: &mut Lan, count: usize, limit: Duration) {
    assert!(lan.run_until(limit, |lan| lan.datagrams().len() >= count), "{count} datagrams: {:?}", lan.datagrams());
    assert_eq!(lan.datagrams().len(), count);
}

/// [`named`], its lease held, its three probes and both announcements out and two quiet seconds
/// after them: a query from here is answered at once (RFC 6762 §6).
fn settled() -> Lan {
    let mut lan = named();
    lan.lease(3_600);
    until(&mut lan, 5, Duration::from_secs(3));
    assert_eq!(lan.node.name_event(), Some(Event::Claimed));
    let quiet = lan.now.after(Duration::from_secs(2));
    lan.fire(quiet);
    assert_eq!(lan.datagrams().len(), 5);
    lan
}

/// A message (RFC 1035 §4.1.1): the header, with `counts` questions, answers and authority
/// records, and then `sections` as they are.
fn message(id: u16, flags: u16, counts: [u16; 3], sections: &[&[u8]]) -> Vec<u8> {
    let mut message = Vec::new();
    for word in [id, flags, counts[0], counts[1], counts[2], 0] {
        message.extend_from_slice(&word.to_be_bytes());
    }
    for section in sections {
        message.extend_from_slice(section);
    }
    message
}

/// A question (RFC 1035 §4.1.2) for this machine's name: QNAME, QTYPE, QCLASS.
fn question(qtype: u16, qclass: u16) -> Vec<u8> {
    [NAME, &qtype.to_be_bytes(), &qclass.to_be_bytes()].concat()
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

/// An address record (RFC 1035 §4.1.3) of this machine's name: NAME, TYPE, CLASS, TTL, RDLENGTH
/// 4, the address.
fn record_of(address: Ipv4Addr, class: u16, ttl: u32) -> Vec<u8> {
    [NAME, &TYPE_A.to_be_bytes(), &class.to_be_bytes(), &ttl.to_be_bytes(), &[0, 4], &address.octets()].concat()
}

/// This machine's address record.
fn record(class: u16, ttl: u32) -> Vec<u8> {
    record_of(A, class, ttl)
}

/// The response RFC 6762 gives a multicast query, and unasked as an announcement: ID 0 and QR
/// and AA alone (§18.1 to §18.4), no question (§6), one answer with cache-flush (§10.2) and the
/// 120 s of a record naming a host (§10).
fn response() -> Vec<u8> {
    message(0, 0x8400, [0, 1, 0], &[&record(CLASS_IN | CACHE_FLUSH, 120)])
}

/// The response RFC 6762 §6.7 gives a legacy resolver: its ID, its question, and a record
/// without cache-flush that lives at most ten seconds.
fn legacy_response(id: u16) -> Vec<u8> {
    message(id, 0x8400, [1, 1, 0], &[&question(TYPE_A, CLASS_IN), &record(CLASS_IN, 10)])
}

/// A probe for this machine's name proposing `address` (RFC 6762 §8.1, §8.2): ID 0 and no flag
/// (§18.1, §18.2), the question `ANY` with `qclass`, and the proposed record in the Authority
/// Section, its class without cache-flush, which §10.2 sets in responses.
fn probe_of(address: Ipv4Addr, qclass: u16) -> Vec<u8> {
    message(0, 0, [1, 0, 1], &[&question(TYPE_ANY, qclass), &record_of(address, CLASS_IN, 120)])
}

/// The node's probe: §8.1 has it sent "QU".
fn probe() -> Vec<u8> {
    probe_of(A, CLASS_IN | UNICAST_RESPONSE)
}

/// Another host's response naming `address` for this machine's name.
fn says(address: Ipv4Addr) -> Vec<u8> {
    message(0, 0x8400, [0, 1, 0], &[&record_of(address, CLASS_IN | CACHE_FLUSH, 120)])
}

/// A message to the group.
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

/// When each of the node's datagrams from `from` on left.
fn times(lan: &Lan, from: usize) -> Vec<Instant> {
    lan.datagrams().into_iter().skip(from).map(|(at, _)| at).collect()
}

/// Whether `later` is `apart` after `earlier`, on a clock of whole milliseconds, which is the
/// responder's: its deadlines are whole, and the instant it starts from need not be.
fn after(earlier: Instant, later: Instant, apart: Duration) -> bool {
    let waited = later.since(earlier);
    waited <= apart && waited + Duration::from_millis(1) > apart
}

/// The claim RFC 6762 §8 asks for, as the node's five datagrams from `from` on, the first probe
/// within §8.1's 250 ms of `start`: three probes 250 ms apart (§8.1), then, 250 ms after the
/// third, the two announcements a second apart (§8.3).
fn claimed(lan: &Lan, from: usize, start: Instant) {
    let probes = [probe(), probe(), probe()].map(multicast);
    let announcements = [response(), response()].map(multicast);
    assert_eq!(since(lan, from), [&probes[..], &announcements].concat());
    let at = times(lan, from);
    assert!(at[0] >= start && at[0].since(start) <= QUARTER, "the first probe, {:?} after", at[0].since(start));
    for (earlier, later, apart) in [(at[0], at[1], QUARTER), (at[1], at[2], QUARTER), (at[2], at[3], QUARTER), (at[3], at[4], SECOND)] {
        assert!(after(earlier, later, apart), "{:?}, and {apart:?} is asked", later.since(earlier));
    }
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

// RFC 6762 §8: "it MUST perform the two startup steps below: Probing (Section 8.1) and
// Announcing (Section 8.3)." §8.1: "250 ms after the first query, the host should send a second;
// then, 250 ms after that, a third. If, by 250 ms after the third probe, no conflicting
// Multicast DNS responses have been received, the host may move to the next step, announcing."
// §8.3: "at least two unsolicited responses, one second apart." The node's clock is the test's,
// and each step is a deadline the node names. Nothing is probed for under an address still under
// probe itself (RFC 5227 §2.1.1), and what became of the name is the node's to say.
#[test]
fn a_held_lease_is_probed_for_three_times_and_its_name_announced_only_then() {
    let mut lan = named();
    lan.acknowledge(&terms(3_600, Some(R)));
    assert!(lan.datagrams().is_empty(), "an address under probe has no name yet");
    assert!(lan.run_until(Duration::from_secs(10), |lan| lan.node.lease().is_some()), "the lease is held");
    let held = lan.now;
    assert_eq!(lan.node.name_event(), None, "the name is not this machine's yet");
    let asked = to_group(MAC_B, (B, 53_000), &ours(7, CLASS_IN));

    until(&mut lan, 3, SECOND);
    assert_eq!(lan.node.name_event(), None);
    lan.deliver(&asked);
    assert_eq!(since(&lan, 0), [probe(), probe(), probe()].map(multicast), "three probes, and no answer under them");

    until(&mut lan, 5, Duration::from_secs(2));
    claimed(&lan, 0, held);
    assert_eq!(lan.node.name_event(), Some(Event::Claimed));
    let later = lan.now.after(Duration::from_secs(10));
    lan.fire(later);
    assert_eq!(lan.datagrams().len(), 5, "owed five, sent five");
    lan.deliver(&asked);
    assert_eq!(since(&lan, 5), [unicast(53_000, legacy_response(7))], "and the name is answered with");
}

// RFC 6762 §8.1: "the host should first wait for a short random delay time, uniformly
// distributed in the range 0-250 ms." The delay is a draw of the node's caller, taken by the call
// that starts the probing and reduced to that range by the responder: 1,000 is 247 ms. A lease
// held before the name is told is probed on by `answer_as`; one that outlives its link, by the
// call that reports the link's return, behind its client's own draws.
#[test]
fn the_first_probe_follows_the_delay_the_callers_draw_gives() {
    let delay = Duration::from_millis(1_000 % 251);
    let mut lan = Lan::new();
    lan.lease(3_600);
    assert!(lan.datagrams().is_empty());
    let told = lan.now;
    lan.node.answer_as(told, Host::new("toyos").unwrap(), || 1_000).unwrap();
    lan.pump();
    assert!(lan.datagrams().is_empty(), "nothing before the delay is over");
    let first = lan.node.next_deadline().expect("the first probe is owed");
    assert!(after(told, first, delay), "{:?} after the name is told", first.since(told));
    until(&mut lan, 5, Duration::from_secs(3));
    assert_eq!(times(&lan, 0)[0], first);
    assert_eq!(lan.datagrams()[0].1, &multicast(probe()));

    let quiet = lan.now.after(Duration::from_secs(5));
    lan.fire(quiet);
    lan.link(false);
    let back = lan.now;
    lan.node.link(back, true, || 1_000);
    lan.pump();
    assert_eq!(lan.datagrams().len(), 5, "nothing before the delay is over");
    until(&mut lan, 6, SECOND);
    assert!(after(back, times(&lan, 5)[0], delay), "{:?} after the link returned", times(&lan, 5)[0].since(back));
}

// RFC 6762 §8: "Whenever a Multicast DNS responder starts up, wakes up from sleep, receives an
// indication of a network interface "Link Change" event, or has any other reason to believe that
// its network connectivity may have changed in some relevant way, it MUST perform the two startup
// steps below: Probing (Section 8.1) and Announcing (Section 8.3)." The node keeps its lease
// across the link, so the address is not new and only the link's return says the name is in
// doubt: it is answered to nobody until it has been probed for again.
#[test]
fn a_held_name_is_probed_for_again_when_the_link_returns_and_announced_only_then() {
    let mut lan = settled();
    let from = lan.datagrams().len();
    lan.link(false);
    let down = lan.now.after(Duration::from_secs(30));
    lan.fire(down);
    assert!(since(&lan, from).is_empty(), "a link that went owes the name nothing");

    lan.link(true);
    let back = lan.now;
    assert!(lan.node.lease().is_some(), "the lease outlived the link");
    until(&mut lan, from + 1, SECOND);
    lan.deliver(&to_group(MAC_B, (B, 53_000), &ours(7, CLASS_IN)));
    assert_eq!(since(&lan, from), [multicast(probe())], "probed for, and answered to nobody under the probe");
    assert_eq!(lan.node.name_event(), None);

    until(&mut lan, from + 5, Duration::from_secs(3));
    claimed(&lan, from, back);
    assert_eq!(lan.node.name_event(), Some(Event::Claimed));
    let quiet = lan.now.after(Duration::from_secs(10));
    lan.fire(quiet);
    assert_eq!(lan.datagrams().len(), from + 5, "owed five, sent five");
}

// A probe or an announcement that falls due with the link down: [udp] has no route for it, so it
// is counted and gone, and the responder holds it sent. The name it then claims was probed for
// on no wire, and nobody can ask for it there; the link's return is told to the responder, which
// probes for it again (RFC 6762 §8) before a host on the link hears it announced.
#[test]
fn what_the_name_is_owed_with_the_link_down_is_counted_and_the_links_return_probes_for_it() {
    let mut lan = named();
    lan.lease(3_600);
    lan.link(false);
    let left = lan.datagrams().len();
    assert!(!lan.run_until(Duration::from_secs(5), |_| false), "every deadline of five seconds");
    assert_eq!(lan.datagrams().len(), left, "nothing leaves a link that is down");
    let unsent = lan.node.counters().get(Counter::NameUnsent);
    assert_eq!(u64::try_from(left).unwrap() + unsent, 5, "three probes and two announcements, each sent or counted");
    assert!(unsent >= 4, "all but a probe that left before the link went");
    assert_eq!(lan.node.name_event(), Some(Event::Claimed), "held, on no wire");

    lan.now = lan.now.after(Duration::from_secs(5));
    lan.link(true);
    let back = lan.now;
    until(&mut lan, left + 5, Duration::from_secs(3));
    claimed(&lan, left, back);
    assert_eq!(lan.node.name_event(), Some(Event::Claimed));
    assert_eq!(lan.node.counters().get(Counter::NameUnsent), unsent);
}

// RFC 6762 §8.1: "During probing, from the time the first probe packet is sent until 250 ms after
// the third probe, if any conflicting Multicast DNS response is received, then the probing host
// MUST defer to the existing host". The node's probes are "QU", so the host that holds the name
// may answer at the node's own address as well as to the group, and either reaches the
// responder. The name is then nobody's here: not announced, not answered with, and said lost.
// The link's return is a reason to probe for it again (§8).
#[test]
fn another_hosts_answer_under_the_probe_takes_the_name_and_the_node_says_so() {
    let to_the_group = to_group(MAC_B, (B, MDNS), &says(B));
    let to_the_node = udp(MAC, MAC_B, (B, MDNS), (A, MDNS), &says(B));
    for defended in [to_the_group, to_the_node] {
        let mut lan = named();
        lan.lease(3_600);
        until(&mut lan, 1, SECOND);
        lan.deliver(&defended);
        assert_eq!(lan.node.name_event(), Some(Event::Lost(Lost::Answered)));
        let later = lan.now.after(Duration::from_secs(60));
        lan.fire(later);
        lan.deliver(&to_group(MAC_B, (B, 53_000), &ours(7, CLASS_IN)));
        lan.deliver(&to_group(MAC_B, (B, MDNS), &ours(0, CLASS_IN)));
        assert_eq!(since(&lan, 0), [multicast(probe())], "one probe, and then no probe, no announcement and no answer");
        assert_eq!(lan.node.name_event(), None, "said once");

        lan.link(false);
        lan.link(true);
        let back = lan.now;
        until(&mut lan, 6, Duration::from_secs(3));
        claimed(&lan, 1, back);
        assert_eq!(lan.node.name_event(), Some(Event::Claimed), "the other host is gone, and the name is this machine's");
    }
}

// RFC 6762 §8.2: "The two records are compared and the lexicographically later data wins. This
// means that if the host finds that its own data is lexicographically later, it simply ignores
// the other host's probe. If the host finds that its own data is lexicographically earlier, then
// it defers to the winning host by waiting one second, and then begins probing for this record
// again." The node is 192.0.2.1: B's 192.0.2.7 is later, and 192.0.2.0 earlier.
#[test]
fn a_later_probe_of_another_hosts_makes_the_node_wait_a_second_and_probe_again() {
    let mut lan = named();
    lan.lease(3_600);
    until(&mut lan, 1, SECOND);
    let first = times(&lan, 0)[0];
    lan.deliver(&to_group(MAC_B, (B, MDNS), &probe_of(Ipv4Addr::new(192, 0, 2, 0), CLASS_IN | UNICAST_RESPONSE)));
    until(&mut lan, 2, SECOND);
    assert!(after(first, times(&lan, 0)[1], QUARTER), "an earlier probe is ignored");

    let heard = lan.now;
    lan.deliver(&to_group(MAC_B, (B, MDNS), &probe_of(B, CLASS_IN | UNICAST_RESPONSE)));
    until(&mut lan, 3, Duration::from_secs(2));
    assert!(after(heard, times(&lan, 0)[2], SECOND), "a later one is deferred to for a second: {:?}", times(&lan, 0)[2].since(heard));
    until(&mut lan, 7, Duration::from_secs(3));
    claimed(&lan, 2, times(&lan, 0)[2]);
    assert_eq!(lan.node.name_event(), Some(Event::Claimed), "nobody answered the second probing");
}

// RFC 6762 §9: a response that conflicts with a held name means the responder "MUST immediately
// reset its conflicted unique record to probing state, and go through the startup steps described
// above in Section 8". §8.1: another host's probe is answered "to defend that name immediately",
// by unicast where it asked "QU".
#[test]
fn a_held_name_is_defended_and_probed_for_again_when_another_host_answers_for_it() {
    let mut lan = settled();
    let from = lan.datagrams().len();
    lan.deliver(&to_group(MAC_B, (B, MDNS), &probe_of(B, CLASS_IN | UNICAST_RESPONSE)));
    assert_eq!(since(&lan, from), [unicast(MDNS, response())], "B's probe is answered at B's address, at once");

    let contested = lan.now;
    lan.deliver(&to_group(MAC_B, (B, MDNS), &says(B)));
    lan.deliver(&to_group(MAC_B, (B, 53_000), &ours(7, CLASS_IN)));
    assert_eq!(lan.datagrams().len(), from + 1, "the name is answered to nobody");
    until(&mut lan, from + 6, Duration::from_secs(3));
    claimed(&lan, from + 1, contested);
    assert_eq!(lan.node.name_event(), Some(Event::Claimed), "B said no more: the name is held again");
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

// RFC 6762 §6: "a Multicast DNS responder MUST NOT (except in the one special case of answering
// probe queries) multicast a record on a given interface until at least one second has elapsed
// since the last time that record was multicast on that particular interface." The bound is the
// responder's, on the clock the node hands it; the answer it holds back is a deadline of the
// node's, and one multicast then answers every query it held.
#[test]
fn a_second_query_inside_a_second_waits_for_the_second_to_end() {
    let mut lan = settled();
    let from = lan.datagrams().len();
    let asked = lan.now;
    lan.deliver(&to_group(MAC_B, (B, MDNS), &ours(0, CLASS_IN)));
    lan.deliver(&to_group(MAC_B, (B, MDNS), &ours(0, CLASS_IN)));
    lan.deliver(&to_group(MAC_R, (R, MDNS), &ours(0, CLASS_IN)));
    assert_eq!(lan.datagrams()[from..], [(asked, &multicast(response()))], "the first is answered at once, the others not yet");

    let owed = lan.node.next_deadline().expect("the held answer is owed");
    assert!(owed <= asked.after(SECOND), "and is a deadline of the node's");
    assert!(lan.run_until(Duration::from_secs(3), |lan| lan.datagrams().len() == from + 2), "the held answer leaves");
    let (answered, again) = lan.datagrams()[from + 1];
    assert_eq!(again, &multicast(response()));
    assert!(after(asked, answered, SECOND), "{:?} after the first", answered.since(asked));

    let later = lan.now.after(Duration::from_secs(10));
    lan.fire(later);
    assert_eq!(lan.datagrams().len(), from + 2, "one multicast answered both");
}

// RFC 3927 §2.6.2: "If the destination address is in the 169.254/16 prefix ... then the sender
// MUST ARP for the destination address and then send the packet directly to the destination on
// the same physical link. This MUST be done whether the interface is configured with a
// Link-Local or a routable IPv4 address." And: "The host MUST NOT send a packet with an IPv4
// Link-Local destination address to any router for forwarding" (§2.7 too). The responder takes
// a link-local source for one on this link (RFC 6762 §11); the node holds a routable lease and
// a router, asks the link for the asker itself, and answers it there. §2.5: the asker's ARP
// reply, its sender link-local, comes in a frame to the link's broadcast address.
#[test]
fn an_answer_to_a_link_local_asker_goes_to_its_own_link_address() {
    let link_local = Ipv4Addr::new(169, 254, 3, 4);
    let mut lan = settled();
    let (from, frames) = (lan.datagrams().len(), lan.sent.len());
    lan.deliver(&to_group(MAC_B, (link_local, 53_000), &ours(5, CLASS_IN)));
    let asked: Vec<&Seen> = lan.sent[frames..].iter().map(|(_, seen)| seen).collect();
    assert_eq!(asked, [&Seen::Arp { request: true, sender: A, target: link_local }], "the asker is asked for, and nothing is the router's");

    lan.deliver(&arp(BROADCAST, false, MAC_B, link_local, A));
    let on_the_link = Udp { to: MAC_B, source: A, source_port: MDNS, destination: link_local, port: 53_000, ttl: 255, payload: legacy_response(5) };
    assert_eq!(since(&lan, from), [on_the_link]);
}

// RFC 6762 §11: "All Multicast DNS responses (including responses sent via unicast) SHOULD be
// sent with IP TTL set to 255." The probes leave the socket the responses do.
#[test]
fn every_message_leaves_with_ttl_255() {
    let mut lan = settled();
    lan.deliver(&to_group(MAC_B, (B, 53_000), &ours(1, CLASS_IN)));
    lan.deliver(&to_group(MAC_B, (B, MDNS), &ours(0, CLASS_IN)));
    let ttls: Vec<u8> = lan.datagrams().iter().map(|(_, udp)| udp.ttl).collect();
    assert_eq!(ttls, [255; 7], "three probes, two announcements, a unicast answer and a multicast one");
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
    until(&mut lan, 5, Duration::from_secs(3));
    lan.deliver(&asked);
    assert_eq!(since(&lan, 5), [unicast(53_000, legacy_response(7))], "held, and its name claimed");

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
    let again = lan.node.answer_as(lan.now, Host::new("toyos").unwrap(), undrawn);
    assert_eq!(again, Err(toyos_net_udp::Error::Refused(toyos_net_udp::Counter::PortInUse)));

    let mut taken = Lan::new();
    taken.node.udp_bind(Ipv4Addr::UNSPECIFIED, mdns, || 0).unwrap();
    let refused = taken.node.answer_as(taken.now, Host::new("toyos").unwrap(), undrawn);
    assert_eq!(refused, Err(toyos_net_udp::Error::Refused(toyos_net_udp::Counter::PortInUse)));
    taken.lease(3_600);
    assert!(!taken.sent.iter().any(|(_, seen)| matches!(seen, Seen::Igmp { .. })), "no group was joined");
}
