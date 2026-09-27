mod common;

use std::net::Ipv4Addr;

use common::*;
use toyos_net_wire::ethernet::FrameBuilder;
use toyos_net_wire::ipv4::Ipv4Packet;
use toyos_net_wire::tcp::{
    Control, EstablishedOptions, RawWindow, SackBlock, SeqNum, SynOptions, TcpBuilder, TcpError, TcpFlags, TcpSegment,
    Timestamps, WindowShift,
};
use toyos_net_wire::udp::{UdpBuilder, UdpChecksum, UdpDatagram, UdpError};
use toyos_net_wire::{BuildError, Port};

fn port(number: u16) -> Port {
    Port::new(number).unwrap()
}

fn udp(ip: &[u8]) -> Result<UdpDatagram<'_>, UdpError> {
    UdpDatagram::parse(&Ipv4Packet::parse(ip).unwrap())
}

fn tcp(ip: &[u8]) -> Result<TcpSegment<'_>, TcpError> {
    TcpSegment::parse(&Ipv4Packet::parse(ip).unwrap())
}

fn dns_udp() -> Vec<u8> {
    payload_of(&ip_of(&hex(V_UDP_DNS)))
}

fn dns_datagram(edit: impl Fn(&mut Vec<u8>)) -> Vec<u8> {
    let mut udp = dns_udp();
    edit(&mut udp);
    ipv4(IP_A, IP_DNS, 17, &udp)
}

fn dns_fixed(edit: impl Fn(&mut Vec<u8>)) -> Vec<u8> {
    dns_datagram(|u| {
        edit(u);
        fix_udp(IP_A, IP_DNS, u);
    })
}

#[test]
fn s_udp_001_dns_query() {
    let bytes = dns_datagram(|_| {});
    let udp = udp(&bytes).unwrap();
    assert_eq!((udp.source_port(), udp.destination_port()), (Some(port(49152)), port(53)));
    assert_eq!(udp.header()[4..6], [0, 37]);
    assert!(matches!(udp.checksum(), UdpChecksum::Present(c) if c.value() == 0xD995));
    assert_eq!(udp.payload().len(), 29);
}

#[test]
fn s_udp_002_ffff_verifies() {
    let bytes = ipv4(IP_A, IP_B, 17, &hex(V_UDP_FFFF));
    assert_eq!(udp(&bytes).unwrap().checksum().field(), 0xFFFF);
}

#[test]
fn s_udp_003_seven_bytes() {
    assert_eq!(udp(&ipv4(IP_A, IP_DNS, 17, &dns_udp()[..7])), Err(UdpError::Truncated));
}

#[test]
fn s_udp_004_length_below_header() {
    for length in [7u16, 0] {
        let bytes = dns_datagram(|u| u[4..6].copy_from_slice(&length.to_be_bytes()));
        assert_eq!(udp(&bytes), Err(UdpError::LengthBelowHeader), "{length}");
    }
}

#[test]
fn s_udp_005_length_overrun() {
    assert_eq!(udp(&dns_datagram(|u| u[5] = 38)), Err(UdpError::LengthOverrun));
}

#[test]
fn s_udp_006_surplus_after_length_ignored() {
    let bytes = dns_fixed(|u| u[5] = 8);
    let udp = udp(&bytes).unwrap();
    assert!(udp.payload().is_empty());
}

#[test]
fn s_udp_007_checksum_covers_only_the_length() {
    let bytes = dns_datagram(|u| {
        u[6..8].copy_from_slice(&[0, 0]);
        let mut covered = pseudo(IP_A, IP_DNS, 17, 8);
        covered.extend_from_slice(u);
        let checksum = oracle_checksum(&covered);
        u[6..8].copy_from_slice(&checksum.to_be_bytes());
        u[5] = 8;
    });
    assert_eq!(udp(&bytes), Err(UdpError::Checksum));
}

#[test]
fn s_udp_008_no_checksum() {
    let bytes = dns_datagram(|u| u[6..8].copy_from_slice(&[0, 0]));
    assert_eq!(udp(&bytes).unwrap().checksum(), UdpChecksum::Absent);
}

#[test]
fn s_udp_009_no_checksum_catches_nothing() {
    let bytes = dns_datagram(|u| {
        u[6..8].copy_from_slice(&[0, 0]);
        u[20] ^= 0xFF;
    });
    assert!(udp(&bytes).is_ok());
}

#[test]
fn s_udp_010_wrong_checksum() {
    assert_eq!(udp(&dns_datagram(|u| u[6..8].copy_from_slice(&[0xD9, 0x96]))), Err(UdpError::Checksum));
}

