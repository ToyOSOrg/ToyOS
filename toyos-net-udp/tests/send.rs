//! Connecting and sending.

mod common;

use std::net::Ipv4Addr;

use common::*;
use toyos_net_udp::{limits, Binding, Counter, Error, Refusal, SocketError, Verdict};
use toyos_net_wire::ethernet::MacAddr;
use toyos_net_wire::ipv4::Ttl;

fn ip4(a: u8, b: u8, c: u8, d: u8) -> Ipv4Addr {
    Ipv4Addr::new(a, b, c, d)
}

fn refused(counter: Counter) -> Result<(), Error> {
    Err(Error::Refused(counter))
}

fn udp_frames(frames: &[Vec<u8>]) -> Vec<Vec<u8>> {
    frames.iter().filter(|f| ip_of(f).is_some_and(|ip| ip.protocol() == toyos_net_wire::ipv4::Protocol::Udp)).cloned().collect()
}

#[test]
fn s_udp_us_012_connect_fixes_the_local_address() {
    let mut u = U::uf();
    let id = u.bind(ANY, 50_001).unwrap();
    u.udp.connect(&mut u.ip, id, DNS, 53).unwrap();
    assert_eq!(u.udp.binding(id), Ok((Binding::Connected { local: A, peer: DNS, peer_port: port(53) }, port(50_001))));
    u.udp.send(&mut u.ip, id, b"hi").unwrap();
    assert_eq!(u.out(), [hex(V_UDP_A_OUT)]);
}

#[test]
fn s_udp_us_013_what_connect_refuses() {
    let mut u = U::uf();
    let id = u.bind(ANY, 50_001).unwrap();
    for (peer, p, rule) in [
        (ANY, 53, Counter::ConnectUnspecified),
        (LIMITED, 53, Counter::ConnectGroup),
        (ip4(192, 0, 2, 255), 53, Counter::ConnectGroup),
        (MDNS, 5353, Counter::ConnectGroup),
        (ip4(127, 0, 0, 1), 53, Counter::SendLoopback),
        (A, 53, Counter::SendToSelf),
        (ip4(240, 0, 0, 1), 53, Counter::SendInvalidDestination),
        (DNS, 0, Counter::ConnectPortZero),
    ] {
        assert_eq!(u.udp.connect(&mut u.ip, id, peer, p), refused(rule), "{peer}:{p}");
        assert_eq!(u.udp.binding(id).unwrap().0, Binding::Any);
    }
}

#[test]
fn s_udp_us_014_connect_needs_a_source() {
    let mut u = U::bare();
    let id = u.bind(ANY, 50_001).unwrap();
    assert_eq!(u.udp.connect(&mut u.ip, id, DNS, 53), refused(Counter::NoSourceAddress));
}

fn connected_to_dns() -> (U, toyos_net_udp::SocketId) {
    let mut u = U::uf();
    let id = u.bind(ANY, 50_001).unwrap();
    u.udp.connect(&mut u.ip, id, DNS, 53).unwrap();
    (u, id)
}

#[test]
fn s_udp_us_015_a_connected_socket_hears_its_peer_alone() {
    let (mut u, id) = connected_to_dns();
    assert_eq!(u.datagram(&udp(DNS, 53, A, 50_001, b"hi")), Some(Verdict::Delivered));
    assert_eq!(u.recv(id).unwrap().unwrap().0, b"hi");
    for (source, sport) in [(B, 53), (DNS, 54)] {
        let stranger = udp(source, sport, A, 50_001, b"hi");
        assert_eq!(u.datagram(&stranger), Some(Verdict::NoSocket));
        let out = u.out();
        assert_eq!(out.len(), 1);
        let error = ip_of(&out[0]).unwrap();
        assert_eq!((error.protocol(), error.payload()[0], error.payload()[1]), (toyos_net_wire::ipv4::Protocol::Icmp, 3, 3));
        assert_eq!(error.payload()[8..], stranger[..]);
    }
    assert_eq!(u.udp.recv(id, &mut [0; 64]), Ok(None));
}

