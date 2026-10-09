//! UDP through the shard: what nobody takes is answered by [ip], what the network says about a
//! connected socket's datagrams reaches it, a datagram waits for its next hop in its sender and
//! never in [ip], and it leaves under the broadcast permission it was accepted with, by the route
//! of the moment it leaves, whatever prefix the address has been renewed with since.

mod common;

use std::net::Ipv4Addr;
use std::time::Duration;

use common::*;
use toyos_net_shard::{Event, Refusal, Shard};
use toyos_net_testnet::Fate;
use toyos_net_ip::Nud;
use toyos_net_udp::{Counter, Error, SocketError};
use toyos_net_wire::ethernet::{Frame, MacAddr};
use toyos_net_wire::ipv4::Ipv4Packet;
use toyos_net_wire::udp::UdpDatagram;
use toyos_net_wire::Instant;

#[test]
fn s_udp_us_035_shard_no_socket_is_a_port_unreachable() {
    let (mut net, a, b) = segment();
    let now = net.now();
    let socket = net.nodes[b].shard.bind(B, Some(port(5000)), || 0).unwrap();
    net.nodes[b].shard.send_to(now, socket, A, 5001, b"hi").unwrap();
    let start = net.wire().len();
    net.advance(Duration::from_millis(1));
    let a_sent: Vec<String> = net.wire()[start..].iter().filter(|c| c.from == a).map(|c| name(&c.frame)).filter(|f| !f.starts_with("ARP")).collect();
    assert_eq!(a_sent, ["IPv4 Icmp to 192.0.2.2"]);
}

#[test]
fn s_udp_us_046_shard_a_port_unreachable_refuses_the_connected_socket_once() {
    let (mut net, a, b) = segment();
    net.link(a, b).rule = Some(Box::new(|frame| if name(frame).starts_with("IPv4 Udp") { Fate::Drop } else { Fate::Pass }));
    let now = net.now();
    let socket = net.nodes[a].shard.bind(A, Some(port(50001)), || 0).unwrap();
    net.nodes[a].shard.udp_connect(now, socket, B, 53).unwrap();
    net.nodes[a].shard.send_to(now, socket, B, 53, b"hi").unwrap();
    let start = net.wire().len();
    net.advance(Duration::from_millis(1));
    let datagram = net.wire()[start..].iter().find(|c| c.from == a && name(&c.frame).starts_with("IPv4 Udp")).expect("the datagram left").frame.clone();
    let now = net.now();
    net.nodes[a].shard.receive(now, &icmp_about(&datagram, 3, 3, [0; 4]));
    let mut buf = [0u8; 16];
    assert_eq!(net.nodes[a].shard.recv_from(socket, &mut buf), Err(Error::Failed(SocketError::Refused)));
    assert_eq!(net.nodes[a].shard.recv_from(socket, &mut buf), Ok(None));
}

#[test]
fn s_ip_icd_012_shard_a_resolution_failure_reaches_the_connected_socket() {
    let (mut net, a, _) = segment();
    let now = net.now();
    let socket = net.nodes[a].shard.bind(A, Some(port(50001)), || 0).unwrap();
    net.nodes[a].shard.udp_connect(now, socket, C, 53).unwrap();
    net.nodes[a].shard.send_to(now, socket, C, 53, b"hi").unwrap();
    let mut buf = [0u8; 16];
    net.advance(Duration::from_millis(2_999));
    assert_eq!(net.nodes[a].shard.recv_from(socket, &mut buf), Ok(None));
    net.advance(Duration::from_millis(1));
    assert_eq!(net.nodes[a].shard.recv_from(socket, &mut buf), Err(Error::Failed(SocketError::NextHopFailed)));
    assert_eq!(net.nodes[a].shard.recv_from(socket, &mut buf), Ok(None));
    let shard = &net.nodes[a].shard;
    assert_eq!((shard.udp_counters().get(Counter::TxUnreachable), shard.ip().counters().get(toyos_net_ip::Counter::NbPendingDropped)), (1, 0), "dropped where it waited, which is not [ip]");
}

