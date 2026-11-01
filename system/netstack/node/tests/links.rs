//! A node over two links, the wireless one added first and the wired one second: each leases by
//! its own client, lookups ask the wired link's resolvers and follow the links up and down, and
//! the machine's name is probed for, held and lost on each link by itself.

use std::net::Ipv4Addr;
use std::time::Duration;

use etherparse::{ArpOperation, LinkSlice, NetSlice, PacketBuilder, SlicedPacket, TransportSlice};
use toyos_dhcp::HostName;
use toyos_dns::Name;
use toyos_mdns::Host;
use toyos_net_node::{Event, IfIndex, Node, Rank};
use toyos_net_shard::{Config, Secrets};
use toyos_net_wire::ethernet::{IndividualMac, MacAddr};
use toyos_net_wire::Instant;

const WIRED: Rank = Rank(0);
const WIRELESS: Rank = Rank(1);
const ROUTER_MAC: [u8; 6] = [2, 0, 0, 0, 0, 0xfe];
const OTHER_HOST_MAC: [u8; 6] = [2, 0, 0, 0, 0, 0xee];
const GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
const GROUP_MAC: [u8; 6] = [0x01, 0x00, 0x5e, 0x00, 0x00, 0xfb];
const MDNS: u16 = 5_353;

/// One network: the node's link address on it, the address its server leases, and its router,
/// which is its DHCP server, and its resolver.
struct Net {
    mac: [u8; 6],
    addr: Ipv4Addr,
    router: Ipv4Addr,
    dns: Ipv4Addr,
}

const WIRED_NET: Net = Net { mac: [2, 0, 0, 0, 0, 0x01], addr: Ipv4Addr::new(192, 0, 2, 10), router: Ipv4Addr::new(192, 0, 2, 1), dns: Ipv4Addr::new(192, 0, 2, 53) };
/// A wired and a wireless link on one LAN: one router and server, one resolver.
const LAN_WIRED: Net = Net { mac: [2, 0, 0, 0, 0, 0x01], addr: Ipv4Addr::new(192, 168, 1, 10), router: Ipv4Addr::new(192, 168, 1, 1), dns: Ipv4Addr::new(192, 168, 1, 53) };
const LAN_WIRELESS: Net = Net { mac: [2, 0, 0, 0, 0, 0x02], addr: Ipv4Addr::new(192, 168, 1, 20), router: Ipv4Addr::new(192, 168, 1, 1), dns: Ipv4Addr::new(192, 168, 1, 53) };
const WIRELESS_NET: Net =
    Net { mac: [2, 0, 0, 0, 0, 0x02], addr: Ipv4Addr::new(198, 51, 100, 10), router: Ipv4Addr::new(198, 51, 100, 1), dns: Ipv4Addr::new(198, 51, 100, 53) };

/// A frame the node sent, as the outside reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Out {
    ArpRequest { sender: Ipv4Addr, target: Ipv4Addr },
    /// A DHCP message: its type and transaction id.
    Dhcp { kind: u8, xid: u32 },
    Udp { source: Ipv4Addr, destination: Ipv4Addr, port: u16, payload: Vec<u8> },
    Other,
}

fn read(frame: &[u8]) -> Out {
    let packet = SlicedPacket::from_ethernet(frame).expect("Ethernet II");
    assert!(matches!(packet.link, Some(LinkSlice::Ethernet2(_))));
    match (&packet.net, &packet.transport) {
        (Some(NetSlice::Arp(arp)), None) if arp.operation() == ArpOperation::REQUEST => {
            let v4 = |b: &[u8]| Ipv4Addr::new(b[0], b[1], b[2], b[3]);
            Out::ArpRequest { sender: v4(arp.sender_protocol_addr()), target: v4(arp.target_protocol_addr()) }
        }
        (Some(NetSlice::Ipv4(_)), Some(TransportSlice::Udp(udp))) if udp.destination_port() == 67 => {
            let message = udp.payload();
            let xid = u32::from_be_bytes(message[4..8].try_into().unwrap());
            let mut at = 240;
            let kind = loop {
                let (code, len) = (message[at], usize::from(message[at + 1]));
                if code == 53 {
                    break message[at + 2];
                }
                at += 2 + len;
            };
            Out::Dhcp { kind, xid }
        }
        (Some(NetSlice::Ipv4(ip)), Some(TransportSlice::Udp(udp))) => {
            let header = ip.header();
            Out::Udp { source: header.source_addr(), destination: header.destination_addr(), port: udp.destination_port(), payload: udp.payload().to_vec() }
        }
        _ => Out::Other,
    }
}

