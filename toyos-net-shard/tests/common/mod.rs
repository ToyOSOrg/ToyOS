//! Fixtures in the specifications' numbers (RFC 5737): A is 192.0.2.1 at 02:00:00:00:00:0a, B is
//! 192.0.2.2 at 02:00:00:00:00:0b, both on 192.0.2.0/24 of one `toyos-net-testnet` segment; C,
//! 192.0.2.3, is on no node. Frames are read back with `toyos-net-wire`'s parsers and named the
//! way a scenario names them.

#![allow(dead_code)]

use std::net::Ipv4Addr;
use std::time::Duration;

use toyos_net_shard::Event;
use toyos_net_tcp::{ConnId, Endpoint, State};
use toyos_net_testnet::Net;
use toyos_net_wire::arp::{Arp, Operation};
use toyos_net_wire::ethernet::{EtherType, Frame, MacAddr};
use toyos_net_wire::ipv4::{Ipv4Packet, Protocol};
use toyos_net_wire::tcp::{TcpFlags, TcpSegment};
use toyos_net_wire::{Instant, Port};

pub const A: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 1);
pub const B: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 2);
pub const C: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 3);
pub const MAC_A: MacAddr = MacAddr([0x02, 0, 0, 0, 0, 0x0a]);
pub const MAC_B: MacAddr = MacAddr([0x02, 0, 0, 0, 0, 0x0b]);
pub const R: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 254);
pub const MAC_R: MacAddr = MacAddr([0x02, 0, 0, 0, 0, 0xfe]);

pub fn port(n: u16) -> Port {
    Port::new(n).unwrap()
}

pub fn ep(addr: Ipv4Addr, p: u16) -> Endpoint {
    Endpoint { addr, port: port(p) }
}

/// A and B on one segment, an hour into the clock, each address verified and announced, and
/// neither holding a neighbour entry for the other.
pub fn segment() -> (Net, usize, usize) {
    let mut net = Net::new(Instant::from_millis(3_600_000));
    let a = net.add_node(MAC_A, A, 24);
    let b = net.add_node(MAC_B, B, 24);
    let verified = |net: &mut Net, n: usize, addr| net.nodes[n].events.contains(&Event::Verified(addr));
    assert!(net.run_until(Duration::from_secs(1), |net| verified(net, a, A) && verified(net, b, B)), "both addresses verified");
    net.advance(toyos_net_ip::limits::acd::ANNOUNCE_INTERVAL * 2);
    let none = |n: usize, addr| net.nodes[n].shard.ip().neighbour(net.nodes[n].shard.iface(), addr).is_none();
    assert!(none(a, B) && none(b, A), "no neighbour entries yet");
    (net, a, b)
}

/// A's connection from 49152 to B's listener on 80, established and accepted.
pub fn established(net: &mut Net, a: usize, b: usize) -> (ConnId, ConnId) {
    connected(net, a, 49152, b, B)
}

/// A's connection from `from` to the listener on 80 that node `peer` at `addr` opens, established
/// and accepted.
pub fn connected(net: &mut Net, a: usize, from: u16, peer: usize, addr: Ipv4Addr) -> (ConnId, ConnId) {
    let listener = net.nodes[peer].shard.listen(addr, Some(port(80)), || 0).unwrap();
    let now = net.now();
    let id = net.nodes[a].shard.connect(now, Some(port(from)), ep(addr, 80)).unwrap();
    let mut accepted = None;
    let done = net.run_until(Duration::from_secs(1), |net| {
        accepted = accepted.or_else(|| net.nodes[peer].shard.accept(listener).unwrap());
        accepted.is_some() && net.nodes[a].shard.status(id).unwrap().state == State::Established
    });
    assert!(done, "the handshake completes");
    (id, accepted.unwrap())
}

/// A frame as a scenario names it: `ARP request 192.0.2.3`, `TCP 49152>80 SYN len 0`.
pub fn name(frame: &[u8]) -> String {
    let frame = Frame::parse(frame).expect("a frame");
    match frame.ether_type() {
        EtherType::Arp => {
            let arp = Arp::parse(frame.body()).expect("ARP");
            match arp.operation {
                Operation::Request if frame.destination() != MacAddr::BROADCAST => format!("ARP poll {}", arp.target_ip),
                Operation::Request => format!("ARP request {}", arp.target_ip),
                Operation::Reply => format!("ARP reply {} to {}", arp.sender_ip, arp.target_ip),
            }
        }
        EtherType::Ipv4 => {
            let ip = Ipv4Packet::parse(frame.body()).expect("IPv4");
            match ip.protocol() {
                Protocol::Tcp => {
                    let tcp = TcpSegment::parse(&ip).expect("TCP");
                    let flags = tcp.flags();
                    let names = [(TcpFlags::SYN, "SYN"), (TcpFlags::RST, "RST"), (TcpFlags::FIN, "FIN"), (TcpFlags::ACK, "ACK")];
                    let set: Vec<&str> = names.iter().filter(|(f, _)| flags.contains(*f)).map(|(_, n)| *n).collect();
                    format!(
                        "TCP {}:{}>{}:{} {} len {}",
                        ip.source(),
                        tcp.source_port().get(),
                        ip.destination(),
                        tcp.destination_port().get(),
                        set.join(","),
                        tcp.payload().len()
                    )
                }
                other => format!("IPv4 {other:?} to {}", ip.destination()),
            }
        }
        other => format!("{other:?}"),
    }
}

