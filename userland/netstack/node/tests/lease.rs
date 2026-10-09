//! The node and its lease, against a server the test scripts from RFC 2131's layouts. No scenario
//! ids: the specifications' scenarios are the client's and the stack's, each tested in its own
//! crate; these are what the node does between them. The scripted server is consistency, ours
//! against ours; what is not ours is the reader of every frame the node emits (`common::outside`)
//! and the order RFC 5227 §2.1.1 and §2.3 give conflict detection.

mod common;

use std::cell::Cell;
use std::net::Ipv4Addr;
use std::rc::Rc;
use std::time::Duration;

use common::*;
use etherparse::{PacketBuilder, TcpOptionElement};
use toyos_net_ip::AddrState;
use toyos_net_node::{Counter, Event, ListenRefused, Wake, WriteRefusal};
use toyos_net_shard::Refusal;
use toyos_net_wire::{Instant, Port};

const START: Instant = Instant::from_millis(3_600_000);
const HOUR: u32 = 3_600;

fn after(seconds: u64) -> Instant {
    START.after(Duration::from_secs(seconds))
}

/// The first four bytes of a DHCP message's `ciaddr` (RFC 2131 §2).
fn ciaddr(message: &[u8]) -> Ipv4Addr {
    Ipv4Addr::from(<[u8; 4]>::try_from(&message[12..16]).unwrap())
}

/// Runs to the node's next DHCP message, which the lease's first renewal is.
fn renewing(wire: &mut Wire) -> u32 {
    let sent = wire.dhcp().len();
    assert!(wire.run_until(Duration::from_secs(400), |wire| wire.dhcp().len() > sent), "the renewal leaves");
    let request = wire.last(REQUEST);
    assert_eq!(ciaddr(request), A, "a renewal names the held address");
    xid(request)
}

#[test]
fn the_lease_end_to_end() {
    let terms = terms(HOUR, Some(R));
    let mut wire = Wire::new();
    wire.link(true);
    let [Seen::Dhcp { to, source, destination, message }] = &wire.sent[..] else { panic!("one DISCOVER, not {:?}", wire.sent) };
    assert_eq!((*to, *source, *destination), (BROADCAST, Ipv4Addr::UNSPECIFIED, Ipv4Addr::BROADCAST));
    assert_eq!(option(message, 53), Some(&[DISCOVER][..]));
    assert_eq!(option(message, 12), Some(&b"toyos"[..]));
    let id = xid(message);

    wire.deliver(&from_server(MAC, A, &message_of(OFFER, id, &terms)));
    let request = wire.last(REQUEST);
    assert_eq!((xid(request), option(request, 50), option(request, 54)), (id, Some(&A.octets()[..]), Some(&R.octets()[..])));

    let acknowledged = wire.sent.len();
    wire.deliver(&from_server(BROADCAST, Ipv4Addr::BROADCAST, &message_of(ACK, id, &terms)));
    assert_eq!((wire.node.lease(), wire.address(A)), (None, Some(AddrState::Tentative)));

    assert!(wire.run_until(Duration::from_secs(10), |wire| wire.node.lease().is_some()), "the lease is held");
    let detection = &wire.sent[acknowledged..];
    assert_eq!(detection.len(), 4, "three probes and an announcement: {detection:?}");
    assert!(detection[..3].iter().all(|frame| frame.is_probe(A)), "{detection:?}");
    assert!(detection[3].is_announcement(A), "{detection:?}");

    let lease = wire.node.lease().unwrap();
    assert_eq!((lease.address, lease.prefix_len, lease.router, lease.dns.as_slice(), lease.server), (A, 24, Some(R), &[DNS][..], R));
    assert_eq!(wire.address(A), Some(AddrState::Announcing));
    assert_eq!(wire.gateways(), [R]);

    assert!(wire.answers_arp_for(A));
    assert!(!wire.answers_arp_for(ELSEWHERE));
    let from = wire.sent.len();
    wire.deliver(&echo_request(A, 7, 1, b"toyos"));
    wire.deliver(&echo_request(ELSEWHERE, 7, 2, b"toyos"));
    let replies: Vec<&Seen> = wire.sent[from..].iter().filter(|frame| matches!(frame, Seen::EchoReply { .. })).collect();
    assert_eq!(replies, [&Seen::EchoReply { source: A, destination: R, id: 7, seq: 1, data: b"toyos".to_vec() }]);
}

