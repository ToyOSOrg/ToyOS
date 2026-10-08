//! A client's datagram sockets on the node. No scenario ids: [udp]'s rules are its own crate's
//! scenarios; these are the node's calls over them, and the word each of [udp]'s refusals is
//! answered in, which the readers' specifications owe a scenario for and do not have. What is
//! not ours: `etherparse` reads every frame the node emits (`lan::outside`), and each test names
//! the RFC its expectation is read from.

mod common;
mod lan;

use std::net::Ipv4Addr;
use std::time::Duration;

use common::{terms, A, MAC, MAC_B, MAC_R};
use lan::{udp, Lan, Seen, Udp, B, OFF_LINK};
use toyos_net_node::{Datagram, Event, Refused};
use toyos_net_shard::Refusal;
use toyos_net_udp::Counter;
use toyos_net_wire::Port;

const ANY: Ipv4Addr = Ipv4Addr::UNSPECIFIED;

fn port(port: u16) -> Option<Port> {
    Some(Port::new(port).expect("not port 0"))
}

/// The draw of a bind that names its port.
fn undrawn() -> u32 {
    panic!("a named port draws nothing")
}

// RFC 6335 §6: the dynamic ports are 49152 to 65535. RFC 6056 §3.3.1, Algorithm 1: the first
// candidate is the draw's offset into the range, then the next port up.
#[test]
fn a_socket_holds_the_port_it_named_or_the_one_its_draw_picked() {
    let mut lan = Lan::new();
    lan.lease(3_600);
    let (named, held) = lan.node.udp_bind(ANY, port(4_000), undrawn).unwrap();
    assert_eq!(held.get(), 4_000);
    let (first, drawn) = lan.node.udp_bind(ANY, None, || 5).unwrap();
    assert_eq!(drawn.get(), 49_152 + 5);
    let (second, next) = lan.node.udp_bind(ANY, None, || 5).unwrap();
    assert_eq!(next.get(), 49_152 + 6, "the port after a taken one");
    let (_, wrapped) = lan.node.udp_bind(ANY, None, || 16_384 + 7).unwrap();
    assert_eq!(wrapped.get(), 49_152 + 7, "a draw past the range is an offset into it");

    for id in [named, first, second] {
        lan.node.udp_send_to(lan.now, id, B, 7, b"toyos").unwrap();
    }
    lan.pump();
    let mut ports: Vec<u16> = lan.datagrams().iter().map(|(_, udp)| udp.source_port).collect();
    ports.sort_unstable();
    assert_eq!(ports, [4_000, 49_157, 49_158], "each datagram is from its socket's port");
}

// RFC 768: source port, destination port, length, checksum, then the octets; RFC 1122 §3.3.1.1:
// a destination on the link goes to its own link address, any other to the router's.
#[test]
fn a_datagram_leaves_as_the_call_named_it() {
    let mut lan = Lan::new();
    lan.lease(3_600);
    let (id, _) = lan.node.udp_bind(ANY, port(4_000), undrawn).unwrap();
    lan.node.udp_send_to(lan.now, id, B, 7, b"to the link").unwrap();
    lan.node.udp_send_to(lan.now, id, OFF_LINK, 9, b"past it").unwrap();
    assert!(lan.datagrams().is_empty(), "accepted is queued, not sent");
    lan.pump();
    let sent: Vec<&Udp> = lan.datagrams().into_iter().map(|(_, udp)| udp).collect();
    let on_link = Udp { to: MAC_B, source: A, source_port: 4_000, destination: B, port: 7, ttl: 64, payload: b"to the link".to_vec() };
    let off_link = Udp { to: MAC_R, source: A, source_port: 4_000, destination: OFF_LINK, port: 9, ttl: 64, payload: b"past it".to_vec() };
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert!(sent.contains(&&on_link) && sent.contains(&&off_link), "{sent:?}");
}