/// C's ARP reply to A, from a MAC of its own.
fn c_answers() -> Vec<u8> {
    let mut frame = [MAC_A.0, MAC_C.0].concat();
    frame.extend_from_slice(&[0x08, 0x06, 0, 1, 8, 0, 6, 4, 0, 2]);
    frame.extend_from_slice(&MAC_C.0);
    frame.extend_from_slice(&C.octets());
    frame.extend_from_slice(&MAC_A.0);
    frame.extend_from_slice(&A.octets());
    frame
}

const MAC_C: MacAddr = MacAddr([0x02, 0, 0, 0, 0, 0x0c]);

/// A, holding B's link address and not C's, with a socket bound to port 5000.
fn b_resolved() -> (toyos_net_testnet::Net, usize, toyos_net_udp::SocketId) {
    let (mut net, a, _) = segment();
    let now = net.now();
    let socket = net.nodes[a].shard.bind(Ipv4Addr::UNSPECIFIED, Some(port(5000)), || 0).unwrap();
    net.nodes[a].shard.send_to(now, socket, B, 9, b"hello").unwrap();
    net.advance(Duration::from_millis(1));
    let shard = &net.nodes[a].shard;
    assert!(shard.ip().neighbour(shard.iface(), B).is_some_and(|entry| entry.mac().is_some()) && shard.ip().neighbour(shard.iface(), C).is_none());
    (net, a, socket)
}

/// One transmit opportunity with all the credit the shard wants: each UDP datagram by its
/// destination and payload, any other frame by its name.
fn left(shard: &mut Shard, now: Instant) -> Vec<String> {
    let mut left = Vec::new();
    shard.transmit(now, usize::MAX, |bytes| {
        let frame = Frame::parse(bytes).expect("a frame");
        let udp = Ipv4Packet::parse(frame.body()).ok().and_then(|ip| UdpDatagram::parse(&ip).ok().map(|udp| format!("UDP {} {}", ip.destination(), String::from_utf8_lossy(udp.payload()))));
        left.push(udp.unwrap_or_else(|| name(bytes)));
    });
    left
}

/// How many datagrams [ip] holds for C while it asks for its link address.
fn held_by_ip(shard: &Shard) -> Option<usize> {
    match shard.ip().neighbour(shard.iface(), C) {
        Some(Nud::Incomplete(asking)) => Some(asking.pending.queued()),
        _ => None,
    }
}

// US-26 through the shard, as built: a datagram whose next hop has no link address
// yet stays in its socket and [ip] is handed none of it; the socket's datagram to another next
// hop leaves past it, and it leaves, in the order its socket accepted it, when the reply arrives.
#[test]
fn s_udp_us_026_shard_a_datagram_waits_in_its_socket_for_its_next_hop_and_holds_back_no_other() {
    let (mut net, a, socket) = b_resolved();
    let now = net.now();
    let shard = &mut net.nodes[a].shard;
    for (destination, data) in [(C, "c1"), (B, "b1"), (C, "c2")] {
        shard.send_to(now, socket, destination, 9, data.as_bytes()).unwrap();
    }
    assert_eq!(left(shard, now), ["UDP 192.0.2.2 b1", "ARP request 192.0.2.3"]);
    assert_eq!(held_by_ip(shard), Some(0), "[ip] holds no sender's datagram");
    assert_eq!(left(shard, now), [""; 0], "what waits asks for nothing again");
    shard.receive(now, &c_answers());
    assert_eq!(left(shard, now), ["UDP 192.0.2.3 c1", "UDP 192.0.2.3 c2"]);
}