#[test]
fn no_frame_is_from_the_address_before_it_is_verified() {
    let mut wire = Wire::acknowledged(&terms(HOUR, Some(R)));
    assert_eq!(wire.node.lease(), None, "acknowledged is not held");
    for _ in 0..16 {
        if wire.node.lease().is_some() {
            break;
        }
        assert_eq!(wire.address(A), Some(AddrState::Tentative));
        assert!(wire.gateways().is_empty(), "no route before its address");
        assert!(!wire.answers_arp_for(A), "a tentative address answers nothing");
        let at = wire.node.next_deadline().expect("conflict detection is running");
        wire.fire(at);
    }
    assert!(wire.node.lease().is_some(), "held within sixteen deadlines");
    let probes = wire.sent.iter().filter(|frame| frame.is_probe(A)).count();
    assert_eq!(probes, 3, "held only once the third probe has left");
    assert!(wire.sent.iter().all(|frame| frame.source() != Some(A)), "{:?}", wire.sent);
}

#[test]
fn expiry_takes_address_route_and_resolvers_in_one_step() {
    let mut wire = Wire::leased(&terms(600, Some(R)));
    assert_eq!((wire.address(A).is_some(), wire.gateways(), wire.node.lease().map(|lease| lease.dns.clone())), (true, vec![R], Some(vec![DNS])));
    let held = wire.sent.len();

    assert!(wire.run_until(Duration::from_secs(700), |wire| wire.node.lease().is_none()), "the lease runs out");
    assert_eq!(wire.now, after(600), "at its expiry");
    assert_eq!((wire.address(A), wire.gateways()), (None, vec![]));
    let renewal = wire.sent[held..].iter().find(|frame| frame.dhcp().is_some()).expect("a renewal");
    assert!(matches!(renewal, Seen::Dhcp { to, source, destination, .. } if (*to, *source, *destination) == (MAC_R, A, R)), "{renewal:?}");
    let Some(Seen::Dhcp { to, source, destination, message }) = wire.sent.last() else { panic!("{:?}", wire.sent.last()) };
    assert_eq!((*to, *source, *destination, option(message, 53)), (BROADCAST, Ipv4Addr::UNSPECIFIED, Ipv4Addr::BROADCAST, Some(&[DISCOVER][..])));
    assert!(!wire.answers_arp_for(A));
}

#[test]
fn a_nak_takes_address_route_and_resolvers_in_one_step() {
    let mut wire = Wire::leased(&terms(600, Some(R)));
    let id = renewing(&mut wire);
    wire.deliver(&from_server(BROADCAST, Ipv4Addr::BROADCAST, &message(NAK, id, Ipv4Addr::UNSPECIFIED, &[(54, R.octets().to_vec())])));
    assert_eq!((wire.node.lease(), wire.address(A), wire.gateways()), (None, None, vec![]));
    assert!(matches!(wire.sent.last(), Some(Seen::Dhcp { source, .. }) if source.is_unspecified()), "{:?}", wire.sent.last());
    assert_ne!(xid(wire.last(DISCOVER)), id, "a new exchange");
    let refused = Event::Dhcp(toyos_dhcp::Refusal { rule: toyos_dhcp::Counter::Nak, peer: toyos_dhcp::Peer::From(R) });
    assert!(wire.node.drain_events().any(|event| event == refused));
    assert!(!wire.answers_arp_for(A));
}

