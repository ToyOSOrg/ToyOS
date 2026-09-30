//! The reachability machine (§6): RFC 4861 §7.3's neighbour unreachability detection with RFC
//! 7048's UNREACHABLE, written over ARP so NDP can reuse it. A request is the probe; a solicited
//! reply or a transport's positive advice is a confirmation; any other ARP packet naming a
//! neighbour we hold is an assertion. Requests to one neighbour are never closer than RETRANS,
//! measured from each request's hand-off; a timer fires once per `fire` and its next deadline is
//! taken from that moment, so a jumped clock never replays missed periods.
//!
//! Each state carries exactly its own fields: a MAC only where one is known, a pending queue
//! only while INCOMPLETE.

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::net::Ipv4Addr;
use core::time::Duration;

use toyos_net_wire::ethernet::MacAddr;
use toyos_net_wire::Instant;

use crate::counters::Counter;
use crate::egress::{FrameKind, Item};
use crate::iface::{Cx, Interface};
use crate::limits::nud::{
    BACKOFF_MULTIPLE, BROADCAST_SOLICIT, DELAY_FIRST_PROBE, FAILED_HOLD, IDLE_LIFETIME, LOCKTIME, MAX_RETRANS,
    RETRANS, TABLE_MAX, UNICAST_SOLICIT,
};
use crate::timers::Timer;
use crate::{route, Event, Flow, Peer};

#[derive(Debug)]
pub enum Nud {
    Incomplete(Incomplete),
    Reachable(Reachable),
    Stale(Linked),
    Delay(Linked),
    Probe(Probing),
    Unreachable(Unreachable),
    Failed(Failed),
}

#[derive(Debug)]
pub struct Incomplete {
    requests: u8,
    pending: VecDeque<Held>,
}

impl Incomplete {
    pub fn requests(&self) -> u8 {
        self.requests
    }

    /// Datagrams waiting for the answer.
    pub fn queued(&self) -> usize {
        self.pending.len()
    }
}

#[derive(Debug)]
pub struct Reachable {
    mac: MacAddr,
    confirmed: Instant,
}

impl Reachable {
    pub fn mac(&self) -> MacAddr {
        self.mac
    }

    pub fn confirmed(&self) -> Instant {
        self.confirmed
    }
}

/// STALE or DELAY: a MAC and nothing else.
#[derive(Debug)]
pub struct Linked {
    mac: MacAddr,
}

impl Linked {
    pub fn mac(&self) -> MacAddr {
        self.mac
    }
}

#[derive(Debug)]
pub struct Probing {
    mac: MacAddr,
    requests: u8,
}

impl Probing {
    pub fn mac(&self) -> MacAddr {
        self.mac
    }

    pub fn requests(&self) -> u8 {
        self.requests
    }
}

#[derive(Debug)]
pub struct Unreachable {
    mac: MacAddr,
    /// Broadcast requests this episode (RFC 7048 §4's k).
    requests: u32,
    /// A request left and its backoff runs.
    backoff: bool,
    /// A datagram went to the MAC since that request.
    sent: bool,
}

impl Unreachable {
    pub fn mac(&self) -> MacAddr {
        self.mac
    }

    pub fn requests(&self) -> u32 {
        self.requests
    }

    /// No request pending: nothing is sent until the next datagram.
    pub fn quiescent(&self) -> bool {
        !self.backoff
    }
}

#[derive(Debug)]
pub struct Failed {
    since: Instant,
}

impl Failed {
    pub fn since(&self) -> Instant {
        self.since
    }
}

impl Nud {
    pub fn mac(&self) -> Option<MacAddr> {
        match self {
            Self::Reachable(r) => Some(r.mac),
            Self::Stale(l) | Self::Delay(l) => Some(l.mac),
            Self::Probe(p) => Some(p.mac),
            Self::Unreachable(u) => Some(u.mac),
            Self::Incomplete(_) | Self::Failed(_) => None,
        }
    }

