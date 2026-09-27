mod common;

use common::*;
use toyos_net_wire::checksum::{Accumulator, Checksum, PseudoHeader, Sum};
use toyos_net_wire::ipv4::Protocol;

fn udp_dns_ip() -> Vec<u8> {
    ip_of(&hex(V_UDP_DNS))
}

#[test]
fn s_ck_001_rfc1071_worked_example() {
    let bytes = hex("00 01 f2 03 f4 f5 f6 f7");
    assert_eq!(Sum::of(&bytes).value(), 0xDDF2);
    assert_eq!(Checksum::of(&bytes).value(), 0x220D);
}

#[test]
fn s_ck_002_empty_input() {
    assert_eq!(Sum::of(&[]).value(), 0x0000);
    assert_eq!(Checksum::of(&[]).value(), 0xFFFF);
}

#[test]
fn s_ck_003_zeros() {
    assert_eq!(Checksum::of(&[0, 0, 0, 0]).value(), 0xFFFF);
}

#[test]
fn s_ck_004_odd_length_pads_with_zero() {
    assert_eq!(Checksum::of(&[1, 2, 3]).value(), 0xFBFD);
    assert_eq!(Checksum::of(&[1, 2, 3, 0]).value(), 0xFBFD);
}

#[test]
fn s_ck_005_streaming_is_split_independent() {
    assert_eq!(Accumulator::new().feed(&[1]).feed(&[2, 3]).sum().checksum().value(), 0xFBFD);
    let ip = udp_dns_ip();
    let header = &ip[..20];
    let whole = Sum::of(header);
    for split in 0..=20 {
        let (a, b) = header.split_at(split);
        assert_eq!(Accumulator::new().feed(a).feed(b).sum(), whole, "split at {split}");
    }
}

#[test]
fn s_ck_006_carry_of_a_carry() {
    let bytes = hex("ff ff ff ff 00 01");
    assert_eq!(Sum::of(&bytes).value(), 0x0001);
    assert_eq!(Checksum::of(&bytes).value(), 0xFFFE);
}

#[test]
fn s_ck_007_many_words_of_ones() {
    let bytes = vec![0xFF; 131_072 * 2];
    assert_eq!(Sum::of(&bytes).value(), 0xFFFF);
    assert_eq!(Checksum::of(&bytes).value(), 0x0000);
}

#[test]
fn s_ck_008_long_pattern_even_and_odd() {
    let bytes: Vec<u8> = (0..65_536u32).map(|i| ((7 * i + 3) % 256) as u8).collect();
    assert_eq!(Checksum::of(&bytes).value(), 0x3FC0);
    assert_eq!(Checksum::of(&bytes[..65_535]).value(), 0x40BC);
}

#[test]
fn s_ck_009_verification_by_summing() {
    assert!(Sum::of(&udp_dns_ip()[..20]).verifies());
}

#[test]
fn s_ck_010_negative_zero_verifies() {
    assert!(Sum::of(&hex("cd 7a 32 85 ff ff")).verifies());
    assert!(Sum::of(&hex("cd 7a 32 85 00 00")).verifies());
}

#[test]
fn s_ck_017_pseudo_header() {
    let udp = payload_of(&udp_dns_ip());
    assert_eq!(udp.len(), 37);
    let mut given = hex("c0 00 02 01 c0 00 02 35 00 11 00 25");
    given.extend_from_slice(&udp);
    assert_eq!(oracle_sum(&given), 0xFFFF);
    let pseudo = PseudoHeader { source: IP_A, destination: IP_DNS, protocol: Protocol::Udp, length: 37 };
    assert!(pseudo.accumulator().feed(&udp).sum().verifies());
}

#[test]
fn s_ck_018_tcp_length_is_the_transport_length() {
    let tcp = payload_of(&ip_of(&hex(V_TCP_SYN)));
    let with = |length| PseudoHeader { source: IP_A, destination: IP_B, protocol: Protocol::Tcp, length };
    assert!(with(40).accumulator().feed(&tcp).sum().verifies());
    assert!(!with(60).accumulator().feed(&tcp).sum().verifies());
}

#[test]
fn s_ck_019_udp_zero_is_sent_as_ffff() {
    let mut udp = hex(V_UDP_FFFF);
    let pseudo = PseudoHeader { source: IP_A, destination: IP_B, protocol: Protocol::Udp, length: 10 };
    assert!(pseudo.accumulator().feed(&udp).sum().verifies());
    udp[6..8].copy_from_slice(&[0, 0]);
    assert_eq!(pseudo.accumulator().feed(&udp).sum().checksum().value(), 0x0000);
}

#[test]
fn s_ck_021_every_header_bit_flip_is_caught() {
    let header = udp_dns_ip()[..20].to_vec();
    for bit in 0..160 {
        let mut flipped = header.clone();
        flipped[bit / 8] ^= 0x80 >> (bit % 8);
        assert!(!Sum::of(&flipped).verifies(), "bit {bit}");
    }
}

#[test]
fn native_order_sum_matches_the_oracle_at_every_length_and_split() {
    let mut rng = Rng(0xC0FFEE);
    for _ in 0..4096 {
        let bytes = rng.bytes(300);
        let (a, b) = bytes.split_at(rng.below(bytes.len() + 1));
        assert_eq!(Accumulator::new().feed(a).feed(b).sum().value(), oracle_sum(&bytes), "{} bytes split at {}", bytes.len(), a.len());
    }
}
