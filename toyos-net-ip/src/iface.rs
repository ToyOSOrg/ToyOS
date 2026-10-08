//! One interface's state, and the context each operation on it borrows beside it.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::net::Ipv4Addr;
use core::time::Duration;

use toyos_net_wire::ethernet::IndividualMac;
use toyos_net_wire::Instant;

use crate::addr::Address;
use crate::counters::Log;
use crate::draw::Draws;
use crate::egress::Control;
use crate::timers::Timers;
use crate::{acd, igmp, limits, nud, IfIndex, Ip};

pub(crate) struct Interface {
    pub mac: IndividualMac,
    pub up: bool,
    /// In the order they were added.
    pub addresses: Vec<Address>,
    pub gateways: Vec<Ipv4Addr>,
    pub active: Option<Ipv4Addr>,
    pub neighbours: BTreeMap<Ipv4Addr, nud::Neighbour>,
    /// A send found the table full of entries in use, and none has become evictable since.
    pub full: bool,
    /// Datagrams its entries queue, held for resolution or released and not yet left:
    /// PENDING_TOTAL bounds them.
    pub held: usize,
    pub reachable: Duration,
    pub reachable_drawn: Instant,
    pub acd: acd::Conflicts,
    pub igmp: igmp::Igmp,
}

pub(crate) struct Cx<'a> {
    pub now: Instant,
    pub iface: IfIndex,
    pub timers: &'a mut Timers,
    pub control: &'a mut Control,
    pub log: &'a mut Log,
    pub draws: &'a mut Draws,
    pub generation: &'a mut u64,
}

impl Cx<'_> {
    pub fn bump(&mut self) {
        *self.generation = self.generation.wrapping_add(1);
    }
}

impl Interface {
    pub fn usable(&self) -> impl Iterator<Item = &Address> + '_ {
        self.addresses.iter().filter(|a| a.usable())
    }

    pub fn is_usable(&self, addr: Ipv4Addr) -> bool {
        self.usable().any(|a| a.cidr.addr() == addr)
    }

    pub fn owns(&self, addr: Ipv4Addr) -> bool {
        self.addresses.iter().any(|a| a.cidr.addr() == addr)
    }

    /// Reachable without a gateway: inside the prefix of a usable address, or in 169.254/16,
    /// which is this link's whatever address the interface holds (RFC 3927 §2.6.2).
    pub fn on_link(&self, addr: Ipv4Addr) -> bool {
        self.usable().any(|a| addr.is_link_local() || a.cidr.contains(addr))
    }

    /// A usable address whose prefix holds `toward`, else the first usable one.
    pub fn source_for(&self, toward: Ipv4Addr) -> Option<Ipv4Addr> {
        self.usable().find(|a| a.cidr.contains(toward)).or_else(|| self.usable().next()).map(|a| a.cidr.addr())
    }

    /// The directed broadcast of one of its usable prefixes.
    pub fn is_directed_broadcast(&self, addr: Ipv4Addr) -> bool {
        self.usable().any(|a| a.cidr.broadcast() == Some(addr))
    }

    /// ReachableTime, redrawn once two hours have passed since the last draw (RFC 4861 §6.3.2).
    pub fn reachable_time(&mut self, draws: &mut Draws, now: Instant) -> Duration {
        if now.since(self.reachable_drawn) >= limits::nud::REACHABLE_REDRAW {
            self.reachable = draws.reachable_time();
            self.reachable_drawn = now;
        }
        self.reachable
    }
}

impl Ip {
    pub(crate) fn split(&mut self, now: Instant, iface: IfIndex) -> Option<(&mut Interface, Cx<'_>)> {
        let Self { ifaces, timers, control, log, draws, generation, .. } = self;
        let interface = ifaces.get_mut(iface.0)?;
        Some((interface, Cx { now, iface, timers, control, log, draws, generation }))
    }
}
