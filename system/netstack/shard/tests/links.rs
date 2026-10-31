//! One shard over two links: a new flow leaves by the link that ranks first and has a route,
//! fails over and returns with it, waits for its own link's room without holding back the
//! other's, and ends when its address is lost.

use std::net::Ipv4Addr;

use toyos_net_shard::{Config, Credit, Event, IfIndex, Rank, Secrets, Shard};
use toyos_net_tcp::{Endpoint, Error, Failure};
use toyos_net_wire::arp::{Arp, Operation};
use toyos_net_wire::ethernet::{EtherType, Frame, FrameBuilder, IndividualMac, MacAddr};
use toyos_net_wire::ipv4::Ipv4Packet;
use toyos_net_wire::{Instant, Port};

const WIRED_MAC: MacAddr = MacAddr([2, 0, 0, 0, 0, 0x01]);
const WIRELESS_MAC: MacAddr = MacAddr([2, 0, 0, 0, 0, 0x02]);
const WIRED: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 10);
const WIRED_ROUTER: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 1);
const WIRELESS: Ipv4Addr = Ipv4Addr::new(198, 51, 100, 10);
const WIRELESS_ROUTER: Ipv4Addr = Ipv4Addr::new(198, 51, 100, 1);
const ROUTER_MAC: MacAddr = MacAddr([2, 0, 0, 0, 0, 0xfe]);
/// Somewhere only a router reaches.
const FAR: Ipv4Addr = Ipv4Addr::new(203, 0, 113, 7);

struct Machine {
    shard: Shard,
    wired: IfIndex,
    wireless: IfIndex,
    now: Instant,
}

fn secrets() -> Secrets {
    Secrets {
        ip: [1; 16],
        resets: [2; 16],
        tcp: toyos_net_tcp::Secrets { isn: [3; 16], timestamp: [4; 16], port_offset: [5; 16], port_index: [6; 16], port_table: [0; 16] },
    }
}

/// An ARP reply from a router at `ip` to `to`.
fn router_reply(ip: Ipv4Addr, to: (MacAddr, Ipv4Addr)) -> Vec<u8> {
    let arp = Arp { operation: Operation::Reply, sender_mac: ROUTER_MAC, sender_ip: ip, target_mac: to.0, target_ip: to.1 };
    let mut frame = vec![0; 64];
    let len = FrameBuilder { destination: to.0, source: IndividualMac::new(ROUTER_MAC).unwrap() }.emit(&arp, &mut frame).unwrap().len();
    frame.truncate(len);
    frame
}

/// The wireless link added first and the wired one second, both up, each address verified
/// and its router known.
fn machine() -> Machine {
    let mut now = Instant::from_millis(3_600_000);
    let mut shard = Shard::new(now, Config { receive_buffer: 65_535, send_buffer: 65_535, secrets: secrets() }).unwrap();
    let wireless = shard.add_interface(now, IndividualMac::new(WIRELESS_MAC).unwrap(), Rank(1));
    let wired = shard.add_interface(now, IndividualMac::new(WIRED_MAC).unwrap(), Rank(0));
    for (iface, addr) in [(wireless, WIRELESS), (wired, WIRED)] {
        shard.link_up(now, iface).unwrap();
        shard.add_address(now, iface, addr, 24).unwrap();
    }
    let mut verified = 0;
    while verified < 2 {
        shard.transmit(now, usize::MAX, |_, _| {});
        verified += shard.drain_events().filter(|e| matches!(e, Event::Verified { .. })).count();
        if verified < 2 {
            now = shard.next_deadline().expect("conflict detection is still running");
            shard.fire(now);
        }
    }
    shard.set_gateways(now, wireless, &[WIRELESS_ROUTER]).unwrap();
    shard.set_gateways(now, wired, &[WIRED_ROUTER]).unwrap();
    Machine { shard, wired, wireless, now }
}

fn far(port: u16) -> Endpoint {
    Endpoint { addr: FAR, port: Port::new(port).unwrap() }
}

/// The IPv4 frames that left with `credit`, each by its interface and source, a router
/// answering every ARP request for it as it leaves, until nothing more leaves.
fn left(m: &mut Machine, mut credit: impl Credit) -> Vec<(IfIndex, Ipv4Addr)> {
    let mut out = Vec::new();
    loop {
        let mut asked = Vec::new();
        let sent = m.shard.transmit(m.now, &mut credit, |iface, bytes| {
            let frame = Frame::parse(bytes).unwrap();
            match frame.ether_type() {
                EtherType::Ipv4 => out.push((iface, Ipv4Packet::parse(frame.body()).unwrap().source())),
                EtherType::Arp => {
                    let arp = Arp::parse(frame.body()).unwrap();
                    if arp.operation == Operation::Request && [WIRED_ROUTER, WIRELESS_ROUTER].contains(&arp.target_ip) {
                        asked.push((iface, router_reply(arp.target_ip, (arp.sender_mac, arp.sender_ip))));
                    }
                }
                EtherType::Other(_) => {}
            }
        });
        for (iface, reply) in &asked {
            m.shard.receive(m.now, *iface, reply);
        }
        if sent == 0 && asked.is_empty() {
            return out;
        }
    }
}

