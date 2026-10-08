//! One node on a wire the test holds, in the specifications' numbers (RFC 5737): the node is at
//! 02:00:00:00:00:0a and is leased 192.0.2.1/24 by the server and router 192.0.2.254 at
//! 02:00:00:00:00:fe, with 192.0.2.53 to resolve at.
//!
//! Every frame the node emits is read by `etherparse` as it leaves ([`outside`]), in every test:
//! its lengths, its IPv4 header checksum and its UDP or ICMP checksum are that crate's sums, not
//! `toyos-net-wire`'s. The frames the node receives are built here from the RFCs' layouts with
//! an RFC 1071 sum written apart from both.

#![allow(dead_code)]

use std::net::Ipv4Addr;
use std::time::Duration;

use etherparse::{ArpOperation, Icmpv4Type, LinkSlice, NetSlice, SlicedPacket, TransportSlice};
use toyos_dhcp::HostName;
use toyos_net_node::Node;
use toyos_net_shard::{Config, Secrets};
use toyos_net_wire::ethernet::{IndividualMac, MacAddr};
use toyos_net_wire::Instant;

pub const MAC: [u8; 6] = [2, 0, 0, 0, 0, 0x0a];
pub const MAC_B: [u8; 6] = [2, 0, 0, 0, 0, 0x0b];
pub const MAC_R: [u8; 6] = [2, 0, 0, 0, 0, 0xfe];
pub const BROADCAST: [u8; 6] = [0xff; 6];
pub const A: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 1);
pub const ELSEWHERE: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 7);
pub const R: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 254);
pub const DNS: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 53);
pub const MASK: Ipv4Addr = Ipv4Addr::new(255, 255, 255, 0);

/// The places `Wire`'s node has for streams and listeners.
pub const PLACES: usize = 8;

pub const DISCOVER: u8 = 1;
pub const OFFER: u8 = 2;
pub const REQUEST: u8 = 3;
pub const DECLINE: u8 = 4;
pub const ACK: u8 = 5;
pub const NAK: u8 = 6;

/// A frame the node emitted, as `etherparse` read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Seen {
    Arp { to: [u8; 6], request: bool, sender: Ipv4Addr, target: Ipv4Addr },
    /// A datagram from port 68 to port 67, and its payload.
    Dhcp { to: [u8; 6], source: Ipv4Addr, destination: Ipv4Addr, message: Vec<u8> },
    EchoReply { source: Ipv4Addr, destination: Ipv4Addr, id: u16, seq: u16, data: Vec<u8> },
}

impl Seen {
    /// The DHCP message type and the message, of a DHCP frame.
    pub fn dhcp(&self) -> Option<(u8, &[u8])> {
        match self {
            Seen::Dhcp { message, .. } => Some((option(message, 53).expect("a message type")[0], message.as_slice())),
            Seen::Arp { .. } | Seen::EchoReply { .. } => None,
        }
    }

    /// The address an IPv4 frame was sent from.
    pub fn source(&self) -> Option<Ipv4Addr> {
        match self {
            Seen::Dhcp { source, .. } | Seen::EchoReply { source, .. } => Some(*source),
            Seen::Arp { .. } => None,
        }
    }

    /// An ARP probe for `address`: a request from 0.0.0.0 (RFC 5227 §2.1.1).
    pub fn is_probe(&self, address: Ipv4Addr) -> bool {
        *self == Seen::Arp { to: BROADCAST, request: true, sender: Ipv4Addr::UNSPECIFIED, target: address }
    }

    /// An ARP announcement of `address`: a request naming it as sender and target (RFC 5227 §2.3).
    pub fn is_announcement(&self, address: Ipv4Addr) -> bool {
        *self == Seen::Arp { to: BROADCAST, request: true, sender: address, target: address }
    }
}

fn v4(bytes: &[u8]) -> Ipv4Addr {
    Ipv4Addr::from(<[u8; 4]>::try_from(bytes).expect("four bytes"))
}

