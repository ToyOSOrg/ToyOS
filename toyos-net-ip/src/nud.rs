//! The reachability machine (§6): RFC 4861 §7.3's neighbour unreachability detection with RFC
//! 7048's UNREACHABLE, written over ARP so NDP can reuse it. A request is the probe; a solicited
//! reply or a transport's positive advice is a confirmation; any other ARP packet naming a
//! neighbour we hold is an assertion. Requests to one neighbour are never closer than RETRANS,
//! measured from each request's hand-off; a timer fires once per `fire` and its next deadline is
//! taken from that moment, so a jumped clock never replays missed periods.
//!
//! Each state carries exactly its own fields: a MAC only where one is known, a waiting request
//! only in a state that sends one, and a pending queue that takes datagrams in INCOMPLETE and only
//! drains in every state resolution leads to (§6.2).
//! Resolution moves INCOMPLETE's queue into the resolved state and gives each datagram a turn in
//! the control queue, where it leaves to the MAC of that moment. While its queue holds any, an
//! entry does not idle out and is evicted only after every other candidate (§6.8).

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::net::Ipv4Addr;
use core::time::Duration;

use toyos_net_wire::ethernet::MacAddr;
use toyos_net_wire::Instant;

use crate::counters::Counter;
use crate::egress::{FrameKind, Item, Turn};
use crate::iface::{Cx, Interface};
use crate::limits::nud::{
    BACKOFF_MULTIPLE, BROADCAST_SOLICIT, DELAY_FIRST_PROBE, FAILED_HOLD, IDLE_LIFETIME, LOCKTIME, MAX_RETRANS,
    PENDING_PER_NEIGHBOUR, RETRANS, TABLE_MAX, UNICAST_SOLICIT,
};
use crate::timers::Timer;
use crate::{route, Event, Flow, Peer};

#[derive(Debug)]
pub enum Nud {
    Incomplete(Incomplete),
    Reachable(Reachable),
    Stale(Stale),
    Delay(Delaying),
    Probe(Probing),
    Unreachable(Unreachable),
    /// Resolution failed; its hold-down is the entry's deadline.
    Failed,
}

/// A frame [ip] built and holds for its next hop, and who is told if it never leaves.
#[derive(Debug)]
pub struct Held {
    pub(crate) frame: Vec<u8>,
    pub(crate) kind: FrameKind,
    pub(crate) flow: Option<Flow>,
}

/// INCOMPLETE's pending queue: the only one that takes datagrams (§6.5).
#[derive(Debug, Default)]
pub struct Pending(VecDeque<Held>);

impl Pending {
    pub fn queued(&self) -> usize {
        self.0.len()
    }

    /// Takes `held`, handing back the oldest when PENDING_PER_NEIGHBOUR wait already (RFC 4861
    /// §7.2.2).
    pub fn push(&mut self, held: Held) -> Option<Held> {
        let oldest = if self.0.len() >= PENDING_PER_NEIGHBOUR { self.0.pop_front() } else { None };
        self.0.push_back(held);
        oldest
    }
}

/// A resolved entry's pending queue: what resolution released that has yet to leave, oldest
/// first. It only drains (§6.2).
#[derive(Debug, Default)]
pub struct Released(VecDeque<Held>);

impl Released {
    pub fn queued(&self) -> usize {
        self.0.len()
    }

    pub fn pop(&mut self) -> Option<Held> {
        self.0.pop_front()
    }
}

/// Entered only by a new entry, and its deadline armed only as its request leaves: a request that
/// finds INCOMPLETE is its one waiting request, so it carries no flag.
#[derive(Debug)]
pub struct Incomplete {
    requests: u8,
    pub pending: Pending,
}

impl Incomplete {
    pub fn requests(&self) -> u8 {
        self.requests
    }
}

#[derive(Debug)]
pub struct Reachable {
    mac: MacAddr,
    confirmed: Instant,
    pub released: Released,
}

impl Reachable {
    pub fn mac(&self) -> MacAddr {
        self.mac
    }

    pub fn confirmed(&self) -> Instant {
        self.confirmed
    }
}

/// STALE or quiescent UNREACHABLE's IDLE_LIFETIME. It passes without deleting the entry only
/// while released datagrams are queued, and the entry then goes as the last leaves (§6.3).
#[derive(Clone, Copy, Debug)]
enum Lifetime {
    Runs,
    Passed,
}

