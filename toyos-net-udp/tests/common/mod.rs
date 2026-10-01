//! Fixture UF over `toyos-net-ip`, built through its public API; a transmit opportunity composed
//! as the shard composes it; scripted draws; and an RFC 1071 sum written here for every
//! "(checksum fixed)" mutation.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::net::Ipv4Addr;

use toyos_net_ip::{Delivery, Event, IfIndex, Instant, Ip, Sent, FRAME};
use toyos_net_udp::{Counter, SocketId, Udp, Verdict};
use toyos_net_wire::ethernet::{Frame, IndividualMac, MacAddr};
use toyos_net_wire::ipv4::{Ipv4Packet, MulticastAddr};
use toyos_net_wire::Port;

pub const MAC_A: MacAddr = MacAddr([2, 0, 0, 0, 0, 0x0a]);
pub const MAC_B: MacAddr = MacAddr([2, 0, 0, 0, 0, 0x0b]);
pub const MAC_DNS: MacAddr = MacAddr([2, 0, 0, 0, 0, 0x35]);
pub const MAC_R: MacAddr = MacAddr([2, 0, 0, 0, 0, 0xfe]);
pub const A: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 1);
pub const B: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 2);
pub const DNS: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 53);
pub const R: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 254);
pub const MDNS: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
pub const ANY: Ipv4Addr = Ipv4Addr::UNSPECIFIED;
pub const LIMITED: Ipv4Addr = Ipv4Addr::BROADCAST;
pub const BASE_MS: u64 = 10_000;

pub fn port(n: u16) -> Port {
    Port::new(n).unwrap()
}

pub fn hex(text: &str) -> Vec<u8> {
    text.split_whitespace().map(|b| u8::from_str_radix(b, 16).unwrap()).collect()
}

pub fn oracle_sum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    for chunk in data.chunks(2) {
        sum += if chunk.len() == 2 { u32::from(chunk[0]) << 8 | u32::from(chunk[1]) } else { u32::from(chunk[0]) << 8 };
        while sum > 0xffff {
            sum = (sum & 0xffff) + (sum >> 16);
        }
    }
    sum as u16
}

/// Recomputes an IPv4 datagram's header checksum and its UDP or ICMP checksum; a UDP checksum
/// field of zero stays "none".
pub fn fix(ip: &mut [u8]) {
    let ihl = usize::from(ip[0] & 0x0f) * 4;
    ip[10] = 0;
    ip[11] = 0;
    let c = !oracle_sum(&ip[..ihl]);
    ip[10..12].copy_from_slice(&c.to_be_bytes());
    let total = usize::from(u16::from_be_bytes([ip[2], ip[3]]));
    let mut pseudo = ip[12..20].to_vec();
    let protocol = ip[9];
    let body = &mut ip[ihl..total];
    match protocol {
        17 => {
            let length = usize::from(u16::from_be_bytes([body[4], body[5]]));
            if body[6] == 0 && body[7] == 0 {
                return;
            }
            body[6] = 0;
            body[7] = 0;
            pseudo.extend_from_slice(&[0, 17]);
            pseudo.extend_from_slice(&(length as u16).to_be_bytes());
            pseudo.extend_from_slice(&body[..length]);
            let mut c = !oracle_sum(&pseudo);
            if c == 0 {
                c = 0xffff;
            }
            body[6..8].copy_from_slice(&c.to_be_bytes());
        }
        1 => {
            body[2] = 0;
            body[3] = 0;
            let c = !oracle_sum(body);
            body[2..4].copy_from_slice(&c.to_be_bytes());
        }
        _ => {}
    }
}

pub fn edit(mut ip: Vec<u8>, change: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    change(&mut ip);
    fix(&mut ip);
    ip
}

pub fn set_source(ip: &mut [u8], a: Ipv4Addr) {
    ip[12..16].copy_from_slice(&a.octets());
}

pub fn set_destination(ip: &mut [u8], a: Ipv4Addr) {
    ip[16..20].copy_from_slice(&a.octets());
}

pub fn set_ports(ip: &mut [u8], source: u16, destination: u16) {
    ip[20..22].copy_from_slice(&source.to_be_bytes());
    ip[22..24].copy_from_slice(&destination.to_be_bytes());
}

/// A UDP datagram, atomic, TTL 64, checksummed.
pub fn udp(source: Ipv4Addr, sport: u16, destination: Ipv4Addr, dport: u16, data: &[u8]) -> Vec<u8> {
    let length = 8 + data.len();
    let total = 20 + length;
    let mut ip = vec![0x45, 0, (total >> 8) as u8, total as u8, 0, 0, 0x40, 0, 64, 17, 0, 0];
    ip.extend_from_slice(&source.octets());
    ip.extend_from_slice(&destination.octets());
    ip.extend_from_slice(&sport.to_be_bytes());
    ip.extend_from_slice(&dport.to_be_bytes());
    ip.extend_from_slice(&(length as u16).to_be_bytes());
    ip.extend_from_slice(&[0xff, 0xff]);
    ip.extend_from_slice(data);
    fix(&mut ip);
    ip
}