#[test]
fn a_new_flow_leaves_by_the_wired_link_though_the_wireless_came_first() {
    let mut m = machine();
    m.shard.connect(m.now, None, far(80)).unwrap();
    let (wired, out) = (m.wired, left(&mut m, usize::MAX));
    assert_eq!(out, [(wired, WIRED)], "its SYN");
}

#[test]
fn a_new_flow_fails_over_to_the_wireless_link_and_the_next_returns_to_the_wired_one() {
    let mut m = machine();
    m.shard.link_down(m.now, m.wired).unwrap();
    m.shard.connect(m.now, None, far(80)).unwrap();
    let (wireless, wired) = (m.wireless, m.wired);
    assert_eq!(left(&mut m, usize::MAX), [(wireless, WIRELESS)], "failed over");
    m.shard.link_up(m.now, wired).unwrap();
    let back = left(&mut m, usize::MAX);
    assert!(back.is_empty(), "nothing moves when the wired link returns: {back:?}");
    m.shard.connect(m.now, None, far(81)).unwrap();
    assert_eq!(left(&mut m, usize::MAX), [(wired, WIRED)], "the next flow is the wired link's again");
}

/// Room on one interface alone.
struct Only(IfIndex);

impl Credit for Only {
    fn room(&self, iface: IfIndex) -> bool {
        iface == self.0
    }
    fn spend(&mut self, _: IfIndex) {}
}

#[test]
fn a_flow_waits_for_its_own_links_room_and_holds_back_none_of_the_others() {
    let mut m = machine();
    let (wired, wireless) = (m.wired, m.wireless);
    m.shard.connect(m.now, None, far(80)).unwrap();
    m.shard.link_down(m.now, wired).unwrap();
    m.shard.connect(m.now, None, far(81)).unwrap();
    m.shard.link_up(m.now, wired).unwrap();
    // The wired flow's SYN was due first; with room on the wireless link alone, the wireless
    // flow's leaves and the wired one's waits.
    assert_eq!(left(&mut m, Only(wireless)), [(wireless, WIRELESS)]);
    assert_eq!(left(&mut m, Only(wired)), [(wired, WIRED)], "the wired flow, at the wired link's next room");
}

#[test]
fn a_flow_from_an_address_taken_away_ends_and_one_from_the_other_link_does_not() {
    let mut m = machine();
    let wired_flow = m.shard.connect(m.now, None, far(80)).unwrap();
    m.shard.link_down(m.now, m.wired).unwrap();
    let wireless_flow = m.shard.connect(m.now, None, far(81)).unwrap();
    m.shard.link_up(m.now, m.wired).unwrap();
    assert_eq!(left(&mut m, usize::MAX).len(), 2, "both SYNs");
    m.shard.remove_address(m.now, m.wired, WIRED).unwrap();
    assert_eq!(m.shard.send(m.now, wired_flow, b"x"), Err(Error::Failed(Failure::AddressGone)));
    assert!(m.shard.status(wireless_flow).is_ok_and(|s| s.state == toyos_net_tcp::State::SynSent));
}

/// A link going down takes no address, so it ends no flow as its address's loss does.
#[test]
fn a_link_that_only_goes_down_keeps_its_address() {
    let mut m = machine();
    m.shard.link_down(m.now, m.wired).unwrap();
    assert!(m.shard.ip().address(m.wired, WIRED).is_some());
}

// ---- one LAN on two links: one gateway address on each ----

const LAN_GATEWAY: Ipv4Addr = Ipv4Addr::new(192, 168, 1, 1);
const LAN_WIRED: Ipv4Addr = Ipv4Addr::new(192, 168, 1, 10);
const LAN_WIRELESS: Ipv4Addr = Ipv4Addr::new(192, 168, 1, 20);

