//! A client's datagram sockets on the node. [udp]'s rules are its own crate's scenarios; these
//! are the node's calls over them, and the word each of [udp]'s refusals is answered in, which
//! the readers' specifications owe a scenario for and do not have. The one id here is US-21's,
//! the broadcast permission, which the node hands a client and a datagram keeps. What is not ours: `etherparse` reads
//! every frame the node emits (`lan::outside`), and each test names the RFC its expectation is
//! read from.

mod common;
mod lan;

use std::net::Ipv4Addr;
use std::time::Duration;

use common::{from_server, message_of, terms, xid, A, ACK, BROADCAST, DNS, MAC, MAC_B, MAC_R, R};
use lan::{udp, Lan, Seen, Udp, B, OFF_LINK};
use toyos_net_node::{Datagram, Event, Refused};
use toyos_net_shard::Refusal;
use toyos_net_udp::Counter;
use toyos_net_wire::Port;

const ANY: Ipv4Addr = Ipv4Addr::UNSPECIFIED;
/// The directed broadcast address of the leased 192.0.2.0/24 (RFC 922 §7).
const SUBNET_BROADCAST: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 255);

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
// resource exhausted, permission denied. Each refusal is reached here by the call a client makes.
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

    let invalid: [(Ipv4Addr, u16, &[u8], Counter); 6] = [
        (B, 0, b"x", Counter::SendPortZero),
        (Ipv4Addr::UNSPECIFIED, 7, b"x", Counter::SendUnspecifiedDestination),
        (Ipv4Addr::LOCALHOST, 7, b"x", Counter::SendLoopback),
        (A, 7, b"x", Counter::SendToSelf),
        (Ipv4Addr::new(240, 0, 0, 1), 7, b"x", Counter::SendInvalidDestination),
        (B, 7, &[0; 1_473], Counter::ExceedsMtu),
    ];
    for (to, port, payload, rule) in invalid {
        refused_send(&mut lan, any, to, port, payload, Refused::InvalidInput, rule);
    }
    for to in [Ipv4Addr::BROADCAST, SUBNET_BROADCAST] {
        refused_send(&mut lan, any, to, 7, b"x", Refused::PermissionDenied, Counter::BroadcastNotPermitted);
    }
    lan.node.udp_send_to(lan.now, any, B, 7, &[0; 1_472]).expect("1,472 octets fill one frame");
    lan.pump();

    for _ in 0..toyos_net_udp::limits::TX_DATAGRAMS {
        lan.node.udp_send_to(lan.now, any, B, 7, b"queued").unwrap();
    }
    refused_send(&mut lan, any, B, 7, b"one more", Refused::ResourceExhausted, Counter::TxQueueFull);
    lan.pump();
    lan.node.udp_send_to(lan.now, any, B, 7, b"later").expect("the same call, once the queue has left");

    assert!(lan.run_until(Duration::from_secs(700), |lan| lan.node.lease().is_none()), "the lease runs out");
    refused_send(&mut lan, any, B, 7, b"late", Refused::NotConnected, Counter::NoSourceAddress);
    refused_send(&mut lan, held, B, 7, b"late", Refused::NotConnected, Counter::SourceAddressNotAssigned);

    let mut routerless = Lan::new();
    routerless.lease_on(&terms(3_600, None));
    let (id, _) = routerless.node.udp_bind(ANY, port(4_000), undrawn).unwrap();
    refused_send(&mut routerless, id, OFF_LINK, 7, b"x", Refused::NotConnected, Counter::NoRoute);
}