pub fn eth(destination: MacAddr, source: MacAddr, ether_type: u16, body: &[u8]) -> Vec<u8> {
    let mut f = destination.0.to_vec();
    f.extend_from_slice(&source.0);
    f.extend_from_slice(&ether_type.to_be_bytes());
    f.extend_from_slice(body);
    f
}

fn arp(op: u16, sender_mac: MacAddr, sender: Ipv4Addr, target_mac: MacAddr, target: Ipv4Addr) -> Vec<u8> {
    let mut a = vec![0, 1, 8, 0, 6, 4];
    a.extend_from_slice(&op.to_be_bytes());
    a.extend_from_slice(&sender_mac.0);
    a.extend_from_slice(&sender.octets());
    a.extend_from_slice(&target_mac.0);
    a.extend_from_slice(&target.octets());
    a
}

pub fn ip_of(frame: &[u8]) -> Option<Ipv4Packet<'_>> {
    Ipv4Packet::parse(Frame::parse(frame).ok()?.body()).ok()
}

pub fn ip_bytes(frame: &[u8]) -> Vec<u8> {
    ip_of(frame).unwrap().bytes().to_vec()
}

pub fn destination_of(frame: &[u8]) -> MacAddr {
    Frame::parse(frame).unwrap().destination()
}

pub fn is_arp_request_for(frame: &[u8], target: Ipv4Addr) -> bool {
    let f = Frame::parse(frame).unwrap();
    toyos_net_wire::arp::Arp::parse(f.body()).is_ok_and(|a| a.operation == toyos_net_wire::arp::Operation::Request && a.target_ip == target)
}

pub struct U {
    pub ip: Ip,
    pub udp: Udp,
    pub if0: IfIndex,
    pub now: u64,
    pub draws: VecDeque<u32>,
    pub events: Vec<Event>,
    base: Vec<(Counter, u64)>,
    ip_base: Vec<(toyos_net_ip::Counter, u64)>,
}

impl U {
    pub fn instant(t: u64) -> Instant {
        Instant::from_millis(BASE_MS + t)
    }

    pub fn clock(&self) -> Instant {
        Self::instant(self.now)
    }

    fn setup_until(ip: &mut Ip, until: Instant) {
        while let Some(d) = ip.next_deadline().filter(|d| *d <= until) {
            ip.fire(d);
            ip.transmit(d, usize::MAX, |_, _| {});
        }
        ip.transmit(until, usize::MAX, |_, _| {});
    }

    /// A's interface up with MAC A and nothing configured.
    pub fn bare() -> Self {
        let mut ip = Ip::new(Instant::from_nanos(0), [0x5a; 16]);
        let if0 = ip.add_interface(Instant::from_nanos(0), IndividualMac::new(MAC_A).unwrap());
        ip.link_up(Instant::from_nanos(0), if0).unwrap();
        ip.join(Instant::from_nanos(0), if0, MulticastAddr::new(MDNS).unwrap()).unwrap();
        Self::setup_until(&mut ip, Self::instant(0));
        let mut u = Self { ip, udp: Udp::new(), if0, now: 0, draws: VecDeque::new(), events: Vec::new(), base: Vec::new(), ip_base: Vec::new() };
        u.rebase();
        u
    }

    /// Fixture UF: 192.0.2.1/24 assigned, router 192.0.2.254, B, the DNS server and the router
    /// resolved, 224.0.0.1 and 224.0.0.251 joined, no socket.
    pub fn uf() -> Self {
        let mut ip = Ip::new(Instant::from_nanos(0), [0x5a; 16]);
        let t = Instant::from_nanos(0);
        let if0 = ip.add_interface(t, IndividualMac::new(MAC_A).unwrap());
        ip.link_up(t, if0).unwrap();
        ip.add_address(t, if0, A, 24).unwrap();
        Self::setup_until(&mut ip, Instant::from_millis(3_000));
        let t = Instant::from_millis(3_000);
        ip.set_gateways(t, if0, &[R]).unwrap();
        ip.join(t, if0, MulticastAddr::new(MDNS).unwrap()).unwrap();
        let t = Instant::from_millis(5_000);
        for (addr, m) in [(B, MAC_B), (DNS, MAC_DNS), (R, MAC_R)] {
            let _ = ip.resolve(t, if0, addr);
            ip.transmit(t, usize::MAX, |_, _| {});
            let _ = ip.receive(t, if0, &eth(MAC_A, m, 0x0806, &arp(2, m, addr, MAC_A, A)));
        }
        Self::setup_until(&mut ip, Self::instant(0));
        let mut u = Self { ip, udp: Udp::new(), if0, now: 0, draws: VecDeque::new(), events: Vec::new(), base: Vec::new(), ip_base: Vec::new() };
        u.rebase();
        u
    }

