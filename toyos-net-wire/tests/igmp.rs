mod common;

use std::net::Ipv4Addr;

use common::*;
use toyos_net_wire::ethernet::{FrameBuilder, MacAddr};
use toyos_net_wire::igmp::{
    self, Deciseconds, GroupRecord, IgmpError, IgmpMessage, IgmpPacket, Query, QueryGroup, QueryVersion, RecordType,
    ReportGroup, V2Builder, V2Kind, V3ReportBuilder,
};
use toyos_net_wire::ipv4::{Ecn, Ipv4Option, Ipv4Packet, Ipv4Source, MulticastAddr, TrafficClass};
use toyos_net_wire::BuildError;

fn parse(bytes: &[u8]) -> Result<IgmpMessage<'_>, IgmpError> {
    IgmpPacket::parse(bytes).map(|packet| packet.message())
}

fn mdns() -> MulticastAddr {
    MulticastAddr::new(Ipv4Addr::new(224, 0, 0, 251)).unwrap()
}

fn query(bytes: &[u8]) -> Query<'_> {
    match parse(bytes) {
        Ok(IgmpMessage::Query(query)) => query,
        other => panic!("{other:?}"),
    }
}

fn report_igmp() -> Vec<u8> {
    payload_of(&ip_of(&hex(V_IGMP_REPORT)))
}

#[test]
fn s_igmp_001_v2_report() {
    assert_eq!(parse(&report_igmp()), Ok(IgmpMessage::V2Report(mdns())));
}

#[test]
fn s_igmp_002_v2_general_query() {
    let bytes = payload_of(&hex(V_IGMP_QUERY_V2));
    let query = query(&bytes);
    assert_eq!((query.version, query.group, query.max_response), (QueryVersion::V2, QueryGroup::General, Deciseconds(100)));
}

#[test]
fn s_igmp_003_v1_query() {
    let bytes = hex(V_IGMP_QUERY_V1);
    let query = query(&bytes);
    assert_eq!((query.version, query.max_response), (QueryVersion::V1, Deciseconds(100)));
}

#[test]
fn s_igmp_004_v3_general_query() {
    let bytes = hex(V_IGMP_QUERY_V3);
    let general = query(&bytes);
    let QueryVersion::V3(v3) = general.version else { panic!("{general:?}") };
    assert_eq!((general.group, general.max_response), (QueryGroup::General, Deciseconds(100)));
    assert_eq!((v3.suppress_router_processing, v3.robustness, v3.interval_code), (false, 2, 125));
    assert_eq!(v3.sources().count(), 0);
    let suppressed = fixed(bytes.clone(), |b| b[8] = 0x0A);
    let QueryVersion::V3(v3) = query(&suppressed).version else { panic!() };
    assert_eq!((v3.suppress_router_processing, v3.robustness), (true, 2));
}

#[test]
fn s_igmp_005_v3_exponential_code() {
    assert_eq!(query(&hex(V_IGMP_QUERY_V3_EXP)).max_response, Deciseconds(224));
}

#[test]
fn s_igmp_006_v2_codes_are_linear() {
    let base = payload_of(&hex(V_IGMP_QUERY_V2));
    assert_eq!(query(&fixed(base.clone(), |b| b[1] = 0x8C)).max_response, Deciseconds(140));
    assert_eq!(query(&fixed(base, |b| b[1] = 0xFF)).max_response, Deciseconds(255));
}

#[test]
fn s_igmp_007_v3_largest_code() {
    assert_eq!(query(&fixed(hex(V_IGMP_QUERY_V3), |b| b[1] = 0xFF)).max_response, Deciseconds(31_744));
}

#[test]
fn s_igmp_008_group_and_source_query() {
    let bytes = hex(V_IGMP_QUERY_V3_SRC);
    let query = query(&bytes);
    assert_eq!(query.group, QueryGroup::Specific(mdns()));
    let QueryVersion::V3(v3) = query.version else { panic!("{query:?}") };
    assert_eq!(v3.sources().collect::<Vec<_>>(), [Ipv4Addr::new(192, 0, 2, 9), Ipv4Addr::new(192, 0, 2, 10)]);
}

