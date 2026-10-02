//! The scenario harness: fixtures reached through the public API only, a clock in scenario
//! milliseconds from the fixture's t = 0, frames out with the time they left, counters as deltas
//! from t = 0, and an RFC 1071 sum written here, independent of the crate's, for every
//! "(checksum fixed)" mutation.

#![allow(dead_code)]

use std::net::Ipv4Addr;

use toyos_net_ip::{AddrState, Counter, Event, IfIndex, Instant, Ip, Nud, Sent, UdpOut, FRAME};
use toyos_net_wire::ipv4::Ttl;
use toyos_net_wire::udp::UdpBuilder;
use toyos_net_wire::Port;
use toyos_net_wire::arp::Arp;
use toyos_net_wire::ethernet::{Frame, IndividualMac, MacAddr};
use toyos_net_wire::ipv4::{Ipv4Packet, MulticastAddr};

pub const MAC_A: MacAddr = MacAddr([2, 0, 0, 0, 0, 0x0a]);
pub const MAC_B: MacAddr = MacAddr([2, 0, 0, 0, 0, 0x0b]);
pub const MAC_R: MacAddr = MacAddr([2, 0, 0, 0, 0, 0xfe]);
pub const MAC_X: MacAddr = MacAddr([2, 0, 0, 0, 0, 0x66]);
pub const MAC_DNS: MacAddr = MacAddr([2, 0, 0, 0, 0, 0x35]);
pub const A: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 1);
pub const B: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 2);
pub const R: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 254);
pub const DNS: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 53);
pub const REMOTE: Ipv4Addr = Ipv4Addr::new(198, 51, 100, 7);
pub const MDNS: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
pub const SECRET: [u8; 16] = [0x5a; 16];
/// Scenario t = 0, after every fixture's setup has settled.
pub const BASE_MS: u64 = 10_000;

pub fn mac(m: MacAddr) -> IndividualMac {
    IndividualMac::new(m).unwrap()
}

pub fn group(a: Ipv4Addr) -> MulticastAddr {
    MulticastAddr::new(a).unwrap()
}

pub fn hex(text: &str) -> Vec<u8> {
    text.split_whitespace().map(|b| u8::from_str_radix(b, 16).unwrap()).collect()
}

/// RFC 1071's sum, big-endian words, odd byte padded: the oracle, not the crate's.
pub fn oracle_sum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    for chunk in data.chunks(2) {
        let word = if chunk.len() == 2 { u32::from(chunk[0]) << 8 | u32::from(chunk[1]) } else { u32::from(chunk[0]) << 8 };
        sum += word;
        while sum > 0xffff {
            sum = (sum & 0xffff) + (sum >> 16);
        }
    }
    sum as u16
}

pub fn fix_ip_header(ip: &mut [u8]) {
    let ihl = usize::from(ip[0] & 0x0f) * 4;
    ip[10] = 0;
    ip[11] = 0;
    let c = !oracle_sum(&ip[..ihl]);
    ip[10..12].copy_from_slice(&c.to_be_bytes());
}

fn pseudo(ip: &[u8], protocol: u8, length: usize) -> Vec<u8> {
    let mut p = ip[12..20].to_vec();
    p.extend_from_slice(&[0, protocol]);
    p.extend_from_slice(&(length as u16).to_be_bytes());
    p
}

/// Recomputes every checksum of an IPv4 datagram: header, then UDP, TCP, ICMP or IGMP.
pub fn fix(ip: &mut [u8]) {
    fix_ip_header(ip);
    let ihl = usize::from(ip[0] & 0x0f) * 4;
    let total = usize::from(u16::from_be_bytes([ip[2], ip[3]]));
    let protocol = ip[9];
    let header = ip[..ihl].to_vec();
    let body = &mut ip[ihl..total];
    match protocol {
        1 | 2 => {
            body[2] = 0;
            body[3] = 0;
            let c = !oracle_sum(body);
            body[2..4].copy_from_slice(&c.to_be_bytes());
        }
        17 => {
            let length = usize::from(u16::from_be_bytes([body[4], body[5]]));
            if body[6] == 0 && body[7] == 0 {
                return;
            }
            body[6] = 0;
            body[7] = 0;
            let mut all = pseudo(&header, 17, length);
            all.extend_from_slice(&body[..length]);
            let mut c = !oracle_sum(&all);
            if c == 0 {
                c = 0xffff;
            }
            body[6..8].copy_from_slice(&c.to_be_bytes());
        }
        6 => {
            body[16] = 0;
            body[17] = 0;
            let mut all = pseudo(&header, 6, body.len());
            all.extend_from_slice(body);
            let c = !oracle_sum(&all);
            body[16..18].copy_from_slice(&c.to_be_bytes());
        }
        _ => {}
    }
}

