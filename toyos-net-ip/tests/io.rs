//! §4 and §5 with `wire.md` §3.3–§3.4 and §5.4–§5.5: frame dispatch, the IPv4 input pipeline,
//! and what [ip] puts on the wire.

mod common;

use std::net::Ipv4Addr;

use common::*;
use toyos_net_ip::{limits, Cast, Counter, Delivery, MTU};
use toyos_net_wire::ethernet::MacAddr;
use toyos_net_wire::ipv4::{Ecn, Ttl};

fn ip4(a: u8, b: u8, c: u8, d: u8) -> Ipv4Addr {
    Ipv4Addr::new(a, b, c, d)
}

fn hi_to(destination: Ipv4Addr) -> Vec<u8> {
    edit(hex(V_UDP_HI), |ip| set_destination(ip, destination))
}

fn hi_from(source: Ipv4Addr) -> Vec<u8> {
    edit(hex(V_UDP_HI), |ip| set_source(ip, source))
}

const MDNS_MAC: MacAddr = MacAddr([1, 0, 0x5e, 0, 0, 0xfb]);

#[test]
fn s_ip_in_001_frame_before_destination() {
    let mut h = H::fixture_i();
    assert!(h.datagram_to(MacAddr::BROADCAST, &hi_to(ip4(192, 0, 2, 9))).is_none());
    assert_eq!(h.count(Counter::IpUnicastInLinkBroadcast), 1);
    assert_eq!(h.count(Counter::IpNotForUs), 0);
}

#[test]
fn s_ip_in_002_unicast_in_a_group_frame() {
    let mut h = H::fixture_i();
    assert!(h.datagram_to(MDNS_MAC, &hex(V_UDP_HI)).is_none());
    assert_eq!(h.count(Counter::IpUnicastInLinkMulticast), 1);
}

#[test]
fn s_ip_in_003_a_group_destination_in_our_frame() {
    let mut h = H::fixture_i();
    h.frame(&eth(MAC_A, MAC_R, 0x0800, &hex(V_IGMP3_QUERY_GEN)));
    let refused: u64 = Counter::ALL.iter().filter(|c| c.name().starts_with("ip.") || c.name().starts_with("igmp.")).map(|&c| h.count(c)).sum();
    assert_eq!(refused, 0);
    assert!(h.run(10_000).iter().any(|o| ip_bytes(&o.frame) == hex(V_IGMP3_CURRENT)));
}

#[test]
fn s_ip_in_004_martian_destinations() {
    let mut h = H::fixture_i();
    for destination in [ip4(0, 1, 2, 3), ip4(240, 0, 0, 1)] {
        assert!(h.datagram(&hi_to(destination)).is_none());
    }
    assert_eq!(h.count(Counter::IpMartianDestination), 2);
}

#[test]
fn s_ip_in_005_invalid_sources() {
    let mut h = H::fixture_i();
    for source in [ip4(0, 1, 2, 3), ip4(192, 0, 2, 0)] {
        assert!(h.datagram(&hi_from(source)).is_none());
    }
    assert_eq!(h.count(Counter::IpInvalidSource), 2);
    let mut h = H::raw();
    let if0 = h.if0;
    h.assign(if0, A, 31, 0);
    h.settle();
    assert_eq!(cast_of(&h.datagram(&hi_from(ip4(192, 0, 2, 0)))), Some(Cast::Unicast));
}

#[test]
fn s_ip_in_006_destination_before_source() {
    let mut h = H::fixture_i();
    let ip = edit(hex(V_UDP_HI), |ip| {
        set_destination(ip, ip4(192, 0, 2, 9));
        set_source(ip, ip4(224, 0, 0, 5));
    });
    assert!(h.datagram(&ip).is_none());
    assert_eq!(h.count(Counter::IpNotForUs), 1);
    assert_eq!(h.count(Counter::IpInvalidSource), 0);
}