#[test]
fn s_igmp_009_sources_overrun() {
    let bytes = fixed(hex(V_IGMP_QUERY_V3_SRC), |b| b[11] = 3);
    assert_eq!(parse(&bytes), Err(IgmpError::QuerySourcesOverrun));
    let bytes = fixed(bytes, |b| b[4..8].copy_from_slice(&IP_A.octets()));
    assert_eq!(parse(&bytes), Err(IgmpError::QueryGroup));
    assert_eq!(parse(&fixed(bytes[..9].to_vec(), |_| {})), Err(IgmpError::QueryLength));
}

#[test]
fn s_igmp_010_bytes_after_sources_ignored() {
    let bytes = fixed(hex(V_IGMP_QUERY_V3_SRC), |b| b.extend_from_slice(&[1, 2, 3, 4]));
    let QueryVersion::V3(v3) = query(&bytes).version else { panic!() };
    assert_eq!(v3.sources().count(), 2);
}

#[test]
fn s_igmp_011_query_of_9_to_11_bytes() {
    for len in 9..=11 {
        let bytes = fixed(hex(V_IGMP_QUERY_V3)[..len].to_vec(), |_| {});
        assert_eq!(parse(&bytes), Err(IgmpError::QueryLength), "{len}");
    }
}

#[test]
fn s_igmp_012_seven_bytes() {
    assert_eq!(parse(&report_igmp()[..7]), Err(IgmpError::Truncated));
}

#[test]
fn s_igmp_013_checksum_changed() {
    let mut bytes = report_igmp();
    bytes[2] ^= 0x10;
    assert_eq!(parse(&bytes), Err(IgmpError::Checksum));
}

#[test]
fn s_igmp_014_trailing_bytes_ignored() {
    assert_eq!(parse(&hex(V_IGMP_REPORT_LONG)), Ok(IgmpMessage::V2Report(mdns())));
}

#[test]
fn s_igmp_015_checksum_covers_trailing_bytes() {
    let mut bytes = hex(V_IGMP_REPORT_LONG);
    fix_message(&mut bytes[..8]);
    assert_eq!(parse(&bytes), Err(IgmpError::Checksum));
}

#[test]
fn s_igmp_016_query_group_unicast() {
    let bytes = fixed(payload_of(&hex(V_IGMP_QUERY_V2)), |b| b[4..8].copy_from_slice(&IP_A.octets()));
    assert_eq!(parse(&bytes), Err(IgmpError::QueryGroup));
}

#[test]
fn s_igmp_017_report_group_not_multicast() {
    for group in [Ipv4Addr::UNSPECIFIED, IP_A] {
        let bytes = fixed(report_igmp(), |b| b[4..8].copy_from_slice(&group.octets()));
        assert_eq!(parse(&bytes), Err(IgmpError::Group), "{group}");
    }
}

#[test]
fn s_igmp_018_leave() {
    assert_eq!(parse(&hex(V_IGMP_LEAVE)), Ok(IgmpMessage::Leave(mdns())));
}

#[test]
fn s_igmp_019_v1_report_max_response_ignored() {
    let bytes = fixed(report_igmp(), |b| {
        b[0] = 0x12;
        b[1] = 0x64;
    });
    assert_eq!(parse(&bytes), Ok(IgmpMessage::V1Report(mdns())));
}

#[test]
fn s_igmp_020_v3_report_unsupported() {
    let bytes = payload_of(&hex(V_IGMP3_LEAVE));
    assert_eq!(parse(&bytes), Err(IgmpError::V3Report));
}

#[test]
fn s_igmp_021_unknown_types() {
    for kind in [0x13, 0x30] {
        assert_eq!(parse(&fixed(report_igmp(), |b| b[0] = kind)), Err(IgmpError::UnknownType), "{kind:#x}");
    }
}

#[test]
fn s_igmp_022_checksum_before_type() {
    let mut bytes = fixed(report_igmp(), |b| b[0] = 0x30);
    bytes[3] ^= 0x01;
    assert_eq!(parse(&bytes), Err(IgmpError::Checksum));
}

fn v2(kind: V2Kind) -> V2Builder {
    V2Builder { kind, group: ReportGroup::new(mdns()).unwrap() }
}

fn source_a() -> Ipv4Source {
    Ipv4Source::new(IP_A).unwrap()
}