#[test]
fn s_udp_011_pseudo_header_catches_misdelivery() {
    let bytes = ipv4(IP_A, Ipv4Addr::new(192, 0, 2, 54), 17, &dns_udp());
    assert_eq!(udp(&bytes), Err(UdpError::Checksum));
}

#[test]
fn s_udp_012_destination_port_zero() {
    assert_eq!(udp(&dns_fixed(|u| u[2..4].copy_from_slice(&[0, 0]))), Err(UdpError::DestinationPortZero));
}

#[test]
fn s_udp_013_source_port_zero() {
    let bytes = dns_fixed(|u| u[0..2].copy_from_slice(&[0, 0]));
    assert_eq!(udp(&bytes).unwrap().source_port(), None);
}

#[test]
fn s_udp_014_largest_datagram() {
    let data = vec![0x61; 65_507];
    let bytes = emit!(&datagram(IP_A, IP_B, UdpBuilder { source: port(1), destination: port(2), data: &data })).unwrap();
    assert_eq!(bytes.len(), 65_535);
    assert_eq!(udp(&bytes).unwrap().payload().len(), 65_507);
}

#[test]
fn s_udp_015_length_checked_before_checksum() {
    let bytes = dns_datagram(|u| {
        u[5] = 7;
        u[7] ^= 1;
    });
    assert_eq!(udp(&bytes), Err(UdpError::LengthBelowHeader));
}

#[test]
fn s_udp_016_the_one_undetectable_bit_flip() {
    // Find data whose checksum field has one bit set; that bit flipped reads as "none".
    let (data, field) = (0..=u16::MAX)
        .map(|word| word.to_be_bytes())
        .find_map(|data| {
            let bytes = emit!(&datagram(IP_B, IP_A, UdpBuilder { source: port(5000), destination: port(5001), data: &data }))
                .unwrap();
            let field = u16::from_be_bytes([bytes[26], bytes[27]]);
            field.is_power_of_two().then_some((data, field))
        })
        .unwrap();
    let mut bytes = emit!(&datagram(IP_B, IP_A, UdpBuilder { source: port(5000), destination: port(5001), data: &data })).unwrap();
    let flipped = u16::from_be_bytes([bytes[26], bytes[27]]) ^ field;
    bytes[26..28].copy_from_slice(&flipped.to_be_bytes());
    assert_eq!(udp(&bytes).unwrap().checksum(), UdpChecksum::Absent);
}

#[test]
fn s_udp_019_emit_dns_query() {
    let frame = hex(V_UDP_DNS);
    let built = emit!(&datagram(IP_A, IP_DNS, UdpBuilder { source: port(49152), destination: port(53), data: &frame[42..] })).unwrap();
    assert_eq!(built[20..], dns_udp());
    assert_eq!(built[26..28], [0xD9, 0x95]);
}

#[test]
fn s_udp_020_emit_computed_zero_as_ffff() {
    let vector = hex(V_UDP_FFFF);
    let built = emit!(&datagram(IP_A, IP_B, UdpBuilder { source: port(1000), destination: port(2000), data: &vector[8..] })).unwrap();
    assert_eq!(built[20..], vector);
}

#[test]
fn s_udp_021_always_checksummed() {
    let built = emit!(&datagram(IP_A, IP_B, UdpBuilder { source: port(1000), destination: port(2000), data: &[0, 0] })).unwrap();
    assert_ne!(built[26..28], [0, 0]);
}

#[test]
fn s_udp_022_too_long() {
    let data = vec![0; 65_528];
    assert_eq!(emit!(&datagram(IP_A, IP_B, UdpBuilder { source: port(1), destination: port(2), data: &data })), Err(BuildError::UdpTooLong));
    let frame = FrameBuilder { destination: MAC_B, source: mac_a() };
    let mut out = junk(2000);
    let data = [0; 1473];
    let fits = datagram(IP_A, IP_B, UdpBuilder { source: port(1), destination: port(2), data: &data[..1472] });
    assert_eq!(frame.emit(&fits, &mut out).unwrap().len(), 1514);
    let over = datagram(IP_A, IP_B, UdpBuilder { source: port(1), destination: port(2), data: &data });
    assert_eq!(frame.emit(&over, &mut out), Err(BuildError::EthBodyTooLong));
}

fn checksummed_under(protocol: u8, source: Ipv4Addr, destination: Ipv4Addr, segment: &mut [u8], field: usize) {
    segment[field..field + 2].copy_from_slice(&[0, 0]);
    let mut covered = pseudo(source, destination, protocol, segment.len() as u16);
    covered.extend_from_slice(segment);
    let checksum = oracle_checksum(&covered);
    segment[field..field + 2].copy_from_slice(&checksum.to_be_bytes());
}