#[derive(Debug)]
pub struct Stale {
    mac: MacAddr,
    lifetime: Lifetime,
    pub released: Released,
}

#[derive(Debug)]
pub struct Delaying {
    mac: MacAddr,
    pub released: Released,
}

#[derive(Debug)]
pub struct Probing {
    mac: MacAddr,
    requests: u8,
    /// A request waits in the control queue.
    queued: bool,
    pub released: Released,
}

impl Probing {
    pub fn requests(&self) -> u8 {
        self.requests
    }
}

/// UNREACHABLE's request (RFC 7048 §3): only an entry with none pending has an idle lifetime.
#[derive(Clone, Copy, Debug)]
enum Solicit {
    Quiescent(Lifetime),
    /// A request waits in the control queue; its backoff starts as it leaves.
    Queued,
    /// A request left and its backoff runs; `sent`: a datagram went to the MAC since.
    Backoff { sent: bool },
}

#[derive(Debug)]
pub struct Unreachable {
    mac: MacAddr,
    /// Broadcast requests this episode (RFC 7048 §4's k).
    requests: u32,
    solicit: Solicit,
    pub released: Released,
}

impl Unreachable {
    /// No request pending: nothing is sent until the next datagram.
    pub fn quiescent(&self) -> bool {
        matches!(self.solicit, Solicit::Quiescent(_))
    }
}

impl Nud {
    pub fn mac(&self) -> Option<MacAddr> {
        match self {
            Self::Reachable(r) => Some(r.mac),
            Self::Stale(s) => Some(s.mac),
            Self::Delay(d) => Some(d.mac),
            Self::Probe(p) => Some(p.mac),
            Self::Unreachable(u) => Some(u.mac),
            Self::Incomplete(_) | Self::Failed => None,
        }
    }

    /// A request of ours may be outstanding: a reply now is solicited (§7.2 (5)).
    fn solicits(&self) -> bool {
        matches!(self, Self::Incomplete(_) | Self::Probe(_) | Self::Unreachable(_))
    }

    fn released_mut(&mut self) -> Option<&mut Released> {
        match self {
            Self::Reachable(Reachable { released, .. })
            | Self::Stale(Stale { released, .. })
            | Self::Delay(Delaying { released, .. })
            | Self::Probe(Probing { released, .. })
            | Self::Unreachable(Unreachable { released, .. }) => Some(released),
            Self::Incomplete(_) | Self::Failed => None,
        }
    }

    /// Released datagrams have yet to leave.
    fn releasing(&self) -> bool {
        match self {
            Self::Reachable(Reachable { released, .. })
            | Self::Stale(Stale { released, .. })
            | Self::Delay(Delaying { released, .. })
            | Self::Probe(Probing { released, .. })
            | Self::Unreachable(Unreachable { released, .. }) => !released.0.is_empty(),
            Self::Incomplete(_) | Self::Failed => false,
        }
    }

    /// A resolved state's released datagrams, taken for the state that follows it.
    fn take_released(&mut self) -> Released {
        self.released_mut().map(core::mem::take).unwrap_or_default()
    }
}

