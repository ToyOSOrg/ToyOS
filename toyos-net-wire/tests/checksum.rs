//! The Internet checksum's scenarios, CK-01 to CK-21.

mod common;

use common::*;
use toyos_net_wire::checksum::{Accumulator, Checksum, PseudoHeader, Sum};
use toyos_net_wire::ipv4::Protocol;
use toyos_net_wire::udp::UdpChecksum;

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
fn s_ck_011_rfc1624_example_equation_3() {
    let updated = Checksum::from_field(0xDD2F).replace([0x55, 0x55], [0x32, 0x85]);
    assert_eq!(updated.value(), 0x0000);
    assert_eq!(Checksum::of(&hex("cd 7a 32 85")), updated);
    // Equation 2, `HC + m + ~m'`, gives the other zero.
    assert_eq!(oracle_sum(&[0xDD, 0x2F, 0x55, 0x55, 0xCD, 0x7A]), 0xFFFF);
}

/// The IPv4 header checksum after `edit` changes the header, both ways.
fn incremental_equals_scratch(edit: impl Fn(&mut [u8]), update: impl Fn(Checksum) -> Checksum) {
    let mut header = udp_dns_ip()[..20].to_vec();
    let before = Checksum::from_field(u16::from_be_bytes([header[10], header[11]]));
    edit(&mut header);
    header[10..12].copy_from_slice(&[0, 0]);
    assert_eq!(update(before).value(), oracle_checksum(&header));
}

#[test]
fn s_ck_012_incremental_ttl_decrement() {
    incremental_equals_scratch(|h| h[8] = 63, |c| c.replace([0x40, 0x11], [0x3F, 0x11]));
}

#[test]
fn s_ck_013_incremental_same_value_is_unchanged() {
    let ip = udp_dns_ip();
    let before = Checksum::from_field(0xB67D);
    assert_eq!(before.replace([ip[8], ip[9]], [ip[8], ip[9]]), before);
}

#[test]
fn s_ck_014_incremental_address() {
    incremental_equals_scratch(
        |h| h[19] = 54,
        |c| c.replace_address(IP_DNS, std::net::Ipv4Addr::new(192, 0, 2, 54)),
    );
}

#[test]
fn s_ck_015_odd_offset_byte_updates_its_word() {
    incremental_equals_scratch(|h| h[9] = 6, |c| c.replace([0x40, 0x11], [0x40, 0x06]));
    // The byte taken as a word of its own gives a different, wrong answer.
    let mut header = udp_dns_ip()[..20].to_vec();
    header[9] = 6;
    header[10..12].copy_from_slice(&[0, 0]);
    assert_ne!(Checksum::from_field(0xB67D).replace([0x11, 0], [0x06, 0]).value(), oracle_checksum(&header));
}

/// A deterministic generator, so a failing case reproduces.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

#[test]
fn s_ck_016_incremental_property() {
    let mut rng = Rng(0xC0FFEE);
    for case in 0..4096 {
        let len = rng.below(65);
        let mut bytes: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
        let start = rng.below(len / 2 + 1) * 2;
        let end = if rng.below(2) == 0 && len % 2 == 1 { len } else { start + rng.below((len - start) / 2 + 1) * 2 };
        let old = bytes[start..end].to_vec();
        let mut checksum = Checksum::of(&bytes);
        for byte in &mut bytes[start..end] {
            *byte = rng.next() as u8;
        }
        for (i, (o, n)) in old.chunks(2).zip(bytes[start..end].chunks(2)).enumerate() {
            // A final odd byte is the high byte of a zero-padded word.
            let word = |c: &[u8]| [c[0], *c.get(1).unwrap_or(&0)];
            assert!(o.len() == 2 || start + 2 * i + 1 == len);
            checksum = checksum.replace(word(o), word(n));
        }
        if bytes.iter().all(|&b| b == 0) {
            // RFC 1624 §3 rests on a nonzero byte in the covered data: with
            // none, equation 3 may give +0 where recomputation gives -0.
            assert_eq!(oracle_checksum(&bytes), 0xFFFF);
            assert!(matches!(checksum.value(), 0x0000 | 0xFFFF), "case {case}");
        } else {
            assert_eq!(checksum.value(), oracle_checksum(&bytes), "case {case}: {len} bytes, span {start}..{end}");
        }
    }
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
fn s_ck_020_udp_incremental_update_keeps_the_zero_rules() {
    let ip = hex(V_UDP_HI);
    let udp = &ip[20..];
    let field = u16::from_be_bytes([udp[6], udp[7]]);
    let old = [udp[8], udp[9]];
    // Search for the payload word whose update makes the checksum compute to zero.
    let new = (0..=u16::MAX)
        .map(u16::to_be_bytes)
        .find(|&new| UdpChecksum::from_field(field).replace(old, new) == UdpChecksum::Present(Checksum::from_field(0)))
        .unwrap();
    assert_eq!(UdpChecksum::from_field(field).replace(old, new).field(), 0xFFFF);
    let mut changed = udp.to_vec();
    changed[8..10].copy_from_slice(&new);
    changed[6..8].copy_from_slice(&[0, 0]);
    let mut covered = pseudo(IP_B, IP_A, 17, 10);
    covered.extend_from_slice(&changed);
    assert_eq!(oracle_checksum(&covered), 0x0000);
    assert_eq!(UdpChecksum::from_field(0).replace(old, new).field(), 0x0000);
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