// US-25 through the shard: the wait is bounded by the socket's own queue, and a send
// past it is refused in the call. Another socket's datagrams to that next hop wait in their own
// queue and take no place of the first's: each one accepted leaves, none overflowed.
#[test]
fn s_udp_us_025_shard_what_waits_for_a_next_hop_is_bounded_by_its_sockets_queue() {
    let (mut net, a, socket) = b_resolved();
    let now = net.now();
    let shard = &mut net.nodes[a].shard;
    let other = shard.bind(Ipv4Addr::UNSPECIFIED, Some(port(5002)), || 0).unwrap();
    let per = toyos_net_udp::limits::TX_DATAGRAMS;
    for n in 0..per {
        shard.send_to(now, socket, C, 9, format!("s{n}").as_bytes()).unwrap();
        shard.send_to(now, other, C, 9, format!("o{n}").as_bytes()).unwrap();
    }
    assert_eq!(left(shard, now), ["ARP request 192.0.2.3"]);
    for destination in [C, B] {
        assert_eq!(shard.send_to(now, socket, destination, 9, b"x"), Err(Error::Refused(Counter::TxQueueFull)), "{destination}");
    }
    assert_eq!(held_by_ip(shard), Some(0));
    shard.receive(now, &c_answers());
    let out = left(shard, now);
    for (sender, tag) in [(socket, 's'), (other, 'o')] {
        let of: Vec<&String> = out.iter().filter(|frame| frame.starts_with(&format!("UDP 192.0.2.3 {tag}"))).collect();
        let expected: Vec<String> = (0..per).map(|n| format!("UDP 192.0.2.3 {tag}{n}")).collect();
        assert_eq!(of, expected.iter().collect::<Vec<_>>(), "{sender:?}");
    }
    assert_eq!((out.len(), shard.ip().counters().get(toyos_net_ip::Counter::NbPendingOverflow)), (2 * per, 0));
}

// US-27 through the shard for a socket that is not connected, and for a closed one:
// each datagram that waited for the hop [ip] gives up is dropped and counted, nobody is told,
// and none is kept: the host announcing itself afterwards brings none out.
#[test]
fn s_udp_us_027_shard_a_failed_next_hop_drops_what_waited_for_it_and_tells_no_unconnected_socket() {
    let (mut net, a, socket) = b_resolved();
    let now = net.now();
    let shard = &mut net.nodes[a].shard;
    let closed = shard.bind(Ipv4Addr::UNSPECIFIED, Some(port(5002)), || 0).unwrap();
    shard.send_to(now, socket, C, 9, b"s").unwrap();
    shard.send_to(now, closed, C, 9, b"c").unwrap();
    assert_eq!(left(shard, now), ["ARP request 192.0.2.3"]);
    shard.udp_close(now, closed).unwrap();
    let start = net.wire().len();
    net.advance(Duration::from_millis(3_000));
    let shard = &mut net.nodes[a].shard;
    assert_eq!((shard.udp_counters().get(Counter::TxUnreachable), shard.ip().counters().get(toyos_net_ip::Counter::NbFailed)), (2, 1));
    assert_eq!(shard.recv_from(socket, &mut [0; 16]), Ok(None));
    let now = net.now();
    net.nodes[a].shard.receive(now, &c_answers());
    net.advance(Duration::from_millis(1));
    assert!(net.wire()[start..].iter().all(|carried| !name(&carried.frame).starts_with("IPv4 Udp")), "{}", dump(&net, start));
}

// A datagram that waited is asked its route again when the routes change: with the link down it
// has none and is dropped and counted, where it would otherwise wait for a next hop the link no
// longer has.
#[test]
fn s_udp_us_024_shard_a_waiting_datagram_whose_route_goes_is_dropped() {
    let (mut net, a, socket) = b_resolved();
    let now = net.now();
    let shard = &mut net.nodes[a].shard;
    shard.send_to(now, socket, C, 9, b"c1").unwrap();
    assert_eq!(left(shard, now), ["ARP request 192.0.2.3"]);
    shard.link_down(now).unwrap();
    assert_eq!((left(shard, now), shard.udp_counters().get(Counter::TxUnreachable)), (vec![], 1));
    shard.link_up(now).unwrap();
    shard.receive(now, &c_answers());
    assert!(left(shard, now).iter().all(|frame| !frame.starts_with("UDP")));
}