#[test]
fn udp_pseudo_header_names_protocol_17() {
    let mut segment = hex(V_UDP_HI)[20..].to_vec();
    checksummed_under(6, IP_B, IP_A, &mut segment, 6);
    assert_eq!(udp(&ipv4(IP_B, IP_A, 6, &segment)), Err(UdpError::Checksum));
}

#[test]
fn tcp_pseudo_header_names_protocol_6() {
    let mut segment = payload_of(&frame_ip(V_TCP_FIN));
    checksummed_under(17, IP_A, IP_B, &mut segment, 16);
    assert_eq!(tcp(&ipv4(IP_A, IP_B, 17, &segment)).map(|s| s.flags()), Err(TcpError::Checksum));
}

#[test]
fn s_udp_023_zero_port_cannot_be_built() {
    assert_eq!(Port::new(0), None);
}

#[test]
fn s_udp_024_emit_hi() {
    let built = emit!(&datagram(IP_B, IP_A, UdpBuilder { source: port(5000), destination: port(5001), data: b"hi" })).unwrap();
    assert_eq!(built, hex(V_UDP_HI));
}

fn frame_ip(vector: &str) -> Vec<u8> {
    ip_of(&hex(vector))
}

fn fin_with(edit: impl Fn(&mut Vec<u8>)) -> Vec<u8> {
    let ip = frame_ip(V_TCP_FIN);
    let mut segment = ip[20..].to_vec();
    edit(&mut segment);
    fix_tcp(IP_A, IP_B, &mut segment);
    ipv4(IP_A, IP_B, 6, &segment)
}

fn fin_options(options: &[u8], flags: u8) -> Vec<u8> {
    fin_with(|s| {
        let mut padded = options.to_vec();
        padded.resize(options.len().div_ceil(4) * 4, 0);
        s[12] = (5 + padded.len() as u8 / 4) << 4;
        s[13] = flags;
        s.splice(20..20, padded);
    })
}

fn options(options: &[u8]) -> Vec<u8> {
    fin_options(options, 0x11)
}

#[test]
fn s_tcp_001_syn() {
    let bytes = frame_ip(V_TCP_SYN);
    let s = tcp(&bytes).unwrap();
    assert_eq!((s.source_port(), s.destination_port()), (port(49152), port(80)));
    assert_eq!((s.sequence(), s.acknowledgment()), (SeqNum::new(0x0102_0304), None));
    assert_eq!(s.flags(), TcpFlags::SYN);
    assert_eq!((s.window(), s.header_len()), (RawWindow(64240), 40));
    let o = s.options();
    assert_eq!((o.mss(), o.sack_permitted(), o.timestamps()), (Some(1460), true, Some(Timestamps { value: 1, echo: 0 })));
    assert_eq!(o.window_scale().map(|w| w.raw()), Some(7));
    assert!(s.payload().is_empty());
}

#[test]
fn s_tcp_002_syn_ack() {
    let bytes = frame_ip(V_TCP_SYNACK);
    let s = tcp(&bytes).unwrap();
    assert_eq!(s.flags(), TcpFlags::SYN | TcpFlags::ACK);
    assert_eq!((s.acknowledgment(), s.window()), (Some(SeqNum::new(0x0102_0305)), RawWindow(65160)));
    let o = s.options();
    assert_eq!((o.mss(), o.sack_permitted(), o.timestamps()), (Some(1460), true, Some(Timestamps { value: 256, echo: 1 })));
    assert_eq!(o.window_scale().map(|w| w.raw()), Some(7));
}

#[test]
fn s_tcp_003_data() {
    let bytes = frame_ip(V_TCP_DATA);
    let s = tcp(&bytes).unwrap();
    assert_eq!(s.flags(), TcpFlags::PSH | TcpFlags::ACK);
    assert_eq!((s.acknowledgment(), s.window()), (Some(SeqNum::new(0xA0B0_C0D1)), RawWindow(502)));
    assert_eq!(s.options().timestamps(), Some(Timestamps { value: 2, echo: 256 }));
    assert_eq!(s.payload(), b"GET / HTTP/1.0\r\n\r\n");
}

#[test]
fn s_tcp_004_sack() {
    let bytes = frame_ip(V_TCP_SACK);
    let s = tcp(&bytes).unwrap();
    assert_eq!((s.flags(), s.acknowledgment()), (TcpFlags::ACK, Some(SeqNum::new(0x0102_0317))));
    assert_eq!(s.options().timestamps(), Some(Timestamps { value: 257, echo: 2 }));
    let blocks: Vec<_> = s.options().sack_blocks().collect();
    assert_eq!(blocks, [SackBlock { left: SeqNum::new(0x0102_0B6F), right: SeqNum::new(0x0102_101F) }]);
}