#[test]
fn s_udp_us_016_connect_purges_strangers() {
    let mut u = U::uf();
    let id = u.bind(ANY, 50_001).unwrap();
    u.datagram(&udp(DNS, 53, A, 50_001, b"1"));
    u.datagram(&udp(DNS, 53, A, 50_001, b"2"));
    u.datagram(&udp(B, 5_000, A, 50_001, b"3"));
    u.udp.connect(&mut u.ip, id, B, 5_000).unwrap();
    assert_eq!(u.count(Counter::RxDroppedOnConnect), 2);
    assert_eq!(u.recv(id).unwrap().unwrap().0, b"3");
    assert_eq!(u.udp.recv(id, &mut [0; 64]), Ok(None));
}

#[test]
fn s_udp_us_017_a_connected_socket_sends_to_its_peer() {
    let (mut u, id) = connected_to_dns();
    assert_eq!(u.udp.send_to(&mut u.ip, id, B, 5_000, b"x"), refused(Counter::ConnectedDestinationMismatch));
    assert_eq!(u.udp.send_to(&mut u.ip, id, DNS, 53, b"x"), Ok(()));
    let bound = u.bind(ANY, 5_002).unwrap();
    assert_eq!(u.udp.send(&mut u.ip, bound, b"x"), refused(Counter::NotConnected));
}

#[test]
fn s_udp_us_018_reconnect() {
    let (mut u, id) = connected_to_dns();
    u.udp.connect(&mut u.ip, id, B, 5_000).unwrap();
    assert_eq!(u.udp.binding(id).unwrap().0, Binding::Connected { local: A, peer: B, peer_port: port(5_000) });
    assert_eq!(u.datagram(&udp(DNS, 53, A, 50_001, b"hi")), Some(Verdict::NoSocket));
    assert_eq!(u.udp.recv(id, &mut [0; 64]), Ok(None));
}

#[test]
fn s_udp_us_019_a_bound_socket_sends() {
    let mut u = U::uf();
    let id = u.bind(ANY, 50_001).unwrap();
    u.udp.send_to(&mut u.ip, id, DNS, 53, b"hi").unwrap();
    let out = u.out();
    assert_eq!(out, [hex(V_UDP_A_OUT)]);
    let ip = ip_of(&out[0]).unwrap();
    assert_eq!((ip.payload()[6], ip.payload()[7]), (0x4f, 0xb3));
    assert!(ip.dont_fragment() && ip.identification() == 0 && ip.ttl() == 64);
}

#[test]
fn s_udp_us_020_what_send_refuses() {
    let mut u = U::uf();
    let id = u.bind(ANY, 50_001).unwrap();
    for (destination, p, rule) in [
        (DNS, 0, Counter::SendPortZero),
        (ANY, 53, Counter::SendUnspecifiedDestination),
        (ip4(127, 0, 0, 1), 53, Counter::SendLoopback),
        (A, 53, Counter::SendToSelf),
        (ip4(240, 0, 0, 1), 53, Counter::SendInvalidDestination),
    ] {
        assert_eq!(u.udp.send_to(&mut u.ip, id, destination, p, b"x"), refused(rule), "{destination}:{p}");
    }
    let logged: Vec<Refusal> = u.udp.drain_refusals().collect();
    assert_eq!(logged, [Refusal { rule: Counter::SendUnspecifiedDestination, local: (ANY, port(50_001)), peer: (ANY, 53) }]);
    assert!(u.out().is_empty());

    for _ in 0..=limits::EVENTS {
        assert_eq!(u.udp.send_to(&mut u.ip, id, ANY, 53, b"x"), refused(Counter::SendUnspecifiedDestination));
    }
    assert_eq!(u.udp.drain_refusals().count(), limits::EVENTS, "an undrained shell holds a bounded list");
    assert_eq!(u.count(Counter::EventOverflow), 1);
}