#[test]
fn s_ip_in_007_our_own_source_on_any_interface() {
    let mut h = H::raw();
    let (if0, if1) = (h.if0, h.add_if1());
    h.assign(if0, A, 24, 0);
    h.assign(if1, ip4(198, 51, 100, 1), 24, 5_000);
    h.settle();
    assert!(h.datagram(&hi_from(ip4(198, 51, 100, 1))).is_none());
    assert_eq!(h.count(Counter::IpOwnSource), 1);
}

#[test]
fn s_ip_in_008_fragments_before_options() {
    let mut h = H::fixture_i();
    let ip = edit(hex(V_IP_LSRR), |ip| ip[6] |= 0x20);
    assert!(h.datagram(&ip).is_none());
    assert_eq!(h.count(Counter::IpFragment), 1);
    assert_eq!(h.count(Counter::IpSourceRoute), 0);
}

#[test]
fn s_ip_in_009_the_delivery_carries_the_tos() {
    let mut h = H::fixture_i();
    let Some(Delivery::Udp(arrival, datagram)) = h.datagram(&hex(V_IP_DSCP)) else { panic!("not delivered") };
    assert_eq!(arrival.packet.traffic_class().dscp(), 46);
    assert_eq!(arrival.packet.traffic_class().ecn(), Ecn::Ce);
    assert_eq!(arrival.cast, Cast::Unicast);
    assert_eq!(arrival.iface, h.if0);
    assert_eq!(datagram.destination_port().get(), 5001);
}

#[test]
fn s_ipp_001_loose_source_route() {
    let mut h = H::fixture_i();
    assert!(h.datagram(&hex(V_IP_LSRR)).is_none());
    assert_eq!(h.count(Counter::IpSourceRoute), 1);
    assert_eq!(h.refusals(Counter::IpSourceRoute)[0].peer, toyos_net_ip::Peer::Ip(B));
}

#[test]
fn s_ipp_002_strict_source_route() {
    let mut h = H::fixture_i();
    assert!(h.datagram(&edit(hex(V_IP_LSRR), |ip| ip[20] = 0x89)).is_none());
    assert_eq!(h.count(Counter::IpSourceRoute), 1);
}

#[test]
fn s_ipp_003_record_route_is_ignored() {
    let mut h = H::fixture_i();
    assert!(matches!(h.datagram(&hex(V_IP_RR)), Some(Delivery::Udp(..))));
}

#[test]
fn s_ipp_004_our_address_and_another() {
    let mut h = H::fixture_i();
    assert_eq!(cast_of(&h.datagram(&hi_to(A))), Some(Cast::Unicast));
    assert!(h.datagram(&hi_to(ip4(192, 0, 2, 7))).is_none());
    assert_eq!(h.count(Counter::IpNotForUs), 1);
}

#[test]
fn s_ipp_005_broadcast_forms() {
    let mut h = H::fixture_i();
    assert_eq!(cast_of(&h.datagram_to(MacAddr::BROADCAST, &hi_to(ip4(255, 255, 255, 255)))), Some(Cast::LimitedBroadcast));
    assert_eq!(cast_of(&h.datagram_to(MacAddr::BROADCAST, &hi_to(ip4(192, 0, 2, 255)))), Some(Cast::SubnetBroadcast));
    assert!(h.datagram(&hi_to(ip4(192, 0, 2, 0))).is_none());
    assert_eq!(h.count(Counter::IpNotForUs), 1);
}

#[test]
fn s_ipp_006_joined_groups() {
    let mut h = H::fixture_i();
    assert!(h.datagram_to(MDNS_MAC, &hi_to(MDNS)).is_some());
    h.ip.leave(h.clock(), h.if0, group(MDNS)).unwrap();
    assert!(h.datagram(&hi_to(MDNS)).is_none());
    assert_eq!(h.count(Counter::IpNotForUs), 1);
    assert!(h.datagram_to(MacAddr([1, 0, 0x5e, 0, 0, 1]), &hi_to(ip4(224, 0, 0, 1))).is_some());
}

#[test]
fn s_ipp_007_invalid_sources() {
    let mut h = H::fixture_i();
    for source in [ip4(224, 0, 0, 5), ip4(255, 255, 255, 255), ip4(240, 0, 0, 1), ip4(127, 0, 0, 1), ip4(192, 0, 2, 255)] {
        assert!(h.datagram(&hi_from(source)).is_none(), "{source}");
    }
    assert_eq!(h.count(Counter::IpInvalidSource), 5);
}