// RFC 768: the source port is where a reply is addressed, and may be zero. RFC 1122 §4.1.3.1: a
// datagram for a port nobody holds is answered with a port unreachable.
#[test]
fn a_datagram_for_a_bound_port_is_handed_over_with_where_it_came_from() {
    let mut lan = Lan::new();
    lan.lease(3_600);
    let (id, _) = lan.node.udp_bind(ANY, port(4_000), undrawn).unwrap();
    let mut out = [0u8; 64];
    assert_eq!(lan.node.udp_recv_from(id, &mut out), Ok(None));

    lan.deliver(&udp(MAC, MAC_B, (B, 9_999), (A, 4_000), b"ping"));
    lan.deliver(&udp(MAC, MAC_R, (OFF_LINK, 0), (A, 4_000), b"unanswerable"));
    lan.deliver(&udp(MAC, MAC_B, (B, 9_999), (A, 4_001), b"nobody's"));

    assert_eq!(lan.node.udp_recv_from(id, &mut out), Ok(Some(Datagram { len: 4, source: B, source_port: port(9_999) })));
    assert_eq!(&out[..4], b"ping");
    assert_eq!(lan.node.udp_recv_from(id, &mut out), Ok(Some(Datagram { len: 12, source: OFF_LINK, source_port: None })));
    assert_eq!(&out[..12], b"unanswerable");
    assert_eq!(lan.node.udp_recv_from(id, &mut out), Ok(None), "port 4001's datagram is nobody's");
    let unreachable: Vec<&Seen> = lan.sent.iter().map(|(_, seen)| seen).filter(|seen| matches!(seen, Seen::PortUnreachable { .. })).collect();
    assert_eq!(unreachable, [&Seen::PortUnreachable { to: B }]);
    assert_eq!(lan.counted(Counter::RxNoSocket), 1);
}

/// One refused send: the word the client hears, and the rule [udp] counted it under.
fn refused_send(lan: &mut Lan, id: toyos_net_node::DatagramId, to: Ipv4Addr, port: u16, payload: &[u8], word: Refused, rule: Counter) {
    let before = lan.counted(rule);
    assert_eq!(lan.node.udp_send_to(lan.now, id, to, port, payload), Err(word), "{}", rule.name());
    assert_eq!(lan.counted(rule), before + 1, "{}", rule.name());
}

// The pipe ABI's words, `toyos::net`'s `ERR_*`: an address in use, not connected, invalid input,
// resource exhausted. Each refusal is reached here by the call a client makes.
#[test]
fn each_refusal_is_answered_in_the_pipes_word_for_it() {
    let mut lan = Lan::new();
    let (any, _) = lan.node.udp_bind(ANY, port(4_000), undrawn).unwrap();
    refused_send(&mut lan, any, B, 7, b"early", Refused::NotConnected, Counter::NoSourceAddress);
    assert_eq!(lan.node.udp_bind(A, port(4_001), undrawn), Err(Refused::InvalidInput), "an address not yet held");
    assert_eq!(lan.counted(Counter::BindAddressNotLocal), 1);

    lan.lease(600);
    assert_eq!(lan.node.udp_bind(ANY, port(4_000), undrawn), Err(Refused::AddrInUse));
    assert_eq!(lan.node.udp_bind(ANY, port(68), undrawn), Err(Refused::AddrInUse), "the DHCP client's port");
    assert_eq!(lan.counted(Counter::PortInUse), 2);
    let (held, _) = lan.node.udp_bind(A, port(4_001), undrawn).unwrap();

    let invalid: [(Ipv4Addr, u16, &[u8], Counter); 8] = [
        (B, 0, b"x", Counter::SendPortZero),
        (Ipv4Addr::UNSPECIFIED, 7, b"x", Counter::SendUnspecifiedDestination),
        (Ipv4Addr::LOCALHOST, 7, b"x", Counter::SendLoopback),
        (A, 7, b"x", Counter::SendToSelf),
        (Ipv4Addr::new(240, 0, 0, 1), 7, b"x", Counter::SendInvalidDestination),
        (Ipv4Addr::BROADCAST, 7, b"x", Counter::BroadcastNotPermitted),
        (Ipv4Addr::new(192, 0, 2, 255), 7, b"x", Counter::BroadcastNotPermitted),
        (B, 7, &[0; 1_473], Counter::ExceedsMtu),
    ];
    for (to, port, payload, rule) in invalid {
        refused_send(&mut lan, any, to, port, payload, Refused::InvalidInput, rule);
    }
    lan.node.udp_send_to(lan.now, any, B, 7, &[0; 1_472]).expect("1,472 octets fill one frame");
    lan.pump();

    for _ in 0..toyos_net_udp::limits::TX_DATAGRAMS {
        lan.node.udp_send_to(lan.now, any, B, 7, b"queued").unwrap();
    }
    refused_send(&mut lan, any, B, 7, b"one more", Refused::ResourceExhausted, Counter::TxQueueFull);
    lan.pump();
    lan.node.udp_send_to(lan.now, any, B, 7, b"later").expect("the same call, once the queue has left");

    // What [udp] logs waits for the node's next pass over the stack's reports.
    lan.fire(lan.now);
    let logged = lan.node.drain_events().any(|event| matches!(event, Event::Stack { refusal: Refusal::Udp(refusal), .. } if refusal.rule == Counter::BroadcastNotPermitted));
    assert!(logged, "a refused broadcast is a line for the log");

    assert!(lan.run_until(Duration::from_secs(700), |lan| lan.node.lease().is_none()), "the lease runs out");
    refused_send(&mut lan, any, B, 7, b"late", Refused::NotConnected, Counter::NoSourceAddress);
    refused_send(&mut lan, held, B, 7, b"late", Refused::NotConnected, Counter::SourceAddressNotAssigned);

    let mut routerless = Lan::new();
    routerless.lease_on(&terms(3_600, None));
    let (id, _) = routerless.node.udp_bind(ANY, port(4_000), undrawn).unwrap();
    refused_send(&mut routerless, id, OFF_LINK, 7, b"x", Refused::NotConnected, Counter::NoRoute);
}

