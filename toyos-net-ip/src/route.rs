//! The host routing table (§3): the connected prefixes of usable addresses and an ordered list of
//! on-link gateways per interface; longest prefix first, then the active gateway, and no
//! forwarding. A lookup considers only interfaces that are up, and with a bound source only the
//! interface holding it (RFC 1122 §3.3.4.2, the strong model).

use core::net::Ipv4Addr;

use toyos_net_wire::addr::{is_host, is_martian, Cidr};
use toyos_net_wire::ipv4::MulticastAddr;
use toyos_net_wire::Instant;

use crate::counters::Counter;
use crate::iface::{Cx, Interface};
use crate::nud::Nud;
use crate::{limits, IfIndex, Ip};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NextHop {
    Neighbour(Ipv4Addr),
    Broadcast,
    Multicast(MulticastAddr),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Route {
    pub iface: IfIndex,
    pub next_hop: NextHop,
    pub source: Ipv4Addr,
}

/// The source a datagram asks to be sent from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// [ip] chooses (§3.5).
    Any,
    /// The socket's address: only the interface holding it is eligible.
    Bound(Ipv4Addr),
    /// 0.0.0.0: only to the limited broadcast, while acquiring an address (RFC 2131 §4.1).
    Unspecified,
}

pub(crate) fn lookup(ifaces: &[Interface], destination: Ipv4Addr, source: Source, bound: Option<IfIndex>) -> Result<Route, Counter> {
    let eligible = |index: usize, i: &Interface| {
        i.up
            && bound.is_none_or(|b| b.0 == index)
            && match source {
                Source::Bound(addr) => i.is_usable(addr),
                Source::Any | Source::Unspecified => true,
            }
    };
    let candidates = || ifaces.iter().enumerate().filter(|&(n, i)| eligible(n, i));
    let pick_source = |i: &Interface, toward: Ipv4Addr| match source {
        Source::Bound(addr) => Ok(addr),
        Source::Unspecified => Err(Counter::RouteNoSourceAddress),
        Source::Any => i.source_for(toward).ok_or(Counter::RouteNoSourceAddress),
    };
    if destination.is_broadcast() {
        let mut up = candidates();
        let (index, i) = up.next().ok_or(Counter::RouteNone)?;
        if up.next().is_some() {
            return Err(Counter::RouteAmbiguousInterface);
        }
        let source = match source {
            Source::Unspecified => Ipv4Addr::UNSPECIFIED,
            _ => pick_source(i, destination)?,
        };
        return Ok(Route { iface: IfIndex(index), next_hop: NextHop::Broadcast, source });
    }
    if is_martian(destination) {
        return Err(Counter::RouteInvalidDestination);
    }
    if ifaces.iter().any(|i| i.owns(destination)) {
        return Err(Counter::RouteLocalDestination);
    }
    if let Some(group) = MulticastAddr::new(destination) {
        let (index, i) = candidates()
            .find(|(_, i)| i.igmp.joined(group))
            .or_else(|| candidates().next())
            .ok_or(Counter::RouteNone)?;
        return Ok(Route { iface: IfIndex(index), next_hop: NextHop::Multicast(group), source: pick_source(i, destination)? });
    }
    if !candidates().any(|(_, i)| i.usable().next().is_some()) {
        return Err(Counter::RouteNoSourceAddress);
    }
    let connected = candidates()
        .flat_map(|(index, i)| i.usable().filter(|a| a.cidr.contains(destination)).map(move |a| (index, i, a.cidr)))
        .fold(None, |best: Option<(usize, &Interface, Cidr)>, (index, i, cidr)| match best {
            Some((_, _, b)) if b.prefix_len() >= cidr.prefix_len() => best,
            _ => Some((index, i, cidr)),
        });
    if let Some((index, i, cidr)) = connected {
        let next_hop = if cidr.broadcast() == Some(destination) { NextHop::Broadcast } else { NextHop::Neighbour(destination) };
        return Ok(Route { iface: IfIndex(index), next_hop, source: pick_source(i, destination)? });
    }
    let (index, i, gateway) = candidates().find_map(|(index, i)| i.active.map(|g| (index, i, g))).ok_or(Counter::RouteNone)?;
    Ok(Route { iface: IfIndex(index), next_hop: NextHop::Neighbour(gateway), source: pick_source(i, gateway)? })
}

/// §3.4: the first gateway not FAILED and not UNREACHABLE; with none, the first UNREACHABLE one,
/// which still has a MAC, else the first.
fn active(i: &Interface) -> Option<Ipv4Addr> {
    let state = |g: &Ipv4Addr| i.neighbours.get(g).map(|n| &n.state);
    i.gateways
        .iter()
        .find(|g| !matches!(state(g), Some(Nud::Failed | Nud::Unreachable(_))))
        .or_else(|| i.gateways.iter().find(|g| matches!(state(g), Some(Nud::Unreachable(_)))))
        .or_else(|| i.gateways.first())
        .copied()
}

/// A neighbour's reachability changed: the active gateway may have too (§3.4).
pub(crate) fn refresh_active(i: &mut Interface, cx: &mut Cx<'_>) {
    let now = active(i);
    if now != i.active {
        if i.active.is_some() && now.is_some() {
            cx.log.count(Counter::RouteGatewaySwitched);
        }
        i.active = now;
        cx.bump();
    }
}

/// Withdraws every gateway no usable prefix holds any more (§3.2).
pub(crate) fn withdraw_off_link(i: &mut Interface, cx: &mut Cx<'_>) {
    let before = i.gateways.len();
    let on_link: alloc::vec::Vec<Ipv4Addr> = i.gateways.iter().copied().filter(|g| i.on_link(*g)).collect();
    for _ in on_link.len()..before {
        cx.log.count(Counter::RouteGatewayWithdrawn);
    }
    i.gateways = on_link;
    i.active = active(i);
}

impl Ip {
    /// Where a datagram to `destination` goes (§3.3), or the refusal a transport reports.
    pub fn route(&mut self, destination: Ipv4Addr, source: Source, iface: Option<IfIndex>) -> Result<Route, Counter> {
        lookup(&self.ifaces, destination, source, iface).inspect_err(|&refusal| self.log.count(refusal))
    }

    /// Installs the interface's gateways, in order of preference (RFC 2132 §3.5). A list with any
    /// refused entry is refused whole and the old one kept (§3.2).
    pub fn set_gateways(&mut self, now: Instant, iface: IfIndex, gateways: &[Ipv4Addr]) -> Result<(), Counter> {
        let now = self.clock(now);
        let local: alloc::vec::Vec<Ipv4Addr> = self.ifaces.iter().flat_map(|i| i.addresses.iter().map(|a| a.cidr.addr())).collect();
        let Some((i, mut cx)) = self.split(now, iface) else {
            self.log.count(Counter::UnknownInterface);
            return Err(Counter::UnknownInterface);
        };
        let refusal = if gateways.len() > limits::GATEWAYS {
            Some(Counter::RouteTooManyGateways)
        } else {
            gateways.iter().find_map(|&g| {
                if !is_host(g) || i.usable().any(|a| a.cidr.is_edge(g)) {
                    Some(Counter::RouteGatewayInvalid)
                } else if local.contains(&g) {
                    Some(Counter::RouteGatewayIsLocal)
                } else if !i.on_link(g) {
                    Some(Counter::RouteGatewayOffLink)
                } else {
                    None
                }
            })
        };
        if let Some(refusal) = refusal {
            cx.log.count(refusal);
            return Err(refusal);
        }
        i.gateways = gateways.to_vec();
        i.active = active(i);
        cx.bump();
        Ok(())
    }
}