#[test]
fn a_renewal_without_a_router_takes_the_route_and_keeps_the_address() {
    let mut wire = Wire::leased(&terms(600, Some(R)));
    let id = renewing(&mut wire);
    let renewed = wire.now;
    wire.deliver(&from_server(MAC, A, &message_of(ACK, id, &terms(600, None))));
    let lease = wire.node.lease().expect("still held");
    assert_eq!((lease.address, lease.router, lease.timers.map(|timers| timers.expiry)), (A, None, Some(renewed.after(Duration::from_secs(600)))));
    assert_eq!((wire.address(A), wire.gateways()), (Some(AddrState::Assigned), vec![]));
    assert_eq!(wire.node.counters().get(Counter::RouterRefused), 0);
}

#[test]
fn a_renewed_prefix_reaches_ip_before_its_router() {
    let mut wire = Wire::leased(&terms(600, Some(R)));
    let id = renewing(&mut wire);
    // A router only the wider prefix holds.
    let (wider, beyond) = (Ipv4Addr::new(255, 255, 254, 0), Ipv4Addr::new(192, 0, 3, 254));
    let terms = [(54, R.octets().to_vec()), (51, 600u32.to_be_bytes().to_vec()), (1, wider.octets().to_vec()), (3, beyond.octets().to_vec()), (6, DNS.octets().to_vec())];
    wire.deliver(&from_server(MAC, A, &message_of(ACK, id, &terms)));
    let lease = wire.node.lease().expect("still held");
    assert_eq!((lease.prefix_len, lease.router), (23, Some(beyond)));
    let shard = wire.node.shard();
    assert_eq!(shard.ip().prefix_len(shard.iface(), A), Some(23));
    assert_eq!(wire.gateways(), [beyond]);
    assert_eq!(wire.node.counters().get(Counter::RouterRefused), 0);
}

#[test]
fn a_lease_that_runs_out_under_the_probe_leaves_no_address() {
    let mut wire = Wire::acknowledged(&terms(1, Some(R)));
    // The clock is late: detection ends past the lease's one second.
    wire.fire(after(2));
    assert!(wire.run_until(Duration::from_secs(10), |wire| wire.dhcp().len() == 3), "the client starts over");
    wire.last(DISCOVER);
    assert_eq!(wire.sent.iter().filter(|frame| frame.is_probe(A)).count(), 3, "detection ran to its end");
    assert_eq!((wire.node.lease(), wire.address(A), wire.gateways()), (None, None, vec![]));
    assert!(!wire.answers_arp_for(A));
}

#[test]
fn a_second_conflict_takes_a_held_lease_and_declines_it() {
    let mut wire = Wire::leased(&terms(HOUR, Some(R)));
    wire.fire(after(100));
    let conflict = arp(BROADCAST, false, MAC_B, A, A);

    let from = wire.sent.len();
    wire.deliver(&conflict);
    assert_eq!(wire.sent[from..].iter().filter(|frame| frame.is_announcement(A)).count(), 1, "defended once: {:?}", &wire.sent[from..]);
    assert_eq!((wire.node.lease().map(|lease| lease.address), wire.gateways()), (Some(A), vec![R]));

    // Inside [ip]'s defend interval (RFC 5227 §2.4 (b)).
    wire.fire(after(105));
    wire.deliver(&conflict);
    assert_eq!((wire.node.lease(), wire.address(A), wire.gateways()), (None, None, vec![]));
    let decline = wire.last(DECLINE);
    assert_eq!((option(decline, 50), option(decline, 54)), (Some(&A.octets()[..]), Some(&R.octets()[..])));
    assert!(!wire.answers_arp_for(A));
}

/// What a listener's owner saw of its wake pipe: the wakes written, and whether the node dropped
/// its end, at which the owner reads the end.
#[derive(Default)]
struct Told {
    wakes: Cell<usize>,
    dropped: Cell<bool>,
}

struct Owner(Rc<Told>);

impl Wake for Owner {
    fn wake(&mut self) -> Result<(), WriteRefusal> {
        self.0.wakes.set(self.0.wakes.get() + 1);
        Ok(())
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        self.0.dropped.set(true);
    }
}

