//! §2 and §3: interface addressing and the host routing table.

mod common;

use std::net::Ipv4Addr;

use common::*;
use toyos_net_ip::{limits, AddrState, Cast, Counter, NextHop, Nud, Route, Source};
use toyos_net_wire::ethernet::MacAddr;
use toyos_net_wire::ipv4::Ttl;

fn route(h: &mut H, destination: Ipv4Addr) -> Result<Route, Counter> {
    h.ip.route(destination, Source::Any, None)
}

fn ip4(a: u8, b: u8, c: u8, d: u8) -> Ipv4Addr {
    Ipv4Addr::new(a, b, c, d)
}

/// Every probing interval scaled as ruled: the first announcement, when the address becomes
/// usable, lies within 200 ms of the add.
#[test]
fn s_ip_addr_001_an_added_address_becomes_a_connected_route() {
    let mut h = H::bare();
    h.ip.add_address(h.clock(), h.if0, A, 24).unwrap();
    assert_eq!(h.ip.address(h.if0, A), Some(AddrState::Tentative));
    assert_eq!(route(&mut h, B), Err(Counter::RouteNoSourceAddress));
    h.run(200);
    assert_eq!(h.ip.address(h.if0, A), Some(AddrState::Announcing));
    h.run(3_000);
    assert_eq!(h.ip.address(h.if0, A), Some(AddrState::Assigned));
    assert_eq!(route(&mut h, ip4(192, 0, 2, 77)), Ok(Route { iface: h.if0, next_hop: NextHop::Neighbour(ip4(192, 0, 2, 77)), source: A }));
}

#[test]
fn s_ip_addr_002_prefix_lengths_0_and_33() {
    let mut h = H::bare();
    for len in [0, 33] {
        assert_eq!(h.ip.add_address(h.clock(), h.if0, A, len), Err(Counter::AddrPrefixInvalid));
    }
    assert_eq!(h.count(Counter::AddrPrefixInvalid), 2);
    assert_eq!(h.ip.address(h.if0, A), None);
}

#[test]
fn s_ip_addr_003_not_unicast() {
    let mut h = H::bare();
    for (addr, len) in [(ip4(0, 0, 0, 0), 24), (ip4(0, 1, 2, 3), 24), (ip4(127, 0, 0, 1), 8), (ip4(224, 0, 0, 1), 24), (ip4(240, 0, 0, 1), 24), (ip4(255, 255, 255, 255), 32)] {
        assert_eq!(h.ip.add_address(h.clock(), h.if0, addr, len), Err(Counter::AddrNotUnicast), "{addr}/{len}");
    }
}

#[test]
fn s_ip_addr_004_network_and_broadcast_forms() {
    let mut h = H::bare();
    for addr in [ip4(192, 0, 2, 0), ip4(192, 0, 2, 255)] {
        assert_eq!(h.ip.add_address(h.clock(), h.if0, addr, 24), Err(Counter::AddrNotUnicast));
    }
    for (addr, len) in [(ip4(192, 0, 2, 0), 31), (ip4(192, 0, 2, 1), 31), (ip4(192, 0, 2, 255), 32)] {
        assert_eq!(h.ip.add_address(h.clock(), h.if0, addr, len), Ok(()), "{addr}/{len}");
    }
}

#[test]
fn s_ip_addr_005_one_address_one_interface() {
    let mut h = H::raw();
    let (if0, if1) = (h.if0, h.add_if1());
    h.assign(if0, A, 24, 0);
    h.settle();
    assert_eq!(h.ip.add_address(h.clock(), if1, A, 24), Err(Counter::AddrDuplicate));
    assert_eq!(h.ip.add_address(h.clock(), if0, A, 25), Ok(()));
    assert_eq!(h.ip.prefix_len(if0, A), Some(25));
    assert_eq!(h.ip.address(if0, A), Some(AddrState::Assigned));
}

#[test]
fn s_ip_addr_006_eight_addresses() {
    let mut h = H::bare();
    for n in 0..limits::ADDRS as u8 {
        h.ip.add_address(h.clock(), h.if0, ip4(192, 0, 2, 10 + n), 24).unwrap();
    }
    assert_eq!(h.ip.add_address(h.clock(), h.if0, ip4(192, 0, 2, 30), 24), Err(Counter::AddrTooMany));
}

fn to(destination: Ipv4Addr) -> Vec<u8> {
    udp(B, destination, 5000, 5001, b"hi")
}

