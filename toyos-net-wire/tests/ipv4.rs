mod common;

use std::net::Ipv4Addr;

use common::*;
use toyos_net_wire::igmp::{self, ReportGroup, V2Builder, V2Kind};
use toyos_net_wire::ipv4::{
    Dscp, Ecn, Form, FragmentOffset, Ipv4Builder, Ipv4Error, Ipv4Option, Ipv4Packet, Ipv4Payload, Ipv4Source, MulticastAddr,
    OptionKind, OtherProtocol, Protocol, RawPayload, TrafficClass, Ttl, ROUTER_ALERT,
};
use toyos_net_wire::udp::{UdpBuilder, UdpDatagram, UdpError};
use toyos_net_wire::{BuildError, Port};

fn parse(bytes: &[u8]) -> Result<Ipv4Packet<'_>, Ipv4Error> {
    Ipv4Packet::parse(bytes)
}

fn fixed(vector: &str, edit: impl Fn(&mut Vec<u8>)) -> Vec<u8> {
    let mut bytes = hex(vector);
    edit(&mut bytes);
    fix_ip(&mut bytes);
    bytes
}

fn set_total(bytes: &mut [u8], total: u16) {
    bytes[2..4].copy_from_slice(&total.to_be_bytes());
}

fn with_options(options: &[u8]) -> Vec<u8> {
    let hi = hex(V_UDP_HI);
    let mut bytes = hi[..20].to_vec();
    bytes.extend_from_slice(options);
    bytes.extend_from_slice(&hi[20..]);
    bytes[0] = 0x45 + (options.len() / 4) as u8;
    let total = bytes.len() as u16;
    set_total(&mut bytes, total);
    fix_ip(&mut bytes);
    bytes
}

fn options_of(bytes: &[u8]) -> Result<Vec<Ipv4Option<'_>>, Ipv4Error> {
    parse(bytes).map(|ip| ip.options().iter().collect())
}

fn builder<P: Ipv4Payload>(source: Ipv4Addr, destination: Ipv4Addr, payload: P) -> Ipv4Builder<'static, P> {
    Ipv4Builder {
        source: Ipv4Source::new(source).unwrap(),
        destination,
        ttl: Ttl::DEFAULT,
        traffic_class: TrafficClass::ZERO,
        form: Form::Atomic,
        options: &[],
        payload,
    }
}

fn raw(bytes: &[u8]) -> RawPayload<'_> {
    RawPayload { protocol: Protocol::from_number(253), bytes }
}

fn udp_hi() -> UdpBuilder<'static> {
    UdpBuilder { source: Port::new(5000).unwrap(), destination: Port::new(5001).unwrap(), data: b"hi" }
}

fn emit<P: Ipv4Payload>(builder: &Ipv4Builder<'_, P>) -> Result<Vec<u8>, BuildError> {
    let mut out = junk(70_000);
    builder.emit(&mut out).map(<[u8]>::to_vec)
}

#[test]
fn s_ip_001_udp_datagram_fields() {
    let bytes = ip_of(&hex(V_UDP_DNS));
    let ip = parse(&bytes).unwrap();
    assert_eq!(ip.header_len(), 20);
    assert_eq!(ip.traffic_class(), TrafficClass::ZERO);
    assert_eq!(ip.total_length(), 57);
    assert_eq!(ip.identification(), 0);
    assert!(ip.dont_fragment() && !ip.more_fragments());
    assert_eq!(ip.fragment_offset(), FragmentOffset::ZERO);
    assert_eq!(ip.ttl(), 64);
    assert_eq!(ip.protocol(), Protocol::Udp);
    assert_eq!((ip.source(), ip.destination()), (IP_A, IP_DNS));
    assert_eq!(ip.options().iter().count(), 0);
    assert_eq!(ip.payload().len(), 37);
}

#[test]
fn s_ip_002_minimal_datagram() {
    let bytes = hex(V_IP_MIN);
    let ip = parse(&bytes).unwrap();
    assert_eq!(ip.total_length(), 20);
    assert!(ip.payload().is_empty());
    assert_eq!(ip.protocol(), Protocol::Other(OtherProtocol::new(253).unwrap()));
}

#[test]
fn s_ip_003_nineteen_bytes() {
    assert_eq!(parse(&hex(V_IP_MIN)[..19]), Err(Ipv4Error::Truncated));
}

#[test]
fn s_ip_004_version_6() {
    assert_eq!(parse(&fixed(V_IP_MIN, |b| b[0] = 0x65)), Err(Ipv4Error::Version));
}

