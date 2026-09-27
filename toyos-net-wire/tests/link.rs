//! Ethernet's and ARP's [wire] scenarios: ETH and ARP.

mod common;

use std::net::Ipv4Addr;

use common::*;
use toyos_net_wire::arp::{Arp, ArpError, Kind, Operation};
use toyos_net_wire::ethernet::{
    EthError, EtherType, Frame, FrameBuilder, GroupDestination, IndividualMac, MacAddr, MacClass, TagProtocol, Tags,
};
use toyos_net_wire::ipv4::{Form, Ipv4Builder, Ipv4Packet, Ipv4Source, MulticastAddr, Protocol, RawPayload, TrafficClass, Ttl};
use toyos_net_wire::{BuildError, Class};

fn frame(text: &str) -> Vec<u8> {
    hex(text)
}

fn arp_request() -> Arp {
    Arp::request(mac_a(), IP_A, IP_B)
}

/// A frame with `field` as its type field and 46 zero bytes of body.
fn typed(field: u16) -> Vec<u8> {
    let mut bytes = hex(V_ARP_REQ)[..12].to_vec();
    bytes.extend_from_slice(&field.to_be_bytes());
    bytes.extend_from_slice(&[0; 46]);
    bytes
}

#[test]
fn s_eth_001_arp_request_frame() {
    let bytes = frame(V_ARP_REQ);
    let frame = Frame::parse(&bytes).unwrap();
    assert_eq!(frame.destination(), MacAddr::BROADCAST);
    assert_eq!(frame.source(), mac_a());
    assert_eq!(frame.tags(), Tags::Untagged);
    assert_eq!(frame.ether_type(), EtherType::Arp);
    assert_eq!(frame.body().len(), 46);
}

#[test]
fn s_eth_002_udp_frame_without_padding() {
    let bytes = frame(V_UDP_DNS);
    let frame = Frame::parse(&bytes).unwrap();
    assert_eq!(frame.destination(), MAC_ROUTER);
    assert_eq!(frame.destination().class(), MacClass::Individual);
    assert_eq!(frame.ether_type(), EtherType::Ipv4);
    assert_eq!(frame.body().len(), 57);
}

#[test]
fn s_eth_003_thirteen_bytes_truncated() {
    assert_eq!(Frame::parse(&frame(V_ARP_REQ)[..13]), Err(EthError::Truncated));
}

#[test]
fn s_eth_004_header_alone_has_an_empty_body() {
    let bytes = &frame(V_UDP_DNS)[..14];
    let frame = Frame::parse(bytes).unwrap();
    assert!(frame.body().is_empty());
    assert_eq!(Ipv4Packet::parse(frame.body()).unwrap_err().name(), "ip.truncated");
}

#[test]
fn s_eth_005_short_frame_accepted() {
    let bytes = &frame(V_ARP_REQ)[..42];
    let frame = Frame::parse(bytes).unwrap();
    assert_eq!(Arp::parse(frame.body()).unwrap(), arp_request());
}

#[test]
fn s_eth_006_length_1500_is_a_length_frame() {
    assert_eq!(Frame::parse(&typed(0x05DC)), Err(EthError::LengthFrame));
    assert_eq!(EthError::LengthFrame.class(), Class::Unsupported);
}

#[test]
fn s_eth_007_length_0_is_a_length_frame() {
    assert_eq!(Frame::parse(&typed(0x0000)), Err(EthError::LengthFrame));
}

#[test]
fn s_eth_008_type_1501_is_undefined() {
    assert_eq!(Frame::parse(&typed(0x05DD)), Err(EthError::UndefinedTypeField));
    assert_eq!(EthError::UndefinedTypeField.class(), Class::Malformed);
}

#[test]
fn s_eth_009_type_1535_is_undefined() {
    assert_eq!(Frame::parse(&typed(0x05FF)), Err(EthError::UndefinedTypeField));
}

#[test]
fn s_eth_010_type_0600_is_another_ethertype() {
    let bytes = typed(0x0600);
    let ether_type = Frame::parse(&bytes).unwrap().ether_type();
    assert!(matches!(ether_type, EtherType::Other(other) if other.value() == 0x0600));
}