/// One host of 192.0.2.0/24, and the directed broadcast address of 192.0.2.0/25 (RFC 922 §7).
const EDGE_OF_25: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 127);
/// The directed broadcast address of 192.0.2.0/24, which under 192.0.2.0/25 is a host off the link.
const EDGE_OF_24: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 255);
/// A router both prefixes hold.
const ROUTER_IN_25: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 126);

/// One transmit opportunity with all the credit the shard wants: each frame's link destination
/// and, of a UDP datagram, its destination and source port; any other frame by its name.
fn opportunity(shard: &mut Shard, now: Instant) -> Vec<(MacAddr, String)> {
    let mut left = Vec::new();
    shard.transmit(now, usize::MAX, |bytes| {
        let frame = Frame::parse(bytes).expect("a frame");
        let udp = Ipv4Packet::parse(frame.body()).ok().and_then(|ip| UdpDatagram::parse(&ip).ok().map(|udp| format!("UDP {} from port {}", ip.destination(), udp.source_port().map_or(0, |p| p.get()))));
        left.push((frame.destination(), udp.unwrap_or_else(|| name(bytes))));
    });
    left
}

/// [ip] counted `count` datagrams refused a link broadcast, and the shard's one line for the
/// rule names `destination`.
fn refused_a_broadcast(shard: &mut Shard, destination: Ipv4Addr, count: u64) {
    let rule = toyos_net_ip::Counter::IpBroadcastNotPermitted;
    assert_eq!(shard.ip().counters().get(rule), count);
    let lines: Vec<Event> = shard.drain_events().filter(|event| matches!(event, Event::Refused { .. })).collect();
    let refusal = toyos_net_ip::Refusal { rule, iface: shard.iface(), peer: toyos_net_ip::Peer::Ip(destination) };
    assert_eq!(lines, [Event::Refused { refusal: Refusal::Ip(refusal), suppressed: 0 }]);
}

// US-21 (decision U-4) across a renewal: the prefix is a DHCP server's value, and a renewal
// rewrites it under a held address. A datagram a socket without the permission had accepted for
// one host of the /24 is, once the address is renewed as a /25, addressed to that prefix's
// directed broadcast: it leaves in no frame, to the link's broadcast address or any other, and is
// counted and logged. A socket closed with the datagram accepted changes nothing: the closed
// sender's datagrams carry what their socket was permitted.
#[test]
fn s_udp_us_021_shard_a_datagram_accepted_for_a_host_is_no_broadcast_after_a_renewal_narrows_the_prefix() {
    for closed in [false, true] {
        let (mut net, a, _) = segment();
        let now = net.now();
        let shard = &mut net.nodes[a].shard;
        shard.drain_events().for_each(drop);
        let socket = shard.bind(Ipv4Addr::UNSPECIFIED, Some(port(5000)), || 0).unwrap();
        assert_eq!(shard.send_to(now, socket, EDGE_OF_25, 5001, b"hi"), Ok(()), "one host of the /24");
        if closed {
            shard.udp_close(now, socket).unwrap();
        }
        assert_eq!(shard.add_address(now, A, 25), Ok(()), "the renewal");
        assert_eq!(opportunity(shard, now), [], "closed: {closed}");
        refused_a_broadcast(shard, EDGE_OF_25, 1);
        assert_eq!(opportunity(shard, now), [], "and it is gone, not waiting");
    }
}