#[test]
fn s_udp_us_021_broadcast_needs_permission() {
    let mut u = U::uf();
    let id = u.bind(ANY, 5_000).unwrap();
    for destination in [LIMITED, ip4(192, 0, 2, 255)] {
        assert_eq!(u.udp.send_to(&mut u.ip, id, destination, 5_001, b"hi"), refused(Counter::BroadcastNotPermitted));
    }
    assert_eq!(u.udp.drain_refusals().count(), 2, "each refusal is logged");
    u.udp.set_broadcast(id, true).unwrap();
    for destination in [LIMITED, ip4(192, 0, 2, 255)] {
        u.udp.send_to(&mut u.ip, id, destination, 5_001, b"hi").unwrap();
    }
    let out = u.out();
    assert_eq!(out.len(), 2);
    for (frame, destination) in out.iter().zip([LIMITED, ip4(192, 0, 2, 255)]) {
        assert_eq!(destination_of(frame), MacAddr::BROADCAST);
        let ip = ip_of(frame).unwrap();
        assert_eq!((ip.destination(), ip.ttl()), (destination, 64));
    }
}

#[test]
fn s_udp_us_022_multicast_ttl() {
    let mut u = U::uf();
    let query = hex(V_UDP_MDNS)[42..].to_vec();
    let mdns = u.bind(ANY, 5353).unwrap();
    u.udp.set_ttl(mdns, Ttl::new(255).unwrap(), Ttl::new(255).unwrap()).unwrap();
    u.udp.send_to(&mut u.ip, mdns, MDNS, 5353, &query).unwrap();
    let expected = edit(hex(V_UDP_MDNS)[14..].to_vec(), |ip| set_source(ip, A));
    let out = u.out();
    assert_eq!(destination_of(&out[0]), MacAddr([1, 0, 0x5e, 0, 0, 0xfb]));
    assert_eq!(ip_bytes(&out[0]), expected);
    let plain = u.bind(ANY, 5354).unwrap();
    u.udp.send_to(&mut u.ip, plain, MDNS, 5353, &query).unwrap();
    assert_eq!(ip_of(&u.out()[0]).unwrap().ttl(), 1);
}

#[test]
fn s_udp_us_023_payload_sizes() {
    let mut u = U::uf();
    let id = u.bind(ANY, 50_001).unwrap();
    u.udp.send_to(&mut u.ip, id, DNS, 53, &[0; 1_472]).unwrap();
    assert_eq!(ip_of(&u.out()[0]).unwrap().total_length(), 1_500);
    assert_eq!(u.udp.send_to(&mut u.ip, id, DNS, 53, &[0; 1_473]), refused(Counter::ExceedsMtu));
    u.udp.send_to(&mut u.ip, id, DNS, 53, &[]).unwrap();
    let empty = u.out();
    assert_eq!(ip_of(&empty[0]).unwrap().payload()[4..6], [0, 8]);
    assert_eq!(limits::MAX_PAYLOAD, 1_472);
}

#[test]
fn s_udp_us_024_the_source_address_must_be_ours() {
    let mut u = U::bare();
    let id = u.bind(ANY, 50_001).unwrap();
    assert_eq!(u.udp.send_to(&mut u.ip, id, DNS, 53, b"x"), refused(Counter::NoSourceAddress));

    let mut u = U::uf();
    u.ip.set_gateways(u.clock(), u.if0, &[]).unwrap();
    let id = u.bind(ANY, 50_001).unwrap();
    assert_eq!(u.udp.send_to(&mut u.ip, id, ip4(198, 51, 100, 7), 53, b"x"), refused(Counter::NoRoute), "an address, and no gateway");

    let mut u = U::uf();
    let id = u.bind(A, 5_001).unwrap();
    u.ip.remove_address(u.clock(), u.if0, A).unwrap();
    assert_eq!(u.udp.send_to(&mut u.ip, id, B, 5_000, b"x"), refused(Counter::SourceAddressNotAssigned));
    u.ip.add_address(u.clock(), u.if0, A, 24).unwrap();
    u.run(300);
    u.udp.send_to(&mut u.ip, id, B, 5_000, b"x").unwrap();
    let out = udp_frames(&u.run(2_000));
    assert_eq!(out.len(), 1);
    assert_eq!(ip_of(&out[0]).unwrap().source(), A);
}

#[test]
fn s_udp_us_025_a_full_queue_refuses() {
    let mut u = U::uf();
    let id = u.bind(ANY, 50_001).unwrap();
    for _ in 0..16 {
        u.udp.send_to(&mut u.ip, id, DNS, 53, &[1; 100]).unwrap();
    }
    assert_eq!(u.udp.send_to(&mut u.ip, id, DNS, 53, &[1; 100]), refused(Counter::TxQueueFull));
    assert_eq!(u.out_with(1).len(), 1);
    assert_eq!(u.udp.send_to(&mut u.ip, id, DNS, 53, &[1; 100]), Ok(()));
}

