//! Connecting and sending.

mod common;

use std::net::Ipv4Addr;

use common::*;
use toyos_net_ip::Nud;
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

/// A host of the link nothing has resolved, and the address it answers ARP from.
const FAR: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 77);
const MAC_FAR: MacAddr = MacAddr([2, 0, 0, 0, 0, 0x77]);

/// FAR's ARP reply to A.
fn far_answers() -> Vec<u8> {
    let mut a = vec![0, 1, 8, 0, 6, 4, 0, 2];
    a.extend_from_slice(&MAC_FAR.0);
    a.extend_from_slice(&FAR.octets());
    a.extend_from_slice(&MAC_A.0);
    a.extend_from_slice(&A.octets());
    eth(MAC_A, MAC_FAR, 0x0806, &a)
}

/// The UDP datagrams among `frames`, each by its destination and payload.
fn datagrams(frames: &[Vec<u8>]) -> Vec<(Ipv4Addr, Vec<u8>)> {
    udp_frames(frames).iter().map(|f| ip_of(f).unwrap()).map(|ip| (ip.destination(), ip.payload()[8..].to_vec())).collect()
}

/// How many datagrams [ip] holds for FAR while it asks for its link address.
fn held_by_ip(u: &U) -> Option<usize> {
    match u.ip.neighbour(u.if0, FAR) {
        Some(Nud::Incomplete(asking)) => Some(asking.pending.queued()),
        _ => None,
    }
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
    u.run(2_999);
    assert_eq!((u.count(Counter::TxUnreachable), u.udp.recv(id, &mut [0; 64])), (0, Ok(None)), "the hop is still asked for");
    u.run(3_000);
    assert_eq!((u.ip_count(toyos_net_ip::Counter::NbFailed), u.count(Counter::TxUnreachable)), (1, 1), "[ip] gave the hop up, and the datagram that waited for it is dropped and counted");
    assert_eq!(u.udp.recv(id, &mut [0; 64]), Err(Error::Failed(SocketError::NextHopFailed)));
    assert_eq!(u.udp.recv(id, &mut [0; 64]), Ok(None));
}

// US-27 for the socket that is not connected: its datagram is a counted drop and
// it is told nothing. Nothing is kept for another try: the host answering once [ip] asks for it
// again brings out no datagram of before.
#[test]
fn s_udp_us_027_an_unconnected_socket_is_told_nothing_and_nothing_is_kept_for_another_try() {
    let mut u = U::uf();
    let id = u.bind(ANY, 50_001).unwrap();
    u.udp.send_to(&mut u.ip, id, FAR, 53, b"1").unwrap();
    let closed = u.bind(ANY, 50_002).unwrap();
    u.udp.send_to(&mut u.ip, closed, FAR, 53, b"2").unwrap();
    u.out();
    u.udp.close(closed).unwrap();
    assert!(datagrams(&u.run(3_000)).is_empty());
    assert_eq!(u.count(Counter::TxUnreachable), 2, "the socket's and the closed socket's");
    assert_eq!(u.udp.recv(id, &mut [0; 64]), Ok(None));

    // Past the hold-down [ip] asks again for a new datagram, and the host answers.
    u.run(30_000);
    u.udp.send_to(&mut u.ip, id, FAR, 53, b"3").unwrap();
    u.out();
    u.frame(&far_answers());
    assert_eq!(datagrams(&u.out()), [(FAR, b"3".to_vec())]);
}

// US-27, for a datagram accepted while [ip] holds its next hop failed (NUD-14): it never
// waited, and is dropped at its turn, counted, and reported to its connected socket once.
#[test]
fn s_udp_us_027_a_datagram_for_a_hop_ip_has_given_up_is_dropped_at_its_turn() {
    let mut u = U::uf();
    let id = u.bind(ANY, 50_001).unwrap();
    u.udp.connect(&mut u.ip, id, FAR, 53).unwrap();
    u.udp.send(&mut u.ip, id, b"q").unwrap();
    u.run(3_000);
    assert_eq!(u.udp.recv(id, &mut [0; 64]), Err(Error::Failed(SocketError::NextHopFailed)));
    assert_eq!(u.udp.send(&mut u.ip, id, b"again"), Ok(()));
    assert_eq!((u.out(), u.count(Counter::TxUnreachable)), (vec![], 2), "no request and no datagram");
    assert_eq!(u.udp.recv(id, &mut [0; 64]), Err(Error::Failed(SocketError::NextHopFailed)));
    assert_eq!(u.udp.recv(id, &mut [0; 64]), Ok(None));
    assert_eq!((u.ip_count(toyos_net_ip::Counter::NbPendingDropped), u.ip_count(toyos_net_ip::Counter::NbFailedRefused)), (0, 0), "[ip] was handed neither");
}