/// The outside reading of a frame the node emitted: Ethernet II from the node's MAC, then ARP, a
/// DHCP client's datagram or an echo reply, each with the lengths and checksums `etherparse`
/// computes. Anything else the node has no business sending here.
pub fn outside(frame: &[u8], mac: [u8; 6]) -> Seen {
    let packet = SlicedPacket::from_ethernet(frame).expect("Ethernet II");
    let Some(LinkSlice::Ethernet2(ethernet)) = &packet.link else { panic!("{:?}", packet.link) };
    assert_eq!(ethernet.source(), mac, "from the node's MAC");
    let to = ethernet.destination();
    match (&packet.net, &packet.transport) {
        (Some(NetSlice::Arp(arp)), None) => {
            assert_eq!(arp.sender_hw_addr(), mac);
            let request = arp.operation() == ArpOperation::REQUEST;
            assert!(request || arp.operation() == ArpOperation::REPLY, "{:?}", arp.operation());
            Seen::Arp { to, request, sender: v4(arp.sender_protocol_addr()), target: v4(arp.target_protocol_addr()) }
        }
        (Some(NetSlice::Ipv4(ip)), Some(transport)) => {
            let header = ip.header();
            assert_eq!(header.header_checksum(), header.to_header().calc_header_checksum(), "the IPv4 header checksum");
            assert!(usize::from(header.total_len()) <= frame.len() - 14, "the datagram fits its frame");
            let (source, destination) = (header.source_addr(), header.destination_addr());
            match transport {
                TransportSlice::Udp(udp) => {
                    assert_eq!(usize::from(header.total_len()), 20 + 8 + udp.payload().len(), "IPv4's length is UDP's");
                    assert_eq!(usize::from(udp.length()), 8 + udp.payload().len(), "the UDP length");
                    let sum = udp.to_header().calc_checksum_ipv4_raw(header.source(), header.destination(), udp.payload()).unwrap();
                    assert_eq!(udp.checksum(), sum, "the UDP checksum");
                    assert_eq!((udp.source_port(), udp.destination_port()), (68, 67));
                    Seen::Dhcp { to, source, destination, message: udp.payload().to_vec() }
                }
                TransportSlice::Icmpv4(icmp) => {
                    assert_eq!(icmp.checksum(), icmp.icmp_type().calc_checksum(icmp.payload()), "the ICMP checksum");
                    let Icmpv4Type::EchoReply(echo) = icmp.icmp_type() else { panic!("{:?}", icmp.icmp_type()) };
                    Seen::EchoReply { source, destination, id: echo.id, seq: echo.seq, data: icmp.payload().to_vec() }
                }
                other => panic!("neither UDP nor ICMP: {other:?}"),
            }
        }
        other => panic!("neither ARP nor IPv4: {other:?}"),
    }
}

/// The value of option `code` in a DHCP message (RFC 2132 §2): the first instance.
pub fn option(message: &[u8], code: u8) -> Option<&[u8]> {
    let mut rest = &message[240..];
    loop {
        match rest {
            [] | [255, ..] => return None,
            [0, after @ ..] => rest = after,
            [found, len, after @ ..] => {
                let (value, next) = after.split_at(usize::from(*len));
                if *found == code {
                    return Some(value);
                }
                rest = next;
            }
            [_] => panic!("an option without a length"),
        }
    }
}

/// Hex, whitespace ignored.
pub fn hex(text: &str) -> Vec<u8> {
    let digits: Vec<u8> = text.bytes().filter(u8::is_ascii_hexdigit).collect();
    digits.chunks(2).map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap()).collect()
}

/// The transaction id of a DHCP message (RFC 2131 §2).
pub fn xid(message: &[u8]) -> u32 {
    u32::from_be_bytes(message[4..8].try_into().unwrap())
}