    pub fn rebase(&mut self) {
        self.base = Counter::ALL.iter().map(|&c| (c, self.udp.counters().get(c))).collect();
        self.ip_base = toyos_net_ip::Counter::ALL.iter().map(|&c| (c, self.ip.counters().get(c))).collect();
        self.ip.drain_events().for_each(drop);
        self.udp.drain_refusals().for_each(drop);
        self.events.clear();
    }

    pub fn count(&self, counter: Counter) -> u64 {
        self.udp.counters().get(counter) - self.base.iter().find(|(c, _)| *c == counter).map_or(0, |(_, n)| *n)
    }

    pub fn ip_count(&self, counter: toyos_net_ip::Counter) -> u64 {
        self.ip.counters().get(counter) - self.ip_base.iter().find(|(c, _)| *c == counter).map_or(0, |(_, n)| *n)
    }

    /// Binds with the next scripted draw when a port is asked for.
    pub fn bind(&mut self, addr: Ipv4Addr, port: u16) -> Result<SocketId, toyos_net_udp::Error> {
        let draws = &mut self.draws;
        self.udp.bind(&self.ip, addr, Port::new(port), || draws.pop_front().expect("a draw is scripted"))
    }

    /// A frame arrives: [ip] admits it, [udp] takes it, and a unicast datagram nobody takes is
    /// answered as the shard answers it.
    pub fn frame(&mut self, frame: &[u8]) -> Option<Verdict> {
        let now = self.clock();
        let verdict = match self.ip.receive(now, self.if0, frame) {
            Some(Delivery::Udp(arrival, datagram)) => {
                let verdict = self.udp.receive(now, &arrival, &datagram);
                if verdict == Verdict::NoSocket {
                    self.ip.port_unreachable(now, &arrival);
                }
                Some(verdict)
            }
            Some(Delivery::Error(error)) => {
                self.udp.icmp_error(&error);
                None
            }
            Some(Delivery::Tcp(..)) | None => None,
        };
        self.news();
        verdict
    }

    /// A datagram from B's MAC to A's.
    pub fn datagram(&mut self, ip: &[u8]) -> Option<Verdict> {
        self.frame(&eth(MAC_A, MAC_B, 0x0800, ip))
    }

    fn news(&mut self) {
        let events: Vec<Event> = self.ip.drain_events().collect();
        for event in &events {
            if let Event::Unreachable(flow) = event {
                self.udp.unreachable(flow);
            }
        }
        self.events.extend(events);
    }

    /// A transmit opportunity with room for `credit` frames, composed as the shard composes it:
    /// [ip]'s frames first, then UDP's, then what UDP's datagrams asked [ip] for.
    pub fn out_with(&mut self, credit: usize) -> Vec<Vec<u8>> {
        let now = self.clock();
        let mut frames = Vec::new();
        let mut spent = self.ip.transmit(now, credit, |_, f| frames.push(f.to_vec()));
        let Self { ip, udp, .. } = self;
        spent += udp.transmit(credit - spent, |out| {
            let mut buf = [0u8; FRAME];
            match ip.send_udp(now, out, &mut buf) {
                Ok(Sent::Frame(n)) => {
                    frames.push(buf[..n].to_vec());
                    true
                }
                Ok(Sent::Held) | Err(_) => false,
            }
        });
        self.ip.transmit(now, credit - spent, |_, f| frames.push(f.to_vec()));
        self.news();
        frames
    }

    pub fn out(&mut self) -> Vec<Vec<u8>> {
        self.out_with(usize::MAX)
    }

    /// Fires [ip]'s deadlines up to `t`, each at its own time, transmitting after each.
    pub fn run(&mut self, t: u64) -> Vec<Vec<u8>> {
        let mut out = self.out();
        while let Some(d) = self.ip.next_deadline().filter(|d| *d <= Self::instant(t)) {
            self.now = (d.nanos() / 1_000_000).saturating_sub(BASE_MS).max(self.now);
            self.ip.fire(d);
            out.extend(self.out());
        }
        self.now = t;
        out.extend(self.out());
        out
    }

    pub fn recv(&mut self, id: SocketId) -> Result<Option<(Vec<u8>, toyos_net_udp::Received)>, toyos_net_udp::Error> {
        let mut buf = [0u8; 65_536];
        self.udp.recv(id, &mut buf).map(|r| r.map(|r| (buf[..r.len].to_vec(), r)))
    }
}