#[test]
fn s_tcp_005_three_sack_blocks() {
    let bytes = hex(V_TCP_SACK3);
    let s = tcp(&bytes).unwrap();
    assert_eq!((s.header_len(), s.options_bytes().len()), (60, 40));
    assert_eq!(s.options().timestamps(), Some(Timestamps { value: 258, echo: 3 }));
    assert_eq!(s.options().sack_blocks().len(), 3);
}

#[test]
fn s_tcp_006_rst() {
    let bytes = frame_ip(V_TCP_RST);
    let s = tcp(&bytes).unwrap();
    assert_eq!((s.flags(), s.acknowledgment(), s.window()), (TcpFlags::RST, None, RawWindow(0)));
}

#[test]
fn s_tcp_007_fin() {
    let bytes = frame_ip(V_TCP_FIN);
    let s = tcp(&bytes).unwrap();
    assert_eq!(s.flags(), TcpFlags::FIN | TcpFlags::ACK);
    assert!(s.options_bytes().is_empty());
}

#[test]
fn s_tcp_008_odd_length_verifies() {
    let bytes = ipv4(IP_A, IP_B, 6, &hex(V_TCP_ODD));
    assert_eq!(tcp(&bytes).unwrap().payload(), b"abc");
}

#[test]
fn s_tcp_009_nineteen_bytes() {
    let ip = frame_ip(V_TCP_FIN);
    assert_eq!(tcp(&ipv4(IP_A, IP_B, 6, &ip[20..39])), Err(TcpError::Truncated));
}

#[test]
fn s_tcp_010_data_offset_below_5() {
    for offset in [4u8, 0] {
        assert_eq!(tcp(&fin_with(|s| s[12] = offset << 4)), Err(TcpError::DataOffset), "{offset}");
    }
}

#[test]
fn s_tcp_011_data_offset_past_the_bytes() {
    assert_eq!(tcp(&fin_with(|s| s[12] = 0x60)), Err(TcpError::HeaderOverrun));
}

#[test]
fn s_tcp_012_data_offset_15_on_59_bytes() {
    let bytes = fin_with(|s| {
        s[12] = 0xF0;
        s.resize(59, 1);
    });
    assert_eq!(tcp(&bytes), Err(TcpError::HeaderOverrun));
}

#[test]
fn s_tcp_013_checksum_changed() {
    let mut bytes = frame_ip(V_TCP_FIN);
    bytes[37] ^= 0x01;
    assert_eq!(tcp(&bytes), Err(TcpError::Checksum));
}

#[test]
fn s_tcp_014_checksum_zero_is_checked() {
    let mut bytes = frame_ip(V_TCP_FIN);
    bytes[36..38].copy_from_slice(&[0, 0]);
    assert_eq!(tcp(&bytes), Err(TcpError::Checksum));
}

#[test]
fn s_tcp_015_wrong_source_address() {
    let segment = payload_of(&frame_ip(V_TCP_SYN));
    assert_eq!(tcp(&ipv4(Ipv4Addr::new(192, 0, 2, 9), IP_B, 6, &segment)), Err(TcpError::Checksum));
}

#[test]
fn s_tcp_016_port_zero() {
    assert_eq!(tcp(&fin_with(|s| s[0..2].copy_from_slice(&[0, 0]))), Err(TcpError::PortZero));
    assert_eq!(tcp(&fin_with(|s| s[2..4].copy_from_slice(&[0, 0]))), Err(TcpError::PortZero));
}

#[test]
fn s_tcp_017_reserved_bits_kept() {
    let bytes = fin_with(|s| s[12] = 0x5F);
    let s = tcp(&bytes).unwrap();
    assert_eq!(s.flags(), TcpFlags::FIN | TcpFlags::ACK);
    assert_eq!(s.header()[12], 0x5F);
}

#[test]
fn s_tcp_018_acknowledgment_without_ack() {
    let ip = frame_ip(V_TCP_RST);
    let mut segment = ip[20..].to_vec();
    segment[8..12].copy_from_slice(&[0x12, 0x34, 0x56, 0x78]);
    fix_tcp(IP_B, IP_A, &mut segment);
    assert_eq!(tcp(&ipv4(IP_B, IP_A, 6, &segment)).unwrap().acknowledgment(), None);
}