#[test]
fn s_ip_addr_007_what_is_for_us() {
    let mut h = H::fixture_i();
    for (destination, frame, cast) in [
        (A, MAC_A, Cast::Unicast),
        (ip4(192, 0, 2, 255), MacAddr::BROADCAST, Cast::SubnetBroadcast),
        (ip4(255, 255, 255, 255), MacAddr::BROADCAST, Cast::LimitedBroadcast),
        (ip4(224, 0, 0, 1), MacAddr([1, 0, 0x5e, 0, 0, 1]), Cast::Multicast(group(ip4(224, 0, 0, 1)))),
        (MDNS, MacAddr([1, 0, 0x5e, 0, 0, 0xfb]), Cast::Multicast(group(MDNS))),
    ] {
        assert_eq!(cast_of(&h.datagram_to(frame, &to(destination))), Some(cast), "{destination}");
    }
    for destination in [ip4(224, 0, 0, 252), ip4(192, 0, 2, 7)] {
        assert_eq!(cast_of(&h.datagram_to(MAC_A, &to(destination))), None);
    }
    assert_eq!(h.count(Counter::IpNotForUs), 2);
}

#[test]
fn s_ip_addr_008_slash_31_and_32() {
    let mut h = H::raw();
    let if0 = h.if0;
    h.assign(if0, ip4(192, 0, 2, 0), 31, 0);
    h.settle();
    assert_eq!(cast_of(&h.datagram(&udp(ip4(192, 0, 2, 1), ip4(192, 0, 2, 0), 5000, 5001, b"hi"))), Some(Cast::Unicast));
    assert_eq!(cast_of(&h.datagram(&udp(ip4(198, 51, 100, 9), ip4(192, 0, 2, 1), 5000, 5001, b"hi"))), None);
    assert_eq!(h.count(Counter::IpNotForUs), 1);

    let mut h = H::raw();
    let if0 = h.if0;
    h.assign(if0, A, 32, 0);
    h.settle();
    assert_eq!(cast_of(&h.datagram(&to(A))), Some(Cast::Unicast));
    assert_eq!(cast_of(&h.datagram(&to(ip4(192, 0, 2, 255)))), None);
    assert_eq!(h.count(Counter::IpNotForUs), 1);
}

#[test]
fn s_ip_addr_009_the_strong_model() {
    let mut h = H::raw();
    let (if0, if1) = (h.if0, h.add_if1());
    h.assign(if0, A, 24, 0);
    h.assign(if1, ip4(198, 51, 100, 1), 24, 5_000);
    h.settle();
    assert_eq!(cast_of(&h.datagram(&to(ip4(198, 51, 100, 1)))), None);
    assert_eq!(h.count(Counter::IpNotForUs), 1);
}

#[test]
fn s_ip_addr_010_limited_broadcast_needs_no_address() {
    let mut h = H::bare();
    let offer = udp(ip4(192, 0, 2, 254), ip4(255, 255, 255, 255), 67, 68, b"offer");
    assert_eq!(cast_of(&h.datagram_to(MacAddr::BROADCAST, &offer)), Some(Cast::LimitedBroadcast));
}

#[test]
fn s_ip_addr_011_a_tentative_address_is_not_ours_yet() {
    let mut h = H::bare();
    h.ip.add_address(h.clock(), h.if0, A, 24).unwrap();
    assert_eq!(cast_of(&h.datagram(&to(A))), None);
    assert_eq!(h.count(Counter::IpTentativeDestination), 1);
    h.frame(&hex(V_ARP_REQ_B));
    assert!(h.out().iter().all(|o| o.arp().is_none_or(|a| a.operation != toyos_net_wire::arp::Operation::Reply)));
    assert_eq!(h.ip.address(h.if0, A), Some(AddrState::Tentative));
    assert_eq!(h.count(Counter::AcdConflict), 0);
}

#[test]
fn s_ip_addr_012_no_classful_broadcast() {
    let mut h = H::raw();
    let if0 = h.if0;
    h.assign(if0, ip4(10, 1, 2, 3), 24, 0);
    h.settle();
    for destination in [ip4(10, 255, 255, 255), ip4(10, 1, 2, 0)] {
        assert_eq!(cast_of(&h.datagram(&udp(ip4(10, 1, 2, 9), destination, 5000, 5001, b"hi"))), None, "{destination}");
    }
    assert_eq!(h.count(Counter::IpNotForUs), 2);
    let delivered = h.datagram_to(MacAddr::BROADCAST, &udp(ip4(10, 1, 2, 9), ip4(10, 1, 2, 255), 5000, 5001, b"hi"));
    assert_eq!(cast_of(&delivered), Some(Cast::SubnetBroadcast));
}