pub const V_UDP_HI: &str = "
45 00 00 1e 00 00 40 00 40 11 b6 cb c0 00 02 02
c0 00 02 01 13 88 13 89 00 0a ec 5b 68 69";

pub const V_ICMP_PORT_UNREACH_GEN: &str = "
45 00 00 3a 00 00 40 00 40 01 b6 bf c0 00 02 01
c0 00 02 02 03 03 81 1c 00 00 00 00 45 00 00 1e
00 00 40 00 40 11 b6 cb c0 00 02 02 c0 00 02 01
13 88 13 89 00 0a ec 5b 68 69";

pub const V_IP_RR: &str = "
47 00 00 26 00 00 40 00 40 11 a9 bc c0 00 02 02
c0 00 02 01 07 07 04 00 00 00 00 00 13 88 13 89
00 0a ec 5b 68 69";

pub const V_IP_NOPS: &str = "
46 00 00 22 00 00 40 00 40 11 b3 c6 c0 00 02 02
c0 00 02 01 01 01 01 00 13 88 13 89 00 0a ec 5b
68 69";

pub const V_UDP_MDNS: &str = "
01 00 5e 00 00 fb 02 00 00 00 00 0b 08 00 45 00
00 39 00 00 40 00 ff 11 d8 b5 c0 00 02 02 e0 00
00 fb 14 e9 14 e9 00 25 76 36 00 00 00 00 00 01
00 00 00 00 00 00 05 74 6f 79 6f 73 05 6c 6f 63
61 6c 00 00 01 00 01";

pub const V_UDP_BCAST: &str = "
ff ff ff ff ff ff 02 00 00 00 00 0b 08 00 45 00
00 1e 00 00 40 00 40 11 78 cd c0 00 02 02 ff ff
ff ff 13 88 13 89 00 0a ae 5d 68 69 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00";

pub const V_UDP_SUBNET_BCAST: &str = "
ff ff ff ff ff ff 02 00 00 00 00 0b 08 00 45 00
00 1e 00 00 40 00 40 11 b5 cd c0 00 02 02 c0 00
02 ff 13 88 13 89 00 0a eb 5d 68 69 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00";

pub const V_UDP_EMPTY: &str = "
02 00 00 00 00 0a 02 00 00 00 00 0b 08 00 45 00
00 1c 00 00 40 00 40 11 b6 cd c0 00 02 02 c0 00
02 01 13 88 13 89 00 08 54 c9 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00";

pub const V_UDP_A_OUT: &str = "
02 00 00 00 00 35 02 00 00 00 00 0a 08 00 45 00
00 1e 00 00 40 00 40 11 b6 98 c0 00 02 01 c0 00
02 35 c3 51 00 35 00 0a 4f b3 68 69 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00";

pub const V_ICMP_PU_A_OUT: &str = "
45 00 00 38 00 00 40 00 40 01 b6 8e c0 00 02 35
c0 00 02 01 03 03 e9 b8 00 00 00 00 45 00 00 1e
00 00 40 00 40 11 b6 98 c0 00 02 01 c0 00 02 35
c3 51 00 35 00 0a 4f b3";

pub const V_DHCP_DISCOVER: &str = "
ff ff ff ff ff ff 02 00 00 00 00 0a 08 00 45 00
01 48 00 00 40 00 40 11 39 a6 00 00 00 00 ff ff
ff ff 00 44 00 43 01 34 13 19 01 01 06 00 9a 3c
21 f7 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 02 00 00 00 00 0a 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 63 82 53 63 35 01 01 3d 0f ff
00 00 00 0a 00 03 00 01 02 00 00 00 00 0a 39 02
05 c0 37 06 01 03 06 33 3a 3b 0c 05 74 6f 79 6f
73 50 00 ff 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00";

pub const V_DHCP_OFFER_FRAME: &str = "
02 00 00 00 00 0a 02 00 00 00 00 fe 08 00 45 00
01 2e 00 00 40 00 40 11 b4 bf c0 00 02 fe c0 00
02 01 00 43 00 44 01 1a a8 44 02 01 06 00 9a 3c
21 f7 00 00 00 00 00 00 00 00 c0 00 02 01 00 00
00 00 00 00 00 00 02 00 00 00 00 0a 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 63 82 53 63 35 01 02 36 04 c0
00 02 fe 33 04 00 00 0e 10 01 04 ff ff ff 00 03
04 c0 00 02 fe 06 04 c0 00 02 35 ff";

impl U {
    /// Binds any address to an ephemeral port with the next scripted draw; the port it got.
    pub fn ephemeral(&mut self) -> Port {
        let id = self.bind(ANY, 0).unwrap();
        self.udp.binding(id).unwrap().1
    }
}