#[derive(Debug)]
pub(crate) struct Neighbour {
    pub state: Nud,
    pub last_request: Option<Instant>,
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

/// Queues a request to `addr`; one the full queue refuses counts as sent, so the machine moves on
/// and never stalls on a loss.
fn request(i: &mut Interface, cx: &mut Cx<'_>, addr: Ipv4Addr) {
    let Some(n) = i.neighbours.get_mut(&addr) else { return };
    match &mut n.state {
        Nud::Incomplete(_) => {}
        Nud::Probe(Probing { queued, .. }) => *queued = true,
        Nud::Unreachable(u) => u.solicit = Solicit::Queued,
        Nud::Reachable(_) | Nud::Stale(_) | Nud::Delay(_) | Nud::Failed => return,
    }
    if !cx.control.push(Item::Request { iface: cx.iface, target: addr }, cx.log) {
        request_left(i, cx, addr);
    }
}

/// A request to `addr` reached the head of the control queue and takes its form from the state of
/// this moment (§6.4): broadcast in INCOMPLETE and UNREACHABLE, to the cached MAC in PROBE, and
/// in every other state none at all, since they send no request (§6.3). It waits no longer.
pub(crate) fn request_leaves(i: &mut Interface, cx: &mut Cx<'_>, addr: Ipv4Addr) -> Option<MacAddr> {
    let to = match &i.neighbours.get(&addr)?.state {
        Nud::Incomplete(_) | Nud::Unreachable(Unreachable { solicit: Solicit::Queued, .. }) => MacAddr::BROADCAST,
        Nud::Probe(Probing { queued: true, mac, .. }) => *mac,
        Nud::Probe(_) | Nud::Unreachable(_) | Nud::Reachable(_) | Nud::Stale(_) | Nud::Delay(_) | Nud::Failed => return None,
    };
    request_left(i, cx, addr);
    Some(to)
}

/// A request to `addr` left, or was lost: its spacing and its state's deadline start now.
fn request_left(i: &mut Interface, cx: &mut Cx<'_>, addr: Ipv4Addr) {
    let now = cx.now;
    let Some(n) = i.neighbours.get_mut(&addr) else { return };
    let wait = match &mut n.state {
        Nud::Incomplete(Incomplete { requests, .. }) => {
            *requests = requests.saturating_add(1);
            RETRANS
        }
        Nud::Probe(Probing { requests, queued: queued @ true, .. }) => {
            *queued = false;
            *requests = requests.saturating_add(1);
            RETRANS
        }
        Nud::Unreachable(s @ Unreachable { solicit: Solicit::Queued, .. }) => {
            s.requests = s.requests.saturating_add(1);
            s.solicit = Solicit::Backoff { sent: false };
            backoff(s.requests)
        }
        Nud::Probe(_) | Nud::Unreachable(_) | Nud::Reachable(_) | Nud::Stale(_) | Nud::Delay(_) | Nud::Failed => return,
    };
    n.last_request = Some(now);
    cx.timers.arm(timer(cx, addr), now.after(wait));
}

/// Makes room for one more entry, evicting FAILED, then quiescent UNREACHABLE, then STALE, each
/// the one unused longest, and after all of them one whose queue still holds released datagrams
/// (§6.8).
fn make_room(i: &mut Interface, cx: &mut Cx<'_>) -> bool {
    if i.neighbours.len() < TABLE_MAX {
        return true;
    }
    let class = |n: &Neighbour| {
        let class = match &n.state {
            Nud::Failed => 0,
            Nud::Unreachable(u) if u.quiescent() => 1,
            Nud::Stale(_) => 2,
            _ => return None,
        };
        Some(if n.state.releasing() { 3 } else { class })
    };
    let victim = i.neighbours.iter().filter_map(|(a, n)| class(n).map(|c| (c, n.used, *a))).min().map(|(_, _, a)| a);
    let Some(victim) = victim else { return false };
    remove(i, cx, victim);
    true
}

/// Deletes `addr`'s entry, and with it the released datagrams its queue still holds, their turns
/// and its queued requests: no item outlives its entry.
fn remove(i: &mut Interface, cx: &mut Cx<'_>, addr: Ipv4Addr) {
    cx.timers.cancel(timer(cx, addr));
    let Some(mut n) = i.neighbours.remove(&addr) else { return };
    let turns = cx.control.purge_entry(cx.iface, addr);
    i.held = i.held.saturating_sub(turns);
    for held in n.state.take_released().0 {
        drop_held(cx, held, Counter::NbPendingEvicted);
    }
}

fn insert(i: &mut Interface, addr: Ipv4Addr, state: Nud, now: Instant, hint: Option<Ipv4Addr>) {
    i.neighbours.insert(addr, Neighbour { state, last_request: None, used: now, hint });
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
        insert(i, addr, Nud::Incomplete(Incomplete { requests: 0, pending: Pending::default() }), now, hint);
        request(i, cx, addr);
        return Link::Pending;
    };
    n.used = now;
    if hint.is_some() {
        n.hint = hint;
    }
    match &mut n.state {
        Nud::Incomplete(_) => Link::Pending,
        Nud::Reachable(Reachable { mac, .. }) | Nud::Delay(Delaying { mac, .. }) | Nud::Probe(Probing { mac, .. }) => Link::Resolved(*mac),
        Nud::Stale(Stale { mac, released, .. }) => {
            let mac = *mac;
            n.state = Nud::Delay(Delaying { mac, released: core::mem::take(released) });
            cx.timers.arm(timer(cx, addr), now.after(DELAY_FIRST_PROBE));
            Link::Resolved(mac)
        }
        Nud::Unreachable(u) => {
            let mac = u.mac;
            match u.solicit {
                Solicit::Quiescent(_) => request(i, cx, addr),
                Solicit::Queued => {}
                Solicit::Backoff { .. } => u.solicit = Solicit::Backoff { sent: true },
            }
            Link::Resolved(mac)
        }
        Nud::Failed => {
            cx.log.count(Counter::NbFailedRefused);
            Link::Failed(Counter::NbFailedRefused)
        }
    }
}