fn listen(wire: &mut Wire, addr: Ipv4Addr, port: u16) -> Result<Rc<Told>, ListenRefused> {
    let told = Rc::new(Told::default());
    wire.node.listen(addr, Port::new(port), Box::new(Owner(told.clone())), || panic!("a named port draws nothing")).map(|_| told)
}

/// Where the peer of the listeners is, at `MAC_B`.
const PEER: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 2);
/// The peer's initial sequence number.
const ISS: u32 = 5000;

/// A segment from the peer's port 40001 to the node's port `to`, built by `etherparse`: a SYN
/// with an MSS option, or the ACK of `ack`.
fn from_peer(to: u16, ack: Option<u32>) -> Vec<u8> {
    let step = PacketBuilder::ethernet2(MAC_B, MAC).ipv4(PEER.octets(), A.octets(), 64);
    let step = match ack {
        None => step.tcp(40_001, to, ISS, 65_535).syn().options(&[TcpOptionElement::MaximumSegmentSize(1460)]).unwrap(),
        Some(ack) => step.tcp(40_001, to, ISS + 1, 65_535).ack(ack),
    };
    let mut frame = Vec::new();
    step.write(&mut frame, &[]).unwrap();
    frame
}

/// The peer's SYN to the node's port `to`, the node's ARP request for the peer answered if it
/// makes one. Returns what the node sent for it.
fn syn(wire: &mut Wire, to: u16) -> Vec<Seen> {
    let from = wire.sent.len();
    wire.deliver(&from_peer(to, None));
    if wire.sent[from..].iter().any(|seen| matches!(seen, Seen::Arp { request: true, target, .. } if *target == PEER)) {
        wire.deliver(&arp(MAC, false, MAC_B, PEER, A));
    }
    wire.sent[from..].to_vec()
}

type Loss = fn(&mut Wire);