fn udp_frame(to: [u8; 6], from: [u8; 6], source: (Ipv4Addr, u16), destination: (Ipv4Addr, u16), payload: &[u8]) -> Vec<u8> {
    let builder = PacketBuilder::ethernet2(from, to).ipv4(source.0.octets(), destination.0.octets(), 64).udp(source.1, destination.1);
    let mut frame = Vec::with_capacity(builder.size(payload.len()));
    builder.write(&mut frame, payload).unwrap();
    frame
}

/// The router's ARP reply to the node on `net`, for `addr`, the router or the resolver.
fn arp_reply(net: &Net, addr: Ipv4Addr) -> Vec<u8> {
    let mut body = vec![0, 1, 0x08, 0, 6, 4, 0, 2];
    body.extend_from_slice(&ROUTER_MAC);
    body.extend_from_slice(&addr.octets());
    body.extend_from_slice(&net.mac);
    body.extend_from_slice(&net.addr.octets());
    [&net.mac[..], &ROUTER_MAC, &[0x08, 0x06], &body].concat()
}

/// `net`'s server's `kind` message for transaction `xid`, offering or acknowledging its address
/// for an hour, with its router and resolver; to the client's own addresses, or broadcast.
fn dhcp(net: &Net, kind: u8, xid: u32, broadcast: bool) -> Vec<u8> {
    let mut m = vec![2, 1, 6, 0];
    m.extend_from_slice(&xid.to_be_bytes());
    m.extend_from_slice(&[0; 8]);
    m.extend_from_slice(&net.addr.octets());
    m.extend_from_slice(&[0; 8]);
    m.extend_from_slice(&net.mac);
    m.resize(236, 0);
    m.extend_from_slice(&[0x63, 0x82, 0x53, 0x63]);
    let options: [(u8, Vec<u8>); 6] = [
        (53, vec![kind]),
        (54, net.router.octets().to_vec()),
        (51, 3_600u32.to_be_bytes().to_vec()),
        (1, vec![255, 255, 255, 0]),
        (3, net.router.octets().to_vec()),
        (6, net.dns.octets().to_vec()),
    ];
    for (code, value) in options {
        m.push(code);
        m.push(u8::try_from(value.len()).unwrap());
        m.extend_from_slice(&value);
    }
    m.push(255);
    match broadcast {
        false => udp_frame(net.mac, ROUTER_MAC, (net.router, 67), (net.addr, 68), &m),
        true => udp_frame([0xff; 6], ROUTER_MAC, (net.router, 67), (Ipv4Addr::BROADCAST, 68), &m),
    }
}

struct Two {
    node: Node,
    wired: IfIndex,
    wireless: IfIndex,
    now: Instant,
    draws: u32,
    /// Every frame the node sent, with the link it left by.
    sent: Vec<(IfIndex, Out)>,
    /// The two links are bridged onto one LAN: a group or broadcast frame sent on one is heard
    /// on the other, and a server's reply reaches both.
    bridged: bool,
    wired_net: &'static Net,
    wireless_net: &'static Net,
}

fn counter(draws: &mut u32) -> impl FnMut() -> u32 + '_ {
    move || {
        *draws += 1;
        *draws
    }
}

impl Two {
    fn new() -> Self {
        let secrets = Secrets {
            ip: [1; 16],
            resets: [2; 16],
            tcp: toyos_net_tcp::Secrets { isn: [3; 16], timestamp: [4; 16], port_offset: [5; 16], port_index: [6; 16], port_table: [0; 16] },
        };
        let now = Instant::from_millis(3_600_000);
        let mut draws = 0x4c00_0000;
        let mut node = Node::new(now, Config { receive_buffer: 65_535, send_buffer: 65_535, secrets }, HostName::new("toyos")).unwrap();
        node.set_places(8);
        let wireless = node.add_link(now, IndividualMac::new(MacAddr(WIRELESS_NET.mac)).unwrap(), WIRELESS, counter(&mut draws));
        let wired = node.add_link(now, IndividualMac::new(MacAddr(WIRED_NET.mac)).unwrap(), WIRED, counter(&mut draws));
        Self { node, wired, wireless, now, draws, sent: Vec::new(), bridged: false, wired_net: &WIRED_NET, wireless_net: &WIRELESS_NET }
    }