#[test]
fn s_ip_addr_013_announcing_is_usable() {
    let mut h = H::bare();
    h.ip.add_address(h.clock(), h.if0, A, 24).unwrap();
    h.run(200);
    assert_eq!(h.ip.address(h.if0, A), Some(AddrState::Announcing));
    assert_eq!(cast_of(&h.datagram(&to(A))), Some(Cast::Unicast));
    h.frame(&hex(V_ARP_REQ_B));
    assert_eq!(h.out().iter().map(|o| o.frame.clone()).collect::<Vec<_>>(), [hex(V_ARP_REPLY_A)]);
}

#[test]
fn s_ip_addr_014_removal_withdraws_route_and_gateway() {
    let mut h = H::fixture_i();
    let generation = h.ip.generation();
    h.ip.remove_address(h.clock(), h.if0, A).unwrap();
    assert_eq!(h.count(Counter::RouteGatewayWithdrawn), 1);
    assert_eq!(h.ip.gateways(h.if0), Some(&[][..]));
    assert!(h.ip.generation() > generation);
    assert_eq!(route(&mut h, B), Err(Counter::RouteNoSourceAddress));
    assert!(!h.ip.is_assigned(A));
}

#[test]
fn s_ip_rte_001_a_connected_destination() {
    let mut h = H::fixture_i();
    assert_eq!(route(&mut h, B), Ok(Route { iface: h.if0, next_hop: NextHop::Neighbour(B), source: A }));
}

#[test]
fn s_ip_rte_002_the_default_gateway() {
    let mut h = H::fixture_i();
    assert_eq!(route(&mut h, REMOTE), Ok(Route { iface: h.if0, next_hop: NextHop::Neighbour(R), source: A }));
}

#[test]
fn s_ip_rte_003_no_gateway() {
    let mut h = H::fixture_i();
    h.ip.set_gateways(h.clock(), h.if0, &[]).unwrap();
    assert_eq!(route(&mut h, REMOTE), Err(Counter::RouteNone));
    assert_eq!(h.count(Counter::RouteNone), 1);
}

#[test]
fn s_ip_rte_004_longest_prefix() {
    let mut h = H::raw();
    let (if0, if1) = (h.if0, h.add_if1());
    h.assign(if0, A, 24, 0);
    h.assign(if1, ip4(192, 0, 2, 129), 25, 5_000);
    h.settle();
    assert_eq!(route(&mut h, ip4(192, 0, 2, 200)).map(|r| r.iface), Ok(if1));
    assert_eq!(route(&mut h, ip4(192, 0, 2, 100)).map(|r| r.iface), Ok(if0));
}

#[test]
fn s_ip_rte_005_gateways_are_on_link_unicast_hosts() {
    let mut h = H::fixture_i();
    for gateway in [ip4(224, 0, 0, 1), ip4(192, 0, 2, 255), ip4(192, 0, 2, 0)] {
        assert_eq!(h.ip.set_gateways(h.clock(), h.if0, &[gateway]), Err(Counter::RouteGatewayInvalid), "{gateway}");
    }
    assert_eq!(h.ip.set_gateways(h.clock(), h.if0, &[A]), Err(Counter::RouteGatewayIsLocal));
    assert_eq!(h.ip.set_gateways(h.clock(), h.if0, &[ip4(198, 51, 100, 1)]), Err(Counter::RouteGatewayOffLink));
    assert_eq!(h.ip.gateways(h.if0), Some(&[R][..]));
}

#[test]
fn s_ip_rte_006_invalid_and_local_destinations() {
    let mut h = H::fixture_i();
    for destination in [ip4(127, 0, 0, 1), ip4(0, 0, 0, 0), ip4(0, 1, 2, 3), ip4(240, 0, 0, 1)] {
        assert_eq!(route(&mut h, destination), Err(Counter::RouteInvalidDestination), "{destination}");
    }
    assert_eq!(route(&mut h, A), Err(Counter::RouteLocalDestination));
}