/// The three ways a held address goes: the server refuses its renewal, the lease runs out, and
/// another host defends the address twice inside [ip]'s defend interval (RFC 5227 §2.4 (b)).
fn losses() -> [(&'static str, Loss); 3] {
    fn nak(wire: &mut Wire) {
        let id = renewing(wire);
        wire.deliver(&from_server(BROADCAST, Ipv4Addr::BROADCAST, &message(NAK, id, Ipv4Addr::UNSPECIFIED, &[(54, R.octets().to_vec())])));
    }
    fn expiry(wire: &mut Wire) {
        assert!(wire.run_until(Duration::from_secs(700), |wire| wire.node.lease().is_none()), "the lease runs out");
    }
    fn conflict(wire: &mut Wire) {
        let conflict = arp(BROADCAST, false, MAC_B, A, A);
        wire.fire(wire.now.after(Duration::from_secs(100)));
        wire.deliver(&conflict);
        wire.fire(wire.now.after(Duration::from_secs(5)));
        wire.deliver(&conflict);
    }
    [("a NAK", nak), ("expiry", expiry), ("a conflict", conflict)]
}

// What takes the machine's address is the wire's doing, not a program's: a NAK, a server that
// is away, two ARP frames. So a listener that named the address stands through its loss as a
// datagram socket bound to it does, with its port and its place and its owner told nothing:
// [ip] takes no segment for an address it does not hold, and [tcp] hands the listener the next
// SYN once the address is the machine's again. A renewal and a link that went and came back
// are no loss at all.
#[test]
fn a_listener_stands_while_its_address_is_lost_and_answers_when_it_is_back() {
    for (how, lose) in losses() {
        let mut wire = Wire::leased(&terms(600, Some(R)));
        let named = listen(&mut wire, A, 22).expect("the held address");
        let any = listen(&mut wire, Ipv4Addr::UNSPECIFIED, 23).expect("every address");
        let stand = |wire: &mut Wire, when: &str| {
            assert_eq!((wire.node.listeners(), wire.node.held(), wire.node.drain_ended_listeners().count()), (2, 2, 0), "{how}, {when}");
            assert_eq!((named.dropped.get(), any.dropped.get(), named.wakes.get(), any.wakes.get()), (false, false, 0, 0), "{how}, {when}");
        };

        let id = renewing(&mut wire);
        wire.deliver(&from_server(MAC, A, &message_of(ACK, id, &terms(600, Some(R)))));
        wire.link(false);
        stand(&mut wire, "the link down");
        wire.link(true);
        let id = xid(wire.last(REQUEST));
        wire.deliver(&from_server(BROADCAST, Ipv4Addr::BROADCAST, &message_of(ACK, id, &terms(600, Some(R)))));
        assert_eq!(wire.node.lease().map(|lease| lease.address), Some(A), "{how}");
        stand(&mut wire, "renewed, and the link back");

        lose(&mut wire);
        assert_eq!((wire.node.lease(), wire.address(A)), (None, None), "{how}");
        stand(&mut wire, "the address lost");
        assert_eq!(syn(&mut wire, 22), [], "{how}: a SYN to an address the machine does not hold");
        stand(&mut wire, "a SYN to the lost address");
        assert_eq!(listen(&mut wire, A, 24).err(), Some(ListenRefused::NotLocal), "{how}");

        assert!(wire.run_until(Duration::from_secs(60), |wire| wire.dhcp().last().is_some_and(|(kind, _)| *kind == DISCOVER)), "{how}: discovery starts over");
        let id = xid(wire.last(DISCOVER));
        wire.deliver(&from_server(MAC, A, &message_of(OFFER, id, &terms(600, Some(R)))));
        wire.deliver(&from_server(BROADCAST, Ipv4Addr::BROADCAST, &message_of(ACK, id, &terms(600, Some(R)))));
        assert!(wire.run_until(Duration::from_secs(10), |wire| wire.node.lease().is_some()), "{how}: the address is held again");
        stand(&mut wire, "the address back");

        let answer = syn(&mut wire, 22);
        let Some(Seen::Tcp(synack)) = answer.last() else { panic!("{how}: {answer:?}") };
        assert!(synack.syn && (synack.from_port, synack.ack) == (22, Some(ISS + 1)), "{how}: {synack:?}");
        wire.deliver(&from_peer(22, Some(synack.seq.wrapping_add(1))));
        assert_eq!((named.wakes.get(), any.wakes.get(), wire.node.listeners(), wire.node.held()), (1, 0, 2, 2), "{how}");
    }
}

#[test]
fn a_router_ip_refuses_is_no_router() {
    let edge = Ipv4Addr::new(192, 0, 2, 255);
    let wire = Wire::leased(&terms(HOUR, Some(edge)));
    assert_eq!(wire.node.lease().map(|lease| lease.router), Some(None));
    assert!(wire.gateways().is_empty());
    assert_eq!(wire.node.counters().get(Counter::RouterRefused), 1);
}

#[test]
fn a_conflict_under_the_probe_declines_and_holds_nothing() {
    let mut wire = Wire::acknowledged(&terms(HOUR, Some(R)));
    let at = wire.node.next_deadline().expect("the first probe is due");
    wire.fire(at);
    assert!(wire.sent.last().is_some_and(|frame| frame.is_probe(A)), "{:?}", wire.sent.last());

    let conflict = wire.now;
    wire.deliver(&arp(BROADCAST, false, MAC_B, A, A));
    let Some(Seen::Dhcp { to, source, destination, message }) = wire.sent.last() else { panic!("{:?}", wire.sent.last()) };
    assert_eq!((*to, *source, *destination), (BROADCAST, Ipv4Addr::UNSPECIFIED, Ipv4Addr::BROADCAST));
    assert_eq!((option(message, 53), option(message, 50), option(message, 54)), (Some(&[DECLINE][..]), Some(&A.octets()[..]), Some(&R.octets()[..])));
    assert_eq!((wire.node.lease(), wire.address(A)), (None, None));
    let events: Vec<Event> = wire.node.drain_events().collect();
    let stack = |event: &Event| matches!(event, Event::Stack { refusal: Refusal::Ip(refusal), .. } if refusal.rule == toyos_net_ip::Counter::AcdConflict);
    let dhcp = |event: &Event| matches!(event, Event::Dhcp(refusal) if refusal.rule == toyos_dhcp::Counter::Declined);
    assert!(events.iter().any(stack) && events.iter().any(dhcp), "{events:?}");

    assert!(wire.run_until(Duration::from_secs(11), |wire| wire.dhcp().last().is_some_and(|(kind, _)| *kind == DISCOVER)), "discovery starts over");
    assert_eq!(wire.now, conflict.after(toyos_dhcp::limits::DECLINE_BACKOFF));
    assert_eq!(wire.node.lease(), None);
}

#[test]
fn a_link_that_returns_keeps_a_held_lease_and_announces_it() {
    let mut wire = Wire::leased(&terms(HOUR, Some(R)));
    wire.fire(after(100));
    wire.link(false);
    assert_eq!((wire.node.lease().map(|lease| lease.address), wire.address(A).is_some(), wire.gateways()), (Some(A), true, vec![R]));

    let down = wire.sent.len();
    wire.link(true);
    assert_eq!(wire.node.lease().map(|lease| lease.address), Some(A));
    assert!(wire.sent[down..].iter().any(|frame| frame.is_announcement(A)), "{:?}", &wire.sent[down..]);
    let request = wire.last(REQUEST);
    assert_eq!((ciaddr(request), option(request, 50), option(request, 54)), (Ipv4Addr::UNSPECIFIED, Some(&A.octets()[..]), None));

    let id = xid(request);
    wire.deliver(&from_server(BROADCAST, Ipv4Addr::BROADCAST, &message_of(ACK, id, &terms(HOUR, Some(R)))));
    let expiry = wire.node.lease().and_then(|lease| lease.timers).map(|timers| timers.expiry);
    assert_eq!(expiry, Some(after(100 + u64::from(HOUR))), "the lease runs from the request the server acknowledged");
    assert_eq!(wire.gateways(), [R]);
}

#[test]
fn a_link_that_returns_with_no_lease_starts_discovery_over_at_once() {
    let mut wire = Wire::new();
    wire.link(true);
    let first = xid(wire.last(DISCOVER));
    wire.link(false);
    wire.link(true);
    assert_eq!(wire.dhcp().len(), 2);
    assert_ne!(xid(wire.last(DISCOVER)), first, "a new exchange");
    assert_eq!(wire.now, START);
}

#[test]
fn a_probe_the_link_drops_under_is_abandoned() {
    let mut wire = Wire::acknowledged(&terms(HOUR, Some(R)));
    let sent = wire.sent.len();
    wire.link(false);
    assert_eq!((wire.node.lease(), wire.address(A)), (None, None));
    assert_eq!((wire.sent.len(), wire.node.counters().get(Counter::DhcpUnsent)), (sent, 1), "its DISCOVER has no link to leave by");
    wire.link(true);
    wire.last(DISCOVER);
    assert_eq!(wire.address(A), None);
}

#[test]
fn a_message_with_no_link_is_counted_and_not_sent() {
    let mut wire = Wire::new();
    let at = wire.node.next_deadline().expect("the client's first wait");
    wire.fire(at);
    assert_eq!((wire.sent.len(), wire.node.counters().get(Counter::DhcpUnsent)), (0, 1));
}

#[test]
fn next_deadline_is_the_earliest_of_the_clients_and_the_stacks() {
    // Selecting: only the client waits.
    let mut wire = Wire::new();
    wire.link(true);
    let (client, stack) = (wire.node.dhcp().next_deadline(), wire.node.shard().next_deadline());
    assert!(client.is_some() && stack.is_none(), "{client:?} {stack:?}");
    assert_eq!(wire.node.next_deadline(), client);

    // Probing: only the stack does.
    let wire = Wire::acknowledged(&terms(600, Some(R)));
    let (client, stack) = (wire.node.dhcp().next_deadline(), wire.node.shard().next_deadline());
    assert!(client.is_none() && stack.is_some(), "{client:?} {stack:?}");
    assert_eq!(wire.node.next_deadline(), stack);

    // Held: both, the stack's second announcement before the client's renewal.
    let mut wire = Wire::leased(&terms(600, Some(R)));
    let (client, stack) = (wire.node.dhcp().next_deadline(), wire.node.shard().next_deadline());
    assert_eq!(client, Some(after(300)));
    assert!(stack.is_some_and(|stack| stack < after(300)), "{stack:?}");
    assert_eq!(wire.node.next_deadline(), stack);

    // One nanosecond early, nothing is due.
    let at = stack.unwrap();
    let sent = wire.sent.len();
    wire.fire(Instant::from_nanos(at.nanos() - 1));
    assert_eq!((wire.sent.len(), wire.node.next_deadline()), (sent, Some(at)));
    wire.fire(at);
    assert!(wire.sent[sent..].iter().any(|frame| frame.is_announcement(A)), "{:?}", &wire.sent[sent..]);
}

#[test]
fn a_reply_without_the_cookie_is_refused_by_name() {
    let mut wire = Wire::new();
    wire.link(true);
    let id = xid(wire.last(DISCOVER));
    let mut bootp = message_of(OFFER, id, &terms(HOUR, Some(R)));
    bootp[236..240].fill(0);
    wire.deliver(&from_server(MAC, A, &bootp));
    wire.last(DISCOVER);
    let refused = Event::Dhcp(toyos_dhcp::Refusal { rule: toyos_dhcp::Counter::BootpReply, peer: toyos_dhcp::Peer::From(R) });
    assert!(wire.node.drain_events().any(|event| event == refused));
    assert_eq!(wire.node.drain_events().count(), 0, "drained");
}

/// The node waiting for the ACK of its REQUEST, and that ACK's frame.
fn requesting() -> (Wire, Vec<u8>) {
    let terms = terms(HOUR, Some(R));
    let mut wire = Wire::new();
    wire.link(true);
    let id = xid(wire.last(DISCOVER));
    wire.deliver(&from_server(MAC, A, &message_of(OFFER, id, &terms)));
    wire.last(REQUEST);
    (wire, from_server(BROADCAST, Ipv4Addr::BROADCAST, &message_of(ACK, id, &terms)))
}

#[test]
fn no_cut_of_the_ack_is_an_ack() {
    let (mut wire, ack) = requesting();
    for len in 0..ack.len() {
        wire.deliver(&ack[..len]);
        assert_eq!(wire.address(A), None, "cut to {len} bytes");
    }
    wire.deliver(&ack);
    assert_eq!(wire.address(A), Some(AddrState::Tentative), "whole, it is one");
}

#[test]
fn no_flipped_bit_of_the_ack_yields_another_lease() {
    let (_, ack) = requesting();
    for index in 0..ack.len() {
        for bit in 0..8 {
            let (mut wire, mut ack) = requesting();
            ack[index] ^= 1 << bit;
            wire.deliver(&ack);
            // A flip the frame's checksums do not cover is in its link addresses, which name no
            // lease: taken or refused, the lease is the server's or none.
            let taken = wire.address(A).is_some();
            assert_eq!(wire.run_until(Duration::from_secs(10), |wire| wire.node.lease().is_some()), taken, "byte {index} bit {bit}");
            let offered = |lease: &toyos_dhcp::Lease| (lease.address, lease.prefix_len, lease.router, lease.dns.clone()) == (A, 24, Some(R), vec![DNS]);
            assert!(wire.node.lease().is_none_or(offered), "byte {index} bit {bit}: {:?}", wire.node.lease());
        }
    }
}