#[test]
fn s_udp_us_026_no_head_of_line_blocking() {
    let mut u = U::uf();
    let id = u.bind(ANY, 50_001).unwrap();
    let far = ip4(192, 0, 2, 77);
    u.udp.send_to(&mut u.ip, id, far, 53, b"1").unwrap();
    u.udp.send_to(&mut u.ip, id, DNS, 53, b"2").unwrap();
    let out = u.out();
    assert_eq!(out.len(), 2);
    assert!(out.iter().any(|f| is_arp_request_for(f, far)));
    assert!(out.iter().any(|f| ip_of(f).is_some_and(|ip| ip.destination() == DNS)));
    let reply = {
        let mut a = vec![0, 1, 8, 0, 6, 4, 0, 2];
        a.extend_from_slice(&[2, 0, 0, 0, 0, 0x77]);
        a.extend_from_slice(&far.octets());
        a.extend_from_slice(&MAC_A.0);
        a.extend_from_slice(&A.octets());
        eth(MAC_A, MacAddr([2, 0, 0, 0, 0, 0x77]), 0x0806, &a)
    };
    u.frame(&reply);
    let out = u.out();
    assert_eq!(out.len(), 1);
    assert_eq!(ip_of(&out[0]).unwrap().destination(), far);

    // With one frame of credit: the held datagram spends none of it.
    let mut u = U::uf();
    let id = u.bind(ANY, 50_001).unwrap();
    u.udp.send_to(&mut u.ip, id, far, 53, b"1").unwrap();
    u.udp.send_to(&mut u.ip, id, DNS, 53, b"2").unwrap();
    let out = u.out_with(1);
    assert_eq!(out.len(), 1);
    assert!(ip_of(&out[0]).is_some_and(|ip| ip.destination() == DNS));
    assert!(is_arp_request_for(&u.out_with(1)[0], far));
}

#[test]
fn s_udp_us_027_a_failed_next_hop_is_reported_once() {
    let mut u = U::uf();
    let id = u.bind(ANY, 50_001).unwrap();
    let far = ip4(192, 0, 2, 77);
    u.udp.connect(&mut u.ip, id, far, 53).unwrap();
    u.udp.send(&mut u.ip, id, b"q").unwrap();
    u.run(3_000);
    assert_eq!(u.ip_count(toyos_net_ip::Counter::NbPendingDropped), 1);
    assert_eq!(u.udp.recv(id, &mut [0; 64]), Err(Error::Failed(SocketError::Unreachable)));
    assert_eq!(u.udp.recv(id, &mut [0; 64]), Ok(None));
}

fn acquisition() -> (U, toyos_net_udp::SocketId) {
    let mut u = U::bare();
    let id = u.bind(ANY, 68).unwrap();
    u.udp.set_acquisition(id).unwrap();
    u.udp.set_broadcast(id, true).unwrap();
    (u, id)
}

#[test]
fn s_udp_us_028_only_the_acquisition_socket_sends_from_nowhere() {
    let (mut u, id) = acquisition();
    u.udp.send_from(&mut u.ip, id, ANY, LIMITED, 67, b"discover").unwrap();
    let out = u.out();
    assert_eq!(ip_of(&out[0]).unwrap().source(), ANY);
    assert_eq!(u.udp.send_from(&mut u.ip, id, ANY, R, 67, b"x"), refused(Counter::SourceNotPermitted));
    let other = u.bind(ANY, 5_000).unwrap();
    u.udp.set_broadcast(other, true).unwrap();
    assert_eq!(u.udp.send_from(&mut u.ip, other, ANY, LIMITED, 67, b"x"), refused(Counter::SourceNotPermitted));
}

#[test]
fn s_udp_us_029_the_discover_byte_for_byte() {
    let (mut u, id) = acquisition();
    let discover = hex(V_DHCP_DISCOVER);
    u.udp.send_from(&mut u.ip, id, ANY, LIMITED, 67, &discover[42..]).unwrap();
    assert_eq!(u.out(), [discover]);
}