#[test]
fn s_tcp_019_urgent_pointer_only_with_urg() {
    let urgent = |flags| fin_with(move |s| {
        s[13] = flags;
        s[18..20].copy_from_slice(&[0, 5]);
    });
    assert_eq!(tcp(&urgent(0x11)).unwrap().urgent_pointer(), None);
    assert_eq!(tcp(&urgent(0x31)).unwrap().urgent_pointer(), Some(5));
}

#[test]
fn s_tcp_020_any_flag_combination() {
    for flags in [0x03, 0x06, 0x05, 0x29, 0xFF, 0x00] {
        let bytes = fin_with(|s| s[13] = flags);
        assert_eq!(tcp(&bytes).unwrap().flags().bits(), flags, "{flags:#x}");
    }
}

#[test]
fn s_tcp_021_ece_and_cwr() {
    let bytes = fin_with(|s| s[13] = 0xD0);
    let flags = tcp(&bytes).unwrap().flags();
    assert!(flags.contains(TcpFlags::ECE | TcpFlags::CWR | TcpFlags::ACK));
}

#[test]
fn s_tcp_022_syn_with_data() {
    let bytes = fin_with(|s| {
        s[13] = 0x02;
        s.extend_from_slice(&[7; 10]);
    });
    assert_eq!(tcp(&bytes).unwrap().payload(), [7; 10]);
}

#[test]
fn s_tcp_023_largest_segment() {
    let bytes = fin_with(|s| s.resize(65_515, 0x42));
    assert_eq!(bytes.len(), 65_535);
    assert_eq!(tcp(&bytes).unwrap().payload().len(), 65_495);
}

#[test]
fn s_tcp_024_window_is_unsigned() {
    let bytes = fin_with(|s| s[14..16].copy_from_slice(&[0xFF, 0xFF]));
    assert_eq!(tcp(&bytes).unwrap().window(), RawWindow(65_535));
}

#[test]
fn s_tcp_025_data_offset_before_checksum() {
    let mut bytes = fin_with(|s| s[12] = 0x40);
    bytes[37] ^= 0x01;
    assert_eq!(tcp(&bytes), Err(TcpError::DataOffset));
}

#[test]
fn s_tcp_026_checksum_before_options() {
    let mut bytes = options(&hex("13 00 00 00"));
    bytes[37] ^= 0x01;
    assert_eq!(tcp(&bytes), Err(TcpError::Checksum));
}

#[test]
fn s_topt_001_no_options() {
    let bytes = frame_ip(V_TCP_FIN);
    let o = tcp(&bytes).unwrap().options();
    assert_eq!((o.mss(), o.window_scale(), o.sack_permitted(), o.timestamps(), o.sack_blocks().len()), (None, None, false, None, 0));
}

#[test]
fn s_topt_002_timestamps_alone() {
    let bytes = options(&hex("01 01 08 0a 00 00 00 02 00 00 01 00"));
    let o = tcp(&bytes).unwrap().options();
    assert_eq!((o.timestamps(), o.mss(), o.sack_permitted()), (Some(Timestamps { value: 2, echo: 256 }), None, false));
}

#[test]
fn s_topt_003_mss_at_odd_offset() {
    assert_eq!(tcp(&options(&hex("01 02 04 05 b4 00 00 00"))).unwrap().options().mss(), Some(1460));
}

#[test]
fn s_topt_004_eol_first() {
    assert_eq!(tcp(&options(&hex("00 02 04 05 b4 00 00 00"))).unwrap().options().mss(), None);
}

#[test]
fn s_topt_005_bytes_after_eol_ignored() {
    assert_eq!(tcp(&options(&hex("02 04 05 b4 00 ff ff ff"))).unwrap().options().mss(), Some(1460));
}

#[test]
fn s_topt_006_mss_length() {
    for area in ["02 03 05 00", "02 05 05 b4 00 00 00 00"] {
        assert_eq!(tcp(&options(&hex(area))), Err(TcpError::OptionLength), "{area}");
    }
}

#[test]
fn s_topt_007_window_scale_length() {
    for area in ["03 02 00 00", "03 04 07 00"] {
        assert_eq!(tcp(&options(&hex(area))), Err(TcpError::OptionLength), "{area}");
    }
}

#[test]
fn s_topt_008_sack_permitted_length() {
    assert_eq!(tcp(&options(&hex("04 03 00 00"))), Err(TcpError::OptionLength));
}

#[test]
fn s_topt_009_timestamps_length() {
    for area in ["08 08 00 00 00 01 00 00", "08 0c 00 00 00 01 00 00 00 00 00 00"] {
        assert_eq!(tcp(&options(&hex(area))), Err(TcpError::OptionLength), "{area}");
    }
}