    /// A request of ours may be outstanding: a reply now is solicited (§7.2 (5)).
    fn solicits(&self) -> bool {
        matches!(self, Self::Incomplete(_) | Self::Probe(_) | Self::Unreachable(_))
    }
}

/// A frame [ip] built and holds for its next hop, and who is told if it never leaves.
#[derive(Debug)]
pub(crate) struct Held {
    pub frame: Vec<u8>,
    pub kind: FrameKind,
    pub flow: Option<Flow>,
}

#[derive(Debug)]
pub(crate) struct Neighbour {
    pub state: Nud,
    pub last_request: Option<Instant>,
    /// A request waits in the control queue.
    pub queued: bool,
    /// When a datagram last went to it: eviction takes the entry unused longest.
    pub used: Instant,
    /// The source a request prefers: the prompting datagram's (§6.3).
    pub hint: Option<Ipv4Addr>,
}

/// Where a datagram goes now (§6.3's "send", §6.7's answer).
#[derive(Debug)]
pub(crate) enum Link {
    Resolved(MacAddr),
    Pending,
    Failed(Counter),
}

fn backoff(requests: u32) -> Duration {
    BACKOFF_MULTIPLE.checked_pow(requests).and_then(|m| RETRANS.checked_mul(m)).map_or(MAX_RETRANS, |d| d.min(MAX_RETRANS))
}

fn timer(cx: &Cx<'_>, addr: Ipv4Addr) -> Timer {
    Timer::Neighbour(cx.iface, addr)
}

/// Queues a request to `addr`, broadcast or to `to`; one the full queue refuses counts as sent,
/// so the machine moves on and never stalls on a loss.
fn request(i: &mut Interface, cx: &mut Cx<'_>, addr: Ipv4Addr, to: Option<MacAddr>) {
    let Some(n) = i.neighbours.get_mut(&addr) else { return };
    n.queued = true;
    if !cx.control.push(Item::Request { iface: cx.iface, target: addr, to }, cx.log) {
        request_left(i, cx, addr);
    }
}

/// A request to `addr` left, or was lost: its spacing and its state's deadline start now.
pub(crate) fn request_left(i: &mut Interface, cx: &mut Cx<'_>, addr: Ipv4Addr) {
    let now = cx.now;
    let Some(n) = i.neighbours.get_mut(&addr) else { return };
    if !core::mem::replace(&mut n.queued, false) {
        return;
    }
    n.last_request = Some(now);
    let wait = match &mut n.state {
        Nud::Incomplete(s) => {
            s.requests = s.requests.saturating_add(1);
            RETRANS
        }
        Nud::Probe(s) => {
            s.requests = s.requests.saturating_add(1);
            RETRANS
        }
        Nud::Unreachable(s) => {
            s.requests = s.requests.saturating_add(1);
            s.backoff = true;
            s.sent = false;
            backoff(s.requests)
        }
        Nud::Reachable(_) | Nud::Stale(_) | Nud::Delay(_) | Nud::Failed(_) => return,
    };
    cx.timers.arm(timer(cx, addr), now.after(wait));
}

/// Makes room for one more entry, evicting FAILED, then quiescent UNREACHABLE, then STALE, each
/// the one unused longest (§6.8).
fn make_room(i: &mut Interface, cx: &mut Cx<'_>) -> bool {
    if i.neighbours.len() < TABLE_MAX {
        return true;
    }
    let class = |n: &Neighbour| match &n.state {
        Nud::Failed(_) => Some(0),
        Nud::Unreachable(u) if u.quiescent() && !n.queued => Some(1),
        Nud::Stale(_) => Some(2),
        _ => None,
    };
    let victim = i.neighbours.iter().filter_map(|(a, n)| class(n).map(|c| (c, n.used, *a))).min().map(|(_, _, a)| a);
    let Some(victim) = victim else { return false };
    i.neighbours.remove(&victim);
    cx.timers.cancel(timer(cx, victim));
    true
}