// US-21 (decision U-4), POSIX's `SO_BROADCAST`: a send to the limited broadcast address or to the
// held prefix's directed one is refused without the socket's permission, in the word that says
// permission, and with it leaves in a frame to the link's broadcast address with the socket's
// TTL, no link address asked for (RFC 919 §7, RFC 922 §7: an IP broadcast is a link broadcast;
// RFC 1122 §3.3.6). The permission is the one socket's, and is taken back as it was given.
#[test]
fn a_broadcast_needs_its_permission() {
    let mut lan = Lan::new();
    lan.lease(3_600);
    let (id, _) = lan.node.udp_bind(ANY, port(5_000), undrawn).unwrap();
    let (other, _) = lan.node.udp_bind(ANY, port(5_002), undrawn).unwrap();
    let frames = lan.sent.len();
    for to in [Ipv4Addr::BROADCAST, SUBNET_BROADCAST] {
        refused_send(&mut lan, id, to, 5_001, b"hi", Refused::PermissionDenied, Counter::BroadcastNotPermitted);
    }
    lan.pump();
    assert_eq!(lan.sent.len(), frames, "nothing left for either");

    assert_eq!(lan.node.udp_set_broadcast(id, true), Ok(()));
    for to in [Ipv4Addr::BROADCAST, SUBNET_BROADCAST] {
        assert_eq!(lan.node.udp_send_to(lan.now, id, to, 5_001, b"hi"), Ok(()));
    }
    lan.pump();
    let left: Vec<Seen> = lan.sent[frames..].iter().map(|(_, seen)| seen.clone()).collect();
    let broadcast = |destination| Seen::Udp(Udp { to: BROADCAST, source: A, source_port: 5_000, destination, port: 5_001, ttl: 64, payload: b"hi".to_vec() });
    assert_eq!(left, [broadcast(Ipv4Addr::BROADCAST), broadcast(SUBNET_BROADCAST)], "each in a link broadcast, and no ARP request before it");

    refused_send(&mut lan, other, Ipv4Addr::BROADCAST, 5_001, b"hi", Refused::PermissionDenied, Counter::BroadcastNotPermitted);
    assert_eq!(lan.node.udp_set_broadcast(id, false), Ok(()));
    for to in [Ipv4Addr::BROADCAST, SUBNET_BROADCAST] {
        refused_send(&mut lan, id, to, 5_001, b"hi", Refused::PermissionDenied, Counter::BroadcastNotPermitted);
    }
    lan.node.udp_send_to(lan.now, id, B, 5_001, b"hi").expect("one host needs no permission");
    lan.pump();
    let destinations: Vec<Ipv4Addr> = lan.datagrams().iter().map(|(_, udp)| udp.destination).collect();
    assert_eq!(destinations, [Ipv4Addr::BROADCAST, SUBNET_BROADCAST, B], "and no broadcast since the permission went");

    lan.node.udp_close(lan.now, id).unwrap();
    assert_eq!(lan.node.udp_set_broadcast(id, true), Err(Refused::NotConnected), "a closed socket is given nothing");
}

// US-21 from the wire: the prefix a datagram's destination is read against is a DHCP server's
// value (RFC 2131 §4.4.5: a renewal's ACK carries the parameters again, RFC 2132 §3.3 the mask).
// A socket without the permission has a datagram accepted for 192.0.2.127, one host of the leased
// /24; the server's ACK of the renewal then names 255.255.255.128, under which that address is
// the directed broadcast (RFC 922 §7). The datagram leaves in no frame, and the drop is counted
// and is the node's line for the log when the opportunity that met it returns.
#[test]
fn a_datagram_accepted_for_a_host_is_no_broadcast_after_the_server_renews_a_narrower_prefix() {
    let edge = Ipv4Addr::new(192, 0, 2, 127);
    let mut lan = Lan::new();
    lan.lease(600);
    let (id, _) = lan.node.udp_bind(ANY, port(5_000), undrawn).unwrap();
    let requests = |lan: &Lan| lan.udp().into_iter().filter(|udp| udp.port == 67).count();
    let sent = requests(&lan);
    assert!(lan.run_until(Duration::from_secs(400), |lan| requests(lan) > sent), "the renewal leaves");
    let renewal = xid(&lan.udp().into_iter().rfind(|udp| udp.port == 67).unwrap().payload);
    assert_eq!(lan.node.drain_events().count(), 0, "nothing is owed the log before the send");

    let frames = lan.sent.len();
    lan.node.udp_send_to(lan.now, id, edge, 5_001, b"hi").expect("one host of the /24");
    let narrower = [(54, R.octets().to_vec()), (51, 600u32.to_be_bytes().to_vec()), (1, vec![255, 255, 255, 128]), (6, DNS.octets().to_vec())];
    lan.deliver(&from_server(MAC, A, &message_of(ACK, renewal, &narrower)));
    assert_eq!(lan.node.lease().map(|lease| lease.prefix_len), Some(25), "the server's prefix is the lease's");
    assert_eq!(lan.sent[frames..], [], "nothing left for it: no broadcast, and no request for a link address");

    let rule = toyos_net_ip::Counter::IpBroadcastNotPermitted;
    let shard = lan.node.shard();
    assert_eq!(shard.ip().counters().get(rule), 1);
    let refusal = toyos_net_ip::Refusal { rule, iface: shard.iface(), peer: toyos_net_ip::Peer::Ip(edge) };
    let logged: Vec<Event> = lan.node.drain_events().collect();
    assert_eq!(logged, [Event::Stack { refusal: Refusal::Ip(refusal), suppressed: 0 }]);
}