#[test]
fn s_ip_rte_007_limited_broadcast_needs_one_interface() {
    let mut h = H::raw();
    let (if0, if1) = (h.if0, h.add_if1());
    h.assign(if0, A, 24, 0);
    h.assign(if1, ip4(198, 51, 100, 1), 24, 5_000);
    h.settle();
    let limited = ip4(255, 255, 255, 255);
    assert_eq!(h.ip.route(limited, Source::Any, None), Err(Counter::RouteAmbiguousInterface));
    let bound = h.ip.route(limited, Source::Any, Some(if1)).unwrap();
    assert_eq!((bound.iface, bound.next_hop), (if1, NextHop::Broadcast));
    let frame = h.send(ip4(198, 51, 100, 1), limited, 5001, 5001, b"hi").unwrap().unwrap();
    assert_eq!(destination_of(&frame), MacAddr::BROADCAST);
    h.ip.link_down(h.clock(), if1).unwrap();
    assert_eq!(h.ip.route(limited, Source::Any, None).map(|r| r.iface), Ok(if0));
}

#[test]
fn s_ip_rte_008_multicast_ttl_is_the_callers() {
    let mut h = H::fixture_i();
    for ttl in [1, 255] {
        let frame = h.send_ttl(A, MDNS, 5353, 5353, b"q", Ttl::new(ttl).unwrap()).unwrap().unwrap();
        assert_eq!(destination_of(&frame), MacAddr([1, 0, 0x5e, 0, 0, 0xfb]));
        assert_eq!(ip_of(&frame).unwrap().ttl(), ttl);
    }
}

#[test]
fn s_ip_rte_009_subnet_broadcast_needs_no_arp() {
    let mut h = H::fixture_i();
    let frame = h.udp_to(ip4(192, 0, 2, 255)).unwrap().unwrap();
    assert_eq!(destination_of(&frame), MacAddr::BROADCAST);
    assert!(h.out().is_empty());
}

#[test]
fn s_ip_rte_010_the_source_of_the_next_hops_prefix() {
    let mut h = H::raw();
    let if0 = h.if0;
    h.assign(if0, A, 24, 0);
    h.assign(if0, ip4(198, 51, 100, 1), 24, 5_000);
    h.ip.set_gateways(toyos_net_ip::Instant::from_millis(9_000), if0, &[R]).unwrap();
    h.settle();
    assert_eq!(route(&mut h, ip4(203, 0, 113, 9)).map(|r| r.source), Ok(A));
    assert_eq!(route(&mut h, ip4(198, 51, 100, 9)).map(|r| r.source), Ok(ip4(198, 51, 100, 1)));
}

#[test]
fn s_ip_rte_011_a_bound_source_picks_its_interface() {
    let mut h = H::raw();
    let (if0, if1) = (h.if0, h.add_if1());
    h.assign(if0, A, 24, 0);
    h.assign(if1, ip4(198, 51, 100, 1), 24, 5_000);
    h.ip.set_gateways(toyos_net_ip::Instant::from_millis(9_000), if0, &[R]).unwrap();
    h.settle();
    let bound = Source::Bound(ip4(198, 51, 100, 1));
    assert_eq!(h.ip.route(B, bound, None), Err(Counter::RouteNone));
    h.ip.set_gateways(h.clock(), if1, &[ip4(198, 51, 100, 254)]).unwrap();
    let r = h.ip.route(B, bound, None).unwrap();
    assert_eq!((r.iface, r.next_hop), (if1, NextHop::Neighbour(ip4(198, 51, 100, 254))));
}

#[test]
fn s_ip_rte_012_only_the_acquiring_source_is_unspecified() {
    let mut h = H::bare();
    h.ip.add_address(h.clock(), h.if0, A, 24).unwrap();
    assert_eq!(h.udp_to(B), Err(Counter::RouteNoSourceAddress));
    let frame = h.send(ip4(0, 0, 0, 0), ip4(255, 255, 255, 255), 68, 67, b"discover").unwrap().unwrap();
    assert_eq!(ip_of(&frame).unwrap().source(), ip4(0, 0, 0, 0));
    assert_eq!(destination_of(&frame), MacAddr::BROADCAST);
}

/// Gateways [192.0.2.254, 192.0.2.253], neither resolved.
fn two_gateways() -> H {
    let mut h = H::fixture_i();
    h.ip.set_gateways(h.clock(), h.if0, &[R, ip4(192, 0, 2, 253)]).unwrap();
    h.rebase();
    h
}

