//! Interface addressing and the host routing table.

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

// US-21 at the route that is taken: a datagram whose socket did not hold the broadcast permission
// when it accepted it leaves in no link broadcast, to the limited address or to a held prefix's
// directed one, and is counted, logged against its destination and reported to its flow. No
// request is sent for either. The same datagram to a host leaves.
#[test]
fn s_udp_us_021_ip_no_link_broadcast_without_the_datagrams_permission() {
    let mut h = H::fixture_i();
    let rule = Counter::IpBroadcastNotPermitted;
    for (n, destination) in [(1, ip4(255, 255, 255, 255)), (2, ip4(192, 0, 2, 255))] {
        assert_eq!(h.send_as(A, destination, 5001, 5001, b"hi", Ttl::DEFAULT, false), Err(rule), "{destination}");
        assert_eq!(h.count(rule), n);
        let refusal = toyos_net_ip::Refusal { rule, iface: h.if0, peer: toyos_net_ip::Peer::Ip(destination) };
        let port = toyos_net_wire::Port::new(5001).unwrap();
        let flow = toyos_net_ip::Flow { source: A, source_port: port, destination, destination_port: port };
        assert_eq!(h.events.split_off(0), [toyos_net_ip::Event::Refused(refusal), toyos_net_ip::Event::Unreachable(flow)]);
    }
    assert!(h.out().is_empty(), "nothing left, and nothing was asked");
    assert_eq!(h.send_as(A, B, 5001, 5001, b"hi", Ttl::DEFAULT, false), Ok(None), "a host's datagram waits for its link address");
    assert_eq!(h.count(rule), 2);
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

const LINK_LOCAL: Ipv4Addr = Ipv4Addr::new(169, 254, 3, 4);
const MAC_L: MacAddr = MacAddr([2, 0, 0, 0, 0, 0x4c]);
/// The network and the broadcast address of 169.254/16.
const LINK_LOCAL_EDGES: [Ipv4Addr; 2] = [Ipv4Addr::new(169, 254, 0, 0), Ipv4Addr::new(169, 254, 255, 255)];

// RFC 3927 §2.6.2: "If the destination address is in the 169.254/16 prefix (excluding the address
// 169.254.255.255, which is the IPv4 Link-Local subnet broadcast address), then the sender MUST
// ARP for the destination address and then send the packet directly to the destination on the
// same physical link. This MUST be done whether the interface is configured with a Link-Local
// or a routable IPv4 address." And: "The host MUST NOT send a packet with an IPv4 Link-Local
// destination address to any router for forwarding." §2.7 says the same of every such packet.
#[test]
fn rfc_3927_2_6_2_a_link_local_destination_is_on_the_link() {
    let mut h = H::fixture_i();
    let on_the_link = Route { iface: h.if0, next_hop: NextHop::Neighbour(LINK_LOCAL), source: A };
    assert_eq!(route(&mut h, LINK_LOCAL), Ok(on_the_link), "with a routable address and a router");
    assert_eq!(route(&mut h, REMOTE).map(|r| r.next_hop), Ok(NextHop::Neighbour(R)), "the router is still the way off the link");
    h.ip.set_gateways(h.clock(), h.if0, &[]).unwrap();
    assert_eq!(route(&mut h, LINK_LOCAL), Ok(on_the_link), "and with no router at all");

    let mut h = H::bare();
    assert_eq!(route(&mut h, LINK_LOCAL), Err(Counter::RouteNoSourceAddress), "an interface with no address sends nothing");

    // The interface is the lowest one up, or the one a bound source names.
    let mut h = H::raw();
    let (if0, if1) = (h.if0, h.add_if1());
    h.assign(if0, A, 24, 0);
    h.assign(if1, ip4(198, 51, 100, 1), 24, 5_000);
    h.settle();
    assert_eq!(route(&mut h, LINK_LOCAL).map(|r| (r.iface, r.source)), Ok((if0, A)));
    let bound = h.ip.route(LINK_LOCAL, Source::Bound(ip4(198, 51, 100, 1)), None);
    assert_eq!(bound, Ok(Route { iface: if1, next_hop: NextHop::Neighbour(LINK_LOCAL), source: ip4(198, 51, 100, 1) }));
    h.ip.link_down(h.clock(), if0).unwrap();
    assert_eq!(route(&mut h, LINK_LOCAL).map(|r| r.iface), Ok(if1));
}

// The same rule on the wire: the request names the destination itself, its reply is a
// neighbour's, and no frame is the router's. RFC 3927 §2.5: a host sends every ARP packet whose
// sender is link-local, a reply too, to the link's broadcast address.
#[test]
fn rfc_3927_2_6_2_a_datagram_to_a_link_local_host_is_resolved_and_sent_to_it() {
    let mut h = H::fixture_i_with(R, MAC_R);
    assert_eq!(h.udp_to(LINK_LOCAL), Ok(None));
    let asked = h.out();
    assert_eq!(asked.len(), 1);
    let request = asked[0].arp().unwrap();
    assert_eq!((asked[0].to(), request.sender_ip, request.target_ip), (MacAddr::BROADCAST, A, LINK_LOCAL));
    h.at(5);
    h.frame(&eth(MacAddr::BROADCAST, MAC_L, 0x0806, &arp_packet(2, MAC_L, LINK_LOCAL, MAC_A, A)));
    assert!(h.is_reachable(LINK_LOCAL));
    assert_eq!(h.count(Counter::ArpSenderOffLink), 0);
    let out = h.out();
    assert_eq!(out.len(), 1);
    assert_eq!((out[0].to(), out[0].ip().unwrap().destination()), (MAC_L, LINK_LOCAL));
    assert!(matches!(h.udp_to(LINK_LOCAL), Ok(Some(frame)) if destination_of(&frame) == MAC_L));
}

// RFC 3927 §2.6.2 excludes 169.254.255.255, "the IPv4 Link-Local subnet broadcast address", from
// what is resolved, and 169.254.0.0 is that prefix's network address: neither is a host. On an
// interface whose own prefix is another they are nothing it may send to: no request asks for
// them, and by §2.7 no router is handed them, though the router is resolved and a frame to it
// could leave at once.
#[test]
fn rfc_3927_2_6_2_the_edges_of_the_link_local_prefix_are_no_destination() {
    let mut h = H::fixture_i_with(R, MAC_R);
    for edge in LINK_LOCAL_EDGES {
        assert_eq!(route(&mut h, edge), Err(Counter::RouteNone), "{edge}");
        assert_eq!(h.udp_to(edge), Err(Counter::RouteNone), "{edge}");
        assert!(h.state(edge).is_none(), "{edge}");
    }
    assert!(h.out().is_empty(), "no request, and no frame to the router");
    assert!(h.events.contains(&toyos_net_ip::Event::Unreachable(toyos_net_ip::Flow {
        source: A,
        source_port: toyos_net_wire::Port::new(5001).unwrap(),
        destination: LINK_LOCAL_EDGES[1],
        destination_port: toyos_net_wire::Port::new(5001).unwrap(),
    })));
}

// RFC 1122 §3.2.1.3: a datagram whose source is a broadcast address is silently discarded, and
// RFC 3927 §2.6.2 names 169.254.255.255 the link-local prefix's broadcast address for every
// host, whatever it holds; 169.254.0.0 is that prefix's network address, and no host's either.
#[test]
fn rfc_3927_the_edges_of_the_link_local_prefix_are_no_source() {
    for edge in LINK_LOCAL_EDGES {
        let mut h = H::fixture_i();
        assert_eq!(cast_of(&h.datagram(&udp(edge, A, 5000, 5001, b"hi"))), None, "{edge}");
        assert_eq!(h.count(Counter::IpInvalidSource), 1, "{edge}");
        assert!(h.out().is_empty(), "{edge}: and nothing answers it");
    }
    let mut h = H::fixture_i();
    assert_eq!(cast_of(&h.datagram(&udp(LINK_LOCAL, A, 5000, 5001, b"hi"))), Some(Cast::Unicast), "a host of the prefix is a source");
}

// RFC 2132 §3.5: the router option lists "routers on the client's subnet". A router is inside a
// prefix the interface holds: what RFC 3927 puts on the link is a neighbour, and no router.
#[test]
fn rfc_3927_a_link_local_address_is_no_gateway_of_a_routable_interface() {
    let mut h = H::fixture_i();
    assert_eq!(h.ip.set_gateways(h.clock(), h.if0, &[ip4(169, 254, 0, 1)]), Err(Counter::RouteGatewayOffLink));
    assert_eq!(h.ip.gateways(h.if0), Some(&[R][..]));
}

// Present state, and wrong: RFC 3927 §2.6.2 has a host that sends from a link-local source to a
// destination outside 169.254/16 "ARP for the destination address and then send the packet ...
// directly to its destination on the same physical link", and "MUST NOT send the packet to any
// router for forwarding". [ip] decides by the destination alone: with a gateway the datagram is
// the router's, and with none it is refused. The track records it; its exit turns both into the
// destination itself.
#[test]
fn rfc_3927_2_6_2_a_link_local_source_still_sends_by_the_router_or_nowhere() {
    let ours = ip4(169, 254, 7, 7);
    let router = ip4(169, 254, 0, 1);
    let mut h = H::raw();
    let if0 = h.if0;
    h.assign(if0, ours, 16, 0);
    h.settle();
    assert_eq!(route(&mut h, REMOTE), Err(Counter::RouteNone));
    h.ip.set_gateways(h.clock(), if0, &[router]).unwrap();
    assert_eq!(route(&mut h, REMOTE), Ok(Route { iface: if0, next_hop: NextHop::Neighbour(router), source: ours }));
}