#[test]
fn s_eth_013_customer_tag() {
    let bytes = frame(V_ETH_8021Q);
    let frame = Frame::parse(&bytes).unwrap();
    let Tags::Single(tag) = frame.tags() else { panic!("{:?}", frame.tags()) };
    assert_eq!(tag.protocol(), TagProtocol::Customer);
    assert_eq!((tag.priority(), tag.drop_eligible(), tag.vlan_id()), (0, false, 100));
    assert_eq!(frame.ether_type(), EtherType::Arp);
    assert_eq!(frame.body().len(), 46);
}

#[test]
fn s_eth_014_priority_tag() {
    let bytes = frame(V_ETH_PRIO);
    let frame = Frame::parse(&bytes).unwrap();
    let Tags::Single(tag) = frame.tags() else { panic!("{:?}", frame.tags()) };
    assert_eq!((tag.priority(), tag.vlan_id()), (5, 0));
    assert_eq!(Arp::parse(frame.body()).unwrap(), arp_request());
}

#[test]
fn s_eth_015_service_then_customer_tag() {
    let bytes = frame(V_ETH_QINQ);
    let frame = Frame::parse(&bytes).unwrap();
    let Tags::Double { outer, inner } = frame.tags() else { panic!("{:?}", frame.tags()) };
    assert_eq!((outer.protocol(), outer.vlan_id()), (TagProtocol::Service, 10));
    assert_eq!((inner.protocol(), inner.vlan_id()), (TagProtocol::Customer, 20));
    assert_eq!(frame.ether_type(), EtherType::Arp);
}

#[test]
fn s_eth_016_three_tags() {
    let mut bytes = frame(V_ARP_REQ)[..12].to_vec();
    bytes.extend_from_slice(&hex("88 a8 00 0a 81 00 00 14 81 00 00 1e 08 06"));
    bytes.extend_from_slice(&[0; 46]);
    assert_eq!(Frame::parse(&bytes), Err(EthError::TooManyTags));
}

#[test]
fn s_eth_017_truncated_tag() {
    let head = &frame(V_ARP_REQ)[..12];
    for tail in ["81 00 00 64", "81 00 00"] {
        let mut bytes = head.to_vec();
        bytes.extend_from_slice(&hex(tail));
        assert_eq!(Frame::parse(&bytes), Err(EthError::TruncatedTag), "{tail}");
    }
}

#[test]
fn s_eth_018_inner_length_field() {
    let mut bytes = frame(V_ARP_REQ)[..12].to_vec();
    bytes.extend_from_slice(&hex("81 00 00 64 00 40"));
    bytes.extend_from_slice(&[0; 46]);
    assert_eq!(Frame::parse(&bytes), Err(EthError::LengthFrame));
}

#[test]
fn s_eth_019_9100_is_not_a_tag() {
    let bytes = typed(0x9100);
    let frame = Frame::parse(&bytes).unwrap();
    assert_eq!(frame.tags(), Tags::Untagged);
    assert_eq!(frame.ether_type().value(), 0x9100);
    assert!(matches!(frame.ether_type(), EtherType::Other(_)));
}

#[test]
fn s_eth_020_group_source() {
    for source in ["01 00 5e 00 00 01", "ff ff ff ff ff ff"] {
        let mut bytes = frame(V_ARP_REQ);
        bytes[6..12].copy_from_slice(&hex(source));
        assert_eq!(Frame::parse(&bytes), Err(EthError::GroupSource), "{source}");
    }
}

#[test]
fn s_eth_021_destination_classes() {
    let class = |text: &str| MacAddr(hex(text).try_into().unwrap()).class();
    assert_eq!(class("01 00 5e 00 00 fb"), MacClass::Group);
    assert_eq!(class("33 33 00 00 00 01"), MacClass::Group);
    assert_eq!(class("ff ff ff ff ff ff"), MacClass::Broadcast);
    assert_eq!(IndividualMac::new(MacAddr::BROADCAST), None, "broadcast is also a group address");
    assert_eq!(class("02 00 00 00 00 0a"), MacClass::Individual);
}

#[test]
fn s_eth_026_emit_arp_request() {
    let mut out = vec![0; 1514];
    let request = arp_request();
    let builder = FrameBuilder { destination: request.frame_destination(), source: mac_a() };
    let built = builder.emit(&request, &mut out).unwrap();
    assert_eq!(built.len(), 60);
    assert_eq!(built, frame(V_ARP_REQ));
    assert!(built[42..].iter().all(|&b| b == 0));
}