/// A datagram with its fields edited and every checksum fixed.
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

/// A minimal IPv4 datagram: atomic, TTL 64, no options, checksums fixed.
pub fn ipv4(source: Ipv4Addr, destination: Ipv4Addr, protocol: u8, payload: &[u8]) -> Vec<u8> {
    let total = 20 + payload.len();
    let mut ip = vec![0x45, 0, (total >> 8) as u8, total as u8, 0, 0, 0x40, 0, 64, protocol, 0, 0];
    ip.extend_from_slice(&source.octets());
    ip.extend_from_slice(&destination.octets());
    ip.extend_from_slice(payload);
    fix(&mut ip);
    ip
}

pub fn udp(source: Ipv4Addr, destination: Ipv4Addr, sport: u16, dport: u16, data: &[u8]) -> Vec<u8> {
    let length = 8 + data.len();
    let mut u = vec![(sport >> 8) as u8, sport as u8, (dport >> 8) as u8, dport as u8, (length >> 8) as u8, length as u8, 0xff, 0xff];
    u.extend_from_slice(data);
    ipv4(source, destination, 17, &u)
}

/// An Ethernet frame around `body`.
pub fn eth(destination: MacAddr, source: MacAddr, ether_type: u16, body: &[u8]) -> Vec<u8> {
    let mut f = destination.0.to_vec();
    f.extend_from_slice(&source.0);
    f.extend_from_slice(&ether_type.to_be_bytes());
    f.extend_from_slice(body);
    f
}

pub fn to_a(ip: &[u8]) -> Vec<u8> {
    eth(MAC_A, MAC_B, 0x0800, ip)
}

pub fn ip_of(frame: &[u8]) -> Option<Ipv4Packet<'_>> {
    let f = Frame::parse(frame).ok()?;
    Ipv4Packet::parse(f.body()).ok()
}

pub fn ip_bytes(frame: &[u8]) -> Vec<u8> {
    ip_of(frame).unwrap().bytes().to_vec()
}

pub fn arp_of(frame: &[u8]) -> Option<Arp> {
    let f = Frame::parse(frame).ok()?;
    Arp::parse(f.body()).ok()
}

pub fn destination_of(frame: &[u8]) -> MacAddr {
    Frame::parse(frame).unwrap().destination()
}

/// A frame left at scenario time `at` ms.
#[derive(Clone, Debug)]
pub struct Out {
    pub at: u64,
    pub iface: IfIndex,
    pub frame: Vec<u8>,
}

impl Out {
    pub fn arp(&self) -> Option<Arp> {
        arp_of(&self.frame)
    }

    pub fn ip(&self) -> Option<Ipv4Packet<'_>> {
        ip_of(&self.frame)
    }

    pub fn to(&self) -> MacAddr {
        destination_of(&self.frame)
    }

    /// An ARP request for `target`.
    pub fn requests(&self, target: Ipv4Addr) -> bool {
        self.arp().is_some_and(|a| a.operation == toyos_net_wire::arp::Operation::Request && a.target_ip == target && !a.sender_ip.is_unspecified() && a.sender_ip != a.target_ip)
    }
}

pub struct H {
    pub ip: Ip,
    pub if0: IfIndex,
    /// Scenario ms of the last call.
    pub now: u64,
    pub events: Vec<Event>,
    counters: Vec<(Counter, u64)>,
    wire: std::collections::BTreeMap<&'static str, u64>,
}

