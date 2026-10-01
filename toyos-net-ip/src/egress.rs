//! Egress (§5). [ip]'s own frames wait in one FIFO the transmit opportunity drains ahead of the
//! data flows: an ARP or IGMP frame is built when it leaves, from the state of that moment, and
//! the timer it feeds starts then. The FIFO holds at most CONTROL_QUEUE frames, at most
//! ECHO_REPLIES of them echo replies; one of [ip]'s own frames past either is dropped and
//! counted, and its producer moves on as if it had left. Three kinds are never dropped: a
//! released datagram's turn, an ACD probe and an ACD announcement. One that finds the FIFO full
//! waits behind it and enters as room appears, ahead of any frame queued later (§5.4, RFC 5227
//! §2.1). A turn holds no datagram: it names the entry whose queue does, and goes with that entry.
//!
//! Every datagram is atomic: DF set, identification 0 (`toyos-net-wire`'s one IPv4 form).

use alloc::collections::VecDeque;
use alloc::vec;
use alloc::vec::Vec;
use core::net::Ipv4Addr;

use toyos_net_wire::arp::{Arp, Operation};
use toyos_net_wire::ethernet::{FrameBuilder, FrameBody, IndividualMac, MacAddr};
use toyos_net_wire::ipv4::{Ipv4Builder, Ipv4Source, Payload, TrafficClass, Ttl};
use toyos_net_wire::udp::UdpBuilder;
use toyos_net_wire::Instant;

use crate::counters::{Counter, Log};
use crate::iface::Cx;
use crate::limits::{nud::PENDING_TOTAL, CONTROL_QUEUE, ECHO_REPLIES};
use crate::nud::{self, Held, Link, Nud};
use crate::route::{NextHop, Route, Source};
use crate::{acd, igmp, Event, Flow, IfIndex, Ip, MTU};

/// The largest frame [ip] builds.
pub const FRAME: usize = toyos_net_wire::ethernet::HEADER_LEN + MTU;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FrameKind {
    Echo,
    Error,
    Datagram,
}

#[derive(Debug)]
pub(crate) enum Item {
    Request { iface: IfIndex, target: Ipv4Addr },
    Reply { iface: IfIndex, to: MacAddr, target: Ipv4Addr, ours: Ipv4Addr },
    Probe { iface: IfIndex, addr: Ipv4Addr },
    Announce { iface: IfIndex, addr: Ipv4Addr, owed: bool },
    Igmp { iface: IfIndex, report: igmp::Report },
    Frame { iface: IfIndex, frame: Vec<u8>, kind: FrameKind },
    Turn(Turn),
}

impl Item {
    fn iface(&self) -> IfIndex {
        match self {
            Self::Request { iface, .. }
            | Self::Reply { iface, .. }
            | Self::Probe { iface, .. }
            | Self::Announce { iface, .. }
            | Self::Igmp { iface, .. }
            | Self::Frame { iface, .. }
            | Self::Turn(Turn { iface, .. }) => *iface,
        }
    }
}

/// A released datagram's place in the FIFO: the oldest of `next_hop`'s released datagrams leaves
/// when it comes (§6.5).
#[derive(Debug)]
pub(crate) struct Turn {
    pub iface: IfIndex,
    pub next_hop: Ipv4Addr,
}

#[derive(Debug, Default)]
pub(crate) struct Control {
    items: VecDeque<Item>,
    echoes: usize,
    /// What may not be dropped and found `items` full, in order: empty whenever it has room.
    waiting: VecDeque<Item>,
}

impl Control {
    fn full(&self) -> bool {
        self.items.len() >= CONTROL_QUEUE
    }

    /// Queues one of [ip]'s own frames; `false` when it was dropped and counted.
    pub fn push(&mut self, item: Item, log: &mut Log) -> bool {
        let echo = matches!(item, Item::Frame { kind: FrameKind::Echo, .. });
        if echo && self.echoes >= ECHO_REPLIES {
            log.count(Counter::IcmpEchoReplyDropped);
            return false;
        }
        if self.full() {
            log.count(Counter::IpControlQueueFull);
            return false;
        }
        if echo {
            self.echoes = self.echoes.saturating_add(1);
        }
        self.items.push_back(item);
        true
    }

    /// Queues a turn, a probe or an announcement, which waits for room rather than being dropped.
    pub fn hold(&mut self, item: Item) {
        if self.full() {
            self.waiting.push_back(item);
        } else {
            self.items.push_back(item);
        }
    }

    fn pop(&mut self) -> Option<Item> {
        let item = self.items.pop_front()?;
        self.forget(&item);
        self.refill();
        Some(item)
    }

    fn forget(&mut self, item: &Item) {
        if matches!(item, Item::Frame { kind: FrameKind::Echo, .. }) {
            self.echoes = self.echoes.saturating_sub(1);
        }
    }

    /// Room goes to the oldest item waiting for it.
    fn refill(&mut self) {
        while !self.full() {
            let Some(item) = self.waiting.pop_front() else { return };
            self.items.push_back(item);
        }
    }