/// The wire from record `start` on, one frame a line, for a failure's message.
pub fn dump(net: &Net, start: usize) -> String {
    net.wire()[start..].iter().map(|c| format!("{:?} node {}: {}\n", c.at, c.from, name(&c.frame))).collect()
}

/// A TCP frame's source port and the sequence space its payload ends at; `None` for anything else
/// and for a segment without payload.
pub fn data(frame: &[u8]) -> Option<(u16, u32, usize)> {
    let frame = Frame::parse(frame).ok()?;
    let ip = Ipv4Packet::parse(frame.body()).ok()?;
    let tcp = TcpSegment::parse(&ip).ok()?;
    let len = tcp.payload().len();
    (len > 0).then(|| (tcp.source_port().get(), tcp.sequence().get().wrapping_add(len as u32), len))
}

/// A TCP frame whose first SACK block lies at or below its acknowledgment: a D-SACK (RFC 2883 §4).
pub fn dsack(frame: &[u8]) -> bool {
    let Ok(frame) = Frame::parse(frame) else { return false };
    let Ok(ip) = Ipv4Packet::parse(frame.body()) else { return false };
    let Ok(tcp) = TcpSegment::parse(&ip) else { return false };
    let first = tcp.options().sack_blocks().next();
    match (first, tcp.acknowledgment()) {
        (Some(block), Some(ack)) => ack.get().wrapping_sub(block.right.get()) < 1 << 31,
        _ => false,
    }
}

/// RFC 1071, written apart from the wire crate.
pub fn sum(bytes: &[u8]) -> u16 {
    let mut sum: u32 = bytes.chunks(2).map(|p| u32::from(p[0]) << 8 | u32::from(*p.get(1).unwrap_or(&0))).sum();
    while sum > 0xFFFF {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

/// A frame from B's MAC to A's carrying `ip`.
pub fn from_b(ip: &[u8]) -> Vec<u8> {
    let mut frame = [MAC_A.0, MAC_B.0].concat();
    frame.extend_from_slice(&[0x08, 0x00]);
    frame.extend_from_slice(ip);
    frame
}

/// A bare TCP segment in its IPv4 datagram (DF, TTL 64), with `flags` as given, checksums fixed.
pub fn segment_bytes(source: (Ipv4Addr, u16), destination: (Ipv4Addr, u16), seq: u32, flags: u8) -> Vec<u8> {
    let mut tcp = Vec::new();
    tcp.extend_from_slice(&source.1.to_be_bytes());
    tcp.extend_from_slice(&destination.1.to_be_bytes());
    tcp.extend_from_slice(&seq.to_be_bytes());
    tcp.extend_from_slice(&[0, 0, 0, 0, 0x50, flags, 0xff, 0xff, 0, 0, 0, 0]);
    let pseudo = [&source.0.octets()[..], &destination.0.octets(), &[0, 6, 0, 20]].concat();
    let check = sum(&[pseudo, tcp.clone()].concat());
    tcp[16..18].copy_from_slice(&check.to_be_bytes());
    let mut ip = vec![0x45, 0, 0, 40, 0, 0, 0x40, 0, 64, 6, 0, 0];
    ip.extend_from_slice(&source.0.octets());
    ip.extend_from_slice(&destination.0.octets());
    let check = sum(&ip);
    ip[10..12].copy_from_slice(&check.to_be_bytes());
    [ip, tcp].concat()
}

/// Hex in the specifications' layout, whitespace ignored.
pub fn hex(text: &str) -> Vec<u8> {
    let digits: Vec<u8> = text.bytes().filter(u8::is_ascii_hexdigit).collect();
    digits.chunks(2).map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap()).collect()
}

/// An ICMP error from B to A about the IPv4 datagram `frame` carried: its header and the first 8
/// bytes behind it quoted (RFC 792), checksums fixed.
pub fn icmp_about(frame: &[u8], kind: u8, code: u8, rest: [u8; 4]) -> Vec<u8> {
    let quoted = &frame[14..14 + 28];
    let mut icmp = vec![kind, code, 0, 0];
    icmp.extend_from_slice(&rest);
    icmp.extend_from_slice(quoted);
    let check = sum(&icmp);
    icmp[2..4].copy_from_slice(&check.to_be_bytes());
    let len = u16::try_from(20 + icmp.len()).unwrap().to_be_bytes();
    let mut ip = vec![0x45, 0, len[0], len[1], 0, 0, 0x40, 0, 64, 1, 0, 0];
    ip.extend_from_slice(&B.octets());
    ip.extend_from_slice(&A.octets());
    let check = sum(&ip);
    ip[10..12].copy_from_slice(&check.to_be_bytes());
    from_b(&[ip, icmp].concat())
}