#[test]
fn s_topt_010_sack_lengths() {
    assert_eq!(tcp(&options(&hex("05 09 00 00 00 00 00 00 00 00 00 00"))), Err(TcpError::SackLength));
    for blocks in 1..=4u8 {
        let mut area = vec![5, 2 + 8 * blocks];
        area.resize(usize::from(2 + 8 * blocks), 0x11);
        assert_eq!(tcp(&options(&area)).unwrap().options().sack_blocks().len(), usize::from(blocks));
    }
}

#[test]
fn s_topt_011_sack_without_blocks() {
    assert_eq!(tcp(&options(&hex("01 01 05 02"))).unwrap().options().sack_blocks().len(), 0);
}

#[test]
fn s_topt_012_md5_skipped() {
    let mut area = vec![19, 18];
    area.resize(18, 0xAB);
    area.extend_from_slice(&hex("01 01 08 0a 00 00 00 02 00 00 01 00"));
    assert_eq!(tcp(&options(&area)).unwrap().options().timestamps(), Some(Timestamps { value: 2, echo: 256 }));
}

#[test]
fn s_topt_013_unknown_kinds_skipped() {
    for area in ["fe 04 12 34", "22 02 01 01"] {
        let bytes = options(&hex(area));
        let o = tcp(&bytes).unwrap().options();
        assert_eq!((o.mss(), o.window_scale(), o.sack_permitted(), o.timestamps()), (None, None, false, None), "{area}");
    }
}

#[test]
fn s_topt_014_length_zero() {
    assert_eq!(tcp(&options(&hex("13 00 00 00"))), Err(TcpError::OptionLength));
}

#[test]
fn s_topt_015_length_one() {
    assert_eq!(tcp(&options(&hex("13 01 00 00"))), Err(TcpError::OptionLength));
}

#[test]
fn s_topt_016_timestamps_past_the_area() {
    assert_eq!(tcp(&options(&hex("08 0a 00 00 00 01 00 00"))), Err(TcpError::OptionOverrun));
}

#[test]
fn s_topt_017_kind_in_last_byte() {
    assert_eq!(tcp(&options(&hex("01 01 01 13"))), Err(TcpError::OptionOverrun));
}

#[test]
fn s_topt_018_repeats() {
    assert_eq!(tcp(&options(&hex("02 04 05 b4 02 04 02 18"))).unwrap().options().mss(), Some(536));
    assert_eq!(tcp(&options(&hex("02 04 05 b4 02 03 02 00"))), Err(TcpError::OptionLength));
}

#[test]
fn s_topt_019_window_scale_above_14() {
    for (shift, effective) in [(0x0F, 14), (0xFF, 14)] {
        let bytes = options(&[1, 3, 3, shift]);
        let scale = tcp(&bytes).unwrap().options().window_scale().unwrap();
        assert_eq!((scale.raw(), scale.effective()), (shift, effective));
    }
}

#[test]
fn s_topt_020_mss_zero() {
    assert_eq!(tcp(&options(&hex("02 04 00 00"))).unwrap().options().mss(), Some(0));
}

#[test]
fn s_topt_021_syn_options_outside_a_syn() {
    let bytes = options(&hex("02 04 05 b4 01 03 03 07 01 01 04 02"));
    let o = tcp(&bytes).unwrap().options();
    assert_eq!((o.mss(), o.window_scale().map(|w| w.raw()), o.sack_permitted()), (Some(1460), Some(7), true));
}

#[test]
fn s_topt_022_sack_in_a_syn() {
    let bytes = fin_options(&hex("01 01 05 0a 00 00 00 01 00 00 00 02"), 0x02);
    let blocks: Vec<_> = tcp(&bytes).unwrap().options().sack_blocks().collect();
    assert_eq!(blocks, [SackBlock { left: SeqNum::new(1), right: SeqNum::new(2) }]);
}

#[test]
fn s_topt_023_timestamps_in_a_syn() {
    let bytes = fin_options(&hex("01 01 08 0a 00 00 00 05 00 00 00 09"), 0x02);
    assert_eq!(tcp(&bytes).unwrap().options().timestamps(), Some(Timestamps { value: 5, echo: 9 }));
}

#[test]
fn s_topt_024_forty_nops() {
    let bytes = options(&[1; 40]);
    let s = tcp(&bytes).unwrap();
    assert_eq!((s.header_len(), s.options().mss(), s.options().timestamps()), (60, None, None));
}