/// A datagram or a flow wants `addr` now (§6.3 "send"): creates INCOMPLETE, moves STALE to
/// DELAY, or asks a quiescent UNREACHABLE for a request.
pub(crate) fn send(i: &mut Interface, cx: &mut Cx<'_>, addr: Ipv4Addr, hint: Option<Ipv4Addr>) -> Link {
    let now = cx.now;
    let Some(n) = i.neighbours.get_mut(&addr) else {
        if !make_room(i, cx) {
            cx.log.count(Counter::NbTableFull);
            return Link::Failed(Counter::NbTableFull);
        }
        let state = Nud::Incomplete(Incomplete { requests: 0, pending: VecDeque::new() });
        i.neighbours.insert(addr, Neighbour { state, last_request: None, queued: false, used: now, hint });
        request(i, cx, addr, None);
        return Link::Pending;
    };
    n.used = now;
    if hint.is_some() {
        n.hint = hint;
    }
    let queued = n.queued;
    match &mut n.state {
        Nud::Incomplete(_) => Link::Pending,
        Nud::Reachable(Reachable { mac, .. }) | Nud::Delay(Linked { mac }) | Nud::Probe(Probing { mac, .. }) => Link::Resolved(*mac),
        Nud::Stale(Linked { mac }) => {
            let mac = *mac;
            n.state = Nud::Delay(Linked { mac });
            cx.timers.arm(timer(cx, addr), now.after(DELAY_FIRST_PROBE));
            Link::Resolved(mac)
        }
        Nud::Unreachable(u) => {
            let mac = u.mac;
            if u.backoff {
                u.sent = true;
            } else if !queued {
                request(i, cx, addr, None);
            }
            Link::Resolved(mac)
        }
        Nud::Failed(_) => {
            cx.log.count(Counter::NbFailedRefused);
            Link::Failed(Counter::NbFailedRefused)
        }
    }
}

/// Holds a frame for an INCOMPLETE neighbour; the queue keeps the newest (RFC 4861 §7.2.2).
pub(crate) fn hold(i: &mut Interface, cx: &mut Cx<'_>, addr: Ipv4Addr, held: Held) {
    let Some(Nud::Incomplete(s)) = i.neighbours.get_mut(&addr).map(|n| &mut n.state) else { return };
    if s.pending.len() >= crate::limits::nud::PENDING_PER_NEIGHBOUR {
        s.pending.pop_front();
        i.held = i.held.saturating_sub(1);
        cx.log.count(Counter::NbPendingOverflow);
    }
    s.pending.push_back(held);
    i.held = i.held.saturating_add(1);
}

/// A deadline of `addr`'s entry.
pub(crate) fn fire(i: &mut Interface, cx: &mut Cx<'_>, addr: Ipv4Addr) {
    let now = cx.now;
    let reachable = i.reachable_time(cx.draws, now);
    let Some(n) = i.neighbours.get_mut(&addr) else { return };
    let spaced = n.last_request.map_or(now, |at| at.after(RETRANS)).max(now);
    match &mut n.state {
        Nud::Incomplete(s) if s.requests < BROADCAST_SOLICIT => request(i, cx, addr, None),
        Nud::Incomplete(_) => fail(i, cx, addr),
        Nud::Reachable(s) => {
            let end = s.confirmed.after(reachable);
            if now < end {
                cx.timers.arm(timer(cx, addr), end);
            } else {
                n.state = Nud::Stale(Linked { mac: s.mac });
                cx.timers.arm(timer(cx, addr), now.after(IDLE_LIFETIME));
            }
        }
        Nud::Delay(s) => {
            n.state = Nud::Probe(Probing { mac: s.mac, requests: 0 });
            probe(i, cx, addr, spaced);
        }
        Nud::Probe(s) if s.requests < UNICAST_SOLICIT => {
            let mac = s.mac;
            request(i, cx, addr, Some(mac));
        }
        Nud::Probe(s) => {
            n.state = Nud::Unreachable(Unreachable { mac: s.mac, requests: 0, backoff: false, sent: false });
            cx.log.count(Counter::NbUnreachable);
            cx.timers.arm(timer(cx, addr), now.after(IDLE_LIFETIME));
            route::refresh_active(i, cx);
        }
        Nud::Unreachable(s) if s.backoff && s.sent => request(i, cx, addr, None),
        Nud::Unreachable(s) if s.backoff => {
            s.backoff = false;
            cx.timers.arm(timer(cx, addr), now.after(IDLE_LIFETIME));
        }
        Nud::Stale(_) | Nud::Unreachable(_) | Nud::Failed(_) => {
            i.neighbours.remove(&addr);
            route::refresh_active(i, cx);
        }
    }
}