#[test]
fn s_ipp_008_unspecified_only_for_igmp() {
    let mut h = H::fixture_i();
    assert!(h.datagram(&hi_from(ip4(0, 0, 0, 0))).is_none());
    assert_eq!(h.count(Counter::IpInvalidSource), 1);
    let report = edit(ip_bytes(&hex(V_IGMP_REPORT)), |ip| set_source(ip, ip4(0, 0, 0, 0)));
    h.datagram_to(MDNS_MAC, &report);
    assert_eq!(h.count(Counter::IpInvalidSource), 1);
    assert_eq!(h.count(Counter::IgmpIgnored), 1, "accepted, then ignored as a report outside v2 mode");
}

#[test]
fn s_ipp_009_our_own_source() {
    let mut h = H::fixture_i();
    assert!(h.datagram(&hi_from(A)).is_none());
    assert_eq!(h.count(Counter::IpOwnSource), 1);
}

#[test]
fn s_ipp_010_loopback_destination() {
    let mut h = H::fixture_i();
    assert!(h.datagram(&hi_to(ip4(127, 0, 0, 1))).is_none());
    assert_eq!(h.count(Counter::IpMartianDestination), 1);
}

#[test]
fn s_ipp_011_unicast_in_a_broadcast_frame() {
    let mut h = H::fixture_i();
    assert!(h.datagram_to(MacAddr::BROADCAST, &hex(V_UDP_HI)).is_none());
    assert_eq!(h.count(Counter::IpUnicastInLinkBroadcast), 1);
}

#[test]
fn s_ipp_012_an_unknown_protocol_to_us() {
    let mut h = H::fixture_i_with(B, MAC_B);
    assert!(h.datagram(&hex(V_IP_MIN)).is_none());
    assert_eq!(h.count(Counter::IpProtocol), 1);
    let out = h.out();
    assert_eq!(out.len(), 1);
    assert_eq!(ip_bytes(&out[0].frame), hex(V_ICMP_PROTO_UNREACH_GEN));
}

#[test]
fn s_ipp_013_an_unknown_protocol_to_all() {
    let mut h = H::fixture_i_with(B, MAC_B);
    let ip = edit(hex(V_IP_MIN), |ip| set_destination(ip, ip4(255, 255, 255, 255)));
    assert!(h.datagram_to(MacAddr::BROADCAST, &ip).is_none());
    assert_eq!(h.count(Counter::IpProtocol), 1);
    assert!(h.out().is_empty());
}

#[test]
fn s_ip_002_minimal_datagram_is_an_unknown_protocol() {
    let mut h = H::fixture_i();
    h.datagram(&hex(V_IP_MIN));
    assert_eq!(h.count(Counter::IpProtocol), 1);
}

#[test]
fn s_ip_020_df_and_mf_is_a_fragment() {
    let mut h = H::fixture_i();
    assert!(h.datagram(&edit(hex(V_UDP_HI), |ip| ip[6] = 0x60)).is_none());
    assert_eq!(h.count(Counter::IpFragment), 1);
}

#[test]
fn s_ip_021_first_fragment() {
    let mut h = H::fixture_i();
    assert!(h.datagram(&hex(V_IP_FRAG_FIRST)).is_none());
    assert_eq!(h.count(Counter::IpFragment), 1);
}

#[test]
fn s_ip_022_last_fragment() {
    let mut h = H::fixture_i();
    assert!(h.datagram(&hex(V_IP_FRAG_LAST)).is_none());
    assert_eq!(h.count(Counter::IpFragment), 1);
}

#[test]
fn s_ip_023_largest_offset() {
    let mut h = H::fixture_i();
    assert!(h.datagram(&edit(hex(V_UDP_HI), |ip| {
        ip[6] = 0x1f;
        ip[7] = 0xff;
    }))
    .is_none());
    assert_eq!(h.count(Counter::IpFragment), 1);
}