#[test]
fn s_igmp_026_emit_join_frame() {
    let datagram = igmp::datagram(source_a(), TrafficClass::ZERO, v2(V2Kind::Report));
    let builder = FrameBuilder { destination: MacAddr::multicast(mdns()), source: mac_a() };
    let mut out = junk(100);
    assert_eq!(builder.emit(&datagram, &mut out).unwrap(), hex(V_IGMP_REPORT));
}

fn emit_leave_like(kind: V2Kind) -> Vec<u8> {
    let mut out = junk(100);
    igmp::datagram(source_a(), TrafficClass::ZERO, v2(kind)).emit(&mut out).unwrap().to_vec()
}

#[test]
fn s_igmp_027_emit_leave() {
    let bytes = emit_leave_like(V2Kind::Leave);
    let ip = Ipv4Packet::parse(&bytes).unwrap();
    assert_eq!(ip.payload(), hex(V_IGMP_LEAVE));
    assert_eq!((ip.destination(), ip.ttl()), (Ipv4Addr::new(224, 0, 0, 2), 1));
    assert_eq!(ip.options().iter().collect::<Vec<_>>(), [Ipv4Option::RouterAlert(0)]);
}

#[test]
fn s_igmp_030_emitted_max_response_is_zero() {
    for kind in [V2Kind::Report, V2Kind::Leave] {
        let bytes = emit_leave_like(kind);
        assert_eq!(bytes[25], 0, "{kind:?}");
    }
    assert_eq!(ReportGroup::new(MulticastAddr::ALL_HOSTS), Err(BuildError::IgmpReportAllHosts));
}

fn network_control() -> TrafficClass {
    TrafficClass::new(48, Ecn::NotEct).unwrap()
}

fn emit_v3(source: Ipv4Addr, records: &[GroupRecord]) -> Vec<u8> {
    let datagram = igmp::datagram(Ipv4Source::new(source).unwrap(), network_control(), V3ReportBuilder { records });
    let mut out = junk(1500);
    datagram.emit(&mut out).unwrap().to_vec()
}

fn record(record: RecordType) -> GroupRecord {
    GroupRecord { group: ReportGroup::new(mdns()).unwrap(), record }
}

#[test]
fn w1_igmp3_join_frame() {
    let records = [record(RecordType::ToExclude)];
    let datagram = igmp::datagram(source_a(), network_control(), V3ReportBuilder { records: &records });
    let builder = FrameBuilder { destination: MacAddr::multicast(MulticastAddr::IGMPV3_ROUTERS), source: mac_a() };
    let mut out = junk(100);
    assert_eq!(builder.emit(&datagram, &mut out).unwrap(), hex(V_IGMP3_JOIN));
}

#[test]
fn w1_igmp3_leave_current_and_unspecified_source() {
    assert_eq!(emit_v3(IP_A, &[record(RecordType::ToInclude)]), hex(V_IGMP3_LEAVE));
    assert_eq!(emit_v3(IP_A, &[record(RecordType::IsExclude)]), hex(V_IGMP3_CURRENT));
    assert_eq!(emit_v3(Ipv4Addr::UNSPECIFIED, &[record(RecordType::ToExclude)]), hex(V_IGMP3_JOIN_UNSPEC));
}

#[test]
fn w1_igmp3_records_layout() {
    let other = ReportGroup::new(MulticastAddr::new(Ipv4Addr::new(239, 1, 2, 3)).unwrap()).unwrap();
    let records = [record(RecordType::ToExclude), GroupRecord { group: other, record: RecordType::IsExclude }];
    let bytes = emit_v3(IP_A, &records);
    let ip = Ipv4Packet::parse(&bytes).unwrap();
    let igmp = ip.payload();
    assert_eq!(oracle_sum(igmp), 0xFFFF);
    assert_eq!(igmp[..2], [0x22, 0]);
    assert_eq!(igmp[4..8], [0, 0, 0, 2]);
    assert_eq!(igmp[8..16], hex("04 00 00 00 e0 00 00 fb")[..]);
    assert_eq!(igmp[16..24], hex("02 00 00 00 ef 01 02 03")[..]);
    assert_eq!(igmp.len(), 24);
}