    /// Drops the turns and requests of `next_hop`'s entry on `iface`, which is gone; returns how
    /// many turns.
    pub fn purge_entry(&mut self, iface: IfIndex, next_hop: Ipv4Addr) -> usize {
        let mut turns = 0usize;
        let mut keep = |item: &Item| match item {
            Item::Turn(t) if t.iface == iface && t.next_hop == next_hop => {
                turns = turns.saturating_add(1);
                false
            }
            Item::Request { iface: on, target } => *on != iface || *target != next_hop,
            _ => true,
        };
        self.items.retain(&mut keep);
        self.waiting.retain(&mut keep);
        self.refill();
        turns
    }

    /// Drops everything waiting for `iface`, whose link went down.
    pub fn purge(&mut self, iface: IfIndex) {
        let (gone, kept): (Vec<Item>, Vec<Item>) = self.items.drain(..).partition(|item| item.iface() == iface);
        self.items = kept.into();
        for item in &gone {
            self.forget(item);
        }
        self.waiting.retain(|item| item.iface() != iface);
        self.refill();
    }
}

/// A UDP datagram a transport hands [ip] at a transmit opportunity.
#[derive(Clone, Copy, Debug)]
pub struct UdpOut<'a> {
    /// The socket's address, or 0.0.0.0 for the acquisition socket's broadcasts.
    pub source: Ipv4Addr,
    pub destination: Ipv4Addr,
    pub ttl: Ttl,
    pub datagram: UdpBuilder<'a>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sent {
    /// This many bytes of the caller's buffer are the frame: one credit spent.
    Frame(usize),
    /// Held for its next hop; it leaves from [`Ip::transmit`] once resolved.
    Held,
}

fn build<B: FrameBody>(source: IndividualMac, destination: MacAddr, body: &B, out: &mut [u8]) -> Option<usize> {
    FrameBuilder { destination, source }.emit(body, out).ok().map(<[u8]>::len)
}

fn build_vec<B: FrameBody>(source: IndividualMac, destination: MacAddr, body: &B) -> Option<Vec<u8>> {
    let mut frame = vec![0; FRAME];
    let len = build(source, destination, body, &mut frame)?;
    frame.truncate(len);
    Some(frame)
}

impl Ip {
    /// A transmit opportunity with room for `credit` frames: [ip]'s own frames and the datagrams
    /// resolution released, in the order they were queued, each handed to `sink` with its
    /// interface. Returns how many left.
    pub fn transmit(&mut self, now: Instant, credit: usize, mut sink: impl FnMut(IfIndex, &[u8])) -> usize {
        let now = self.clock(now);
        let mut sent = 0usize;
        while sent < credit {
            let Some(item) = self.control.pop() else { break };
            if self.emit(now, item, &mut sink) {
                sent = sent.saturating_add(1);
            }
        }
        sent
    }

    /// Builds one item into a frame and hands it over; `false` when the state it was queued for
    /// is gone and nothing left.
    fn emit(&mut self, now: Instant, item: Item, sink: &mut impl FnMut(IfIndex, &[u8])) -> bool {
        let iface = item.iface();
        let Some((i, mut cx)) = self.split(now, iface) else { return false };
        let mac = i.mac;
        let mut arp = |arp: &Arp, to: MacAddr| -> bool {
            let mut frame = [0u8; 60];
            match build(mac, to, arp, &mut frame).and_then(|len| frame.get(..len)) {
                Some(frame) => {
                    sink(iface, frame);
                    true
                }
                None => false,
            }
        };
        match item {
            Item::Request { target, .. } => {
                let hint = i.neighbours.get(&target).and_then(|n| n.hint);
                let Some(to) = nud::request_leaves(i, &mut cx, target) else { return false };
                let sender = hint.filter(|h| i.is_usable(*h)).or_else(|| i.source_for(target));
                let left = sender.is_some_and(|s| arp(&Arp::request(mac, s, target), to));
                if left {
                    cx.log.count(Counter::ArpRequestsSent);
                }
                left
            }
            Item::Reply { to, target, ours, .. } => {
                if !i.is_usable(ours) {
                    return false;
                }
                let reply = Arp { operation: Operation::Reply, sender_mac: mac.get(), sender_ip: ours, target_mac: to, target_ip: target };
                let left = arp(&reply, to);
                if left {
                    cx.log.count(Counter::ArpRepliesSent);
                }
                left
            }
            Item::Probe { addr, .. } => {
                let queued = i.addresses.iter().any(|a| a.cidr.addr() == addr && matches!(a.phase, crate::addr::Phase::Tentative { queued: true, .. }));
                if !queued {
                    return false;
                }
                acd::probed(i, &mut cx, addr);
                arp(&Arp::probe(mac, addr), MacAddr::BROADCAST)
            }
            Item::Announce { addr, owed, .. } => {
                if !i.is_usable(addr) {
                    return false;
                }
                acd::announced(i, &mut cx, addr, owed);
                arp(&Arp::announcement(mac, addr), MacAddr::BROADCAST)
            }
            Item::Igmp { report, .. } => {
                let mut frame = [0u8; FRAME];
                match igmp::emit(i, &mut cx, &report, &mut frame).and_then(|len| frame.get(..len)) {
                    Some(frame) => {
                        sink(iface, frame);
                        true
                    }
                    None => false,
                }
            }
            Item::Frame { frame, kind, .. } => {
                sink(iface, &frame);
                sent(&mut cx, kind);
                true
            }
            Item::Turn(Turn { next_hop, .. }) => {
                let Some(held) = nud::leave(i, &mut cx, next_hop) else { return false };
                sink(iface, &held.frame);
                sent(&mut cx, held.kind);
                true
            }
        }
    }