/// Both links on one /24, each with the same gateway address, and a datagram socket bound to
/// each link's address.
fn one_lan() -> (Machine, toyos_net_udp::SocketId, toyos_net_udp::SocketId) {
    let mut now = Instant::from_millis(3_600_000);
    let mut shard = Shard::new(now, Config { receive_buffer: 65_535, send_buffer: 65_535, secrets: secrets() }).unwrap();
    let wired = shard.add_interface(now, IndividualMac::new(WIRED_MAC).unwrap(), Rank(0));
    let wireless = shard.add_interface(now, IndividualMac::new(WIRELESS_MAC).unwrap(), Rank(1));
    for (iface, addr) in [(wired, LAN_WIRED), (wireless, LAN_WIRELESS)] {
        shard.link_up(now, iface).unwrap();
        shard.add_address(now, iface, addr, 24).unwrap();
    }
    let mut verified = 0;
    while verified < 2 {
        shard.transmit(now, usize::MAX, |_, _| {});
        verified += shard.drain_events().filter(|e| matches!(e, Event::Verified { .. })).count();
        if verified < 2 {
            now = shard.next_deadline().expect("conflict detection is still running");
            shard.fire(now);
        }
    }
    for iface in [wired, wireless] {
        shard.set_gateways(now, iface, &[LAN_GATEWAY]).unwrap();
    }
    let from_wired = shard.bind(LAN_WIRED, Port::new(5000), || 0).unwrap();
    let from_wireless = shard.bind(LAN_WIRELESS, Port::new(5001), || 0).unwrap();
    (Machine { shard, wired, wireless, now }, from_wired, from_wireless)
}

/// The UDP datagrams that left in one opportunity, by interface and source; ARP answered by
/// nobody.
fn datagrams(m: &mut Machine) -> Vec<(IfIndex, Ipv4Addr)> {
    let mut out = Vec::new();
    m.shard.transmit(m.now, usize::MAX, |iface, bytes| {
        let frame = Frame::parse(bytes).unwrap();
        if matches!(frame.ether_type(), EtherType::Ipv4) {
            out.push((iface, Ipv4Packet::parse(frame.body()).unwrap().source()));
        }
    });
    out
}

#[test]
fn a_gateway_that_fails_on_one_link_drops_no_datagram_waiting_on_the_other() {
    let (mut m, from_wired, from_wireless) = one_lan();
    let (wired, wireless) = (m.wired, m.wireless);
    m.shard.send_to(m.now, from_wired, FAR, 9, b"wired").unwrap();
    assert!(datagrams(&mut m).is_empty(), "the wired datagram waits for the gateway's link address");
    // The wireless link asks a retransmission later, so its question is still open when the
    // wired link gives the gateway up; nobody answers the wired one, and the wireless one only
    // after.
    m.now = m.shard.next_deadline().expect("the wired link's resolution is running");
    m.shard.fire(m.now);
    datagrams(&mut m);
    m.shard.send_to(m.now, from_wireless, FAR, 9, b"wireless").unwrap();
    assert!(datagrams(&mut m).is_empty(), "the wireless datagram waits for the same address on its own link");
    let failed = m.shard.udp_counters().get(toyos_net_udp::Counter::TxUnreachable);
    while m.shard.udp_counters().get(toyos_net_udp::Counter::TxUnreachable) == failed {
        m.now = m.shard.next_deadline().expect("the wired link's resolution is still running");
        m.shard.fire(m.now);
        datagrams(&mut m);
    }
    assert_eq!(m.shard.udp_counters().get(toyos_net_udp::Counter::TxUnreachable), failed + 1, "the wired datagram alone");
    m.shard.receive(m.now, wireless, &router_reply(LAN_GATEWAY, (WIRELESS_MAC, LAN_WIRELESS)));
    assert_eq!(datagrams(&mut m), [(wireless, LAN_WIRELESS)], "the wireless datagram leaves");
    assert!(m.shard.ip().neighbour(wired, LAN_GATEWAY).is_some_and(|n| matches!(n, toyos_net_ip::Nud::Failed)), "the premise: the wired link gave it up");
}