/// PROBE's first unicast request, now or at the spacing boundary.
fn probe(i: &mut Interface, cx: &mut Cx<'_>, addr: Ipv4Addr, at: Instant) {
    if at <= cx.now {
        let mac = i.neighbours.get(&addr).and_then(|n| n.state.mac());
        request(i, cx, addr, mac);
    } else {
        cx.timers.arm(timer(cx, addr), at);
    }
}

/// A held datagram that will never leave: counted, and its sender told (§6.5, §9.6).
pub(crate) fn drop_held(cx: &mut Cx<'_>, held: Held) {
    cx.log.count(Counter::NbPendingDropped);
    if let Some(flow) = held.flow {
        cx.log.event(Event::Unreachable(flow));
    }
}

/// INCOMPLETE gave up: every held datagram is dropped.
fn fail(i: &mut Interface, cx: &mut Cx<'_>, addr: Ipv4Addr) {
    let now = cx.now;
    let Some(n) = i.neighbours.get_mut(&addr) else { return };
    let Nud::Incomplete(s) = core::mem::replace(&mut n.state, Nud::Failed(Failed { since: now })) else { return };
    for held in s.pending {
        i.held = i.held.saturating_sub(1);
        drop_held(cx, held);
    }
    cx.log.count(Counter::NbFailed);
    cx.log.event(Event::Failed { iface: cx.iface, next_hop: addr });
    cx.timers.arm(timer(cx, addr), now.after(FAILED_HOLD));
    route::refresh_active(i, cx);
}

/// INCOMPLETE learned `mac`: its datagrams leave in arrival order, ahead of any sent later (§6.5).
fn resolved(cx: &mut Cx<'_>, addr: Ipv4Addr, mac: MacAddr, pending: VecDeque<Held>) {
    for mut held in pending {
        if let Some((destination, _)) = held.frame.split_first_chunk_mut::<6>() {
            *destination = mac.0;
        }
        cx.control.release(cx.iface, held);
    }
    cx.log.count(Counter::NbResolved);
    cx.log.event(Event::Resolved { iface: cx.iface, next_hop: addr });
}

fn mac_changed(cx: &mut Cx<'_>, addr: Ipv4Addr, old: MacAddr, new: MacAddr) {
    cx.log.refuse(Counter::ArpMacChanged, cx.iface, Peer::MacChange { ip: addr, old, new });
}

/// Enters REACHABLE, confirmed now.
fn reach(i: &mut Interface, cx: &mut Cx<'_>, addr: Ipv4Addr, mac: MacAddr) {
    let now = cx.now;
    let reachable = i.reachable_time(cx.draws, now);
    let Some(n) = i.neighbours.get_mut(&addr) else { return };
    let old = core::mem::replace(&mut n.state, Nud::Reachable(Reachable { mac, confirmed: now }));
    cx.timers.arm(timer(cx, addr), now.after(reachable));
    match old {
        Nud::Incomplete(s) => resolved(cx, addr, mac, s.pending),
        other => {
            if let Some(was) = other.mac().filter(|was| *was != mac) {
                mac_changed(cx, addr, was, mac);
            }
        }
    }
    route::refresh_active(i, cx);
}