// The same across a renewal that widens the prefix: under the /25 192.0.2.255 is a host behind
// the router, and under the /24 it is the directed broadcast.
#[test]
fn s_udp_us_021_shard_a_datagram_accepted_for_the_router_is_no_broadcast_after_a_renewal_widens_the_prefix() {
    let (mut net, a, _) = segment();
    let now = net.now();
    let shard = &mut net.nodes[a].shard;
    shard.add_address(now, A, 25).unwrap();
    shard.set_gateways(now, &[ROUTER_IN_25]).unwrap();
    shard.drain_events().for_each(drop);
    let socket = shard.bind(Ipv4Addr::UNSPECIFIED, Some(port(5000)), || 0).unwrap();
    assert_eq!(shard.send_to(now, socket, EDGE_OF_24, 5001, b"hi"), Ok(()), "a host behind the router");
    assert_eq!(shard.add_address(now, A, 24), Ok(()), "the renewal");
    assert_eq!(opportunity(shard, now), []);
    refused_a_broadcast(shard, EDGE_OF_24, 1);
}

// US-21 for a datagram that waited: accepted for one host of the /24 whose link address nobody
// answers for, it waits in its socket; the renewal makes its destination the /25's directed
// broadcast, and it is asked its route again. The one whose socket held the permission at the
// call leaves in a link broadcast, and the other is [ip]'s refusal, counted and logged.
#[test]
fn s_udp_us_021_shard_a_datagram_that_waited_for_a_host_is_asked_its_route_again_and_keeps_its_permission() {
    let (mut net, a, _) = segment();
    let now = net.now();
    let shard = &mut net.nodes[a].shard;
    shard.drain_events().for_each(drop);
    let permitted = shard.bind(Ipv4Addr::UNSPECIFIED, Some(port(5000)), || 0).unwrap();
    let unpermitted = shard.bind(Ipv4Addr::UNSPECIFIED, Some(port(5002)), || 0).unwrap();
    shard.udp_set_broadcast(permitted, true).unwrap();
    for socket in [permitted, unpermitted] {
        assert_eq!(shard.send_to(now, socket, EDGE_OF_25, 5001, b"hi"), Ok(()));
    }
    shard.udp_set_broadcast(permitted, false).unwrap();
    assert_eq!(opportunity(shard, now), [(MacAddr::BROADCAST, "ARP request 192.0.2.127".to_string())], "both wait for the host");
    assert_eq!(shard.add_address(now, A, 25), Ok(()), "the renewal");
    assert_eq!(opportunity(shard, now), [(MacAddr::BROADCAST, "UDP 192.0.2.127 from port 5000".to_string())]);
    refused_a_broadcast(shard, EDGE_OF_25, 1);
}

// The permission is the one the socket held at the call (POSIX's `SO_BROADCAST` is read by the
// send): given after a datagram was accepted it covers nothing accepted before, and taken back
// it still covers what was accepted under it.
#[test]
fn s_udp_us_021_shard_a_datagram_carries_the_permission_of_its_call() {
    let (mut net, a, _) = segment();
    let now = net.now();
    let shard = &mut net.nodes[a].shard;
    shard.drain_events().for_each(drop);
    let permitted = shard.bind(Ipv4Addr::UNSPECIFIED, Some(port(5000)), || 0).unwrap();
    let unpermitted = shard.bind(Ipv4Addr::UNSPECIFIED, Some(port(5002)), || 0).unwrap();
    shard.udp_set_broadcast(permitted, true).unwrap();
    for socket in [permitted, unpermitted] {
        assert_eq!(shard.send_to(now, socket, EDGE_OF_25, 5001, b"hi"), Ok(()));
    }
    shard.udp_set_broadcast(permitted, false).unwrap();
    shard.udp_set_broadcast(unpermitted, true).unwrap();
    assert_eq!(shard.add_address(now, A, 25), Ok(()), "the renewal");
    assert_eq!(opportunity(shard, now), [(MacAddr::BROADCAST, "UDP 192.0.2.127 from port 5000".to_string())]);
    refused_a_broadcast(shard, EDGE_OF_25, 1);
}
