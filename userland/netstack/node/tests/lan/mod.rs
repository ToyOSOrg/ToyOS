//! `common`'s node on a link with a second host and a way off it, for what leaves the node
//! besides its lease: B is 192.0.2.7 at 02:00:00:00:00:0b, and 198.51.100.7 is a host behind the
//! router.
//!
//! Every frame the node emits is read by `etherparse` as it leaves ([`outside`]) and kept with
//! the time it left: the lengths, the IPv4 header checksum and the UDP, ICMP or IGMP checksum are
//! that crate's sums. The datagrams the node receives are built here from RFC 791's and RFC 768's
//! layouts, with `common`'s RFC 1071 sum.

#![allow(dead_code)]

use std::net::Ipv4Addr;
use std::time::Duration;

use etherparse::icmpv4::DestUnreachableHeader;
use etherparse::{ArpOperation, Icmpv4Type, LinkSlice, NetSlice, SlicedPacket, TransportSlice};
use toyos_net_node::Node;
use toyos_net_wire::Instant;

use crate::common::{arp, batch, from_server, message_of, option, sum, terms, xid, Wire, A, ACK, BROADCAST, DISCOVER, ELSEWHERE, MAC, MAC_B, MAC_R, OFFER, R};

pub const B: Ipv4Addr = ELSEWHERE;
pub const OFF_LINK: Ipv4Addr = Ipv4Addr::new(198, 51, 100, 7);

/// A UDP datagram the node emitted, as `etherparse` read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Udp {
    /// The frame's destination.
    pub to: [u8; 6],
    pub source: Ipv4Addr,
    pub source_port: u16,
    pub destination: Ipv4Addr,
    pub port: u16,
    pub ttl: u8,
    pub payload: Vec<u8>,
}

/// A frame the node emitted, as `etherparse` read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Seen {
    Arp { request: bool, sender: Ipv4Addr, target: Ipv4Addr },
    Udp(Udp),
    /// An IGMP message whole, and what its IPv4 header says: the type-of-service octet and the
    /// options' bytes among it.
    Igmp { to: [u8; 6], source: Ipv4Addr, destination: Ipv4Addr, ttl: u8, tos: u8, options: Vec<u8>, message: Vec<u8> },
    /// An ICMP destination unreachable, code 3.
    PortUnreachable { to: Ipv4Addr },
}

fn v4(bytes: &[u8]) -> Ipv4Addr {
    Ipv4Addr::from(<[u8; 4]>::try_from(bytes).expect("four bytes"))
}