// A closed socket's port is free at once, and what it had accepted still leaves: in the first
// opportunity after the close, not one later.
#[test]
fn a_closed_socket_frees_its_port_and_what_it_accepted_still_leaves() {
    let mut lan = Lan::new();
    lan.lease(3_600);
    let (id, _) = lan.node.udp_bind(ANY, port(4_000), undrawn).unwrap();
    lan.node.udp_send_to(lan.now, id, B, 7, b"first").unwrap();
    lan.pump();
    assert_eq!(lan.datagrams().len(), 1, "B's link address is known from here");

    lan.node.udp_send_to(lan.now, id, B, 7, b"second").unwrap();
    lan.node.udp_send_to(lan.now, id, B, 7, b"third").unwrap();
    assert_eq!(lan.node.udp_close(lan.now, id), Ok(()));
    lan.opportunity();
    let payloads: Vec<&[u8]> = lan.datagrams().iter().map(|(_, udp)| udp.payload.as_slice()).collect();
    assert_eq!(payloads, [&b"first"[..], &b"second"[..], &b"third"[..]]);
    assert!(lan.datagrams().iter().all(|(_, udp)| udp.source_port == 4_000));

    let mut out = [0u8; 8];
    assert_eq!(lan.node.udp_send_to(lan.now, id, B, 7, b"after"), Err(Refused::NotConnected));
    assert_eq!(lan.node.udp_recv_from(id, &mut out), Err(Refused::NotConnected));
    assert_eq!(lan.node.udp_close(lan.now, id), Err(Refused::NotConnected));
    let (again, held) = lan.node.udp_bind(ANY, port(4_000), undrawn).expect("the port is free");
    assert_eq!(held.get(), 4_000);
    assert_ne!(again, id, "a closed socket's id names no later socket");
    lan.deliver(&udp(MAC, MAC_B, (B, 9_999), (A, 4_000), b"ping"));
    assert_eq!(lan.node.udp_recv_from(id, &mut out), Err(Refused::NotConnected));
    assert_eq!(lan.node.udp_recv_from(again, &mut out).map(|got| got.map(|datagram| datagram.len)), Ok(Some(4)));
}