/// Holds a frame for an INCOMPLETE neighbour; the queue keeps the newest (RFC 4861 §7.2.2).
pub(crate) fn hold(i: &mut Interface, cx: &mut Cx<'_>, addr: Ipv4Addr, held: Held) {
    let Some(Nud::Incomplete(s)) = i.neighbours.get_mut(&addr).map(|n| &mut n.state) else { return };
    if s.pending.push(held).is_some() {
        i.held = i.held.saturating_sub(1);
        cx.log.count(Counter::NbPendingOverflow);
    }
    i.held = i.held.saturating_add(1);
}

/// A deadline of `addr`'s entry.
pub(crate) fn fire(i: &mut Interface, cx: &mut Cx<'_>, addr: Ipv4Addr) {
    let now = cx.now;
    let reachable = i.reachable_time(cx.draws, now);
    let Some(n) = i.neighbours.get_mut(&addr) else { return };
    let spaced = n.last_request.map_or(now, |at| at.after(RETRANS)).max(now);
    let releasing = n.state.releasing();
    match &mut n.state {
        Nud::Incomplete(s) if s.requests < BROADCAST_SOLICIT => request(i, cx, addr),
        Nud::Incomplete(s) => {
            let pending = core::mem::take(&mut s.pending);
            n.state = Nud::Failed;
            fail(i, cx, addr, pending);
        }
        Nud::Reachable(s) => {
            let end = s.confirmed.after(reachable);
            if now < end {
                cx.timers.arm(timer(cx, addr), end);
            } else {
                n.state = Nud::Stale(Stale { mac: s.mac, lifetime: Lifetime::Runs, released: core::mem::take(&mut s.released) });
                cx.timers.arm(timer(cx, addr), now.after(IDLE_LIFETIME));
            }
        }
        Nud::Delay(s) => {
            n.state = Nud::Probe(Probing { mac: s.mac, requests: 0, queued: false, released: core::mem::take(&mut s.released) });
            probe(i, cx, addr, spaced);
        }
        Nud::Probe(s) if s.requests < UNICAST_SOLICIT => request(i, cx, addr),
        Nud::Probe(s) => {
            let released = core::mem::take(&mut s.released);
            n.state = Nud::Unreachable(Unreachable { mac: s.mac, requests: 0, solicit: Solicit::Quiescent(Lifetime::Runs), released });
            cx.log.count(Counter::NbUnreachable);
            cx.timers.arm(timer(cx, addr), now.after(IDLE_LIFETIME));
            route::refresh_active(i, cx);
        }
        Nud::Unreachable(Unreachable { solicit: Solicit::Backoff { sent: true }, .. }) => request(i, cx, addr),
        Nud::Unreachable(Unreachable { solicit: solicit @ Solicit::Backoff { sent: false }, .. }) => {
            *solicit = Solicit::Quiescent(Lifetime::Runs);
            cx.timers.arm(timer(cx, addr), now.after(IDLE_LIFETIME));
        }
        // A request is pending, so it is not idle: its next deadline starts as the request leaves.
        Nud::Unreachable(Unreachable { solicit: Solicit::Queued, .. }) => {}
        // Not idle while released datagrams are its: `leave` deletes it once they have left.
        Nud::Stale(Stale { lifetime, .. }) | Nud::Unreachable(Unreachable { solicit: Solicit::Quiescent(lifetime), .. }) if releasing => {
            *lifetime = Lifetime::Passed;
        }
        Nud::Stale(_) | Nud::Unreachable(_) | Nud::Failed => {
            remove(i, cx, addr);
            route::refresh_active(i, cx);
        }
    }
}

/// The turn of `addr`'s oldest released datagram came, and is spent: the datagram leaves now, to
/// the MAC of this moment.
pub(crate) fn leave(i: &mut Interface, cx: &mut Cx<'_>, addr: Ipv4Addr) -> Option<Held> {
    i.held = i.held.saturating_sub(1);
    let n = i.neighbours.get_mut(&addr)?;
    let mac = n.state.mac()?;
    let mut held = n.state.released_mut()?.pop()?;
    if let Some((destination, _)) = held.frame.split_first_chunk_mut::<6>() {
        *destination = mac.0;
    }
    let idle = matches!(
        n.state,
        Nud::Stale(Stale { lifetime: Lifetime::Passed, .. }) | Nud::Unreachable(Unreachable { solicit: Solicit::Quiescent(Lifetime::Passed), .. })
    );
    if idle && !n.state.releasing() {
        remove(i, cx, addr);
        route::refresh_active(i, cx);
    }
    Some(held)
}