#[test]
fn s_ip_005_version_5() {
    assert_eq!(parse(&fixed(V_IP_MIN, |b| b[0] = 0x55)), Err(Ipv4Error::Version));
}

#[test]
fn s_ip_006_header_length_below_5() {
    for first in [0x44, 0x40] {
        let mut stale = hex(V_IP_MIN);
        stale[0] = first;
        assert_eq!(parse(&stale), Err(Ipv4Error::HeaderLength));
        // A checksum computed over the whole 20 bytes changes nothing.
        let mut summed = stale.clone();
        summed[10..12].copy_from_slice(&[0, 0]);
        let checksum = oracle_checksum(&summed);
        summed[10..12].copy_from_slice(&checksum.to_be_bytes());
        assert_eq!(parse(&summed), Err(Ipv4Error::HeaderLength));
    }
}

#[test]
fn s_ip_007_header_overrun() {
    let mut bytes = hex(V_IP_MIN);
    bytes[0] = 0x46;
    assert_eq!(parse(&bytes), Err(Ipv4Error::HeaderOverrun));
}

#[test]
fn s_ip_008_total_below_header() {
    assert_eq!(parse(&fixed(V_IP_MIN, |b| set_total(b, 19))), Err(Ipv4Error::TotalLengthBelowHeader));
}

#[test]
fn s_ip_009_total_zero() {
    assert_eq!(parse(&fixed(V_IP_MIN, |b| set_total(b, 0))), Err(Ipv4Error::TotalLengthBelowHeader));
}

#[test]
fn s_ip_010_total_inside_options() {
    assert_eq!(parse(&fixed(V_IP_RR, |b| set_total(b, 24))), Err(Ipv4Error::TotalLengthBelowHeader));
}

fn udp_dns_ip_with(edit: impl Fn(&mut Vec<u8>)) -> Vec<u8> {
    let mut bytes = ip_of(&hex(V_UDP_DNS));
    edit(&mut bytes);
    fix_ip(&mut bytes);
    bytes
}

#[test]
fn s_ip_011_total_overrun() {
    assert_eq!(parse(&udp_dns_ip_with(|b| set_total(b, 58))), Err(Ipv4Error::TotalLengthOverrun));
}

#[test]
fn s_ip_012_truncated_fragment_is_truncated() {
    let bytes = udp_dns_ip_with(|b| {
        set_total(b, 58);
        b[6] |= 0x20;
    });
    assert_eq!(parse(&bytes), Err(Ipv4Error::TotalLengthOverrun));
}

#[test]
fn s_ip_013_link_padding_is_not_payload() {
    let mut bytes = ip_of(&hex(V_UDP_DNS));
    bytes.extend_from_slice(&[0xEE; 10]);
    let ip = parse(&bytes).unwrap();
    assert_eq!(ip.payload().len(), 37);
    assert_eq!(ip.bytes().len(), 57);
}

#[test]
fn s_ip_014_padded_igmp_body() {
    let body = hex(V_IGMP_REPORT)[14..].to_vec();
    assert_eq!(body.len(), 46);
    let ip = parse(&body).unwrap();
    assert_eq!(ip.total_length(), 32);
    assert_eq!(ip.payload().len(), 8);
}

#[test]
fn s_ip_015_checksum_off_by_one() {
    let mut bytes = ip_of(&hex(V_UDP_DNS));
    assert_eq!(bytes[10..12], [0xb6, 0x7d]);
    bytes[11] = 0x7e;
    assert_eq!(parse(&bytes), Err(Ipv4Error::HeaderChecksum));
}

#[test]
fn s_ip_016_checksum_zero_is_checked() {
    let mut bytes = ip_of(&hex(V_UDP_DNS));
    bytes[10..12].copy_from_slice(&[0, 0]);
    assert_eq!(parse(&bytes), Err(Ipv4Error::HeaderChecksum));
}

#[test]
fn s_ip_017_options_are_covered() {
    let mut bytes = hex(V_IP_NOPS);
    assert_eq!(bytes[20], 0x01);
    bytes[20] = 0x00;
    assert_eq!(parse(&bytes), Err(Ipv4Error::HeaderChecksum));
}

#[test]
fn s_ip_018_payload_is_not_covered() {
    let mut bytes = ip_of(&hex(V_UDP_DNS));
    bytes[40] ^= 0x01;
    let ip = parse(&bytes).unwrap();
    assert_eq!(UdpDatagram::parse(&ip), Err(UdpError::Checksum));
}