    /// Both links on one LAN behind one server.
    fn bridged() -> Self {
        Self { bridged: true, wired_net: &LAN_WIRED, wireless_net: &LAN_WIRELESS, ..Self::new() }
    }

    fn net(&self, link: IfIndex) -> &'static Net {
        if link == self.wired { self.wired_net } else { self.wireless_net }
    }

    fn other(&self, link: IfIndex) -> IfIndex {
        if link == self.wired { self.wireless } else { self.wired }
    }

    fn deliver(&mut self, link: IfIndex, frame: &[u8]) {
        let mut frames = Some(frame);
        self.node.receive(self.now, link, 256, |sink| frames.take().map(&mut *sink).is_some(), counter(&mut self.draws));
        self.pump();
    }

    /// Every frame the node will send with all the credit it wants, the router answering each
    /// ARP request for its own or its resolver's address, on the link it came by.
    fn pump(&mut self) {
        loop {
            let mut out = Vec::new();
            let mut heard = Vec::new();
            let bridged = self.bridged;
            self.node.transmit(
                self.now,
                usize::MAX,
                |link, frame| {
                    out.push((link, read(frame)));
                    // A group or broadcast frame's destination has its low bit of the first byte set.
                    if bridged && frame[0] & 1 == 1 {
                        heard.push((link, frame.to_vec()));
                    }
                },
                counter(&mut self.draws),
            );
            if out.is_empty() {
                return;
            }
            for (link, frame) in heard {
                let other = self.other(link);
                let mut frames = Some(frame.as_slice());
                self.node.receive(self.now, other, 256, |sink| frames.take().map(&mut *sink).is_some(), counter(&mut self.draws));
            }
            let replies: Vec<(IfIndex, Vec<u8>)> = out
                .iter()
                .filter_map(|(link, seen)| {
                    let net = self.net(*link);
                    match seen {
                        Out::ArpRequest { target, .. } if *target == net.router || *target == net.dns => Some((*link, arp_reply(net, *target))),
                        _ => None,
                    }
                })
                .collect();
            self.sent.extend(out);
            for (link, reply) in replies {
                let mut frames = Some(reply.as_slice());
                self.node.receive(self.now, link, 256, |sink| frames.take().map(&mut *sink).is_some(), counter(&mut self.draws));
            }
        }
    }

    fn link(&mut self, link: IfIndex, up: bool) {
        self.node.link(self.now, link, up, counter(&mut self.draws));
        self.pump();
    }

    /// Fires deadline after deadline until `done`, giving up past `limit`.
    fn run_until(&mut self, limit: Duration, mut done: impl FnMut(&mut Two) -> bool) -> bool {
        let end = self.now.after(limit);
        loop {
            if done(self) {
                return true;
            }
            match self.node.next_deadline() {
                Some(at) if at <= end => {
                    self.now = self.now.max(at);
                    self.node.fire(self.now, counter(&mut self.draws));
                    self.pump();
                }
                _ => return false,
            }
        }
    }

    /// The last DHCP message of `kind` that left by `link`: its transaction id.
    fn last_dhcp(&self, link: IfIndex, kind: u8) -> u32 {
        self.sent.iter().rev().find_map(|(at, out)| match out {
            Out::Dhcp { kind: k, xid } if *at == link && *k == kind => Some(*xid),
            _ => None,
        })
        .expect("a DHCP message of that type on the link")
    }

    /// A server's reply on `link`, which on a bridged LAN reaches the other link too.
    fn reply(&mut self, link: IfIndex, frame: &[u8]) {
        if self.bridged {
            let other = self.other(link);
            self.deliver(other, frame);
        }
        self.deliver(link, frame);
    }

    /// `link` up, and its server's offer and acknowledgement through to a lease held.
    fn lease(&mut self, link: IfIndex) {
        self.link(link, true);
        let net = self.net(link);
        let xid = self.last_dhcp(link, 1);
        // A server on a LAN two links share broadcasts its replies, so each link hears both.
        let broadcast = self.bridged;
        self.reply(link, &dhcp(net, 2, xid, broadcast));
        assert_eq!(self.last_dhcp(link, 3), xid, "the REQUEST");
        self.reply(link, &dhcp(net, 5, xid, broadcast));
        assert!(self.run_until(Duration::from_secs(10), |two| two.node.lease(link).is_some()), "the lease is held");
    }

    fn leased() -> Self {
        Self::new().leasing()
    }

    fn leasing(mut self) -> Self {
        let (wired, wireless) = (self.wired, self.wireless);
        self.lease(wireless);
        self.lease(wired);
        self
    }

    /// The DNS queries that left from `from`, each with its link and resolver.
    fn queries(&self, from: usize) -> Vec<(IfIndex, Ipv4Addr)> {
        self.sent[from..]
            .iter()
            .filter_map(|(link, out)| match out {
                Out::Udp { destination, port: 53, .. } => Some((*link, *destination)),
                _ => None,
            })
            .collect()
    }

    fn resolve(&mut self, text: &str) {
        let name = Name::parse(text).expect("a host name");
        self.node.resolve(self.now, b"view", name, counter(&mut self.draws)).expect("a lookup started");
        self.pump();
    }
}