impl H {
    /// Interface `if0` with MAC A and its link up at time 0, nothing settled.
    pub fn raw() -> Self {
        let mut ip = Ip::new(Instant::from_nanos(0), SECRET);
        let if0 = ip.add_interface(Instant::from_nanos(0), mac(MAC_A));
        ip.link_up(Instant::from_nanos(0), if0).unwrap();
        Self { ip, if0, now: 0, events: Vec::new(), counters: Vec::new(), wire: std::collections::BTreeMap::default() }
    }

    /// `if0` up with no address, at t = 0.
    pub fn bare() -> Self {
        let mut h = Self::raw();
        h.settle();
        h
    }

    /// Scenario ms to the crate's clock.
    pub fn instant(t: u64) -> Instant {
        Instant::from_millis(BASE_MS + t)
    }

    /// Setup time: every deadline up to `until`, each at its own time, with unlimited credit.
    pub fn setup_until(&mut self, until: Instant) {
        while let Some(d) = self.ip.next_deadline().filter(|d| *d <= until) {
            self.ip.fire(d);
            self.ip.transmit(d, usize::MAX, |_, _| {});
        }
        self.ip.transmit(until, usize::MAX, |_, _| {});
    }

    /// Runs the setup to t = 0, then zeroes the scenario's view.
    pub fn settle(&mut self) {
        self.setup_until(Self::instant(0));
        self.now = 0;
        self.rebase();
    }

    /// Counters and events count from here.
    pub fn rebase(&mut self) {
        self.counters = Counter::ALL.iter().map(|&c| (c, self.ip.counters().get(c))).collect();
        self.wire = self.ip.wire_counters().collect();
        self.ip.drain_events().for_each(drop);
        self.events.clear();
    }

    /// Adds `addr/len` at setup time `at_ms` and runs conflict detection to assigned.
    pub fn assign(&mut self, iface: IfIndex, addr: Ipv4Addr, len: u8, at_ms: u64) {
        let at = Instant::from_millis(at_ms);
        self.ip.add_address(at, iface, addr, len).unwrap();
        self.setup_until(Instant::from_millis(at_ms + 3_000));
        assert_eq!(self.ip.address(iface, addr), Some(AddrState::Assigned));
    }

    /// Fixture I's configuration, not yet settled.
    pub fn raw_i() -> Self {
        let mut h = Self::raw();
        let if0 = h.if0;
        h.assign(if0, A, 24, 0);
        let t = Instant::from_millis(3_000);
        h.ip.set_gateways(t, if0, &[R]).unwrap();
        h.ip.join(t, if0, group(MDNS)).unwrap();
        h.setup_until(Instant::from_millis(5_000));
        h
    }

    /// Fixture I: 192.0.2.1/24 assigned on if0 through conflict detection, gateway 192.0.2.254,
    /// 224.0.0.251 joined, IGMPv3, every bucket full.
    pub fn fixture_i() -> Self {
        let mut h = Self::raw_i();
        h.settle();
        h
    }

    pub fn clock(&self) -> Instant {
        Self::instant(self.now)
    }

    /// Fixture I with `addr` REACHABLE at `m`, confirmed at t = 0: resolved during setup, so no
    /// request of ours is recent, and confirmed by advice at t = 0.
    pub fn fixture_i_with(addr: Ipv4Addr, m: MacAddr) -> Self {
        let mut h = Self::raw_i();
        let setup = Instant::from_millis(5_000);
        let _ = h.ip.resolve(setup, h.if0, addr);
        h.ip.transmit(setup, usize::MAX, |_, _| {});
        let reply = eth(MAC_A, m, 0x0806, &arp_packet(2, m, addr, MAC_A, A));
        let _ = h.ip.receive(setup, h.if0, &reply);
        h.settle();
        h.ip.advise(h.clock(), addr, toyos_net_ip::Advice::Confirmed);
        assert!(matches!(h.ip.neighbour(h.if0, addr), Some(Nud::Reachable(r)) if r.confirmed() == H::instant(0)));
        h.rebase();
        h
    }