#[test]
fn s_ip_019_reserved_flag_is_kept() {
    let bytes = fixed(V_UDP_HI, |b| b[6] = 0xC0);
    let ip = parse(&bytes).unwrap();
    assert!(ip.dont_fragment());
    assert!(!ip.is_fragment());
    assert_eq!(ip.bytes()[6], 0xC0);
    assert_eq!(ip.bytes(), bytes);
}

#[test]
fn s_ip_020_df_and_mf() {
    let bytes = fixed(V_UDP_HI, |b| b[6] = 0x60);
    let ip = parse(&bytes).unwrap();
    assert!(ip.dont_fragment() && ip.more_fragments() && ip.is_fragment());
}

#[test]
fn s_ip_021_first_fragment() {
    let bytes = hex(V_IP_FRAG_FIRST);
    let ip = parse(&bytes).unwrap();
    assert!(ip.more_fragments() && !ip.dont_fragment());
    assert_eq!(ip.fragment_offset().units(), 0);
    assert_eq!(ip.identification(), 0x4D2F);
    assert!(ip.is_fragment());
}

#[test]
fn s_ip_022_last_fragment() {
    let bytes = hex(V_IP_FRAG_LAST);
    let ip = parse(&bytes).unwrap();
    assert_eq!(ip.fragment_offset().units(), 185);
    assert_eq!(ip.fragment_offset().bytes(), 1480);
    assert!(!ip.more_fragments() && ip.is_fragment());
}

#[test]
fn s_ip_023_largest_offset() {
    let bytes = fixed(V_UDP_HI, |b| b[6..8].copy_from_slice(&[0x1F, 0xFF]));
    let ip = parse(&bytes).unwrap();
    assert_eq!(ip.fragment_offset().units(), 8191);
    assert!(!ip.more_fragments() && ip.is_fragment());
}

#[test]
fn s_ip_024_dscp_and_ecn() {
    let bytes = hex(V_IP_DSCP);
    let class = parse(&bytes).unwrap().traffic_class();
    assert_eq!((class.dscp().value(), class.ecn()), (46, Ecn::Ce));
}

#[test]
fn s_ip_027_identification_of_atomic_datagram() {
    let bytes = fixed(V_UDP_HI, |b| b[4..6].copy_from_slice(&[0x12, 0x34]));
    let ip = parse(&bytes).unwrap();
    assert_eq!(ip.identification(), 0x1234);
    assert_eq!(UdpDatagram::parse(&ip).unwrap().payload(), b"hi");
}

#[test]
fn s_ip_028_largest_datagram() {
    let mut bytes = hex(V_IP_MIN);
    bytes.resize(65_535, 0);
    set_total(&mut bytes, 65_535);
    fix_ip(&mut bytes);
    assert_eq!(parse(&bytes).unwrap().payload().len(), 65_515);
}

#[test]
fn s_ip_029_protocol_numbers() {
    let typed = [(1, Protocol::Icmp), (2, Protocol::Igmp), (6, Protocol::Tcp), (17, Protocol::Udp)];
    for (number, protocol) in typed {
        let bytes = fixed(V_IP_MIN, |b| b[9] = number);
        assert_eq!(parse(&bytes).unwrap().protocol(), protocol);
        assert_eq!(OtherProtocol::new(number), None);
    }
    for number in [0, 41, 50, 132, 253, 255] {
        let bytes = fixed(V_IP_MIN, |b| b[9] = number);
        let protocol = parse(&bytes).unwrap().protocol();
        assert!(matches!(protocol, Protocol::Other(other) if other.value() == number), "{number}");
        assert_eq!(protocol.number(), number);
    }
}

#[test]
fn s_ip_030_emit_udp_datagram() {
    let frame = hex(V_UDP_DNS);
    let udp = UdpBuilder { source: Port::new(49152).unwrap(), destination: Port::new(53).unwrap(), data: &frame[42..] };
    assert_eq!(emit(&builder(IP_A, IP_DNS, udp)).unwrap(), ip_of(&frame));
}

#[test]
fn s_ip_031_emit_router_alert() {
    let group = MulticastAddr::new(Ipv4Addr::new(224, 0, 0, 251)).unwrap();
    let report = V2Builder { kind: V2Kind::Report, group: ReportGroup::new(group).unwrap() };
    let datagram = Ipv4Builder { ttl: Ttl::LINK, options: ROUTER_ALERT, ..builder(IP_A, group.get(), report) };
    let built = emit(&datagram).unwrap();
    assert_eq!(built, ip_of(&hex(V_IGMP_REPORT)));
    assert_eq!(built[0], 0x46);
    assert_eq!(emit(&igmp::datagram(Ipv4Source::new(IP_A).unwrap(), TrafficClass::ZERO, report)).unwrap(), built);
}