/// The ones'-complement sum of `bytes` (RFC 1071).
fn checksum(bytes: &[u8]) -> u16 {
    let mut sum: u32 = bytes.chunks(2).map(|c| u32::from(u16::from_be_bytes([c[0], *c.get(1).unwrap_or(&0)]))).sum();
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

/// A bare TCP segment from `from` to `to` in a frame from the gateway to `mac`.
fn segment(mac: MacAddr, from: (Ipv4Addr, u16), to: (Ipv4Addr, u16), seq: u32, ack: u32, flags: u8) -> Vec<u8> {
    let mut tcp = Vec::new();
    tcp.extend_from_slice(&from.1.to_be_bytes());
    tcp.extend_from_slice(&to.1.to_be_bytes());
    tcp.extend_from_slice(&seq.to_be_bytes());
    tcp.extend_from_slice(&ack.to_be_bytes());
    tcp.extend_from_slice(&[0x50, flags, 0xff, 0xff, 0, 0, 0, 0]);
    let pseudo = [&from.0.octets()[..], &to.0.octets(), &[0, 6, 0, 20]].concat();
    let check = checksum(&[pseudo, tcp.clone()].concat());
    tcp[16..18].copy_from_slice(&check.to_be_bytes());
    let mut ip = vec![0x45, 0, 0, 40, 0, 0, 0x40, 0, 64, 6, 0, 0];
    ip.extend_from_slice(&from.0.octets());
    ip.extend_from_slice(&to.0.octets());
    let check = checksum(&ip);
    ip[10..12].copy_from_slice(&check.to_be_bytes());
    [&mac.0[..], &ROUTER_MAC.0, &[0x08, 0x00], &ip, &tcp].concat()
}

fn gateway_state(m: &Machine, iface: IfIndex) -> Option<&'static str> {
    m.shard.ip().neighbour(iface, LAN_GATEWAY).map(|n| match n {
        toyos_net_ip::Nud::Reachable(_) => "reachable",
        toyos_net_ip::Nud::Stale(_) => "stale",
        toyos_net_ip::Nud::Delay(_) => "delay",
        _ => "other",
    })
}

/// Resolves the gateway on each link, a datagram from each link's socket asking, and leaves
/// both entries stale.
fn both_stale(m: &mut Machine, from_wired: toyos_net_udp::SocketId, from_wireless: toyos_net_udp::SocketId) {
    let (wired, wireless) = (m.wired, m.wireless);
    for (socket, iface, mine) in [(from_wired, wired, (WIRED_MAC, LAN_WIRED)), (from_wireless, wireless, (WIRELESS_MAC, LAN_WIRELESS))] {
        m.shard.send_to(m.now, socket, FAR, 9, b"x").unwrap();
        datagrams(m);
        m.shard.receive(m.now, iface, &router_reply(LAN_GATEWAY, mine));
        datagrams(m);
    }
    let later = m.now.after(std::time::Duration::from_secs(120));
    while let Some(at) = m.shard.next_deadline().filter(|at| *at <= later) {
        m.now = at;
        m.shard.fire(at);
        datagrams(m);
    }
    m.now = later;
    assert_eq!((gateway_state(m, wired), gateway_state(m, wireless)), (Some("stale"), Some("stale")), "the premise: both stale");
}

/// A connection's progress confirms the next hop of its own link: where the wired and the
/// wireless link share one gateway address, a flow on the wireless link refreshes the wireless
/// entry and leaves the wired one as it was.
#[test]
fn a_flows_progress_confirms_its_own_links_gateway_where_two_links_share_one() {
    let (mut m, from_wired, from_wireless) = one_lan();
    let (wired, wireless) = (m.wired, m.wireless);
    // A connection from the wireless link, made while the wired one is down.
    m.shard.link_down(m.now, wired).unwrap();
    let flow = m.shard.connect(m.now, None, far(80)).unwrap();
    m.shard.receive(m.now, wireless, &router_reply(LAN_GATEWAY, (WIRELESS_MAC, LAN_WIRELESS)));
    let mut syn = None;
    for _ in 0..2 {
        m.shard.transmit(m.now, usize::MAX, |iface, bytes| {
            if iface == wireless && bytes.len() >= 54 && bytes[23] == 6 {
                syn = Some((u16::from_be_bytes([bytes[34], bytes[35]]), u32::from_be_bytes([bytes[38], bytes[39], bytes[40], bytes[41]])));
            }
        });
        m.shard.receive(m.now, wireless, &router_reply(LAN_GATEWAY, (WIRELESS_MAC, LAN_WIRELESS)));
    }
    let (port, iss) = syn.expect("its SYN, by the wireless link");
    m.shard.link_up(m.now, wired).unwrap();
    m.shard.receive(m.now, wireless, &segment(WIRELESS_MAC, (FAR, 80), (LAN_WIRELESS, port), 7_000, iss.wrapping_add(1), 0x12));
    datagrams(&mut m);
    assert!(m.shard.status(flow).is_ok_and(|s| s.state == toyos_net_tcp::State::Established), "the premise: connected");
    both_stale(&mut m, from_wired, from_wireless);
    // Two bytes sent and acknowledged: SND.UNA advances, and the advice is the wireless link's.
    assert_eq!(m.shard.send(m.now, flow, b"hi"), Ok(2));
    datagrams(&mut m);
    m.shard.receive(m.now, wireless, &segment(WIRELESS_MAC, (FAR, 80), (LAN_WIRELESS, port), 7_001, iss.wrapping_add(3), 0x10));
    assert_eq!(gateway_state(&m, wireless), Some("reachable"), "the flow's own gateway entry, confirmed");
    assert_eq!(gateway_state(&m, wired), Some("stale"), "the other link's entry for the same address, untouched");
}
