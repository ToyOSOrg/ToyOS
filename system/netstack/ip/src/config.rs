//! What the shell and the transports tell [ip]: addresses, link state, group membership,
//! reachability advice and a flow's need for its next hop; and time.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::net::Ipv4Addr;

use toyos_net_wire::addr::{is_host, Cidr};
use toyos_net_wire::ipv4::MulticastAddr;
use toyos_net_wire::Instant;

use crate::addr::{Address, Phase};
use crate::counters::Counter;
use crate::nud::Link;
use crate::route::{self, NextHop, Source};
use crate::timers::Timer;
use crate::{acd, igmp, limits, nud, Advice, IfIndex, IgmpMode, Ip, Resolution};

impl Ip {
    fn unknown(&mut self) -> Counter {
        self.log.count(Counter::UnknownInterface);
        Counter::UnknownInterface
    }

    /// Adds `addr/prefix_len` to `iface` as tentative: conflict detection runs before it is used
    /// (RFC 5227 §2.1). The address already held with another prefix length takes the new one
    /// and keeps its state, which is how a renewal arrives.
    pub fn add_address(&mut self, now: Instant, iface: IfIndex, addr: Ipv4Addr, prefix_len: u8) -> Result<(), Counter> {
        let now = self.clock(now);
        let cidr = match Cidr::new(addr, prefix_len) {
            None => Err(Counter::AddrPrefixInvalid),
            Some(cidr) if !is_host(addr) || cidr.is_edge(addr) => Err(Counter::AddrNotUnicast),
            Some(_) if self.ifaces.iter().enumerate().any(|(n, i)| n != iface.0 && i.owns(addr)) => Err(Counter::AddrDuplicate),
            Some(cidr) => Ok(cidr),
        };
        let cidr = cidr.inspect_err(|&refusal| self.log.count(refusal))?;
        let Some((i, mut cx)) = self.split(now, iface) else { return Err(self.unknown()) };
        if let Some(held) = i.addresses.iter_mut().find(|a| a.cidr.addr() == addr) {
            if held.cidr != cidr {
                held.cidr = cidr;
                if held.usable() {
                    route::withdraw_off_link(i, &mut cx);
                    cx.bump();
                }
            }
            return Ok(());
        }
        if i.addresses.len() >= limits::ADDRS {
            cx.log.count(Counter::AddrTooMany);
            return Err(Counter::AddrTooMany);
        }
        i.addresses.push(Address { cidr, phase: Phase::Tentative { probes: 0, queued: false }, defended: None });
        acd::start(i, &mut cx, addr);
        Ok(())
    }

    /// Removes one of the interface's addresses; gateways that leave every usable prefix go with
    /// it.
    pub fn remove_address(&mut self, now: Instant, iface: IfIndex, addr: Ipv4Addr) -> Result<(), Counter> {
        let now = self.clock(now);
        let Some((i, mut cx)) = self.split(now, iface) else { return Err(self.unknown()) };
        let Some(index) = i.addresses.iter().position(|a| a.cidr.addr() == addr) else {
            cx.log.count(Counter::AddrUnknown);
            return Err(Counter::AddrUnknown);
        };
        let removed = i.addresses.remove(index);
        cx.timers.cancel(Timer::Acd(iface, addr));
        if removed.usable() {
            route::withdraw_off_link(i, &mut cx);
            cx.bump();
        }
        Ok(())
    }

    pub fn link_up(&mut self, now: Instant, iface: IfIndex) -> Result<(), Counter> {
        let now = self.clock(now);
        let Some((i, mut cx)) = self.split(now, iface) else { return Err(self.unknown()) };
        if !i.up {
            i.up = true;
            cx.bump();
            acd::link_up(i, &mut cx);
            igmp::rejoin(i, &mut cx);
        }
        Ok(())
    }

    /// Every neighbour entry goes, probing is abandoned, IGMP stops and nothing waits to leave;
    /// addresses and gateways stay.
    pub fn link_down(&mut self, now: Instant, iface: IfIndex) -> Result<(), Counter> {
        let now = self.clock(now);
        let Some((i, mut cx)) = self.split(now, iface) else { return Err(self.unknown()) };
        if i.up {
            i.up = false;
            cx.bump();
            nud::flush(i, &mut cx);
            acd::link_down(i, &mut cx);
            igmp::link_down(i, &mut cx);
            route::refresh_active(i, &mut cx);
        }
        Ok(())
    }

    pub fn join(&mut self, now: Instant, iface: IfIndex, group: MulticastAddr) -> Result<(), Counter> {
        let now = self.clock(now);
        let Some((i, mut cx)) = self.split(now, iface) else { return Err(self.unknown()) };
        igmp::join(i, &mut cx, group)
    }

    pub fn leave(&mut self, now: Instant, iface: IfIndex, group: MulticastAddr) -> Result<(), Counter> {
        let now = self.clock(now);
        let Some((i, mut cx)) = self.split(now, iface) else { return Err(self.unknown()) };
        igmp::leave(i, &mut cx, group);
        Ok(())
    }

    pub fn igmp_mode(&self, iface: IfIndex) -> Option<IgmpMode> {
        self.ifaces.get(iface.0).map(|i| i.igmp.mode())
    }

    /// A transport's advice about its peer `remote`; it lands on the next hop's entry, the
    /// gateway's for an off-link peer.
    pub fn advise(&mut self, now: Instant, remote: Ipv4Addr, advice: Advice) {
        let now = self.clock(now);
        let Ok(route) = route::lookup(&self.ifaces, remote, Source::Any, None) else { return };
        let NextHop::Neighbour(next_hop) = route.next_hop else { return };
        let Some((i, mut cx)) = self.split(now, route.iface) else { return };
        nud::advise(i, &mut cx, next_hop, advice == Advice::Confirmed);
    }

    /// Whether a flow may build a segment for `next_hop` now: a flow never parks one in
    /// [ip]. A request this queues prefers the flow's `source`.
    pub fn resolve(&mut self, now: Instant, iface: IfIndex, next_hop: Ipv4Addr, source: Ipv4Addr) -> Resolution {
        let now = self.clock(now);
        let Some((i, mut cx)) = self.split(now, iface) else { return Resolution::Failed };
        if !i.up {
            return Resolution::Pending;
        }
        match nud::send(i, &mut cx, next_hop, Some(source)) {
            Link::Resolved(mac) => Resolution::Resolved(mac),
            Link::Pending => Resolution::Pending,
            Link::Failed(_) => Resolution::Failed,
        }
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.timers.next()
    }

    /// Processes every deadline at or before `now`, in deadline order, each state object once.
    pub fn fire(&mut self, now: Instant) {
        let now = self.clock(now);
        let mut reports: BTreeMap<IfIndex, Vec<igmp::Record>> = BTreeMap::new();
        for timer in self.timers.due(now) {
            let iface = match timer {
                Timer::Neighbour(iface, _) | Timer::Acd(iface, _) | Timer::Igmp(iface, _) => iface,
            };
            let Some((i, mut cx)) = self.split(now, iface) else { continue };
            match timer {
                Timer::Neighbour(_, addr) => nud::fire(i, &mut cx, addr),
                Timer::Acd(_, addr) => acd::fire(i, &mut cx, addr),
                Timer::Igmp(_, t) => igmp::fire(i, &mut cx, t, reports.entry(iface).or_default()),
            }
        }
        for (iface, records) in reports {
            if let Some((i, mut cx)) = self.split(now, iface) {
                igmp::flush(i, &mut cx, records);
            }
        }
    }
}