#[test]
fn s_eth_027_emit_over_junk() {
    let mut out = junk(1514);
    let request = arp_request();
    let builder = FrameBuilder { destination: MacAddr::BROADCAST, source: mac_a() };
    assert_eq!(builder.emit(&request, &mut out).unwrap(), frame(V_ARP_REQ));
}

fn raw_datagram(payload: &[u8]) -> Ipv4Builder<'static, RawPayload<'_>> {
    Ipv4Builder {
        source: Ipv4Source::new(IP_B).unwrap(),
        destination: IP_A,
        ttl: Ttl::DEFAULT,
        traffic_class: TrafficClass::ZERO,
        form: Form::Atomic,
        options: &[],
        payload: RawPayload { protocol: Protocol::from_number(253), bytes: payload },
    }
}

#[test]
fn s_eth_028_minimum_body_padding() {
    let builder = FrameBuilder { destination: MAC_A, source: mac_b() };
    let mut out = junk(1514);
    assert_eq!(builder.emit(&raw_datagram(&[0x11; 26]), &mut out).unwrap().len(), 60);
    assert_eq!(builder.emit(&raw_datagram(&[0x11; 27]), &mut out).unwrap().len(), 61);
}

#[test]
fn s_eth_029_maximum_body() {
    let builder = FrameBuilder { destination: MAC_A, source: mac_b() };
    let mut out = junk(2000);
    assert_eq!(builder.emit(&raw_datagram(&[0; 1480]), &mut out).unwrap().len(), 1514);
    assert_eq!(builder.emit(&raw_datagram(&[0; 1481]), &mut out), Err(BuildError::EthBodyTooLong));
    assert_eq!(BuildError::EthBodyTooLong.name(), "eth.body-too-long");
}

#[test]
fn s_eth_031_multicast_mac_mapping() {
    let mac = |a, b, c, d| MacAddr::multicast(MulticastAddr::new(Ipv4Addr::new(a, b, c, d)).unwrap());
    assert_eq!(mac(224, 0, 0, 251), MacAddr([0x01, 0x00, 0x5e, 0x00, 0x00, 0xfb]));
    assert_eq!(mac(239, 128, 0, 251), MacAddr([0x01, 0x00, 0x5e, 0x00, 0x00, 0xfb]));
    assert_eq!(mac(224, 0, 0, 1), MacAddr([0x01, 0x00, 0x5e, 0x00, 0x00, 0x01]));
    assert_eq!(mac(224, 128, 0, 1), MacAddr([0x01, 0x00, 0x5e, 0x00, 0x00, 0x01]));
    assert_eq!(mac(239, 255, 255, 250), MacAddr([0x01, 0x00, 0x5e, 0x7f, 0xff, 0xfa]));
    assert_eq!(MulticastAddr::new(IP_A), None);
}

#[test]
fn s_eth_032_broadcast_destinations_map_to_all_ones() {
    // 255.255.255.255 and 192.0.2.255 on 192.0.2.0/24 are both broadcast.
    assert_eq!(GroupDestination::Broadcast.mac(), MacAddr::BROADCAST);
    assert_eq!(MacAddr::BROADCAST, MacAddr([0xff; 6]));
}

fn arp_body(text: &str) -> Vec<u8> {
    frame(text)[14..].to_vec()
}

#[test]
fn s_arp_001_request() {
    let arp = Arp::parse(&arp_body(V_ARP_REQ)).unwrap();
    assert_eq!(arp.operation, Operation::Request);
    assert_eq!((arp.sender_mac, arp.sender_ip), (MAC_A, IP_A));
    assert_eq!((arp.target_mac, arp.target_ip), (MacAddr::ZERO, IP_B));
    assert_eq!(arp.kind(), Kind::Ordinary);
}

#[test]
fn s_arp_002_reply() {
    let arp = Arp::parse(&arp_body(V_ARP_REPLY)).unwrap();
    assert_eq!(arp.operation, Operation::Reply);
    assert_eq!((arp.sender_mac, arp.sender_ip), (MAC_B, IP_B));
    assert_eq!((arp.target_mac, arp.target_ip), (MAC_A, IP_A));
}

#[test]
fn s_arp_003_probe() {
    let arp = Arp::parse(&arp_body(V_ARP_PROBE)).unwrap();
    assert_eq!(arp.operation, Operation::Request);
    assert_eq!((arp.sender_ip, arp.target_ip), (Ipv4Addr::UNSPECIFIED, IP_A));
    assert_eq!(arp.kind(), Kind::Probe);
}

