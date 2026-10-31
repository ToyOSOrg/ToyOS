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