/// The outside reading of a frame the node emitted: Ethernet II from the node's MAC, then ARP,
/// UDP, IGMP or a port unreachable, each with the lengths and checksums `etherparse` computes.
pub fn outside(frame: &[u8]) -> Seen {
    let packet = SlicedPacket::from_ethernet(frame).expect("Ethernet II");
    let Some(LinkSlice::Ethernet2(ethernet)) = &packet.link else { panic!("{:?}", packet.link) };
    assert_eq!(ethernet.source(), MAC, "from the node's MAC");
    let to = ethernet.destination();
    match (&packet.net, &packet.transport) {
        (Some(NetSlice::Arp(arp)), None) => {
            assert_eq!(arp.sender_hw_addr(), MAC);
            let request = arp.operation() == ArpOperation::REQUEST;
            assert!(request || arp.operation() == ArpOperation::REPLY, "{:?}", arp.operation());
            Seen::Arp { request, sender: v4(arp.sender_protocol_addr()), target: v4(arp.target_protocol_addr()) }
        }
        (Some(NetSlice::Ipv4(ip)), Some(transport)) => {
            let header = ip.header();
            assert_eq!(header.header_checksum(), header.to_header().calc_header_checksum(), "the IPv4 header checksum");
            assert!(usize::from(header.total_len()) <= frame.len() - 14, "the datagram fits its frame");
            let (source, destination, ttl) = (header.source_addr(), header.destination_addr(), header.ttl());
            match transport {
                TransportSlice::Udp(udp) => {
                    assert_eq!(usize::from(header.total_len()), 20 + 8 + udp.payload().len(), "IPv4's length is UDP's");
                    assert_eq!(usize::from(udp.length()), 8 + udp.payload().len(), "the UDP length");
                    let sum = udp.to_header().calc_checksum_ipv4_raw(header.source(), header.destination(), udp.payload()).unwrap();
                    assert_eq!(udp.checksum(), sum, "the UDP checksum");
                    Seen::Udp(Udp { to, source, source_port: udp.source_port(), destination, port: udp.destination_port(), ttl, payload: udp.payload().to_vec() })
                }
                TransportSlice::Igmp(igmp) => {
                    assert!(igmp.is_checksum_valid(), "the IGMP checksum");
                    Seen::Igmp { to, source, destination, ttl, tos: header.slice()[1], options: header.options().to_vec(), message: igmp.slice().to_vec() }
                }
                TransportSlice::Icmpv4(icmp) => {
                    assert_eq!(icmp.checksum(), icmp.icmp_type().calc_checksum(icmp.payload()), "the ICMP checksum");
                    assert_eq!(icmp.icmp_type(), Icmpv4Type::DestinationUnreachable(DestUnreachableHeader::Port));
                    Seen::PortUnreachable { to: destination }
                }
                other => panic!("neither UDP, IGMP nor ICMP: {other:?}"),
            }
        }
        other => panic!("neither ARP nor IPv4: {other:?}"),
    }
}

/// A UDP datagram (RFC 768) in an IPv4 datagram (RFC 791: no options, DF, TTL 64) in a frame from
/// `from` to `to`.
pub fn udp(to: [u8; 6], from: [u8; 6], source: (Ipv4Addr, u16), destination: (Ipv4Addr, u16), payload: &[u8]) -> Vec<u8> {
    let length = u16::try_from(8 + payload.len()).unwrap().to_be_bytes();
    let mut udp = [&source.1.to_be_bytes()[..], &destination.1.to_be_bytes(), &length, &[0, 0], payload].concat();
    let pseudo = [&source.0.octets()[..], &destination.0.octets(), &[0, 17, length[0], length[1]], &udp].concat();
    // A sum of zero is sent as all ones: zero says no checksum was computed.
    let check = match sum(&pseudo) {
        0 => 0xFFFF,
        check => check,
    };
    udp[6..8].copy_from_slice(&check.to_be_bytes());
    let total = u16::try_from(20 + udp.len()).unwrap().to_be_bytes();
    let mut ip = vec![0x45, 0, total[0], total[1], 0, 0, 0x40, 0, 64, 17, 0, 0];
    ip.extend_from_slice(&source.0.octets());
    ip.extend_from_slice(&destination.0.octets());
    let check = sum(&ip);
    ip[10..12].copy_from_slice(&check.to_be_bytes());
    [&to[..], &from, &[0x08, 0x00], &ip, &udp].concat()
}

pub struct Lan {
    pub node: Node,
    pub now: Instant,
    /// Every frame the node emitted, in order, with the time it left.
    pub sent: Vec<(Instant, Seen)>,
    /// The last draw handed out: each is the one before plus one.
    draws: u32,
}

fn draw(draws: &mut u32) -> impl FnMut() -> u32 + '_ {
    move || {
        *draws += 1;
        *draws
    }
}

impl Lan {
    /// `common`'s node, an hour into the clock, its link down.
    pub fn new() -> Self {
        let Wire { node, now, .. } = Wire::new();
        Self { node, now, sent: Vec::new(), draws: 0x6b00_0000 }
    }

    /// One transmit opportunity with all the credit the node wants. Returns how many frames left.
    pub fn opportunity(&mut self) -> usize {
        let (sent, now) = (&mut self.sent, self.now);
        self.node.transmit(now, usize::MAX, |frame| sent.push((now, outside(frame))), draw(&mut self.draws))
    }

