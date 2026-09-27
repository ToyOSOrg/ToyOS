#![allow(dead_code)]

use std::net::Ipv4Addr;

use toyos_net_wire::ethernet::{IndividualMac, MacAddr};

pub const MAC_A: MacAddr = MacAddr([0x02, 0, 0, 0, 0, 0x0a]);
pub const MAC_B: MacAddr = MacAddr([0x02, 0, 0, 0, 0, 0x0b]);
pub const MAC_ROUTER: MacAddr = MacAddr([0x02, 0, 0, 0, 0, 0xfe]);
pub const IP_A: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 1);
pub const IP_B: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 2);
pub const IP_DNS: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 53);
pub const IP_ROUTER: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 254);
pub const IP_REMOTE: Ipv4Addr = Ipv4Addr::new(198, 51, 100, 7);

pub fn mac_a() -> IndividualMac {
    IndividualMac::new(MAC_A).unwrap()
}

pub fn mac_b() -> IndividualMac {
    IndividualMac::new(MAC_B).unwrap()
}

pub fn hex(text: &str) -> Vec<u8> {
    text.split_whitespace().map(|byte| u8::from_str_radix(byte, 16).unwrap()).collect()
}

/// Written apart from the crate's checksum, so a fixed-up mutation is judged by a second implementation.
pub fn oracle_sum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    for pair in data.chunks(2) {
        let word = u32::from(pair[0]) << 8 | u32::from(*pair.get(1).unwrap_or(&0));
        sum += word;
        while sum > 0xFFFF {
            sum = (sum & 0xFFFF) + (sum >> 16);
        }
    }
    sum as u16
}

pub fn oracle_checksum(data: &[u8]) -> u16 {
    !oracle_sum(data)
}

pub fn pseudo(source: Ipv4Addr, destination: Ipv4Addr, protocol: u8, length: u16) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&source.octets());
    bytes.extend_from_slice(&destination.octets());
    bytes.extend_from_slice(&[0, protocol]);
    bytes.extend_from_slice(&length.to_be_bytes());
    bytes
}

fn header_len(ip: &[u8]) -> usize {
    usize::from(ip[0] & 0x0F) * 4
}

pub fn fix_ip(ip: &mut [u8]) {
    let len = header_len(ip);
    ip[10..12].copy_from_slice(&[0, 0]);
    let checksum = oracle_checksum(&ip[..len]);
    ip[10..12].copy_from_slice(&checksum.to_be_bytes());
}

pub fn fix_udp(source: Ipv4Addr, destination: Ipv4Addr, udp: &mut [u8]) {
    let length = u16::from_be_bytes([udp[4], udp[5]]);
    udp[6..8].copy_from_slice(&[0, 0]);
    let mut covered = pseudo(source, destination, 17, length);
    covered.extend_from_slice(&udp[..usize::from(length)]);
    let checksum = match oracle_checksum(&covered) {
        0 => 0xFFFF,
        checksum => checksum,
    };
    udp[6..8].copy_from_slice(&checksum.to_be_bytes());
}

pub fn fix_tcp(source: Ipv4Addr, destination: Ipv4Addr, tcp: &mut [u8]) {
    tcp[16..18].copy_from_slice(&[0, 0]);
    let mut covered = pseudo(source, destination, 6, tcp.len() as u16);
    covered.extend_from_slice(tcp);
    let checksum = oracle_checksum(&covered);
    tcp[16..18].copy_from_slice(&checksum.to_be_bytes());
}

pub fn fix_message(message: &mut [u8]) {
    message[2..4].copy_from_slice(&[0, 0]);
    let checksum = oracle_checksum(message);
    message[2..4].copy_from_slice(&checksum.to_be_bytes());
}

pub fn fix_datagram(ip: &mut [u8]) {
    let hlen = header_len(ip);
    let total = usize::from(u16::from_be_bytes([ip[2], ip[3]])).min(ip.len());
    let source = Ipv4Addr::new(ip[12], ip[13], ip[14], ip[15]);
    let destination = Ipv4Addr::new(ip[16], ip[17], ip[18], ip[19]);
    let protocol = ip[9];
    if hlen <= total {
        let payload = &mut ip[hlen..total];
        match protocol {
            1 | 2 if payload.len() >= 4 => fix_message(payload),
            6 if payload.len() >= 20 => fix_tcp(source, destination, payload),
            17 if payload.len() >= 8 && (8..=payload.len()).contains(&usize::from(u16::from_be_bytes([payload[4], payload[5]]))) => {
                fix_udp(source, destination, payload)
            }
            _ => {}
        }
    }
    if hlen >= 20 && hlen <= ip.len() {
        fix_ip(ip);
    }
}