fn with_built_options(options: &[Ipv4Option<'_>]) -> Result<Vec<u8>, BuildError> {
    emit(&Ipv4Builder { options, ..builder(IP_B, IP_A, udp_hi()) })
}

#[test]
fn s_ip_032_option_padding() {
    let record = [Ipv4Option::Other { kind: OptionKind::new(7).unwrap(), data: &[4] }];
    let built = with_built_options(&record).unwrap();
    assert_eq!(built[0], 0x46);
    assert_eq!(built[20..24], [0x07, 0x03, 0x04, 0x00]);
    let ip = parse(&built).unwrap();
    assert_eq!(ip.options().iter().collect::<Vec<_>>(), record);
}

#[test]
fn s_ip_033_forty_option_bytes() {
    let kind = OptionKind::new(68).unwrap();
    let built = with_built_options(&[Ipv4Option::Other { kind, data: &[0; 38] }]).unwrap();
    assert_eq!(built[0], 0x4F);
    assert_eq!(
        with_built_options(&[Ipv4Option::Other { kind, data: &[0; 38] }, Ipv4Option::Other { kind, data: &[] }]),
        Err(BuildError::IpOptionsTooLong)
    );
    assert_eq!(with_built_options(&[Ipv4Option::Other { kind, data: &[0; 39] }]), Err(BuildError::IpOptionsTooLong));
}

#[test]
fn s_ip_034_largest_total_length() {
    assert_eq!(emit(&builder(IP_B, IP_A, raw(&[0; 65_515]))).unwrap().len(), 65_535);
    assert_eq!(emit(&builder(IP_B, IP_A, raw(&[0; 65_516]))), Err(BuildError::IpTooLong));
}

#[test]
fn s_ip_036_ttl_zero() {
    assert_eq!(Ttl::new(0), Err(BuildError::IpTtlZero));
    assert_eq!(BuildError::IpTtlZero.name(), "ip.ttl-zero");
    assert_eq!(Ttl::new(1), Ok(Ttl::LINK));
}

#[test]
fn s_ip_037_invalid_sources() {
    for source in [Ipv4Addr::new(224, 0, 0, 1), Ipv4Addr::BROADCAST, Ipv4Addr::new(240, 0, 0, 1)] {
        assert_eq!(Ipv4Source::new(source), Err(BuildError::IpInvalidSource), "{source}");
    }
    assert!(Ipv4Source::new(Ipv4Addr::UNSPECIFIED).is_ok());
}

#[test]
fn s_ip_038_emit_over_junk() {
    let frame = hex(V_UDP_DNS);
    let udp = UdpBuilder { source: Port::new(49152).unwrap(), destination: Port::new(53).unwrap(), data: &frame[42..] };
    let mut out = junk(200);
    assert_eq!(builder(IP_A, IP_DNS, udp).emit(&mut out).unwrap(), ip_of(&frame));
}

#[test]
fn s_ip_039_atomic_and_fragment_forms() {
    let atomic = emit(&builder(IP_B, IP_A, udp_hi())).unwrap();
    assert_eq!(atomic[4..8], [0, 0, 0x40, 0]);
    let form = Form::Fragmentable { identification: 0x4D2F, more_fragments: true, offset: FragmentOffset::ZERO };
    assert_eq!(emit(&Ipv4Builder { form, ..builder(IP_B, IP_A, udp_hi()) }).unwrap(), hex(V_IP_FRAG_FIRST));
    let form = Form::Fragmentable { identification: 0x4D2F, more_fragments: false, offset: FragmentOffset::new(185).unwrap() };
    let last = RawPayload { protocol: Protocol::Udp, bytes: b"tail" };
    assert_eq!(emit(&Ipv4Builder { form, ..builder(IP_B, IP_A, last) }).unwrap(), hex(V_IP_FRAG_LAST));
    assert_eq!(FragmentOffset::new(8192), None);
}

#[test]
fn s_ip_040_emit_dscp_and_ecn() {
    let traffic_class = TrafficClass::new(Dscp::new(46).unwrap(), Ecn::Ce);
    assert_eq!(emit(&Ipv4Builder { traffic_class, ..builder(IP_B, IP_A, udp_hi()) }).unwrap(), hex(V_IP_DSCP));
    assert_eq!(Dscp::new(64), None);
}

#[test]
fn s_ipo_001_nops_and_eol_are_not_options() {
    let bytes = hex(V_IP_NOPS);
    assert_eq!(options_of(&bytes).unwrap(), []);
}

#[test]
fn s_ipo_002_record_route() {
    let bytes = hex(V_IP_RR);
    let ip = parse(&bytes).unwrap();
    let options: Vec<_> = ip.options().iter().collect();
    let [Ipv4Option::Other { kind, data }] = options[..] else { panic!("{options:?}") };
    assert_eq!((kind.value(), kind.copied(), kind.class(), kind.number()), (7, false, 0, 7));
    assert_eq!(data, [4, 0, 0, 0, 0]);
    assert_eq!(ip.payload().len(), 10);
}

#[test]
fn s_ipo_003_router_alert() {
    let bytes = ip_of(&hex(V_IGMP_REPORT));
    assert_eq!(options_of(&bytes).unwrap(), [Ipv4Option::RouterAlert(0)]);
}

#[test]
fn s_ipo_004_router_alert_length() {
    assert_eq!(options_of(&with_options(&hex("94 03 00 00"))), Err(Ipv4Error::RouterAlertLength));
    assert_eq!(options_of(&with_options(&hex("94 06 00 00 00 00 00 00"))), Err(Ipv4Error::RouterAlertLength));
}

#[test]
fn s_ipo_005_option_length_one() {
    assert_eq!(options_of(&with_options(&hex("07 01 00 00"))), Err(Ipv4Error::OptionLength));
}

#[test]
fn s_ipo_006_option_length_zero_terminates() {
    assert_eq!(options_of(&with_options(&hex("07 00 00 00"))), Err(Ipv4Error::OptionLength));
}

#[test]
fn s_ipo_007_option_overrun() {
    assert_eq!(options_of(&with_options(&hex("07 08 04 00"))), Err(Ipv4Error::OptionOverrun));
}

#[test]
fn s_ipo_008_kind_in_last_byte() {
    assert_eq!(options_of(&with_options(&hex("01 01 01 07"))), Err(Ipv4Error::OptionOverrun));
}

#[test]
fn s_ipo_009_bytes_after_eol_ignored() {
    assert_eq!(options_of(&with_options(&hex("00 ff ff ff"))).unwrap(), []);
}

#[test]
fn s_ipo_010_router_alert_without_eol() {
    assert_eq!(options_of(&with_options(&hex("94 04 00 00"))).unwrap(), [Ipv4Option::RouterAlert(0)]);
}

#[test]
fn s_ipo_011_unknown_kind_is_opaque() {
    let bytes = with_options(&hex("9e 04 aa bb"));
    let options = options_of(&bytes).unwrap();
    let [Ipv4Option::Other { kind, data }] = options[..] else { panic!("{options:?}") };
    assert_eq!((kind.value(), kind.copied(), kind.class(), kind.number()), (158, true, 0, 30));
    assert_eq!(data, [0xaa, 0xbb]);
}

#[test]
fn s_ipo_012_option_filling_the_area() {
    let mut area = vec![68, 40];
    area.resize(40, 0);
    let bytes = with_options(&area);
    let options = options_of(&bytes).unwrap();
    let [Ipv4Option::Other { kind, data }] = options[..] else { panic!("{options:?}") };
    assert_eq!((kind.value(), data.len()), (68, 38));
}

#[test]
fn s_ipo_013_option_past_a_full_area() {
    let mut area = vec![68, 41];
    area.resize(40, 0);
    assert_eq!(options_of(&with_options(&area)), Err(Ipv4Error::OptionOverrun));
}

#[test]
fn s_ipo_014_repeated_options_in_order() {
    let bytes = with_options(&hex("94 04 00 00 94 04 00 01"));
    assert_eq!(options_of(&bytes).unwrap(), [Ipv4Option::RouterAlert(0), Ipv4Option::RouterAlert(1)]);
}

#[test]
fn s_ipo_015_stream_id_exposed() {
    let bytes = with_options(&hex("88 04 00 01"));
    let ip = parse(&bytes).unwrap();
    let options: Vec<_> = ip.options().iter().collect();
    assert_eq!(options, [Ipv4Option::Other { kind: OptionKind::new(0x88).unwrap(), data: &[0, 1] }]);
    assert_eq!(UdpDatagram::parse(&ip).unwrap().payload(), b"hi");
}

#[test]
fn s_ipo_016_security_option_exposed() {
    let mut area = vec![130, 11];
    area.resize(12, 0);
    let bytes = with_options(&area);
    let ip = parse(&bytes).unwrap();
    let options: Vec<_> = ip.options().iter().collect();
    let [Ipv4Option::Other { kind, data }] = options[..] else { panic!("{options:?}") };
    assert_eq!((kind.value(), data.len()), (130, 9));
    assert_eq!(UdpDatagram::parse(&ip).unwrap().payload(), b"hi");
}