/// The line the log gets for a send [udp] refused under `rule`, from port 4000 bound to any.
fn line(rule: Counter, peer: (Ipv4Addr, u16), suppressed: u64) -> Event {
    let local = (ANY, port(4_000).unwrap());
    Event::Stack { refusal: Refusal::Udp(toyos_net_udp::Refusal { rule, local, peer }), suppressed }
}

// A refusal [udp] logs (US-20's 0.0.0.0, US-21's broadcast) is the node's line for the log when
// the send that met it returns: no frame, deadline or link change comes between. The shard lets
// one line a rule through in 10 s, and the next after them says how many it stood for.
#[test]
fn a_refusal_is_logged_by_the_call_that_met_it() {
    let mut lan = Lan::new();
    lan.lease(3_600);
    let (id, _) = lan.node.udp_bind(ANY, port(4_000), undrawn).unwrap();
    assert_eq!(lan.node.drain_events().count(), 0, "nothing is owed the log before the send");

    assert_eq!(lan.node.udp_send_to(lan.now, id, Ipv4Addr::BROADCAST, 7, b"x"), Err(Refused::PermissionDenied));
    let logged: Vec<Event> = lan.node.drain_events().collect();
    assert_eq!(logged, [line(Counter::BroadcastNotPermitted, (Ipv4Addr::BROADCAST, 7), 0)]);

    assert_eq!(lan.node.udp_send_to(lan.now, id, ANY, 7, b"x"), Err(Refused::InvalidInput));
    let logged: Vec<Event> = lan.node.drain_events().collect();
    assert_eq!(logged, [line(Counter::SendUnspecifiedDestination, (ANY, 7), 0)]);

    assert_eq!(lan.node.udp_send_to(lan.now, id, SUBNET_BROADCAST, 7, b"x"), Err(Refused::PermissionDenied));
    assert_eq!(lan.node.udp_send_to(lan.now, id, B, 0, b"x"), Err(Refused::InvalidInput));
    lan.node.udp_send_to(lan.now, id, B, 7, b"x").unwrap();
    assert_eq!(lan.node.drain_events().count(), 0, "a second inside 10 s, a refusal that is no line, and a datagram accepted");

    let later = lan.now.after(Duration::from_secs(10));
    assert_eq!(lan.node.udp_send_to(later, id, Ipv4Addr::BROADCAST, 9, b"x"), Err(Refused::PermissionDenied));
    let logged: Vec<Event> = lan.node.drain_events().collect();
    assert_eq!(logged, [line(Counter::BroadcastNotPermitted, (Ipv4Addr::BROADCAST, 9), 1)]);
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

// RFC 1122 §2.3.2.1 and RFC 4861 §7.2.2 have address resolution give up after its requests. A
// client's datagram for a host of the link nobody answers for waits in its own socket while [ip]
// asks, and is dropped and counted when [ip] gives the host up. The client's socket is not
// connected and is told nothing: its next receive finds no datagram and no error, and its next
// send is accepted.
#[test]
fn a_clients_datagram_for_a_host_nobody_answers_for_is_dropped_and_the_client_is_told_nothing() {
    const NOBODY: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 9);
    let mut lan = Lan::new();
    lan.lease(3_600);
    let (id, _) = lan.node.udp_bind(ANY, port(4_000), undrawn).unwrap();
    lan.node.udp_send_to(lan.now, id, NOBODY, 7, b"anyone").unwrap();
    lan.node.udp_send_to(lan.now, id, B, 7, b"b").unwrap();
    lan.pump();
    let payloads: Vec<&[u8]> = lan.datagrams().iter().map(|(_, udp)| udp.payload.as_slice()).collect();
    assert_eq!(payloads, [&b"b"[..]], "the datagram behind the waiting one left");

    let sent = lan.now;
    assert!(lan.run_until(Duration::from_secs(10), |lan| lan.counted(Counter::TxUnreachable) == 1), "the datagram is dropped");
    assert_eq!(lan.now, sent.after(Duration::from_secs(3)), "when [ip] gave the host up");
    assert_eq!(lan.datagrams().len(), 1);
    assert_eq!(lan.node.udp_recv_from(id, &mut [0u8; 8]), Ok(None));
    assert_eq!(lan.node.udp_send_to(lan.now, id, B, 7, b"again"), Ok(()));
}
