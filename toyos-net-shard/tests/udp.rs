//! UDP through the shard: what nobody takes is answered by [ip], what the network says about a
//! connected socket's datagrams reaches it, and a datagram leaves under the broadcast permission
//! it was accepted with, whatever prefix the address has been renewed with since.

mod common;

use std::net::Ipv4Addr;
use std::time::Duration;

use common::*;
use toyos_net_shard::{Event, Refusal, Shard};
use toyos_net_testnet::Fate;
use toyos_net_udp::{Error, SocketError};
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