    /// A UDP datagram at a transmit opportunity (§5): written into `frame` when its link
    /// destination is known, or held here for its next hop, which spends no credit. A refusal is
    /// counted and the datagram's flow is told it is unreachable.
    pub fn send_udp(&mut self, now: Instant, out: &UdpOut<'_>, frame: &mut [u8; FRAME]) -> Result<Sent, Counter> {
        let now = self.clock(now);
        let flow = Flow {
            source: out.source,
            source_port: out.datagram.source,
            destination: out.destination,
            destination_port: out.datagram.destination,
        };
        let source = if out.source.is_unspecified() { Source::Unspecified } else { Source::Bound(out.source) };
        let sent = self.route(out.destination, source, None).and_then(|route| {
            let builder = Ipv4Builder {
                source: Ipv4Source::new(route.source).map_err(|_| Counter::RouteNoSourceAddress)?,
                destination: out.destination,
                ttl: out.ttl,
                traffic_class: TrafficClass::ZERO,
                options: &[],
                payload: out.datagram,
            };
            self.datagram(now, &route, &builder, FrameKind::Datagram, Some(flow), Some(frame))
        });
        if sent.is_err() {
            self.log.event(Event::Unreachable(flow));
        }
        sent
    }

    /// Sends one of [ip]'s own ICMP messages along `route`, into the control queue or held for
    /// its next hop.
    pub(crate) fn own<P: Payload>(&mut self, now: Instant, route: &Route, builder: &Ipv4Builder<'_, P>, kind: FrameKind) {
        let _ = self.datagram(now, route, builder, kind, None, None);
    }

    fn datagram<P: Payload>(
        &mut self,
        now: Instant,
        route: &Route,
        builder: &Ipv4Builder<'_, P>,
        kind: FrameKind,
        flow: Option<Flow>,
        out: Option<&mut [u8; FRAME]>,
    ) -> Result<Sent, Counter> {
        if builder.length().map_or(true, |len| len > MTU) {
            self.log.count(Counter::IpExceedsMtu);
            return Err(Counter::IpExceedsMtu);
        }
        let Some((i, mut cx)) = self.split(now, route.iface) else { return Err(Counter::UnknownInterface) };
        let (destination, hold) = match route.next_hop {
            NextHop::Broadcast => (MacAddr::BROADCAST, None),
            NextHop::Multicast(group) => (MacAddr::multicast(group), None),
            NextHop::Neighbour(addr) => {
                let holds = matches!(i.neighbours.get(&addr).map(|n| &n.state), None | Some(Nud::Incomplete(_)));
                if holds && i.held >= PENDING_TOTAL {
                    cx.log.count(Counter::NbPendingFull);
                    return Err(Counter::NbPendingFull);
                }
                match nud::send(i, &mut cx, addr, Some(route.source)) {
                    Link::Resolved(mac) => (mac, None),
                    Link::Pending => (MacAddr::ZERO, Some(addr)),
                    Link::Failed(refusal) => return Err(refusal),
                }
            }
        };
        let mac = i.mac;
        if let Some(addr) = hold {
            let frame = build_vec(mac, destination, builder).ok_or(Counter::IpExceedsMtu)?;
            nud::hold(i, &mut cx, addr, Held { frame, kind, flow });
            return Ok(Sent::Held);
        }
        match out {
            Some(out) => build(mac, destination, builder, out).map(Sent::Frame).ok_or(Counter::IpExceedsMtu),
            None => {
                let frame = build_vec(mac, destination, builder).ok_or(Counter::IpExceedsMtu)?;
                if cx.control.push(Item::Frame { iface: route.iface, frame, kind }, cx.log) {
                    Ok(Sent::Held)
                } else {
                    Err(Counter::IpControlQueueFull)
                }
            }
        }
    }
}

fn sent(cx: &mut Cx<'_>, kind: FrameKind) {
    match kind {
        FrameKind::Echo => cx.log.count(Counter::IcmpEchoRepliesSent),
        FrameKind::Error => cx.log.count(Counter::IcmpErrorsSent),
        FrameKind::Datagram => {}
    }
}

/// Queues an ARP reply to `request`'s sender for `ours`, unicast to it (§6.4).
pub(crate) fn reply(cx: &mut Cx<'_>, request: &Arp, ours: Ipv4Addr) {
    cx.control.push(Item::Reply { iface: cx.iface, to: request.sender_mac, target: request.sender_ip, ours }, cx.log);
}
