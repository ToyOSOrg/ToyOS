//! Interfaces that tie are chosen by rank, then by when each was added: the
//! default route, failover to the next as one goes down and its return.

mod common;

use std::net::Ipv4Addr;

use common::*;
use toyos_net_ip::{Counter, IfIndex, Instant, Ip, NextHop, Rank, Route, Source};
use toyos_net_wire::ethernet::MacAddr;

const WIRED: Rank = Rank(0);
const WIRELESS: Rank = Rank(1);

/// One interface's address and its gateway, on a /24 of its own.
struct Net {
    mac: MacAddr,
    addr: Ipv4Addr,
    gateway: Ipv4Addr,
}

const NET_1: Net = Net { mac: MacAddr([2, 0, 0, 0, 1, 1]), addr: Ipv4Addr::new(192, 0, 2, 10), gateway: Ipv4Addr::new(192, 0, 2, 1) };
const NET_2: Net = Net { mac: MacAddr([2, 0, 0, 0, 2, 2]), addr: Ipv4Addr::new(198, 51, 100, 10), gateway: Ipv4Addr::new(198, 51, 100, 1) };
const NET_3: Net = Net { mac: MacAddr([2, 0, 0, 0, 3, 3]), addr: Ipv4Addr::new(203, 0, 113, 10), gateway: Ipv4Addr::new(203, 0, 113, 1) };

/// Somewhere only a gateway reaches.
const OFF_LINK: Ipv4Addr = Ipv4Addr::new(192, 88, 99, 1);

/// Each net's interface added in turn with its rank, up, addressed and given
/// its gateway.
fn machine(nets: &[(&Net, Rank)]) -> (Ip, Vec<IfIndex>, Instant) {
    let mut ip = Ip::new(Instant::from_nanos(0), SECRET);
    let mut now = Instant::from_millis(0);
    let mut ifaces = Vec::new();
    for &(net, rank) in nets {
        let iface = ip.add_interface(now, mac(net.mac), rank);
        ip.link_up(now, iface).unwrap();
        ip.add_address(now, iface, net.addr, 24).unwrap();
        ifaces.push(iface);
    }
    let settled = Instant::from_millis(10_000);
    while let Some(deadline) = ip.next_deadline().filter(|d| *d <= settled) {
        ip.fire(deadline);
        ip.transmit(deadline, usize::MAX, |_, _| {});
        now = deadline;
    }
    for (&(net, _), &iface) in nets.iter().zip(&ifaces) {
        ip.set_gateways(now, iface, &[net.gateway]).unwrap();
    }
    (ip, ifaces, settled)
}

fn via(ip: &mut Ip, destination: Ipv4Addr) -> Result<Route, Counter> {
    ip.route(destination, Source::Any, None)
}

fn by(iface: IfIndex, net: &Net) -> Result<Route, Counter> {
    Ok(Route { iface, next_hop: NextHop::Neighbour(net.gateway), source: net.addr })
}

#[test]
fn the_default_route_leaves_by_the_lower_rank_whichever_came_first() {
    let (mut ip, ifaces, _) = machine(&[(&NET_1, WIRELESS), (&NET_2, WIRED)]);
    assert_eq!(via(&mut ip, OFF_LINK), by(ifaces[1], &NET_2));
}

#[test]
fn among_equal_ranks_the_first_added_leads() {
    let (mut ip, ifaces, _) = machine(&[(&NET_1, WIRELESS), (&NET_2, WIRED), (&NET_3, WIRED)]);
    assert_eq!(via(&mut ip, OFF_LINK), by(ifaces[1], &NET_2));
    let (mut ip, ifaces, _) = machine(&[(&NET_3, WIRED), (&NET_2, WIRED)]);
    assert_eq!(via(&mut ip, OFF_LINK), by(ifaces[0], &NET_3));
}

#[test]
fn a_link_that_goes_down_hands_the_default_route_on_and_takes_it_back_on_return() {
    let (mut ip, ifaces, now) = machine(&[(&NET_1, WIRELESS), (&NET_2, WIRED)]);
    ip.link_down(now, ifaces[1]).unwrap();
    assert_eq!(via(&mut ip, OFF_LINK), by(ifaces[0], &NET_1), "failover to the wireless link");
    ip.link_up(now, ifaces[1]).unwrap();
    assert_eq!(via(&mut ip, OFF_LINK), by(ifaces[1], &NET_2), "the wired link's return");
    ip.link_down(now, ifaces[0]).unwrap();
    ip.link_down(now, ifaces[1]).unwrap();
    assert_eq!(via(&mut ip, OFF_LINK), Err(Counter::RouteNoSourceAddress), "no link up");
}

#[test]
fn a_connected_prefix_beats_a_lower_ranks_gateway() {
    let (mut ip, ifaces, _) = machine(&[(&NET_1, WIRELESS), (&NET_2, WIRED)]);
    let neighbour = Ipv4Addr::new(192, 0, 2, 77);
    assert_eq!(via(&mut ip, neighbour), Ok(Route { iface: ifaces[0], next_hop: NextHop::Neighbour(neighbour), source: NET_1.addr }));
}

#[test]
fn a_bound_source_keeps_its_own_interface_whatever_its_rank() {
    let (mut ip, ifaces, now) = machine(&[(&NET_1, WIRELESS), (&NET_2, WIRED)]);
    assert_eq!(ip.route(OFF_LINK, Source::Bound(NET_1.addr), None), by(ifaces[0], &NET_1));
    // Its link down, the flow has no route, and does not move to the other address.
    ip.link_down(now, ifaces[0]).unwrap();
    assert_eq!(ip.route(OFF_LINK, Source::Bound(NET_1.addr), None), Err(Counter::RouteNoSourceAddress));
}

/// Room on one interface alone.
struct Only(IfIndex, usize);

impl toyos_net_ip::Credit for Only {
    fn room(&self, iface: IfIndex) -> bool {
        iface == self.0 && self.1 > 0
    }
    fn spend(&mut self, _: IfIndex) {
        self.1 -= 1;
    }
}

#[test]
fn a_link_without_room_holds_back_no_other_links_frames() {
    let (mut ip, ifaces, now) = machine(&[(&NET_1, WIRED), (&NET_2, WIRED)]);
    // An ARP request queued on each, the first's ahead.
    ip.resolve(now, ifaces[0], NET_1.gateway, NET_1.addr);
    ip.resolve(now, ifaces[1], NET_2.gateway, NET_2.addr);
    let mut out = Vec::new();
    assert_eq!(ip.transmit(now, Only(ifaces[1], 8), |iface, _| out.push(iface)), 1);
    assert_eq!(out, [ifaces[1]], "the second link's request, past the first's");
    out.clear();
    assert_eq!(ip.transmit(now, 8usize, |iface, _| out.push(iface)), 1);
    assert_eq!(out, [ifaces[0]], "the first's, still queued");
}