/// Built here, not by the crate.
pub fn ipv4(source: Ipv4Addr, destination: Ipv4Addr, protocol: u8, payload: &[u8]) -> Vec<u8> {
    let total = (20 + payload.len()) as u16;
    let mut ip = vec![0x45, 0];
    ip.extend_from_slice(&total.to_be_bytes());
    ip.extend_from_slice(&[0, 0, 0x40, 0, 64, protocol, 0, 0]);
    ip.extend_from_slice(&source.octets());
    ip.extend_from_slice(&destination.octets());
    fix_ip(&mut ip);
    ip.extend_from_slice(payload);
    ip
}

pub fn ip_of(frame: &[u8]) -> Vec<u8> {
    let ip = &frame[14..];
    ip[..usize::from(u16::from_be_bytes([ip[2], ip[3]]))].to_vec()
}

pub fn payload_of(ip: &[u8]) -> Vec<u8> {
    ip[header_len(ip)..usize::from(u16::from_be_bytes([ip[2], ip[3]]))].to_vec()
}

/// So a builder that skips a byte shows it.
pub fn junk(len: usize) -> Vec<u8> {
    vec![0xAA; len]
}

pub const V_ARP_REQ: &str = "
ff ff ff ff ff ff 02 00 00 00 00 0a 08 06 00 01
08 00 06 04 00 01 02 00 00 00 00 0a c0 00 02 01
00 00 00 00 00 00 c0 00 02 02 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00";

pub const V_ARP_REPLY: &str = "
02 00 00 00 00 0a 02 00 00 00 00 0b 08 06 00 01
08 00 06 04 00 02 02 00 00 00 00 0b c0 00 02 02
02 00 00 00 00 0a c0 00 02 01 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00";

pub const V_ARP_PROBE: &str = "
ff ff ff ff ff ff 02 00 00 00 00 0a 08 06 00 01
08 00 06 04 00 01 02 00 00 00 00 0a 00 00 00 00
00 00 00 00 00 00 c0 00 02 01 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00";

pub const V_ARP_ANNOUNCE: &str = "
ff ff ff ff ff ff 02 00 00 00 00 0a 08 06 00 01
08 00 06 04 00 01 02 00 00 00 00 0a c0 00 02 01
00 00 00 00 00 00 c0 00 02 01 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00";

pub const V_ETH_8021Q: &str = "
ff ff ff ff ff ff 02 00 00 00 00 0a 81 00 00 64
08 06 00 01 08 00 06 04 00 01 02 00 00 00 00 0a
c0 00 02 01 00 00 00 00 00 00 c0 00 02 02 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00";

pub const V_ETH_PRIO: &str = "
ff ff ff ff ff ff 02 00 00 00 00 0a 81 00 a0 00
08 06 00 01 08 00 06 04 00 01 02 00 00 00 00 0a
c0 00 02 01 00 00 00 00 00 00 c0 00 02 02 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00";

pub const V_ETH_QINQ: &str = "
ff ff ff ff ff ff 02 00 00 00 00 0a 88 a8 00 0a
81 00 00 14 08 06 00 01 08 00 06 04 00 01 02 00
00 00 00 0a c0 00 02 01 00 00 00 00 00 00 c0 00
02 02 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00";

pub const V_UDP_DNS: &str = "
02 00 00 00 00 fe 02 00 00 00 00 0a 08 00 45 00
00 39 00 00 40 00 40 11 b6 7d c0 00 02 01 c0 00
02 35 c0 00 00 35 00 25 d9 95 12 34 01 00 00 01
00 00 00 00 00 00 07 65 78 61 6d 70 6c 65 03 63
6f 6d 00 00 01 00 01";

pub const V_UDP_FFFF: &str = "03 e8 07 d0 00 0a ff ff 70 1e";