/// PROBE's first unicast request, now or at the spacing boundary.
fn probe(i: &mut Interface, cx: &mut Cx<'_>, addr: Ipv4Addr, at: Instant) {
    if at <= cx.now {
        request(i, cx, addr);
    } else {
        cx.timers.arm(timer(cx, addr), at);
    }
}

/// A held datagram that will never leave: counted, and its sender told (§6.5, §9.6).
fn drop_held(cx: &mut Cx<'_>, held: Held, counter: Counter) {
    cx.log.count(counter);
    if let Some(flow) = held.flow {
        cx.log.event(Event::Unreachable(flow));
    }
}

/// INCOMPLETE gave up: every held datagram is dropped.
fn fail(i: &mut Interface, cx: &mut Cx<'_>, addr: Ipv4Addr, pending: Pending) {
    let now = cx.now;
    for held in pending.0 {
        i.held = i.held.saturating_sub(1);
        drop_held(cx, held, Counter::NbPendingDropped);
    }
    cx.log.count(Counter::NbFailed);
    cx.log.event(Event::Failed { iface: cx.iface, next_hop: addr });
    cx.timers.arm(timer(cx, addr), now.after(FAILED_HOLD));
    route::refresh_active(i, cx);
}

/// What a state entering at `mac` takes over: INCOMPLETE's datagrams, each given its turn in
/// arrival order ahead of any sent later (§6.5), or what its resolved predecessor still holds.
fn inherit(cx: &mut Cx<'_>, addr: Ipv4Addr, n: &mut Neighbour, mac: MacAddr) -> Released {
    let Nud::Incomplete(s) = &mut n.state else {
        if let Some(was) = n.state.mac().filter(|was| *was != mac) {
            mac_changed(cx, addr, was, mac);
        }
        return n.state.take_released();
    };
    let pending = core::mem::take(&mut s.pending).0;
    for _ in &pending {
        cx.control.hold(Item::Turn(Turn { iface: cx.iface, next_hop: addr }));
    }
    cx.log.count(Counter::NbResolved);
    cx.log.event(Event::Resolved { iface: cx.iface, next_hop: addr });
    Released(pending)
}

fn mac_changed(cx: &mut Cx<'_>, addr: Ipv4Addr, old: MacAddr, new: MacAddr) {
    cx.log.refuse(Counter::ArpMacChanged, cx.iface, Peer::MacChange { ip: addr, old, new });
}

/// Enters REACHABLE, confirmed now.
fn reach(i: &mut Interface, cx: &mut Cx<'_>, addr: Ipv4Addr, mac: MacAddr) {
    let now = cx.now;
    let reachable = i.reachable_time(cx.draws, now);
    let Some(n) = i.neighbours.get_mut(&addr) else { return };
    let released = inherit(cx, addr, n, mac);
    n.state = Nud::Reachable(Reachable { mac, confirmed: now, released });
    cx.timers.arm(timer(cx, addr), now.after(reachable));
    route::refresh_active(i, cx);
}

/// Enters STALE at `mac`.
fn stale(i: &mut Interface, cx: &mut Cx<'_>, addr: Ipv4Addr, mac: MacAddr) {
    let now = cx.now;
    let Some(n) = i.neighbours.get_mut(&addr) else { return };
    let released = inherit(cx, addr, n, mac);
    n.state = Nud::Stale(Stale { mac, lifetime: Lifetime::Runs, released });
    cx.timers.arm(timer(cx, addr), now.after(IDLE_LIFETIME));
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
    insert(i, addr, Nud::Stale(Stale { mac, lifetime: Lifetime::Runs, released: Released::default() }), now, None);
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
                let released = state.take_released();
                *state = Nud::Probe(Probing { mac, requests: 0, queued: false, released });
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
        let held = match n.state {
            Nud::Incomplete(s) => s.pending.0,
            mut state => state.take_released().0,
        };
        for held in held {
            drop_held(cx, held, Counter::NbPendingDropped);
        }
    }
    cx.control.purge(cx.iface);
    i.held = 0;
}