#[test]
fn each_link_leases_its_own_address_by_its_own_client() {
    let two = Two::leased();
    let discovers: Vec<_> = two.sent.iter().filter(|(_, out)| matches!(out, Out::Dhcp { kind: 1, .. })).collect();
    assert!(discovers.iter().any(|(link, _)| *link == two.wired) && discovers.iter().any(|(link, _)| *link == two.wireless));
    assert_ne!(two.last_dhcp(two.wired, 1), two.last_dhcp(two.wireless, 1), "two exchanges");
    assert_eq!(two.node.lease(two.wired).map(|l| l.address), Some(WIRED_NET.addr));
    assert_eq!(two.node.lease(two.wireless).map(|l| l.address), Some(WIRELESS_NET.addr));
}

#[test]
fn lookups_ask_the_wired_links_resolver_fail_over_and_return() {
    let mut two = Two::leased();
    let (wired, wireless) = (two.wired, two.wireless);
    assert_eq!(two.node.asked(), Some(wired), "wired before wireless, though it came second");
    let from = two.sent.len();
    two.resolve("one.example");
    assert_eq!(two.queries(from), [(wired, WIRED_NET.dns)]);

    two.link(wired, false);
    assert_eq!(two.node.asked(), Some(wireless), "failed over");
    let from = two.sent.len();
    two.resolve("two.example");
    assert_eq!(two.queries(from), [(wireless, WIRELESS_NET.dns)]);

    two.link(wired, true);
    assert!(two.run_until(Duration::from_secs(10), |two| two.node.asked() == Some(wired)), "the wired link verifies its lease and is asked again");
    let from = two.sent.len();
    two.resolve("three.example");
    assert_eq!(two.queries(from), [(wired, WIRED_NET.dns)]);
}

/// Whichever link goes down last, lookups keep asking the link they asked while it was up.
#[test]
fn with_no_link_up_lookups_ask_the_lease_they_asked() {
    let mut two = Two::leased();
    let (wired, wireless) = (two.wired, two.wireless);
    two.link(wireless, false);
    two.link(wired, false);
    assert_eq!(two.node.asked(), Some(wired), "the wireless link down first, then the wired one it asked");
    let mut two = Two::leased();
    two.link(wired, false);
    assert_eq!(two.node.asked(), Some(wireless), "failed over");
    two.link(wireless, false);
    assert_eq!(two.node.asked(), Some(wireless), "the wired link down first: the wireless one it then asked");
}