fn fail(h: &mut H, gateway: Ipv4Addr) {
    let start = h.now;
    let _ = h.ip.resolve(h.clock(), h.if0, gateway, A);
    h.out();
    h.run(start + 3_000);
    assert!(matches!(h.state(gateway), Some(Nud::Failed)));
}

#[test]
fn s_ip_rte_013_a_failed_gateway_is_passed_over() {
    let mut h = two_gateways();
    let generation = h.ip.generation();
    fail(&mut h, R);
    assert_eq!(route(&mut h, REMOTE).map(|r| r.next_hop), Ok(NextHop::Neighbour(ip4(192, 0, 2, 253))));
    assert_eq!(h.count(Counter::RouteGatewaySwitched), 1);
    assert!(h.ip.generation() > generation);
    h.run(3_000 + limits::nud::FAILED_HOLD.as_millis() as u64);
    assert!(h.state(R).is_none());
    assert_eq!(route(&mut h, REMOTE).map(|r| r.next_hop), Ok(NextHop::Neighbour(R)));
}

#[test]
fn s_ip_rte_014_with_no_good_gateway() {
    let mut h = two_gateways();
    fail(&mut h, R);
    fail(&mut h, ip4(192, 0, 2, 253));
    assert_eq!(route(&mut h, REMOTE).map(|r| r.next_hop), Ok(NextHop::Neighbour(R)));

    let mut h = two_gateways();
    let other = ip4(192, 0, 2, 253);
    fail(&mut h, R);
    h.stale(other, MAC_X);
    h.udp_to(REMOTE).unwrap();
    let t = h.now;
    h.run(t + 8_000);
    assert!(matches!(h.state(other), Some(Nud::Unreachable(_))));
    assert!(matches!(h.state(R), Some(Nud::Failed)));
    assert_eq!(route(&mut h, REMOTE).map(|r| r.next_hop), Ok(NextHop::Neighbour(other)));
}

#[test]
fn s_ip_rte_015_what_moves_the_generation() {
    let mut h = H::fixture_i_with(R, MAC_R);
    let mut last = h.ip.generation();
    let mut moved = |h: &H| {
        let now = h.ip.generation();
        let changed = now != last;
        last = now;
        changed
    };
    h.run(45_000);
    assert!(h.is_stale(R));
    assert!(!moved(&h), "REACHABLE to STALE");
    h.ip.add_address(h.clock(), h.if0, ip4(192, 0, 2, 9), 24).unwrap();
    assert!(!moved(&h), "a tentative address alters no lookup");
    h.run(45_200);
    assert!(moved(&h), "address added");
    h.ip.remove_address(h.clock(), h.if0, ip4(192, 0, 2, 9)).unwrap();
    assert!(moved(&h), "address removed");
    h.ip.set_gateways(h.clock(), h.if0, &[R, ip4(192, 0, 2, 253)]).unwrap();
    assert!(moved(&h), "gateway list changed");
    fail(&mut h, ip4(192, 0, 2, 50));
    assert!(!moved(&h), "a neighbour that is no gateway failing");
    h.udp_to(REMOTE).unwrap();
    let t = h.now;
    h.run(t + 7_999);
    assert!(!moved(&h), "STALE, DELAY and PROBE are usable");
    h.run(t + 8_000);
    assert!(matches!(h.state(R), Some(Nud::Unreachable(_))));
    assert!(moved(&h), "active gateway changed");
    h.ip.link_down(h.clock(), h.if0).unwrap();
    assert!(moved(&h), "link down");
    h.ip.link_up(h.clock(), h.if0).unwrap();
    assert!(moved(&h), "link up");
}

#[test]
fn s_ip_rte_016_no_forwarding() {
    let mut h = H::fixture_i();
    assert!(h.datagram(&udp(B, REMOTE, 5000, 5001, b"hi")).is_none());
    assert_eq!(h.count(Counter::IpNotForUs), 1);
    assert!(h.out().is_empty());
}

#[test]
fn s_ip_rte_017_lookups_send_nothing() {
    let mut h = H::fixture_i();
    for t in (0..=600_000).step_by(1_000) {
        h.at(t);
        assert!(route(&mut h, REMOTE).is_ok());
        assert!(h.out().is_empty());
    }
    assert!(h.run(600_000).is_empty());
}