    /// Resolves `addr` on if0 by a request and its reply, now.
    pub fn reach(&mut self, addr: Ipv4Addr, m: MacAddr) {
        let now = self.clock();
        let _ = self.ip.resolve(now, self.if0, addr);
        self.ip.transmit(now, usize::MAX, |_, _| {});
        self.reply_from(addr, m);
        self.ip.transmit(now, usize::MAX, |_, _| {});
        assert!(matches!(self.ip.neighbour(self.if0, addr), Some(Nud::Reachable(_))), "{addr} did not resolve");
    }

    /// `addr` at `m` answers a request of A's.
    pub fn reply_from(&mut self, addr: Ipv4Addr, m: MacAddr) {
        self.frame(&eth(MAC_A, m, 0x0806, &arp_packet(2, m, addr, MAC_A, A)));
    }

    /// Hosts on `ours`'s /24, from .100 up, ask `iface` for `ours` at `at`: their replies fill
    /// the control queue every interface shares.
    pub fn fill_control_queue(&mut self, iface: IfIndex, at: Instant, ours: Ipv4Addr) {
        let [a, b, c, _] = ours.octets();
        for n in 0..toyos_net_ip::limits::CONTROL_QUEUE as u8 {
            let m = MacAddr([2, 1, 0, 0, 0, n]);
            let request = arp_packet(1, m, Ipv4Addr::new(a, b, c, 100 + n), MacAddr::ZERO, ours);
            let _ = self.ip.receive(at, iface, &eth(MacAddr::BROADCAST, m, 0x0806, &request));
        }
        self.collect();
    }

    pub fn count(&self, counter: Counter) -> u64 {
        let base = self.counters.iter().find(|(c, _)| *c == counter).map_or(0, |(_, n)| *n);
        self.ip.counters().get(counter) - base
    }

    pub fn wire(&self, name: &str) -> u64 {
        let now = self.ip.wire_counters().find(|(n, _)| *n == name).map_or(0, |(_, v)| v);
        now - self.wire.get(name).copied().unwrap_or(0)
    }

    /// A frame received on if0 at the current time.
    pub fn frame(&mut self, frame: &[u8]) -> Option<toyos_net_ip::Delivery<'static>> {
        let leaked: &'static [u8] = Box::leak(frame.to_vec().into_boxed_slice());
        let delivery = self.ip.receive(self.clock(), self.if0, leaked);
        self.collect();
        delivery
    }

    /// An IPv4 datagram from B's MAC to A's.
    pub fn datagram(&mut self, ip: &[u8]) -> Option<toyos_net_ip::Delivery<'static>> {
        self.frame(&to_a(ip))
    }

    /// Moves the clock to `t` without firing anything.
    pub fn at(&mut self, t: u64) {
        self.now = t;
    }

    /// Fires every deadline up to and including `t`, each at its own time, with unlimited credit
    /// after each: a live loop. Returns every frame that left.
    pub fn run(&mut self, t: u64) -> Vec<Out> {
        let mut out = self.out();
        while let Some(d) = self.ip.next_deadline().filter(|d| *d <= Self::instant(t)) {
            self.now = (d.nanos() / 1_000_000).saturating_sub(BASE_MS).max(self.now);
            self.ip.fire(d);
            out.extend(self.drain_at(d));
        }
        self.now = t;
        out.extend(self.out());
        out
    }

    /// One `fire` at `t`, then a transmit opportunity.
    pub fn fire(&mut self, t: u64) -> Vec<Out> {
        self.now = t;
        self.ip.fire(self.clock());
        self.out()
    }

    /// A transmit opportunity with unlimited credit now.
    pub fn out(&mut self) -> Vec<Out> {
        let now = self.clock();
        self.drain_at(now)
    }

    /// A transmit opportunity with room for `credit` frames.
    pub fn out_with(&mut self, credit: usize) -> Vec<Out> {
        let at = self.now;
        let mut frames = Vec::new();
        self.ip.transmit(self.clock(), credit, |iface, f| frames.push(Out { at, iface, frame: f.to_vec() }));
        self.collect();
        frames
    }

    fn drain_at(&mut self, now: Instant) -> Vec<Out> {
        let at = (now.nanos() / 1_000_000).saturating_sub(BASE_MS);
        let mut frames = Vec::new();
        self.ip.transmit(now, usize::MAX, |iface, f| frames.push(Out { at, iface, frame: f.to_vec() }));
        self.collect();
        frames
    }

    pub fn collect(&mut self) {
        self.events.extend(self.ip.drain_events());
    }

    pub fn state(&self, addr: Ipv4Addr) -> Option<&Nud> {
        self.ip.neighbour(self.if0, addr)
    }

    pub fn is_reachable(&self, addr: Ipv4Addr) -> bool {
        matches!(self.state(addr), Some(Nud::Reachable(_)))
    }

    pub fn is_stale(&self, addr: Ipv4Addr) -> bool {
        matches!(self.state(addr), Some(Nud::Stale(_)))
    }

    pub fn mac_of(&self, addr: Ipv4Addr) -> Option<MacAddr> {
        self.state(addr).and_then(Nud::mac)
    }

    pub fn refusals(&self, rule: Counter) -> Vec<toyos_net_ip::Refusal> {
        self.events.iter().filter_map(|e| match e {
            Event::Refused(r) if r.rule == rule => Some(*r),
            _ => None,
        }).collect()
    }
}