#[test]
fn s_ip_025_ttl_zero_is_delivered() {
    let mut h = H::fixture_i();
    assert!(matches!(h.datagram(&edit(hex(V_UDP_HI), |ip| ip[8] = 0)), Some(Delivery::Udp(..))));
}

#[test]
fn s_ip_026_ttl_one_is_delivered() {
    let mut h = H::fixture_i();
    assert!(matches!(h.datagram(&edit(hex(V_UDP_HI), |ip| ip[8] = 1)), Some(Delivery::Udp(..))));
}

#[test]
fn s_ip_029_other_protocols() {
    let mut h = H::fixture_i();
    for protocol in [0u8, 41, 50, 132, 253, 255] {
        h.datagram(&ipv4(B, A, protocol, &[]));
    }
    assert_eq!(h.count(Counter::IpProtocol), 6);
}

#[test]
fn s_ip_035_the_mtu_bounds_a_send() {
    let mut h = H::fixture_i_with(B, MAC_B);
    assert!(h.send(A, B, 5001, 5001, &[0; 1_472]).unwrap().is_some());
    assert_eq!(h.send(A, B, 5001, 5001, &[0; 1_473]), Err(Counter::IpExceedsMtu));
}

#[test]
fn s_ipo_015_stream_id_is_delivered() {
    let mut h = H::fixture_i();
    let ip = edit(hex(V_IP_NOPS), |ip| ip[20..24].copy_from_slice(&[0x88, 4, 0, 1]));
    assert!(matches!(h.datagram(&ip), Some(Delivery::Udp(..))));
}