/// Enters STALE at `mac`.
fn stale(i: &mut Interface, cx: &mut Cx<'_>, addr: Ipv4Addr, mac: MacAddr) {
    let now = cx.now;
    let Some(n) = i.neighbours.get_mut(&addr) else { return };
    let old = core::mem::replace(&mut n.state, Nud::Stale(Linked { mac }));
    cx.timers.arm(timer(cx, addr), now.after(IDLE_LIFETIME));
    match old {
        Nud::Incomplete(s) => resolved(cx, addr, mac, s.pending),
        other => {
            if let Some(was) = other.mac().filter(|was| *was != mac) {
                mac_changed(cx, addr, was, mac);
            }
        }
    }
    route::refresh_active(i, cx);
}

/// Whether a reply from `addr` now answers a request of ours.
pub(crate) fn solicits(i: &Interface, addr: Ipv4Addr) -> bool {
    i.neighbours.get(&addr).is_some_and(|n| n.state.solicits())
}

/// A solicited reply (§7.2 (5)): the host asked, so it overrides any MAC (RFC 4861 §7.2.5).
pub(crate) fn confirm(i: &mut Interface, cx: &mut Cx<'_>, addr: Ipv4Addr, mac: MacAddr) {
    reach(i, cx, addr, mac);
}

/// Any other ARP packet naming a neighbour we hold (§7.2 (6), §7.3): a new MAC is taken only into
/// STALE, and never within LOCKTIME of a confirmation.
pub(crate) fn assert(i: &mut Interface, cx: &mut Cx<'_>, addr: Ipv4Addr, mac: MacAddr) {
    let now = cx.now;
    let Some(n) = i.neighbours.get(&addr) else { return };
    match &n.state {
        Nud::Reachable(r) if r.mac != mac && now.since(r.confirmed) < LOCKTIME => {
            let old = r.mac;
            cx.log.refuse(Counter::ArpOverrideLocked, cx.iface, Peer::MacChange { ip: addr, old, new: mac });
        }
        state if state.mac() == Some(mac) => {}
        _ => stale(i, cx, addr, mac),
    }
}

/// An ARP request for one of our addresses from an on-link host we hold nothing for: the reply
/// path needs it, and STALE makes it verified before it is trusted (§7.3).
pub(crate) fn learn(i: &mut Interface, cx: &mut Cx<'_>, addr: Ipv4Addr, mac: MacAddr) {
    if i.neighbours.contains_key(&addr) || !make_room(i, cx) {
        return;
    }
    let now = cx.now;
    i.neighbours.insert(addr, Neighbour { state: Nud::Stale(Linked { mac }), last_request: None, queued: false, used: now, hint: None });
    cx.timers.arm(timer(cx, addr), now.after(IDLE_LIFETIME));
}

/// A transport's advice about the next hop `addr` (§6.9).
pub(crate) fn advise(i: &mut Interface, cx: &mut Cx<'_>, addr: Ipv4Addr, confirmed: bool) {
    let now = cx.now;
    let Some(n) = i.neighbours.get_mut(&addr) else { return };
    let spaced = n.last_request.map_or(now, |at| at.after(RETRANS)).max(now);
    match (&mut n.state, confirmed) {
        (Nud::Reachable(r), true) => r.confirmed = now,
        (state @ (Nud::Stale(_) | Nud::Delay(_) | Nud::Probe(_) | Nud::Unreachable(_)), true) => {
            if let Some(mac) = state.mac() {
                reach(i, cx, addr, mac);
            }
        }
        (state @ (Nud::Reachable(_) | Nud::Stale(_) | Nud::Delay(_)), false) => {
            if let Some(mac) = state.mac() {
                *state = Nud::Probe(Probing { mac, requests: 0 });
                probe(i, cx, addr, spaced);
            }
        }
        _ => {}
    }
}

/// The link went down: every entry goes, and with it every datagram held here (§6.10).
pub(crate) fn flush(i: &mut Interface, cx: &mut Cx<'_>) {
    for (addr, n) in core::mem::take(&mut i.neighbours) {
        cx.timers.cancel(timer(cx, addr));
        if let Nud::Incomplete(s) = n.state {
            for held in s.pending {
                drop_held(cx, held);
            }
        }
    }
    for held in cx.control.purge(cx.iface) {
        drop_held(cx, held);
    }
    i.held = 0;
}