/// RFC 1071.
pub fn sum(bytes: &[u8]) -> u16 {
    let mut sum: u32 = bytes.chunks(2).map(|p| u32::from(p[0]) << 8 | u32::from(*p.get(1).unwrap_or(&0))).sum();
    while sum > 0xFFFF {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

fn ethernet(to: [u8; 6], from: [u8; 6], ether_type: [u8; 2], body: &[u8]) -> Vec<u8> {
    [&to[..], &from, &ether_type, body].concat()
}

/// An IPv4 datagram from `source` (DF, TTL 64) in a frame from `from` to `to`.
fn ipv4(to: [u8; 6], from: [u8; 6], source: Ipv4Addr, destination: Ipv4Addr, protocol: u8, payload: &[u8]) -> Vec<u8> {
    let total = u16::try_from(20 + payload.len()).unwrap().to_be_bytes();
    let mut ip = vec![0x45, 0, total[0], total[1], 0, 0, 0x40, 0, 64, protocol, 0, 0];
    ip.extend_from_slice(&source.octets());
    ip.extend_from_slice(&destination.octets());
    let check = sum(&ip);
    ip[10..12].copy_from_slice(&check.to_be_bytes());
    ip.extend_from_slice(payload);
    ethernet(to, from, [0x08, 0x00], &ip)
}

/// A server message (RFC 2131 §2): BOOTREPLY for the node's MAC, the cookie, the message type,
/// then `options` and END.
pub fn message(kind: u8, xid: u32, yiaddr: Ipv4Addr, options: &[(u8, Vec<u8>)]) -> Vec<u8> {
    let mut m = vec![2, 1, 6, 0];
    m.extend_from_slice(&xid.to_be_bytes());
    m.extend_from_slice(&[0; 8]);
    m.extend_from_slice(&yiaddr.octets());
    m.extend_from_slice(&[0; 8]);
    m.extend_from_slice(&MAC);
    m.resize(236, 0);
    m.extend_from_slice(&[0x63, 0x82, 0x53, 0x63]);
    for (code, value) in [(53, vec![kind])].iter().chain(options) {
        m.push(*code);
        m.push(u8::try_from(value.len()).unwrap());
        m.extend_from_slice(value);
    }
    m.push(255);
    m
}

/// A server message offering or acknowledging `A` on `terms`.
pub fn message_of(kind: u8, xid: u32, terms: &[(u8, Vec<u8>)]) -> Vec<u8> {
    message(kind, xid, A, terms)
}

/// The options of a lease of `seconds` from the server: its identifier, the time, the mask, the
/// router if any, and the resolver.
pub fn terms(seconds: u32, router: Option<Ipv4Addr>) -> Vec<(u8, Vec<u8>)> {
    let mut options = vec![(54, R.octets().to_vec()), (51, seconds.to_be_bytes().to_vec()), (1, MASK.octets().to_vec())];
    options.extend(router.map(|r| (3, r.octets().to_vec())));
    options.push((6, DNS.octets().to_vec()));
    options
}

/// `payload` from the server's port 67 to `destination`'s port 68, in a frame to `to`.
pub fn from_server(to: [u8; 6], destination: Ipv4Addr, payload: &[u8]) -> Vec<u8> {
    let length = u16::try_from(8 + payload.len()).unwrap().to_be_bytes();
    let mut udp = vec![0, 67, 0, 68, length[0], length[1], 0, 0];
    udp.extend_from_slice(payload);
    let pseudo = [&R.octets()[..], &destination.octets(), &[0, 17, length[0], length[1]], &udp].concat();
    let check = sum(&pseudo);
    udp[6..8].copy_from_slice(&check.to_be_bytes());
    ipv4(to, MAC_R, R, destination, 17, &udp)
}

/// An ARP packet (RFC 826) from `mac`, which says it is at `sender`, about `target`.
pub fn arp(to: [u8; 6], request: bool, mac: [u8; 6], sender: Ipv4Addr, target: Ipv4Addr) -> Vec<u8> {
    let mut body = vec![0, 1, 0x08, 0, 6, 4, 0, if request { 1 } else { 2 }];
    body.extend_from_slice(&mac);
    body.extend_from_slice(&sender.octets());
    body.extend_from_slice(&if request { [0; 6] } else { MAC });
    body.extend_from_slice(&target.octets());
    ethernet(to, mac, [0x08, 0x06], &body)
}

/// An echo request (RFC 792) from the router to `destination`, in a frame to the node.
pub fn echo_request(destination: Ipv4Addr, id: u16, seq: u16, data: &[u8]) -> Vec<u8> {
    let mut icmp = vec![8, 0, 0, 0];
    icmp.extend_from_slice(&id.to_be_bytes());
    icmp.extend_from_slice(&seq.to_be_bytes());
    icmp.extend_from_slice(data);
    let check = sum(&icmp);
    icmp[2..4].copy_from_slice(&check.to_be_bytes());
    ipv4(MAC, MAC_R, R, destination, 1, &icmp)
}

pub struct Wire {
    pub node: Node,
    mac: [u8; 6],
    pub now: Instant,
    /// Every frame the node emitted, in order.
    pub sent: Vec<Seen>,
    /// The last draw handed out: each is the one before plus one.
    draws: u32,
}

fn draw(draws: &mut u32) -> impl FnMut() -> u32 + '_ {
    move || {
        *draws += 1;
        *draws
    }
}

impl Wire {
    /// The node at the fixtures' MAC.
    pub fn new() -> Self {
        Self::at(MAC)
    }

    /// The node an hour into the clock, its link down.
    pub fn at(mac: [u8; 6]) -> Self {
        let secrets = Secrets {
            ip: [1; 16],
            resets: [2; 16],
            tcp: toyos_net_tcp::Secrets { isn: [3; 16], timestamp: [4; 16], port_offset: [5; 16], port_index: [6; 16], port_table: [0; 16] },
        };
        let now = Instant::from_millis(3_600_000);
        let config = Config { mac: IndividualMac::new(MacAddr(mac)).unwrap(), receive_buffer: 65_535, send_buffer: 65_535, secrets };
        let mut draws = 0x5a00_0000;
        let mut node = Node::new(now, config, HostName::new("toyos"), draw(&mut draws)).unwrap();
        node.set_places(now, PLACES);
        Self { node, mac, now, sent: Vec::new(), draws }
    }

    /// Offers the node all the credit it wants until it sends nothing more, the router answering
    /// each ARP request for its own address.
    pub fn pump(&mut self) {
        loop {
            let from = self.sent.len();
            let (sent, mac) = (&mut self.sent, self.mac);
            self.node.transmit(self.now, usize::MAX, |frame| sent.push(outside(frame, mac)));
            if self.sent.len() == from {
                return;
            }
            let asked = self.sent[from..].iter().filter(|seen| matches!(seen, Seen::Arp { request: true, sender, target, .. } if *target == R && *sender == A)).count();
            for _ in 0..asked {
                self.node.receive(self.now, &arp(MAC, false, MAC_R, R, A), draw(&mut self.draws));
            }
        }
    }

    /// The next draw is `next`: the transaction id of the exchange the next call starts.
    pub fn seed(&mut self, next: u32) {
        self.draws = next.wrapping_sub(1);
    }

    pub fn deliver(&mut self, frame: &[u8]) {
        self.node.receive(self.now, frame, draw(&mut self.draws));
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
    pub fn run_until(&mut self, limit: Duration, done: impl Fn(&Wire) -> bool) -> bool {
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

    /// The DHCP messages the node has sent, each with its type.
    pub fn dhcp(&self) -> Vec<(u8, &[u8])> {
        self.sent.iter().filter_map(Seen::dhcp).collect()
    }

    /// The last DHCP message the node sent, which is of type `kind`.
    pub fn last(&self, kind: u8) -> &[u8] {
        let (found, message) = *self.dhcp().last().expect("a DHCP message");
        assert_eq!(found, kind, "the last DHCP message's type");
        message
    }

    /// The interface's address `address`, in [ip]'s words.
    pub fn address(&self, address: Ipv4Addr) -> Option<toyos_net_ip::AddrState> {
        let shard = self.node.shard();
        shard.ip().address(shard.iface(), address)
    }

    pub fn gateways(&self) -> Vec<Ipv4Addr> {
        let shard = self.node.shard();
        shard.ip().gateways(shard.iface()).expect("the interface").to_vec()
    }

    /// Whether the node answers an ARP request for `address` from the router.
    pub fn answers_arp_for(&mut self, address: Ipv4Addr) -> bool {
        let from = self.sent.len();
        self.deliver(&arp(BROADCAST, true, MAC_R, R, address));
        self.sent[from..].contains(&Seen::Arp { to: MAC_R, request: false, sender: address, target: R })
    }

    /// The link up and the server's OFFER and ACK of `A` on `terms` delivered: the address is
    /// under probe.
    pub fn acknowledged(terms: &[(u8, Vec<u8>)]) -> Self {
        let mut wire = Self::new();
        wire.link(true);
        let id = xid(wire.last(DISCOVER));
        wire.deliver(&from_server(MAC, A, &message_of(OFFER, id, terms)));
        assert_eq!(xid(wire.last(REQUEST)), id);
        wire.deliver(&from_server(BROADCAST, Ipv4Addr::BROADCAST, &message_of(ACK, id, terms)));
        wire
    }

    /// [`Self::acknowledged`], and conflict detection run to its end: the lease is held.
    pub fn leased(terms: &[(u8, Vec<u8>)]) -> Self {
        let mut wire = Self::acknowledged(terms);
        assert!(wire.run_until(Duration::from_secs(10), |wire| wire.node.lease().is_some()), "the lease is held");
        wire
    }
}