#[test]
fn s_topt_025_syn_without_timestamps() {
    let bytes = hex(V_TCP_SYN_NOTS);
    let o = tcp(&bytes).unwrap().options();
    assert_eq!((o.mss(), o.sack_permitted(), o.timestamps()), (Some(1460), true, None));
    assert_eq!(o.window_scale().map(|w| w.raw()), Some(7));
}

fn syn_options(timestamps: Option<Timestamps>) -> SynOptions {
    SynOptions { mss: Some(1460), sack_permitted: true, timestamps, window_scale: Some(WindowShift::new(7).unwrap()) }
}

fn segment<'a>(source: u16, destination: u16, sequence: u32, control: Control<'a>, window: u16, data: &'a [u8]) -> TcpBuilder<'a> {
    TcpBuilder { source: port(source), destination: port(destination), sequence: SeqNum::new(sequence), control, window: RawWindow(window), data }
}

fn a_to_b(segment: TcpBuilder<'_>) -> Vec<u8> {
    emit!(&datagram(IP_A, IP_B, segment)).unwrap()
}

fn b_to_a(segment: TcpBuilder<'_>) -> Vec<u8> {
    emit!(&datagram(IP_B, IP_A, segment)).unwrap()
}

fn a_syn() -> TcpBuilder<'static> {
    segment(49152, 80, 0x0102_0304, Control::Syn(syn_options(Some(Timestamps { value: 1, echo: 0 }))), 64240, &[])
}

fn a_data() -> TcpBuilder<'static> {
    let options = EstablishedOptions { timestamps: Some(Timestamps { value: 2, echo: 256 }), sack: &[] };
    let control = Control::Ack { acknowledgment: SeqNum::new(0xA0B0_C0D1), push: true, fin: false, options };
    segment(49152, 80, 0x0102_0305, control, 502, b"GET / HTTP/1.0\r\n\r\n")
}

fn b_sack(blocks: &[SackBlock], echo: u32, value: u32, ack: u32) -> Vec<u8> {
    let options = EstablishedOptions { timestamps: Some(Timestamps { value, echo }), sack: blocks };
    let control = Control::Ack { acknowledgment: SeqNum::new(ack), push: false, fin: false, options };
    b_to_a(segment(80, 49152, 0xA0B0_C0D1, control, 509, &[]))
}

fn block(left: u32, right: u32) -> SackBlock {
    SackBlock { left: SeqNum::new(left), right: SeqNum::new(right) }
}

fn b_rst(timestamps: Option<Timestamps>) -> Vec<u8> {
    let control = Control::Rst { acknowledgment: None, options: EstablishedOptions { timestamps, sack: &[] } };
    b_to_a(segment(80, 49152, 0xA0B0_C0D1, control, 0, &[]))
}

#[test]
fn s_tser_001_emit_syn() {
    let mut out = junk(200);
    let frame = FrameBuilder { destination: MAC_B, source: mac_a() };
    assert_eq!(frame.emit(&datagram(IP_A, IP_B, a_syn()), &mut out).unwrap(), hex(V_TCP_SYN));
}

#[test]
fn s_tser_002_emit_syn_ack() {
    let options = syn_options(Some(Timestamps { value: 256, echo: 1 }));
    let control = Control::SynAck { acknowledgment: SeqNum::new(0x0102_0305), options };
    assert_eq!(b_to_a(segment(80, 49152, 0xA0B0_C0D0, control, 65160, &[])), frame_ip(V_TCP_SYNACK));
}

#[test]
fn s_tser_003_emit_syn_without_timestamps() {
    let built = a_to_b(segment(49154, 80, 0x0506_0708, Control::Syn(syn_options(None)), 64240, &[]));
    assert_eq!(built, hex(V_TCP_SYN_NOTS));
    assert_eq!(built[40..], hex("02 04 05 b4 01 01 04 02 01 03 03 07")[..]);
}

#[test]
fn s_tser_004_emit_syn_layouts() {
    let options = SynOptions { sack_permitted: false, ..syn_options(Some(Timestamps { value: 1, echo: 0 })) };
    let built = a_to_b(segment(49152, 80, 1, Control::Syn(options), 64240, &[]));
    assert_eq!(built[32], 0xA0);
    assert_eq!(built[40..], hex("02 04 05 b4 01 01 08 0a 00 00 00 01 00 00 00 00 01 03 03 07")[..]);
    let mss_only = SynOptions { mss: Some(1460), ..SynOptions::default() };
    let built = a_to_b(segment(49152, 80, 1, Control::Syn(mss_only), 64240, &[]));
    assert_eq!((built[32], &built[40..]), (0x60, &hex("02 04 05 b4")[..]));
}