    /// Opportunities until the node sends nothing more, the router and B answering each ARP
    /// request for their own address.
    pub fn pump(&mut self) {
        loop {
            let from = self.sent.len();
            if self.opportunity() == 0 {
                return;
            }
            let asked: Vec<Ipv4Addr> = self.sent[from..]
                .iter()
                .filter_map(|(_, seen)| match seen {
                    Seen::Arp { request: true, sender, target } if *sender == A && (*target == R || *target == B) => Some(*target),
                    _ => None,
                })
                .collect();
            for target in asked {
                let mac = if target == R { MAC_R } else { MAC_B };
                self.node.receive(self.now, batch(&[&arp(MAC, false, mac, target, A)]), draw(&mut self.draws));
            }
        }
    }

    pub fn deliver(&mut self, frame: &[u8]) {
        self.node.receive(self.now, batch(&[frame]), draw(&mut self.draws));
        self.pump();
    }

    pub fn link(&mut self, up: bool) {
        self.node.link(self.now, up, draw(&mut self.draws));
        self.pump();
    }

    /// Moves the clock to `at` and fires what is due.
    pub fn fire(&mut self, at: Instant) {
        self.now = self.now.max(at);
        self.node.fire(self.now, draw(&mut self.draws));
        self.pump();
    }

    /// Fires deadline after deadline until `done`, giving up at the first deadline more than
    /// `limit` away.
    pub fn run_until(&mut self, limit: Duration, done: impl Fn(&Lan) -> bool) -> bool {
        let end = self.now.after(limit);
        for _ in 0..100_000 {
            if done(self) {
                return true;
            }
            match self.node.next_deadline() {
                Some(at) if at <= end => self.fire(at),
                _ => return false,
            }
        }
        panic!("100,000 deadlines without the clock passing {limit:?}");
    }

    /// The link up and the server's OFFER and ACK of `A` on `terms` delivered: the address is
    /// under probe.
    pub fn acknowledge(&mut self, terms: &[(u8, Vec<u8>)]) {
        self.link(true);
        let discover = self.udp().into_iter().rfind(|udp| udp.port == 67).expect("a DISCOVER").payload.clone();
        assert_eq!(option(&discover, 53), Some(&[DISCOVER][..]));
        let id = xid(&discover);
        self.deliver(&from_server(MAC, A, &message_of(OFFER, id, terms)));
        self.deliver(&from_server(BROADCAST, Ipv4Addr::BROADCAST, &message_of(ACK, id, terms)));
        assert_eq!(self.node.lease(), None, "acknowledged is not held");
    }

    /// [`Self::acknowledge`], and conflict detection run to its end: the lease is held.
    pub fn lease_on(&mut self, terms: &[(u8, Vec<u8>)]) {
        self.acknowledge(terms);
        assert!(self.run_until(Duration::from_secs(10), |lan| lan.node.lease().is_some()), "the lease is held");
    }

    /// A lease of `A`/24 for `seconds`, through the router.
    pub fn lease(&mut self, seconds: u32) {
        self.lease_on(&terms(seconds, Some(R)));
    }

    /// Every UDP datagram the node has sent.
    pub fn udp(&self) -> Vec<&Udp> {
        self.sent
            .iter()
            .filter_map(|(_, seen)| match seen {
                Seen::Udp(udp) => Some(udp),
                _ => None,
            })
            .collect()
    }

    /// The UDP datagrams the node has sent that are not its DHCP client's, each with the time it
    /// left.
    pub fn datagrams(&self) -> Vec<(Instant, &Udp)> {
        self.sent
            .iter()
            .filter_map(|(at, seen)| match seen {
                Seen::Udp(udp) if udp.port != 67 => Some((*at, udp)),
                _ => None,
            })
            .collect()
    }

    /// One of [udp]'s counters.
    pub fn counted(&self, counter: toyos_net_udp::Counter) -> u64 {
        self.node.shard().udp_counters().get(counter)
    }
}