pub const V_TCP_SYN: &str = "
02 00 00 00 00 0b 02 00 00 00 00 0a 08 00 45 00
00 3c 00 00 40 00 40 06 b6 b8 c0 00 02 01 c0 00
02 02 c0 00 00 50 01 02 03 04 00 00 00 00 a0 02
fa f0 04 b4 00 00 02 04 05 b4 04 02 08 0a 00 00
00 01 00 00 00 00 01 03 03 07";

pub const V_TCP_SYNACK: &str = "
02 00 00 00 00 0a 02 00 00 00 00 0b 08 00 45 00
00 3c 00 00 40 00 40 06 b6 b8 c0 00 02 02 c0 00
02 01 00 50 c0 00 a0 b0 c0 d0 01 02 03 05 a0 12
fe 88 9e 89 00 00 02 04 05 b4 04 02 08 0a 00 00
01 00 00 00 00 01 01 03 03 07";

pub const V_TCP_DATA: &str = "
02 00 00 00 00 0b 02 00 00 00 00 0a 08 00 45 00
00 46 00 00 40 00 40 06 b6 ae c0 00 02 01 c0 00
02 02 c0 00 00 50 01 02 03 05 a0 b0 c0 d1 80 18
01 f6 eb 2d 00 00 01 01 08 0a 00 00 00 02 00 00
01 00 47 45 54 20 2f 20 48 54 54 50 2f 31 2e 30
0d 0a 0d 0a";

pub const V_TCP_SACK: &str = "
02 00 00 00 00 0a 02 00 00 00 00 0b 08 00 45 00
00 40 00 00 40 00 40 06 b6 b4 c0 00 02 02 c0 00
02 01 00 50 c0 00 a0 b0 c0 d1 01 02 03 17 b0 10
01 fd 76 24 00 00 01 01 08 0a 00 00 01 01 00 00
00 02 01 01 05 0a 01 02 0b 6f 01 02 10 1f";

pub const V_TCP_RST: &str = "
02 00 00 00 00 0a 02 00 00 00 00 0b 08 00 45 00
00 28 00 00 40 00 40 06 b6 cc c0 00 02 02 c0 00
02 01 00 50 c0 00 a0 b0 c0 d1 00 00 00 00 50 04
00 00 0a 0a 00 00 00 00 00 00 00 00";

pub const V_TCP_FIN: &str = "
02 00 00 00 00 0b 02 00 00 00 00 0a 08 00 45 00
00 28 00 00 40 00 40 06 b6 cc c0 00 02 01 c0 00
02 02 c0 00 00 50 01 02 03 17 a0 b0 c0 d1 50 11
01 f6 03 ee 00 00 00 00 00 00 00 00";

pub const V_TCP_SYN_NOTS: &str = "
45 00 00 34 00 00 40 00 40 06 b6 c0 c0 00 02 01
c0 00 02 02 c0 02 00 50 05 06 07 08 00 00 00 00
80 02 fa f0 23 bc 00 00 02 04 05 b4 01 01 04 02
01 03 03 07";

pub const V_TCP_SACK3: &str = "
45 00 00 50 00 00 40 00 40 06 b6 a4 c0 00 02 02
c0 00 02 01 00 50 c0 00 a0 b0 c0 d1 01 02 1b 6f
f0 10 01 fd 04 13 00 00 01 01 08 0a 00 00 01 02
00 00 00 03 01 01 05 1a 01 02 20 00 01 02 25 b4
01 02 30 00 01 02 35 b4 01 02 40 00 01 02 45 b4";

pub const V_TCP_ODD: &str = "
c0 00 00 50 01 02 03 17 a0 b0 c0 d1 50 18 01 f6
3f 81 00 00 61 62 63";

pub const V_ICMP_ECHO: &str = "
45 00 00 24 00 00 40 00 40 01 b6 d5 c0 00 02 01
c0 00 02 02 08 00 66 68 00 01 00 01 61 62 63 64
65 66 67 68";

pub const V_ICMP_REPLY: &str = "
45 00 00 24 00 00 40 00 40 01 b6 d5 c0 00 02 02
c0 00 02 01 00 00 6e 68 00 01 00 01 61 62 63 64
65 66 67 68";

pub const V_ICMP_PORT_UNREACH: &str = "
45 00 00 38 00 00 40 00 40 01 b6 8e c0 00 02 35
c0 00 02 01 03 03 63 0c 00 00 00 00 45 00 00 39
00 00 40 00 40 11 b6 7d c0 00 02 01 c0 00 02 35
c0 00 00 35 00 25 d9 95";