#[test]
fn s_ipo_016_security_option_is_delivered() {
    let mut h = H::fixture_i();
    let mut udp_part = hex(V_UDP_HI)[20..].to_vec();
    let mut ip = vec![0x48, 0, 0, 0, 0, 0, 0x40, 0, 64, 17, 0, 0, 192, 0, 2, 2, 192, 0, 2, 1];
    ip.extend_from_slice(&[130, 11, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    ip.append(&mut udp_part);
    let total = ip.len() as u16;
    ip[2..4].copy_from_slice(&total.to_be_bytes());
    fix(&mut ip);
    assert!(matches!(h.datagram(&ip), Some(Delivery::Udp(..))));
}

#[test]
fn s_udp_022_the_mtu_refuses_1473_bytes() {
    let mut h = H::fixture_i_with(B, MAC_B);
    assert_eq!(h.send(A, B, 5001, 5001, &[0; 1_473]), Err(Counter::IpExceedsMtu));
    assert_eq!(h.count(Counter::IpExceedsMtu), 1);
    assert!(h.send(A, B, 5001, 5001, &[0; 1_472]).unwrap().is_some());
}

fn arp_request_for_b(ether_type_field: &[u8]) -> Vec<u8> {
    let mut f = MacAddr::BROADCAST.0.to_vec();
    f.extend_from_slice(&MAC_B.0);
    f.extend_from_slice(ether_type_field);
    f.extend_from_slice(&arp_packet(1, MAC_B, B, MacAddr::ZERO, A));
    f
}

#[test]
fn s_eth_010_type_0600_is_unknown() {
    let mut h = H::fixture_i();
    h.frame(&arp_request_for_b(&[0x06, 0x00]));
    assert_eq!(h.count(Counter::EthUnknownType), 1);
}

#[test]
fn s_eth_011_ipv6() {
    let mut h = H::fixture_i();
    h.frame(&arp_request_for_b(&[0x86, 0xdd]));
    assert_eq!(h.count(Counter::EthIpv6), 1);
}

#[test]
fn s_eth_012_lldp() {
    let mut h = H::fixture_i();
    h.frame(&arp_request_for_b(&[0x88, 0xcc]));
    assert_eq!(h.count(Counter::EthUnknownType), 1);
}

#[test]
fn s_eth_014_a_priority_tag_is_untagged() {
    let mut h = H::as_b();
    h.frame(&hex(V_ETH_PRIO));
    let out = h.out();
    assert_eq!(out.len(), 1, "the ARP inside is processed");
    assert_eq!(out[0].arp().unwrap().target_ip, A);
}

#[test]
fn s_eth_019_9100_is_not_a_tag() {
    let mut h = H::fixture_i();
    h.frame(&arp_request_for_b(&[0x91, 0x00]));
    assert_eq!(h.count(Counter::EthUnknownType), 1);
}

#[test]
fn s_eth_022_another_hosts_mac() {
    let mut h = H::fixture_i();
    h.frame(&eth(MacAddr([2, 0, 0, 0, 0, 0x99]), MAC_B, 0x0800, &hex(V_UDP_HI)));
    assert_eq!(h.count(Counter::EthNotForUs), 1);
}

#[test]
fn s_eth_023_group_macs_follow_membership() {
    let mut h = H::fixture_i();
    assert!(h.datagram_to(MDNS_MAC, &hi_to(MDNS)).is_some());
    h.ip.leave(h.clock(), h.if0, group(MDNS)).unwrap();
    assert!(h.datagram_to(MDNS_MAC, &hi_to(MDNS)).is_none());
    assert_eq!(h.count(Counter::EthNotForUs), 1);
    assert!(h.datagram_to(MacAddr([1, 0, 0x5e, 0, 0, 1]), &hi_to(ip4(224, 0, 0, 1))).is_some());
}

#[test]
fn s_eth_024_vlans_are_refused() {
    let mut h = H::as_b();
    h.frame(&hex(V_ETH_8021Q));
    h.frame(&hex(V_ETH_QINQ));
    assert_eq!(h.count(Counter::EthVlan), 2);
    assert!(h.out().is_empty());
}

#[test]
fn s_eth_025_our_own_source() {
    let mut h = H::fixture_i();
    h.frame(&eth(MAC_A, MAC_A, 0x0800, &hex(V_UDP_HI)));
    assert_eq!(h.count(Counter::EthOwnSource), 1);
}

#[test]
fn s_eth_032_broadcast_destinations_go_to_the_broadcast_mac() {
    let mut h = H::fixture_i();
    for destination in [ip4(255, 255, 255, 255), ip4(192, 0, 2, 255)] {
        let frame = h.udp_to(destination).unwrap().unwrap();
        assert_eq!(destination_of(&frame), MacAddr::BROADCAST);
    }
}

/// Every IPv4 frame [ip] emits, for datagrams to a spread of unicast destinations.
fn unicast_frames() -> Vec<Vec<u8>> {
    let mut h = H::fixture_i_with(B, MAC_B);
    let mut frames = Vec::new();
    for destination in [B, DNS, REMOTE, ip4(192, 0, 2, 77)] {
        frames.extend(h.udp_to(destination).unwrap());
    }
    h.frame(&eth(MAC_A, MAC_DNS, 0x0806, &arp_packet(2, MAC_DNS, DNS, MAC_A, A)));
    h.reply_from(R, MAC_R);
    frames.extend(h.run(5_000).into_iter().map(|o| o.frame));
    frames
}

#[test]
fn s_eth_033_a_unicast_destination_is_never_link_broadcast() {
    let frames = unicast_frames();
    assert!(frames.iter().filter(|f| ip_of(f).is_some()).count() >= 3);
    for frame in frames.iter().filter(|f| ip_of(f).is_some()) {
        assert_ne!(destination_of(frame), MacAddr::BROADCAST);
    }
}

#[test]
fn s_ip_out_001_prop_every_datagram_is_atomic() {
    let mut h = H::fixture_i_with(B, MAC_B);
    let mut rng = Rng(0x0dd5);
    let mut frames = Vec::new();
    for step in 0..300u64 {
        h.at(step * 37);
        match rng.below(5) {
            0 => frames.extend(h.udp_to([B, MDNS, ip4(255, 255, 255, 255), REMOTE][rng.below(4) as usize]).ok().flatten()),
            1 => {
                h.datagram(&edit(hex(V_ICMP_ECHO), |ip| {
                    set_source(ip, B);
                    set_destination(ip, A);
                }));
            }
            2 => {
                if let Some(Delivery::Udp(arrival, _)) = h.datagram(&hex(V_UDP_HI)) {
                    h.ip.port_unreachable(h.clock(), &arrival);
                }
            }
            3 => {
                let g = group(ip4(239, 0, 0, rng.below(8) as u8 + 1));
                let _ = if rng.below(2) == 0 { h.ip.join(h.clock(), h.if0, g) } else { h.ip.leave(h.clock(), h.if0, g) };
            }
            _ => {
                h.datagram(&hex(V_IP_MIN));
            }
        }
        frames.extend(h.run(step * 37 + 36).into_iter().map(|o| o.frame));
    }
    let datagrams: Vec<_> = frames.iter().filter_map(|f| ip_of(f)).collect();
    assert!(datagrams.len() > 200);
    for ip in datagrams {
        assert!(ip.dont_fragment() && !ip.more_fragments());
        assert_eq!((ip.fragment_offset().units(), ip.identification()), (0, 0));
    }
}

#[test]
fn s_ip_out_002_ttl() {
    let mut h = H::fixture_i_with(B, MAC_B);
    assert_eq!(ip_of(&h.udp_to(B).unwrap().unwrap()).unwrap().ttl(), 64);
    let multicast = h.send_ttl(A, MDNS, 5353, 5353, b"q", Ttl::new(1).unwrap()).unwrap().unwrap();
    assert_eq!(ip_of(&multicast).unwrap().ttl(), 1);
    h.ip.join(h.clock(), h.if0, group(ip4(239, 1, 2, 3))).unwrap();
    let report = h.out();
    assert_eq!(report[0].ip().unwrap().ttl(), 1);
    assert_eq!(Ttl::new(0), Err(toyos_net_wire::BuildError::IpTtlZero));
}

#[test]
fn s_ip_out_003_tos() {
    let mut h = H::fixture_i_with(B, MAC_B);
    assert_eq!(ip_of(&h.udp_to(B).unwrap().unwrap()).unwrap().traffic_class().byte(), 0);
    h.ip.join(h.clock(), h.if0, group(ip4(239, 1, 2, 3))).unwrap();
    assert_eq!(h.out()[0].ip().unwrap().traffic_class().byte(), 0xc0);
    let Some(Delivery::Udp(arrival, _)) = h.datagram(&hex(V_IP_DSCP)) else { panic!() };
    h.ip.port_unreachable(h.clock(), &arrival);
    assert_eq!(h.out()[0].ip().unwrap().traffic_class().byte(), 0);
}

#[test]
fn s_ip_out_004_the_mtu() {
    let mut h = H::fixture_i_with(B, MAC_B);
    assert_eq!(MTU, 1_500);
    assert_eq!(h.send(A, B, 5001, 5001, &[0; 1_473]), Err(Counter::IpExceedsMtu));
    let frame = h.send(A, B, 5001, 5001, &[0; 1_472]).unwrap().unwrap();
    assert_eq!(ip_of(&frame).unwrap().total_length(), 1_500);
}

#[test]
fn s_ip_out_005_link_destinations() {
    let mut h = H::fixture_i_with(B, MAC_B);
    assert_eq!(destination_of(&h.udp_to(ip4(192, 0, 2, 255)).unwrap().unwrap()), MacAddr::BROADCAST);
    assert_eq!(destination_of(&h.udp_to(MDNS).unwrap().unwrap()), MDNS_MAC);
    assert_eq!(destination_of(&h.udp_to(B).unwrap().unwrap()), MAC_B);
    for frame in unicast_frames().iter().filter(|f| ip_of(f).is_some_and(|ip| !ip.destination().is_broadcast() && !ip.destination().is_multicast())) {
        assert_ne!(destination_of(frame), MacAddr::BROADCAST);
    }
}

#[test]
fn s_ip_out_006_the_control_queue_is_bounded() {
    let mut h = H::fixture_i();
    for n in 0..=limits::CONTROL_QUEUE as u8 {
        let _ = h.ip.resolve(h.clock(), h.if0, ip4(192, 0, 2, 100 + n));
    }
    assert_eq!(h.count(Counter::IpControlQueueFull), 1);
    assert_eq!(h.out().len(), limits::CONTROL_QUEUE);
}