/// RFC 6762 §14's laptop: a wired and a wireless link bridged onto one LAN. Each link hears
/// the other's group frames and the server's replies to both, and the machine claims its name
/// on both and refuses no server's reply.
#[test]
fn on_one_lan_by_two_links_the_name_is_claimed_on_both_and_no_reply_is_refused() {
    let mut two = Two::bridged().leasing();
    let (wired, wireless) = (two.wired, two.wireless);
    assert_eq!(two.node.lease(wired).map(|l| l.address), Some(LAN_WIRED.addr));
    assert_eq!(two.node.lease(wireless).map(|l| l.address), Some(LAN_WIRELESS.addr));
    two.node.answer_as(two.now, Host::new("toyos").unwrap(), counter(&mut two.draws)).unwrap();
    two.pump();
    let mut events: Vec<Event> = two.node.drain_events().collect();
    let claimed = |events: &[Event], link| events.contains(&Event::Name { link, event: toyos_mdns::Event::Claimed });
    assert!(two.run_until(Duration::from_secs(10), |two| {
        events.extend(two.node.drain_events());
        claimed(&events, wired) && claimed(&events, wireless)
    }), "claimed on both links: {events:?}");
    assert!(!events.iter().any(|e| matches!(e, Event::Name { event: toyos_mdns::Event::Lost, .. })), "{events:?}");
    assert!(!events.iter().any(|e| matches!(e, Event::Dhcp { .. })), "no server's reply refused: {events:?}");
    assert!(two.node.shard().ip().counters().get(toyos_net_ip::Counter::IpOwnSource) > 0, "the premise: each link heard the other's messages, which [ip] refused");
    assert!(two.node.counters().get(toyos_net_node::Counter::DhcpOtherLink) > 0, "the premise: each link heard the other's replies");
}

const NAME: &[u8] = b"\x05toyos\x05local\x00";

/// Another host's response naming `address` for this machine's name.
fn says(address: Ipv4Addr) -> Vec<u8> {
    let mut m = Vec::new();
    for word in [0u16, 0x8400, 0, 1, 0, 0] {
        m.extend_from_slice(&word.to_be_bytes());
    }
    m.extend_from_slice(NAME);
    m.extend_from_slice(&[0, 1, 0x80, 1]);
    m.extend_from_slice(&120u32.to_be_bytes());
    m.extend_from_slice(&[0, 4]);
    m.extend_from_slice(&address.octets());
    m
}

/// The mDNS messages to the group that left by `link`, each by its source.
fn to_group(two: &Two, link: IfIndex) -> Vec<Ipv4Addr> {
    two.sent
        .iter()
        .filter_map(|(at, out)| match out {
            Out::Udp { source, destination, port: MDNS, .. } if *at == link && *destination == GROUP => Some(*source),
            _ => None,
        })
        .collect()
}

#[test]
fn the_name_is_probed_for_on_each_link_from_that_links_address_and_lost_on_one_alone() {
    let mut two = Two::leased();
    let (wired, wireless) = (two.wired, two.wireless);
    two.node.answer_as(two.now, Host::new("toyos").unwrap(), counter(&mut two.draws)).unwrap();
    two.pump();
    let mut events = Vec::new();
    let mut conflicted = false;
    let claimed = two.run_until(Duration::from_secs(10), |two| {
        if !conflicted && !to_group(two, wireless).is_empty() {
            // Another host on the wireless network answers for the name under its probe.
            let neighbour = Ipv4Addr::new(198, 51, 100, 99);
            let frame = udp_frame(GROUP_MAC, OTHER_HOST_MAC, (neighbour, MDNS), (GROUP, MDNS), &says(neighbour));
            two.deliver(wireless, &frame);
            conflicted = true;
        }
        events.extend(two.node.drain_events());
        events.contains(&Event::Name { link: wired, event: toyos_mdns::Event::Claimed })
    });
    assert!(claimed, "the wired link's name is claimed: {events:?}");
    assert!(events.contains(&Event::Name { link: wireless, event: toyos_mdns::Event::Lost }), "lost on the wireless link: {events:?}");
    assert!(!events.contains(&Event::Name { link: wireless, event: toyos_mdns::Event::Claimed }));
    assert!(to_group(&two, wired).iter().all(|&source| source == WIRED_NET.addr));
    assert!(to_group(&two, wireless).iter().all(|&source| source == WIRELESS_NET.addr));
    assert!(!to_group(&two, wired).is_empty(), "probed for on the wired link");
}