pub const V_ICMP_FRAG_NEEDED: &str = "
45 00 00 38 00 00 40 00 40 01 b5 c5 c0 00 02 fe
c0 00 02 01 03 04 13 a5 00 00 05 78 45 00 05 dc
00 00 40 00 40 06 48 e0 c0 00 02 01 c6 33 64 07
c0 01 01 bb 11 11 11 11";

pub const V_UDP_HI: &str = "
45 00 00 1e 00 00 40 00 40 11 b6 cb c0 00 02 02
c0 00 02 01 13 88 13 89 00 0a ec 5b 68 69";

pub const V_ICMP_PORT_UNREACH_GEN: &str = "
45 00 00 3a 00 00 40 00 40 01 b6 bf c0 00 02 01
c0 00 02 02 03 03 81 1c 00 00 00 00 45 00 00 1e
00 00 40 00 40 11 b6 cb c0 00 02 02 c0 00 02 01
13 88 13 89 00 0a ec 5b 68 69";

pub const V_ICMP_TIME_EXCEEDED: &str = "
45 00 00 38 00 00 40 00 40 01 b5 c5 c0 00 02 fe
c0 00 02 01 0b 00 11 21 00 00 00 00 45 00 05 dc
00 00 40 00 40 06 48 e0 c0 00 02 01 c6 33 64 07
c0 01 01 bb 11 11 11 11";

pub const V_ICMP_PARAM_PROBLEM: &str = "
0c 00 fc 20 14 00 00 00 45 00 05 dc 00 00 40 00
40 06 48 e0 c0 00 02 01 c6 33 64 07 c0 01 01 bb
11 11 11 11";

pub const V_ICMP_TIMESTAMP_REQ: &str = "
0d 00 f1 f7 00 07 00 01 01 00 00 00 00 00 00 00
00 00 00 00";

pub const V_ICMP_REDIRECT: &str = "
05 01 54 21 c0 00 02 fe 45 00 05 dc 00 00 40 00
40 06 48 e0 c0 00 02 01 c6 33 64 07 c0 01 01 bb
11 11 11 11";

pub const V_IGMP_REPORT: &str = "
01 00 5e 00 00 fb 02 00 00 00 00 0a 08 00 46 00
00 20 00 00 40 00 01 02 41 db c0 00 02 01 e0 00
00 fb 94 04 00 00 16 00 09 04 e0 00 00 fb 00 00
00 00 00 00 00 00 00 00 00 00 00 00";

pub const V_IGMP_QUERY_V2: &str = "
46 00 00 20 00 00 00 00 01 02 81 d8 c0 00 02 fe
e0 00 00 01 94 04 00 00 11 64 ee 9b 00 00 00 00";

pub const V_IGMP_QUERY_V1: &str = "11 00 ee ff 00 00 00 00";

pub const V_IGMP_QUERY_V3: &str = "11 64 ec 1e 00 00 00 00 02 7d 00 00";

pub const V_IGMP_QUERY_V3_EXP: &str = "11 8c eb f6 00 00 00 00 02 7d 00 00";

pub const V_IGMP_QUERY_V3_SRC: &str = "
11 64 87 0c e0 00 00 fb 02 7d 00 02 c0 00 02 09
c0 00 02 0a";

pub const V_IGMP_LEAVE: &str = "17 00 08 04 e0 00 00 fb";

pub const V_IGMP_REPORT_LONG: &str = "16 00 6b 66 e0 00 00 fb de ad be ef";

pub const V_IP_RR: &str = "
47 00 00 26 00 00 40 00 40 11 a9 bc c0 00 02 02
c0 00 02 01 07 07 04 00 00 00 00 00 13 88 13 89
00 0a ec 5b 68 69";

pub const V_IP_NOPS: &str = "
46 00 00 22 00 00 40 00 40 11 b3 c6 c0 00 02 02
c0 00 02 01 01 01 01 00 13 88 13 89 00 0a ec 5b
68 69";

pub const V_IP_LSRR: &str = "
47 00 00 26 00 00 40 00 40 11 2e f9 c0 00 02 02
c0 00 02 01 83 07 04 c0 00 02 fe 00 13 88 13 89
00 0a ec 5b 68 69";

