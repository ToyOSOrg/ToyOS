//! Address conflict detection (RFC 5227, §8). A new address probes on RFC 5227's schedule scaled
//! by the owner's ruling (`limits::acd`), is usable from its first announcement, and is then
//! defended once per DEFEND_INTERVAL and given up on a second conflict inside it (§2.4 (b)): one
//! forged packet cannot take an address away. Every interval starts when its frame leaves.

use core::net::Ipv4Addr;

use toyos_net_wire::ethernet::MacAddr;
use toyos_net_wire::Instant;

use crate::addr::{Address, Phase};
use crate::counters::Counter;
use crate::draw::Purpose;
use crate::egress::Item;
use crate::iface::{Cx, Interface};
use crate::limits::acd::{
    ANNOUNCE_INTERVAL, ANNOUNCE_NUM, ANNOUNCE_WAIT, CONFLICT_RESET, DEFEND_INTERVAL, MAX_CONFLICTS, PROBE_MAX, PROBE_MIN,
    PROBE_NUM, PROBE_WAIT, RATE_LIMIT_INTERVAL,
};
use crate::timers::Timer;
use crate::{igmp, route, Event, Peer};

/// An interface's conflict history, for the rate limit on new candidates (§8.3).
#[derive(Debug, Default)]
pub(crate) struct Conflicts {
    count: u32,
    last: Option<Instant>,
    /// When the last new address's first probe left.
    started: Option<Instant>,
}

fn timer(cx: &Cx<'_>, addr: Ipv4Addr) -> Timer {
    Timer::Acd(cx.iface, addr)
}

fn record(i: &mut Interface, addr: Ipv4Addr) -> Option<&mut Address> {
    i.addresses.iter_mut().find(|a| a.cidr.addr == addr)
}

/// Schedules a tentative address's first probe: a draw in [0, PROBE_WAIT], deferred past
/// RATE_LIMIT_INTERVAL after the last probe start once MAX_CONFLICTS conflicts were seen.
pub(crate) fn start(i: &mut Interface, cx: &mut Cx<'_>, addr: Ipv4Addr) {
    let now = cx.now;
    if !i.up {
        return;
    }
    if i.acd.last.is_some_and(|at| now.since(at) >= CONFLICT_RESET) {
        i.acd.count = 0;
    }
    let mut at = now.after(cx.draws.between(Purpose::Acd, core::time::Duration::ZERO, PROBE_WAIT));
    if i.acd.count >= MAX_CONFLICTS {
        if let Some(permitted) = i.acd.started.map(|s| s.after(RATE_LIMIT_INTERVAL)).filter(|p| *p > at) {
            at = permitted;
            cx.log.count(Counter::AcdRateLimited);
        }
    }
    cx.timers.arm(timer(cx, addr), at);
}

fn announce(i: &mut Interface, cx: &mut Cx<'_>, addr: Ipv4Addr, owed: bool) {
    if let Some(Address { phase: Phase::Usable { queued, .. }, .. }) = record(i, addr).filter(|_| owed) {
        *queued = true;
    }
    if !cx.control.push(Item::Announce { iface: cx.iface, addr, owed }, cx.log) {
        announced(i, cx, addr, owed);
    }
}

/// A deadline of `addr`: the next probe, the end of ANNOUNCE_WAIT, or an owed announcement.
pub(crate) fn fire(i: &mut Interface, cx: &mut Cx<'_>, addr: Ipv4Addr) {
    let first = i.usable().next().is_none();
    let Some(a) = record(i, addr) else { return };
    match a.phase {
        Phase::Tentative { probes, queued: false } if probes < PROBE_NUM => {
            a.phase = Phase::Tentative { probes, queued: true };
            if !cx.control.push(Item::Probe { iface: cx.iface, addr }, cx.log) {
                probed(i, cx, addr);
            }
        }
        Phase::Tentative { probes, .. } if probes >= PROBE_NUM => {
            a.phase = Phase::Usable { assigned: false, owed: ANNOUNCE_NUM, queued: false };
            announce(i, cx, addr, true);
            cx.log.count(Counter::AcdVerified);
            cx.log.event(Event::Verified { iface: cx.iface, addr });
            cx.bump();
            route::refresh_active(i, cx);
            if first {
                igmp::rejoin(i, cx);
            }
        }
        Phase::Usable { owed, queued: false, .. } if owed > 0 => announce(i, cx, addr, true),
        Phase::Tentative { .. } | Phase::Usable { .. } => {}
    }
}