// US-26 as built: a datagram whose next hop is unresolved stays in its socket's queue, and
// [ip]'s queue for that next hop holds none of it.
#[test]
fn s_udp_us_026_a_datagram_waits_for_its_next_hop_in_its_socket_and_never_in_ip() {
    let mut u = U::uf();
    let id = u.bind(ANY, 50_001).unwrap();
    for n in 0..3u8 {
        u.udp.send_to(&mut u.ip, id, FAR, 53, &[n]).unwrap();
    }
    let out = u.out();
    assert!(out.len() == 1 && is_arp_request_for(&out[0], FAR), "one request, and no datagram");
    assert_eq!(held_by_ip(&u), Some(0));
    assert_eq!(u.out(), Vec::<Vec<u8>>::new(), "a waiting datagram asks for nothing again");
    u.frame(&far_answers());
    assert_eq!(datagrams(&u.out()), [(FAR, vec![0]), (FAR, vec![1]), (FAR, vec![2])]);
    assert_eq!(u.ip_count(toyos_net_ip::Counter::NbPendingOverflow), 0);
}

// US-26's order: datagrams leave in FIFO order per socket, which for a socket some of whose
// datagrams wait is the order it accepted them in for each next hop. One accepted for the hop
// after its link address is known does not pass those that waited for it.
#[test]
fn s_udp_us_026_datagrams_to_one_next_hop_leave_in_the_order_they_were_accepted() {
    let mut u = U::uf();
    let id = u.bind(ANY, 50_001).unwrap();
    for (destination, data) in [(FAR, b"f1"), (DNS, b"d1"), (FAR, b"f2"), (B, b"b1"), (DNS, b"d2"), (FAR, b"f3")] {
        u.udp.send_to(&mut u.ip, id, destination, 53, data).unwrap();
    }
    assert_eq!(datagrams(&u.out()), [(DNS, b"d1".to_vec()), (B, b"b1".to_vec()), (DNS, b"d2".to_vec())]);
    u.frame(&far_answers());
    u.udp.send_to(&mut u.ip, id, FAR, 53, b"f4").unwrap();
    u.udp.send_to(&mut u.ip, id, DNS, 53, b"d3").unwrap();
    assert_eq!(
        datagrams(&u.out()),
        [(FAR, b"f1".to_vec()), (FAR, b"f2".to_vec()), (FAR, b"f3".to_vec()), (FAR, b"f4".to_vec()), (DNS, b"d3".to_vec())]
    );
}

// US-25: what waits is bounded by the queue it waits in, `limits::TX_DATAGRAMS` a socket,
// and a send past it is refused in the call, whatever its destination. Nothing waits anywhere
// else: [ip] holds none, and every one accepted leaves once the next hop answers.
#[test]
fn s_udp_us_025_what_waits_for_a_next_hop_is_bounded_by_its_sockets_queue() {
    let mut u = U::uf();
    let id = u.bind(ANY, 50_001).unwrap();
    for n in 0..limits::TX_DATAGRAMS {
        u.udp.send_to(&mut u.ip, id, FAR, 53, &[n as u8]).unwrap();
    }
    u.out();
    for destination in [FAR, DNS] {
        assert_eq!(u.udp.send_to(&mut u.ip, id, destination, 53, b"x"), refused(Counter::TxQueueFull), "{destination}");
    }
    assert_eq!((held_by_ip(&u), u.ip_count(toyos_net_ip::Counter::NbPendingOverflow)), (Some(0), 0));
    u.frame(&far_answers());
    let expected: Vec<(Ipv4Addr, Vec<u8>)> = (0..limits::TX_DATAGRAMS).map(|n| (FAR, vec![n as u8])).collect();
    assert_eq!(datagrams(&u.out()), expected);
    assert_eq!(u.udp.send_to(&mut u.ip, id, DNS, 53, b"x"), Ok(()));
}

// US-53 with US-26: the closed sender mixes sockets, so its datagrams wait each for its own
// next hop too. One that waits holds back no other closed socket's, and leaves when its hop
// answers.
#[test]
fn s_udp_us_053_a_closed_sockets_datagram_waits_for_its_next_hop_and_holds_back_no_other() {
    let mut u = U::uf();
    let first = u.bind(ANY, 50_001).unwrap();
    u.udp.send_to(&mut u.ip, first, FAR, 53, b"far").unwrap();
    u.udp.close(first).unwrap();
    let second = u.bind(ANY, 50_002).unwrap();
    u.udp.send_to(&mut u.ip, second, DNS, 53, b"near").unwrap();
    u.udp.close(second).unwrap();
    let out = u.out();
    assert_eq!(datagrams(&out), [(DNS, b"near".to_vec())]);
    assert!(out.iter().any(|f| is_arp_request_for(f, FAR)));
    u.frame(&far_answers());
    assert_eq!(datagrams(&u.out()), [(FAR, b"far".to_vec())]);
    assert_eq!(u.count(Counter::TxDiscardedOnClose), 0);
}

// A waiting datagram's next hop is the route's of the moment it leaves: when the routes change
// it is asked again, and with no route left it is dropped and counted (US-24: the address
// it was accepted from is no longer assigned).
#[test]
fn s_udp_us_024_a_waiting_datagram_whose_route_goes_is_dropped() {
    let mut u = U::uf();
    let id = u.bind(ANY, 50_001).unwrap();
    u.udp.send_to(&mut u.ip, id, FAR, 53, b"1").unwrap();
    u.out();
    u.ip.remove_address(u.clock(), u.if0, A).unwrap();
    assert_eq!((datagrams(&u.out()), u.count(Counter::TxUnreachable)), (vec![], 1));
    u.frame(&far_answers());
    assert_eq!(datagrams(&u.out()), vec![]);
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