#[test]
fn s_arp_004_announcement() {
    let arp = Arp::parse(&arp_body(V_ARP_ANNOUNCE)).unwrap();
    assert_eq!((arp.sender_ip, arp.target_ip), (IP_A, IP_A));
    assert_eq!(arp.kind(), Kind::Announcement);
}

#[test]
fn s_arp_005_twenty_seven_bytes() {
    assert_eq!(Arp::parse(&arp_body(V_ARP_REQ)[..27]), Err(ArpError::Truncated));
}

#[test]
fn s_arp_006_seven_bytes() {
    assert_eq!(Arp::parse(&arp_body(V_ARP_REQ)[..7]), Err(ArpError::Truncated));
}

fn arp_with(offset: usize, bytes: &[u8]) -> Result<Arp, ArpError> {
    let mut arp = arp_body(V_ARP_REQ);
    arp[offset..offset + bytes.len()].copy_from_slice(bytes);
    Arp::parse(&arp)
}

#[test]
fn s_arp_007_ieee802_hardware() {
    assert_eq!(arp_with(0, &[0, 6]), Err(ArpError::HardwareType));
    assert_eq!(ArpError::HardwareType.class(), Class::Unsupported);
}

#[test]
fn s_arp_008_hardware_zero() {
    assert_eq!(arp_with(0, &[0, 0]), Err(ArpError::HardwareType));
}

#[test]
fn s_arp_009_protocol_ipv6() {
    assert_eq!(arp_with(2, &[0x86, 0xdd]), Err(ArpError::ProtocolType));
}

#[test]
fn s_arp_010_hardware_length_7() {
    assert_eq!(arp_with(4, &[7]), Err(ArpError::AddressLength));
}

#[test]
fn s_arp_011_protocol_length_16() {
    assert_eq!(arp_with(5, &[16]), Err(ArpError::AddressLength));
}

#[test]
fn s_arp_012_types_before_lengths() {
    let mut arp = arp_body(V_ARP_REQ);
    arp[1] = 6;
    arp[4] = 7;
    assert_eq!(Arp::parse(&arp), Err(ArpError::HardwareType));
}

#[test]
fn s_arp_013_other_operations() {
    for operation in [0u8, 3, 4, 8, 9] {
        assert_eq!(arp_with(6, &[0, operation]), Err(ArpError::Operation), "{operation}");
    }
}

#[test]
fn s_arp_014_padding_ignored() {
    let mut arp = arp_body(V_ARP_REQ);
    arp[28..].fill(0xEE);
    assert_eq!(Arp::parse(&arp).unwrap(), arp_request());
}

#[test]
fn s_arp_015_request_target_mac_exposed() {
    let arp = arp_with(18, &hex("02 00 00 00 00 99")).unwrap();
    assert_eq!(arp.target_mac, MacAddr([2, 0, 0, 0, 0, 0x99]));
}

fn emit_arp(arp: &Arp, source: IndividualMac) -> Vec<u8> {
    let mut out = junk(100);
    FrameBuilder { destination: arp.frame_destination(), source }.emit(arp, &mut out).unwrap().to_vec()
}

#[test]
fn s_arp_019_emit_request() {
    assert_eq!(emit_arp(&arp_request(), mac_a()), frame(V_ARP_REQ));
}

#[test]
fn s_arp_020_emit_reply() {
    let request = Arp::parse(&arp_body(V_ARP_REQ)).unwrap();
    assert_eq!(emit_arp(&Arp::reply(mac_b(), IP_B, &request), mac_b()), frame(V_ARP_REPLY));
}

#[test]
fn s_arp_021_emit_probe_and_announcement() {
    assert_eq!(emit_arp(&Arp::probe(mac_a(), IP_A), mac_a()), frame(V_ARP_PROBE));
    assert_eq!(emit_arp(&Arp::announcement(mac_a(), IP_A), mac_a()), frame(V_ARP_ANNOUNCE));
}

#[test]
fn s_arp_022_interface_mac_is_individual() {
    assert_eq!(IndividualMac::new(MacAddr([0x01, 0x00, 0x5e, 0, 0, 1])), None);
    assert!(IndividualMac::new(MAC_A).is_some());
}