#[test]
fn s_tser_005_emit_data() {
    assert_eq!(a_to_b(a_data()), frame_ip(V_TCP_DATA));
}

#[test]
fn s_tser_006_emit_sack() {
    assert_eq!(b_sack(&[block(0x0102_0B6F, 0x0102_101F)], 2, 257, 0x0102_0317), frame_ip(V_TCP_SACK));
    let three = [block(0x0102_2000, 0x0102_25B4), block(0x0102_3000, 0x0102_35B4), block(0x0102_4000, 0x0102_45B4)];
    let built = b_sack(&three, 3, 258, 0x0102_1B6F);
    assert_eq!(built, hex(V_TCP_SACK3));
    assert_eq!(built.len() - 40, 40);
}

#[test]
fn s_tser_007_sack_block_limits() {
    let blocks = [block(1, 2), block(3, 4), block(5, 6), block(7, 8), block(9, 10)];
    let with = |timestamps, sack| {
        let control = Control::Ack { acknowledgment: SeqNum::new(1), push: false, fin: false, options: EstablishedOptions { timestamps, sack } };
        emit!(&datagram(IP_B, IP_A, segment(80, 49152, 1, control, 509, &[])))
    };
    let timestamps = Some(Timestamps { value: 1, echo: 1 });
    assert_eq!(with(timestamps, &blocks[..4]), Err(BuildError::TcpTooManySackBlocks));
    let built = with(None, &blocks[..4]).unwrap();
    assert_eq!(built.len() - 40, 36);
    assert_eq!(built[40..44], [0x01, 0x01, 0x05, 0x22]);
    assert_eq!(with(None, &blocks), Err(BuildError::TcpTooManySackBlocks));
}

#[test]
fn s_tser_009_window_shift_limit() {
    assert_eq!(WindowShift::new(15), Err(BuildError::TcpWindowScaleTooLarge));
    assert_eq!(WindowShift::new(14).map(WindowShift::get), Ok(14));
}

#[test]
fn s_tser_010_emit_rst() {
    assert_eq!(b_rst(None), frame_ip(V_TCP_RST));
}

#[test]
fn s_tser_011_emit_rst_with_timestamps() {
    let built = b_rst(Some(Timestamps { value: 0, echo: 77 }));
    assert_eq!(built[40..], hex("01 01 08 0a 00 00 00 00 00 00 00 4d")[..]);
}

#[test]
fn s_tser_012_emitted_reserved_and_urgent_are_zero() {
    let syn_ack = Control::SynAck { acknowledgment: SeqNum::new(1), options: syn_options(None) };
    let fin = Control::Ack { acknowledgment: SeqNum::new(1), push: false, fin: true, options: EstablishedOptions::default() };
    let rst_ack = Control::Rst { acknowledgment: Some(SeqNum::new(1)), options: EstablishedOptions::default() };
    let built = [
        a_to_b(a_syn()),
        a_to_b(a_data()),
        b_rst(None),
        b_to_a(segment(80, 49152, 1, syn_ack, 1, &[])),
        b_to_a(segment(80, 49152, 1, fin, 1, &[])),
        b_to_a(segment(80, 49152, 1, rst_ack, 1, &[])),
    ];
    for ip in &built {
        let s = tcp(ip).unwrap();
        assert_eq!(s.header()[12] & 0x0F, 0);
        assert!(!s.flags().contains(TcpFlags::URG));
        assert_eq!(s.header()[18..20], [0, 0]);
    }
}

#[test]
fn s_tser_013_emit_odd_segment() {
    let control = Control::Ack { acknowledgment: SeqNum::new(0xA0B0_C0D1), push: true, fin: false, options: EstablishedOptions::default() };
    let built = a_to_b(segment(49152, 80, 0x0102_0317, control, 502, b"abc"));
    assert_eq!(built[20..], hex(V_TCP_ODD));
}

#[test]
fn s_tser_014_emit_over_junk() {
    let mut out = junk(200);
    assert_eq!(datagram(IP_A, IP_B, a_data()).emit(&mut out).unwrap(), frame_ip(V_TCP_DATA));
}

#[test]
fn s_tser_015_too_long() {
    let control = Control::Ack { acknowledgment: SeqNum::new(1), push: false, fin: false, options: EstablishedOptions::default() };
    let data = vec![0; 65_496];
    assert_eq!(emit!(&datagram(IP_A, IP_B, segment(1, 2, 1, control, 1, &data))), Err(BuildError::TcpTooLong));
    assert!(emit!(&datagram(IP_A, IP_B, segment(1, 2, 1, control, 1, &data[1..]))).is_ok());
}