pub fn arp_frame(destination: MacAddr, source: MacAddr, arp: &Arp) -> Vec<u8> {
    let builder = toyos_net_wire::ethernet::FrameBuilder { destination, source: IndividualMac::new(source).unwrap() };
    let mut out = vec![0u8; 60];
    builder.emit(arp, &mut out).unwrap().to_vec()
}

/// An ARP packet as the frame body of a 60-byte frame, edited: operation, sender MAC, sender IP,
/// target MAC, target IP, frame destination.
pub fn arp_packet(op: u16, sender_mac: MacAddr, sender_ip: Ipv4Addr, target_mac: MacAddr, target_ip: Ipv4Addr) -> Vec<u8> {
    let mut a = vec![0, 1, 8, 0, 6, 4];
    a.extend_from_slice(&op.to_be_bytes());
    a.extend_from_slice(&sender_mac.0);
    a.extend_from_slice(&sender_ip.octets());
    a.extend_from_slice(&target_mac.0);
    a.extend_from_slice(&target_ip.octets());
    a
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

pub const V_IGMP_QUERY_V3_SRC: &str = "
11 64 87 0c e0 00 00 fb 02 7d 00 02 c0 00 02 09
c0 00 02 0a";

pub const V_IGMP_LEAVE: &str = "17 00 08 04 e0 00 00 fb";

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

pub const V_IGMP3_QUERY_GEN: &str = "
46 c0 00 24 00 00 40 00 01 02 41 14 c0 00 02 fe
e0 00 00 01 94 04 00 00 11 64 ec 1e 00 00 00 00
02 7d 00 00";

pub const V_IGMP3_QUERY_NORA: &str = "
45 c0 00 20 00 00 40 00 01 02 d6 1c c0 00 02 fe
e0 00 00 01 11 64 ec 1e 00 00 00 00 02 7d 00 00";

pub const V_IGMP3_QUERY_GROUP: &str = "
46 c0 00 24 00 00 40 00 01 02 40 1a c0 00 02 fe
e0 00 00 fb 94 04 00 00 11 0a 0b 7d e0 00 00 fb
02 7d 00 00";

pub const V_IGMP1_QUERY: &str = "
45 00 00 1c 00 00 00 00 01 02 16 e1 c0 00 02 fe
e0 00 00 01 11 00 ee ff 00 00 00 00";

pub const V_ICMP_PROTO_UNREACH_GEN: &str = "
45 00 00 30 00 00 40 00 40 01 b6 c9 c0 00 02 01
c0 00 02 02 03 02 fc fd 00 00 00 00 45 00 00 14
00 00 40 00 40 fd b5 e9 c0 00 02 02 c0 00 02 01";

pub const V_ARP_REQ_B: &str = "
ff ff ff ff ff ff 02 00 00 00 00 0b 08 06 00 01
08 00 06 04 00 01 02 00 00 00 00 0b c0 00 02 02
00 00 00 00 00 00 c0 00 02 01 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00";

pub const V_ARP_REPLY_A: &str = "
02 00 00 00 00 0b 02 00 00 00 00 0a 08 06 00 01
08 00 06 04 00 02 02 00 00 00 00 0a c0 00 02 01
02 00 00 00 00 0b c0 00 02 02 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00";

pub const V_ARP_REQ_OTHER: &str = "
ff ff ff ff ff ff 02 00 00 00 00 0b 08 06 00 01
08 00 06 04 00 01 02 00 00 00 00 0b c0 00 02 02
00 00 00 00 00 00 c0 00 02 07 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00";

pub const V_ARP_PROBE_B: &str = "
ff ff ff ff ff ff 02 00 00 00 00 0b 08 06 00 01
08 00 06 04 00 01 02 00 00 00 00 0b 00 00 00 00
00 00 00 00 00 00 c0 00 02 01 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00";

pub const V_ARP_REPLY_TO_PROBE: &str = "
02 00 00 00 00 0b 02 00 00 00 00 0a 08 06 00 01
08 00 06 04 00 02 02 00 00 00 00 0a c0 00 02 01
02 00 00 00 00 0b 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00";

pub const V_ARP_CONFLICT_B: &str = "
ff ff ff ff ff ff 02 00 00 00 00 0b 08 06 00 01
08 00 06 04 00 01 02 00 00 00 00 0b c0 00 02 01
00 00 00 00 00 00 c0 00 02 01 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00";

pub const V_ARP_ROUTER_MOVED: &str = "
ff ff ff ff ff ff 02 00 00 00 00 66 08 06 00 01
08 00 06 04 00 01 02 00 00 00 00 66 c0 00 02 fe
00 00 00 00 00 00 c0 00 02 fe 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00";

pub const V_ARP_REQ_R: &str = "
ff ff ff ff ff ff 02 00 00 00 00 0a 08 06 00 01
08 00 06 04 00 01 02 00 00 00 00 0a c0 00 02 01
00 00 00 00 00 00 c0 00 02 fe 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00";

pub const V_ARP_REPLY_R: &str = "
02 00 00 00 00 0a 02 00 00 00 00 fe 08 06 00 01
08 00 06 04 00 02 02 00 00 00 00 fe c0 00 02 fe
02 00 00 00 00 0a c0 00 02 01 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00";

pub const V_ARP_POLL_R: &str = "
02 00 00 00 00 fe 02 00 00 00 00 0a 08 06 00 01
08 00 06 04 00 01 02 00 00 00 00 0a c0 00 02 01
00 00 00 00 00 00 c0 00 02 fe 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00";

pub const V_ARP_REPLY_R_MOVED: &str = "
02 00 00 00 00 0a 02 00 00 00 00 66 08 06 00 01
08 00 06 04 00 02 02 00 00 00 00 66 c0 00 02 fe
02 00 00 00 00 0a c0 00 02 01 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00";

pub const V_FRAG_1: &str = "
45 00 00 24 1a 2b 20 00 40 11 bc 9a c0 00 02 02
c0 00 02 01 13 88 13 89 00 20 7b b4 30 31 32 33
34 35 36 37";

impl H {
    /// A UDP datagram handed to [ip] now: the frame when it left at once, `None` when held.
    pub fn send(&mut self, source: Ipv4Addr, destination: Ipv4Addr, sport: u16, dport: u16, data: &[u8]) -> Result<Option<Vec<u8>>, Counter> {
        self.send_ttl(source, destination, sport, dport, data, Ttl::DEFAULT)
    }

    pub fn send_ttl(&mut self, source: Ipv4Addr, destination: Ipv4Addr, sport: u16, dport: u16, data: &[u8], ttl: Ttl) -> Result<Option<Vec<u8>>, Counter> {
        let mut frame = [0u8; FRAME];
        let datagram = UdpBuilder { source: Port::new(sport).unwrap(), destination: Port::new(dport).unwrap(), data };
        let out = UdpOut { source, destination, ttl, datagram };
        let sent = self.ip.send_udp(self.clock(), &out, &mut frame);
        self.collect();
        sent.map(|s| match s {
            Sent::Frame(n) => Some(frame[..n].to_vec()),
            Sent::Held => None,
        })
    }

    /// A UDP datagram from A to `destination`, 5001 → 5001.
    pub fn udp_to(&mut self, destination: Ipv4Addr) -> Result<Option<Vec<u8>>, Counter> {
        self.send(A, destination, 5001, 5001, b"hi")
    }

    /// `addr` at `m` asks who has A: learned into STALE, answered.
    pub fn stale(&mut self, addr: Ipv4Addr, m: MacAddr) {
        self.frame(&eth(MacAddr::BROADCAST, m, 0x0806, &arp_packet(1, m, addr, MacAddr::ZERO, A)));
        self.out();
        assert!(self.is_stale(addr), "{addr} is not STALE");
    }
}

/// A seeded xorshift, for the property tests' generators.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

pub const MAC_A1: MacAddr = MacAddr([2, 0, 0, 0, 0, 0x1a]);

impl H {
    /// A second interface with its own MAC and link up, at setup time 0.
    pub fn add_if1(&mut self) -> IfIndex {
        let if1 = self.ip.add_interface(Instant::from_nanos(0), mac(MAC_A1));
        self.ip.link_up(Instant::from_nanos(0), if1).unwrap();
        if1
    }

    /// ToyOS as B: MAC B, 192.0.2.2/24, A resolved at setup.
    pub fn as_b() -> Self {
        let mut ip = Ip::new(Instant::from_nanos(0), SECRET);
        let if0 = ip.add_interface(Instant::from_nanos(0), mac(MAC_B));
        ip.link_up(Instant::from_nanos(0), if0).unwrap();
        let mut h = Self { ip, if0, now: 0, events: Vec::new(), counters: Vec::new(), wire: std::collections::BTreeMap::default() };
        h.assign(if0, B, 24, 0);
        let setup = Instant::from_millis(5_000);
        let _ = h.ip.resolve(setup, if0, A);
        h.ip.transmit(setup, usize::MAX, |_, _| {});
        let _ = h.ip.receive(setup, if0, &eth(MAC_B, MAC_A, 0x0806, &arp_packet(2, MAC_A, A, MAC_B, B)));
        h.settle();
        h
    }

    /// A datagram in a frame to `destination` from B's MAC.
    pub fn datagram_to(&mut self, destination: MacAddr, ip: &[u8]) -> Option<toyos_net_ip::Delivery<'static>> {
        self.frame(&eth(destination, MAC_B, 0x0800, ip))
    }
}

/// The destination class and interface a datagram was delivered with, or `None`.
pub fn cast_of(d: &Option<toyos_net_ip::Delivery<'_>>) -> Option<toyos_net_ip::Cast> {
    match d {
        Some(toyos_net_ip::Delivery::Udp(arrival, _)) | Some(toyos_net_ip::Delivery::Tcp(arrival, _)) => Some(arrival.cast),
        _ => None,
    }
}

impl H {
    /// A host with MAC `m` holding `addr/len`, assigned through conflict detection, nothing learned.
    pub fn host(m: MacAddr, addr: Ipv4Addr, len: u8) -> Self {
        let mut ip = Ip::new(Instant::from_nanos(0), SECRET);
        let if0 = ip.add_interface(Instant::from_nanos(0), mac(m));
        ip.link_up(Instant::from_nanos(0), if0).unwrap();
        let mut h = Self { ip, if0, now: 0, events: Vec::new(), counters: Vec::new(), wire: std::collections::BTreeMap::default() };
        h.assign(if0, addr, len, 0);
        h.settle();
        h
    }
}