/// A probe for `addr` left, or was lost.
pub(crate) fn probed(i: &mut Interface, cx: &mut Cx<'_>, addr: Ipv4Addr) {
    let now = cx.now;
    let Some(Address { phase: Phase::Tentative { probes, queued: queued @ true }, .. }) = record(i, addr) else { return };
    *queued = false;
    *probes = probes.saturating_add(1);
    let wait = if *probes < PROBE_NUM { cx.draws.between(Purpose::Acd, PROBE_MIN, PROBE_MAX) } else { ANNOUNCE_WAIT };
    if *probes == 1 {
        i.acd.started = Some(now);
    }
    cx.timers.arm(timer(cx, addr), now.after(wait));
}

/// An announcement of `addr` left, or was lost; a defence owes nothing.
pub(crate) fn announced(i: &mut Interface, cx: &mut Cx<'_>, addr: Ipv4Addr, owed: bool) {
    let now = cx.now;
    let Some(Address { phase: Phase::Usable { assigned, owed: left, queued: queued @ true }, .. }) = record(i, addr).filter(|_| owed) else { return };
    *queued = false;
    *left = left.saturating_sub(1);
    if *left > 0 {
        cx.timers.arm(timer(cx, addr), now.after(ANNOUNCE_INTERVAL));
    } else {
        *assigned = true;
    }
}

/// Removes `addr` after a conflict, and tells the shell which kind.
fn lose(i: &mut Interface, cx: &mut Cx<'_>, addr: Ipv4Addr, mac: MacAddr) {
    let now = cx.now;
    let Some(index) = i.addresses.iter().position(|a| a.cidr.addr == addr) else { return };
    let removed = i.addresses.remove(index);
    cx.timers.cancel(timer(cx, addr));
    cx.log.refuse(Counter::AcdConflict, cx.iface, Peer::Arp { ip: addr, mac });
    i.acd.count = i.acd.count.saturating_add(1);
    i.acd.last = Some(now);
    if removed.usable() {
        cx.log.event(Event::Lost { iface: cx.iface, addr, mac });
        route::withdraw_off_link(i, cx);
        cx.bump();
    } else {
        cx.log.event(Event::Conflict { iface: cx.iface, addr, mac });
    }
}

/// An ARP packet from another MAC naming `addr`, one of ours, as its sender; or a probe for it
/// while it is tentative (§7.2 (3), RFC 5227 §2.1.1, §2.4).
pub(crate) fn conflict(i: &mut Interface, cx: &mut Cx<'_>, addr: Ipv4Addr, mac: MacAddr) {
    let now = cx.now;
    let Some(a) = record(i, addr) else { return };
    match a.phase {
        Phase::Tentative { .. } => lose(i, cx, addr, mac),
        Phase::Usable { .. } if a.defended.is_some_and(|at| now.since(at) <= DEFEND_INTERVAL) => lose(i, cx, addr, mac),
        Phase::Usable { .. } => {
            a.defended = Some(now);
            cx.log.refuse(Counter::AcdDefended, cx.iface, Peer::Arp { ip: addr, mac });
            announce(i, cx, addr, false);
        }
    }
}

/// The link came up: held addresses stay usable and are announced twice, and probing starts for
/// any address added while it was down (§8.6).
pub(crate) fn link_up(i: &mut Interface, cx: &mut Cx<'_>) {
    let addrs: alloc::vec::Vec<(Ipv4Addr, bool)> = i.addresses.iter().map(|a| (a.cidr.addr, a.usable())).collect();
    for (addr, usable) in addrs {
        if usable {
            if let Some(Address { phase: Phase::Usable { owed, .. }, .. }) = record(i, addr) {
                *owed = ANNOUNCE_NUM;
            }
            announce(i, cx, addr, true);
        } else {
            start(i, cx, addr);
        }
    }
}

/// The link went down: probing is abandoned and its addresses removed, unverified (§6.10).
pub(crate) fn link_down(i: &mut Interface, cx: &mut Cx<'_>) {
    let iface = cx.iface;
    for a in &mut i.addresses {
        cx.timers.cancel(Timer::Acd(iface, a.cidr.addr));
        if let Phase::Usable { owed, queued, .. } = &mut a.phase {
            *owed = 0;
            *queued = false;
        }
    }
    let tentative = i.addresses.iter().filter(|a| !a.usable()).map(|a| a.cidr.addr);
    for addr in tentative.collect::<alloc::vec::Vec<_>>() {
        cx.log.event(Event::NotVerified { iface, addr });
    }
    i.addresses.retain(Address::usable);
}