pub const V_IP_FRAG_FIRST: &str = "
45 00 00 1e 4d 2f 20 00 40 11 89 9c c0 00 02 02
c0 00 02 01 13 88 13 89 00 0a ec 5b 68 69";

pub const V_IP_FRAG_LAST: &str = "
45 00 00 18 4d 2f 00 b9 40 11 a8 e9 c0 00 02 02
c0 00 02 01 74 61 69 6c";

pub const V_IP_DSCP: &str = "
45 bb 00 1e 00 00 40 00 40 11 b6 10 c0 00 02 02
c0 00 02 01 13 88 13 89 00 0a ec 5b 68 69";

pub const V_IP_MIN: &str = "
45 00 00 14 00 00 40 00 40 fd b5 e9 c0 00 02 02
c0 00 02 01";

// ip.md §18: the IGMPv3 report builder's vectors (§16.3 W-1).

pub const V_IGMP3_JOIN: &str = "
01 00 5e 00 00 16 02 00 00 00 00 0a 08 00 46 c0
00 28 00 00 40 00 01 02 41 f8 c0 00 02 01 e0 00
00 16 94 04 00 00 22 00 f9 02 00 00 00 01 04 00
00 00 e0 00 00 fb 00 00 00 00 00 00";

pub const V_IGMP3_LEAVE: &str = "
46 c0 00 28 00 00 40 00 01 02 41 f8 c0 00 02 01
e0 00 00 16 94 04 00 00 22 00 fa 02 00 00 00 01
03 00 00 00 e0 00 00 fb";

pub const V_IGMP3_CURRENT: &str = "
46 c0 00 28 00 00 40 00 01 02 41 f8 c0 00 02 01
e0 00 00 16 94 04 00 00 22 00 fb 02 00 00 00 01
02 00 00 00 e0 00 00 fb";

pub const V_IGMP3_JOIN_UNSPEC: &str = "
46 c0 00 28 00 00 40 00 01 02 03 fa 00 00 00 00
e0 00 00 16 94 04 00 00 22 00 f9 02 00 00 00 01
04 00 00 00 e0 00 00 fb";

pub const V_ICMP_PROTO_UNREACH_GEN: &str = "
45 00 00 30 00 00 40 00 40 01 b6 c9 c0 00 02 01
c0 00 02 02 03 02 fc fd 00 00 00 00 45 00 00 14
00 00 40 00 40 fd b5 e9 c0 00 02 02 c0 00 02 01";

pub const FRAMES: &[&str] = &[
    V_ARP_REQ, V_ARP_REPLY, V_ARP_PROBE, V_ARP_ANNOUNCE, V_ETH_8021Q, V_ETH_PRIO, V_ETH_QINQ, V_UDP_DNS, V_TCP_SYN,
    V_TCP_SYNACK, V_TCP_DATA, V_TCP_SACK, V_TCP_RST, V_TCP_FIN, V_IGMP_REPORT, V_IGMP3_JOIN,
];

pub const DATAGRAMS: &[&str] = &[
    V_TCP_SYN_NOTS, V_TCP_SACK3, V_ICMP_ECHO, V_ICMP_REPLY, V_ICMP_PORT_UNREACH, V_ICMP_FRAG_NEEDED, V_UDP_HI,
    V_ICMP_PORT_UNREACH_GEN, V_ICMP_TIME_EXCEEDED, V_IGMP_QUERY_V2, V_IP_RR, V_IP_NOPS, V_IP_LSRR, V_IP_FRAG_FIRST,
    V_IP_FRAG_LAST, V_IP_DSCP, V_IP_MIN, V_IGMP3_LEAVE, V_IGMP3_CURRENT, V_IGMP3_JOIN_UNSPEC, V_ICMP_PROTO_UNREACH_GEN,
];

pub const ICMP_MESSAGES: &[&str] = &[V_ICMP_PARAM_PROBLEM, V_ICMP_TIMESTAMP_REQ, V_ICMP_REDIRECT];

pub const IGMP_MESSAGES: &[&str] = &[
    V_IGMP_QUERY_V1, V_IGMP_QUERY_V3, V_IGMP_QUERY_V3_EXP, V_IGMP_QUERY_V3_SRC, V_IGMP_LEAVE, V_IGMP_REPORT_LONG,
];
