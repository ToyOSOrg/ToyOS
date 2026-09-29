//! The IGMP host (§10): IGMPv3 (RFC 9776) for any-source joins, so a joined group's state is
//! always EXCLUDE {} (RFC 5790 §4), with RFC 9776 §7.2.1's IGMPv2 compatibility mode (RFC 2236)
//! and no IGMPv1 mode: a v1 query is refused and logged. 224.0.0.1 is always joined and never
//! reported. Records due at one moment leave in one report; a state change's retransmissions
//! are timed from the hand-off of the one before.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::net::Ipv4Addr;
use core::time::Duration;

use toyos_net_wire::ethernet::MacAddr;
use toyos_net_wire::igmp::{self as wire, Deciseconds, GroupRecord, IgmpMessage, Query, QueryGroup, QueryVersion, ReportGroup, V2Builder, V2Kind, V3ReportBuilder};
use toyos_net_wire::ipv4::{Ecn, Ipv4Option, Ipv4Packet, Ipv4Source, MulticastAddr, TrafficClass};
use toyos_net_wire::Instant;

use crate::counters::Counter;
use crate::draw::Purpose;
use crate::egress::Item;
use crate::iface::{Cx, Interface};
use crate::limits::igmp::{QUERY_INTERVAL, ROBUSTNESS, UNSOLICITED_REPORT, V2_UNSOLICITED_REPORT};
use crate::limits::{IGMP_GROUPS, IGMP_QUERY_SOURCES};
use crate::timers;
use crate::{Peer, MTU};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Timer {
    /// The response to a general query: the interface timer.
    General,
    /// A group-specific or group-and-source-specific response.
    Response(MulticastAddr),
    /// A state change's next transmission.
    Change(MulticastAddr),
    /// IGMPv2 mode: a group's report delay.
    V2Report(MulticastAddr),
    /// IGMPv2 mode ends.
    V2Querier,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IgmpMode {
    V3,
    V2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChangeKind {
    Join,
    Leave,
}

#[derive(Clone, Copy, Debug)]
struct Change {
    kind: ChangeKind,
    /// Transmissions still owed.
    left: u8,
    queued: bool,
    seq: u32,
}

#[derive(Debug, Default)]
struct Group {
    refs: u32,
    change: Option<Change>,
    /// A pending v3 response's recorded sources: empty for a group response (§10.5).
    response: Option<Vec<Ipv4Addr>>,
    /// IGMPv2 mode: ours was the last report on the link (RFC 2236 §3).
    last_reporter: bool,
}

impl Group {
    fn idle(&self) -> bool {
        self.refs == 0 && self.change.is_none()
    }
}

#[derive(Debug)]
pub(crate) struct Igmp {
    groups: BTreeMap<MulticastAddr, Group>,
    /// IGMPv2 compatibility mode lasts until then.
    v2_until: Option<Instant>,
    robustness: u8,
    seq: u32,
}

impl Default for Igmp {
    fn default() -> Self {
        Self { groups: BTreeMap::new(), v2_until: None, robustness: ROBUSTNESS, seq: 0 }
    }
}

impl Igmp {
    pub fn joined(&self, group: MulticastAddr) -> bool {
        group == MulticastAddr::ALL_HOSTS || self.groups.get(&group).is_some_and(|g| g.refs > 0)
    }

    pub fn mode(&self) -> IgmpMode {
        if self.v2_until.is_some() {
            IgmpMode::V2
        } else {
            IgmpMode::V3
        }
    }

    fn joined_groups(&self) -> impl Iterator<Item = MulticastAddr> + '_ {
        self.groups.iter().filter(|(_, g)| g.refs > 0).map(|(&group, _)| group)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    IsInclude,
    IsExclude,
    ToInclude,
    ToExclude,
}

#[derive(Clone, Debug)]
pub(crate) struct Record {
    group: MulticastAddr,
    kind: Kind,
    sources: Vec<Ipv4Addr>,
    /// A state change's transmission, accounted when it leaves.
    change: Option<u32>,
}

#[derive(Debug)]
pub(crate) enum Report {
    V3(Vec<Record>),
    /// A v2 report; a join's carries its change.
    V2(MulticastAddr, Option<u32>),
    Leave(MulticastAddr),
}

fn timer(cx: &Cx<'_>, t: Timer) -> timers::Timer {
    timers::Timer::Igmp(cx.iface, t)
}

/// RFC 9776 §4 and W-7: internetwork control, TTL 1, Router Alert.
fn traffic_class() -> TrafficClass {
    TrafficClass::new(48, Ecn::NotEct).unwrap_or(TrafficClass::ZERO)
}

fn deciseconds(d: Deciseconds) -> Duration {
    Duration::from_millis(u64::from(d.0).saturating_mul(100))
}

/// Room for records in one report: an MTU less the IPv4 header with its Router Alert.
const REPORT_ROOM: usize = MTU.saturating_sub(24).saturating_sub(wire::HEADER_LEN);

/// Queues `records` in as few reports as fit the MTU.
pub(crate) fn flush(i: &mut Interface, cx: &mut Cx<'_>, records: Vec<Record>) {
    let mut report = Vec::new();
    let mut room = REPORT_ROOM;
    for record in records {
        let size = record.sources.len().saturating_mul(4).saturating_add(8);
        if size > room && !report.is_empty() {
            let full = core::mem::take(&mut report);
            enqueue(i, cx, Report::V3(full));
            room = REPORT_ROOM;
        }
        room = room.saturating_sub(size);
        report.push(record);
    }
    if !report.is_empty() {
        enqueue(i, cx, Report::V3(report));
    }
}

fn enqueue(i: &mut Interface, cx: &mut Cx<'_>, report: Report) {
    let changes: Vec<(MulticastAddr, u32)> = match &report {
        Report::V3(records) => records.iter().filter_map(|r| r.change.map(|seq| (r.group, seq))).collect(),
        Report::V2(group, Some(seq)) => alloc::vec![(*group, *seq)],
        Report::V2(_, None) | Report::Leave(_) => Vec::new(),
    };
    if !cx.control.push(Item::Igmp { iface: cx.iface, report }, cx.log) {
        for (group, seq) in changes {
            changed(i, cx, group, seq);
        }
    }
}

/// A state change's transmission left, or was lost.
fn changed(i: &mut Interface, cx: &mut Cx<'_>, group: MulticastAddr, seq: u32) {
    let now = cx.now;
    let v2 = i.igmp.mode() == IgmpMode::V2;
    let Some(g) = i.igmp.groups.get_mut(&group) else { return };
    let Some(change) = g.change.as_mut().filter(|c| c.seq == seq && c.queued) else { return };
    change.queued = false;
    change.left = change.left.saturating_sub(1);
    if change.left > 0 {
        let interval = if v2 { V2_UNSOLICITED_REPORT } else { UNSOLICITED_REPORT };
        let at = now.after(cx.draws.after(Purpose::Igmp, interval));
        cx.timers.arm(timer(cx, Timer::Change(group)), at);
    } else {
        g.change = None;
        if g.idle() {
            i.igmp.groups.remove(&group);
        }
    }
}

/// A new state for `group`: RV transmissions of its record, the first at once, replacing any
/// change still owed (RFC 9776 §5.1).
fn change(i: &mut Interface, cx: &mut Cx<'_>, group: MulticastAddr, kind: ChangeKind, batch: &mut Vec<Record>) {
    let robustness = i.igmp.robustness;
    let mode = i.igmp.mode();
    i.igmp.seq = i.igmp.seq.wrapping_add(1);
    let seq = i.igmp.seq;
    cx.timers.cancel(timer(cx, Timer::Change(group)));
    let Some(g) = i.igmp.groups.get_mut(&group) else { return };
    match (mode, kind) {
        (IgmpMode::V3, _) => {
            g.change = Some(Change { kind, left: robustness, queued: true, seq });
            let kind = if kind == ChangeKind::Join { Kind::ToExclude } else { Kind::ToInclude };
            batch.push(Record { group, kind, sources: Vec::new(), change: Some(seq) });
        }
        (IgmpMode::V2, ChangeKind::Join) => {
            g.change = Some(Change { kind, left: 2, queued: true, seq });
            enqueue(i, cx, Report::V2(group, Some(seq)));
        }
        (IgmpMode::V2, ChangeKind::Leave) => {
            let last = g.last_reporter;
            g.change = None;
            if g.idle() {
                i.igmp.groups.remove(&group);
            }
            if last {
                enqueue(i, cx, Report::Leave(group));
            }
        }
    }
}

pub(crate) fn join(i: &mut Interface, cx: &mut Cx<'_>, group: MulticastAddr) -> Result<(), Counter> {
    if group == MulticastAddr::ALL_HOSTS {
        return Ok(());
    }
    let joined = i.igmp.joined_groups().count();
    let g = i.igmp.groups.entry(group).or_default();
    if g.refs > 0 {
        g.refs = g.refs.saturating_add(1);
        return Ok(());
    }
    if joined >= IGMP_GROUPS {
        if g.idle() {
            i.igmp.groups.remove(&group);
        }
        cx.log.count(Counter::IgmpTooManyGroups);
        return Err(Counter::IgmpTooManyGroups);
    }
    g.refs = 1;
    if i.up {
        let mut batch = Vec::new();
        change(i, cx, group, ChangeKind::Join, &mut batch);
        flush(i, cx, batch);
    }
    Ok(())
}

pub(crate) fn leave(i: &mut Interface, cx: &mut Cx<'_>, group: MulticastAddr) {
    let Some(g) = i.igmp.groups.get_mut(&group).filter(|g| g.refs > 0) else { return };
    g.refs = g.refs.saturating_sub(1);
    if g.refs > 0 {
        return;
    }
    g.response = None;
    cx.timers.cancel(timer(cx, Timer::Response(group)));
    cx.timers.cancel(timer(cx, Timer::V2Report(group)));
    if i.up {
        let mut batch = Vec::new();
        change(i, cx, group, ChangeKind::Leave, &mut batch);
        flush(i, cx, batch);
    } else {
        i.igmp.groups.remove(&group);
    }
}

/// Every joined group's join again, as new: after the link came up, or when the interface's first
/// usable address appeared (§10.3 (6), §10.8).
pub(crate) fn rejoin(i: &mut Interface, cx: &mut Cx<'_>) {
    let groups: Vec<MulticastAddr> = i.igmp.joined_groups().collect();
    let mut batch = Vec::new();
    for group in groups {
        change(i, cx, group, ChangeKind::Join, &mut batch);
    }
    flush(i, cx, batch);
}

/// The link went down: every timer stops and the mode returns to v3 (§10.8).
pub(crate) fn link_down(i: &mut Interface, cx: &mut Cx<'_>) {
    cx.timers.cancel_iface(cx.iface, |t| matches!(t, timers::Timer::Igmp(..)));
    i.igmp.v2_until = None;
    i.igmp.groups.retain(|_, g| g.refs > 0);
    for g in i.igmp.groups.values_mut() {
        g.change = None;
        g.response = None;
        g.last_reporter = false;
    }
}

fn router_alert(packet: &Ipv4Packet<'_>) -> bool {
    packet.options().iter().any(|o| matches!(o, Ipv4Option::RouterAlert(_)))
}

/// A received IGMP message the IP layer admitted (§10.4, §10.7).
pub(crate) fn input(i: &mut Interface, cx: &mut Cx<'_>, packet: &Ipv4Packet<'_>, message: IgmpMessage<'_>) {
    match message {
        IgmpMessage::Query(query) => self::query(i, cx, packet, &query),
        IgmpMessage::V2Report(group) | IgmpMessage::V1Report(group) => {
            let running = cx.timers.get(timer(cx, Timer::V2Report(group))).is_some();
            if i.igmp.mode() == IgmpMode::V2 && running {
                cx.timers.cancel(timer(cx, Timer::V2Report(group)));
                if let Some(g) = i.igmp.groups.get_mut(&group) {
                    g.last_reporter = false;
                }
            } else {
                cx.log.count(Counter::IgmpIgnored);
            }
        }
        IgmpMessage::Leave(_) => cx.log.count(Counter::IgmpIgnored),
    }
}

fn query(i: &mut Interface, cx: &mut Cx<'_>, packet: &Ipv4Packet<'_>, query: &Query<'_>) {
    let now = cx.now;
    let peer = Peer::Ip(packet.source());
    if query.version == QueryVersion::V1 {
        return cx.log.refuse(Counter::IgmpV1Query, cx.iface, peer);
    }
    if !router_alert(packet) {
        return cx.log.refuse(Counter::IgmpQueryNoRouterAlert, cx.iface, peer);
    }
    let destination = packet.destination();
    let to_us = i.is_usable(destination);
    match query.group {
        QueryGroup::General if !(destination == MulticastAddr::ALL_HOSTS.get() || to_us) => {
            return cx.log.refuse(Counter::IgmpGeneralQueryDestination, cx.iface, peer);
        }
        QueryGroup::Specific(group) if !(destination == group.get() || to_us) => {
            return cx.log.count(Counter::IgmpQueryDestination);
        }
        QueryGroup::Specific(group) if !i.igmp.joined(group) => return cx.log.count(Counter::IgmpQueryOtherGroup),
        QueryGroup::General | QueryGroup::Specific(_) => {}
    }
    let max = deciseconds(query.max_response);
    match (&query.version, query.group) {
        (QueryVersion::V2, QueryGroup::General) => enter_v2(i, cx, max),
        (QueryVersion::V3(v3), _) if v3.robustness != 0 => i.igmp.robustness = v3.robustness,
        _ => {}
    }
    if i.igmp.mode() == IgmpMode::V2 {
        let groups: Vec<MulticastAddr> = match query.group {
            QueryGroup::General => i.igmp.joined_groups().collect(),
            QueryGroup::Specific(group) => alloc::vec![group],
        };
        for group in groups {
            let at = now.after(cx.draws.after(Purpose::Igmp, max));
            let key = timer(cx, Timer::V2Report(group));
            if cx.timers.get(key).is_none_or(|running| running > at) {
                cx.timers.arm(key, at);
            }
        }
        return;
    }
    let at = now.after(cx.draws.after(Purpose::Igmp, max));
    if cx.timers.get(timer(cx, Timer::General)).is_some_and(|general| general < at) {
        return;
    }
    let group = match query.group {
        QueryGroup::General => return cx.timers.arm(timer(cx, Timer::General), at),
        QueryGroup::Specific(group) => group,
    };
    let sources: Vec<Ipv4Addr> = match &query.version {
        QueryVersion::V3(v3) => v3.sources().collect(),
        QueryVersion::V1 | QueryVersion::V2 => Vec::new(),
    };
    let key = timer(cx, Timer::Response(group));
    let Some(g) = i.igmp.groups.get_mut(&group) else { return };
    let recorded = match g.response.take() {
        None => sources,
        Some(list) if sources.is_empty() || list.is_empty() => Vec::new(),
        Some(mut list) => {
            list.extend(sources.into_iter().filter(|s| !list.contains(s)).collect::<Vec<_>>());
            list
        }
    };
    g.response = Some(if recorded.len() > IGMP_QUERY_SOURCES { Vec::new() } else { recorded });
    let at = cx.timers.get(key).map_or(at, |pending| pending.min(at));
    cx.timers.arm(key, at);
}

/// An IGMPv2 general query: v2 mode for the Older Version Querier Present Interval, 2 × 125 s
/// plus the query's Max Response Time (RFC 9776 §8.12; IP-D13), and every v3 response and
/// retransmission cancelled on entry (§7.2.1).
fn enter_v2(i: &mut Interface, cx: &mut Cx<'_>, max: Duration) {
    let until = cx.now.after(QUERY_INTERVAL.saturating_mul(2).saturating_add(max));
    if i.igmp.v2_until.is_none() {
        cx.log.count(Counter::IgmpModeV2);
        cx.timers.cancel_iface(cx.iface, |t| matches!(t, timers::Timer::Igmp(_, Timer::General | Timer::Response(_) | Timer::Change(_))));
        for g in i.igmp.groups.values_mut() {
            g.change = None;
            g.response = None;
        }
        i.igmp.groups.retain(|_, g| g.refs > 0);
    }
    i.igmp.v2_until = Some(until);
    cx.timers.arm(timer(cx, Timer::V2Querier), until);
}

/// A deadline of the IGMP host; v3 records join `batch`, to leave together.
pub(crate) fn fire(i: &mut Interface, cx: &mut Cx<'_>, t: Timer, batch: &mut Vec<Record>) {
    match t {
        Timer::General => {
            let groups: Vec<MulticastAddr> = i.igmp.joined_groups().collect();
            batch.extend(groups.into_iter().map(|group| Record { group, kind: Kind::IsExclude, sources: Vec::new(), change: None }));
        }
        Timer::Response(group) => {
            let joined = i.igmp.joined(group);
            let Some(sources) = i.igmp.groups.get_mut(&group).and_then(|g| g.response.take()) else { return };
            if joined {
                let kind = if sources.is_empty() { Kind::IsExclude } else { Kind::IsInclude };
                batch.push(Record { group, kind, sources, change: None });
            }
        }
        Timer::Change(group) => {
            let mode = i.igmp.mode();
            let Some(change) = i.igmp.groups.get_mut(&group).and_then(|g| g.change.as_mut()).filter(|c| !c.queued && c.left > 0) else { return };
            change.queued = true;
            let seq = change.seq;
            match (mode, change.kind) {
                (IgmpMode::V3, ChangeKind::Join) => batch.push(Record { group, kind: Kind::ToExclude, sources: Vec::new(), change: Some(seq) }),
                (IgmpMode::V3, ChangeKind::Leave) => batch.push(Record { group, kind: Kind::ToInclude, sources: Vec::new(), change: Some(seq) }),
                (IgmpMode::V2, _) => enqueue(i, cx, Report::V2(group, Some(seq))),
            }
        }
        Timer::V2Report(group) => {
            if i.igmp.joined(group) {
                enqueue(i, cx, Report::V2(group, None));
            }
        }
        Timer::V2Querier => {
            i.igmp.v2_until = None;
            cx.timers.cancel_iface(cx.iface, |t| matches!(t, timers::Timer::Igmp(_, Timer::V2Report(_))));
            cx.log.count(Counter::IgmpModeV3);
        }
    }
}

/// Builds `report` as it leaves, from the interface's source of this moment (0.0.0.0 while it
/// has none, RFC 9776 §4.2.14); a change record overtaken by a newer change is left out.
pub(crate) fn emit(i: &mut Interface, cx: &mut Cx<'_>, report: &Report, out: &mut [u8]) -> Option<usize> {
    let source = Ipv4Source::new(i.usable().next().map_or(Ipv4Addr::UNSPECIFIED, |a| a.cidr.addr)).ok()?;
    let mac = i.mac;
    let current = |i: &Interface, group: MulticastAddr, seq: u32| {
        i.igmp.groups.get(&group).and_then(|g| g.change).is_some_and(|c| c.seq == seq && c.queued)
    };
    let len = match report {
        Report::V3(records) => {
            let live: Vec<&Record> = records.iter().filter(|r| r.change.is_none_or(|seq| current(i, r.group, seq))).collect();
            let groups: Vec<GroupRecord<'_>> = live
                .iter()
                .filter_map(|r| {
                    let group = ReportGroup::new(r.group).ok()?;
                    Some(match r.kind {
                        Kind::IsInclude => GroupRecord::IsInclude(group, &r.sources),
                        Kind::IsExclude => GroupRecord::IsExclude(group),
                        Kind::ToInclude => GroupRecord::ToInclude(group),
                        Kind::ToExclude => GroupRecord::ToExclude(group),
                    })
                })
                .collect();
            let len = if groups.is_empty() {
                None
            } else {
                let datagram = wire::datagram(source, traffic_class(), V3ReportBuilder { records: &groups });
                let destination = MacAddr::multicast(MulticastAddr::IGMPV3_ROUTERS);
                toyos_net_wire::ethernet::FrameBuilder { destination, source: mac }.emit(&datagram, out).ok().map(<[u8]>::len)
            };
            for (group, seq) in live.iter().filter_map(|r| r.change.map(|seq| (r.group, seq))).collect::<Vec<_>>() {
                changed(i, cx, group, seq);
            }
            len
        }
        Report::V2(group, seq) => {
            if seq.is_some_and(|seq| !current(i, *group, seq)) {
                return None;
            }
            let message = V2Builder { kind: V2Kind::Report, group: ReportGroup::new(*group).ok()? };
            let datagram = wire::datagram(source, traffic_class(), message);
            let len = toyos_net_wire::ethernet::FrameBuilder { destination: MacAddr::multicast(*group), source: mac }.emit(&datagram, out).ok().map(<[u8]>::len);
            if let Some(g) = i.igmp.groups.get_mut(group) {
                g.last_reporter = true;
            }
            if let Some(seq) = seq {
                changed(i, cx, *group, *seq);
            }
            len
        }
        Report::Leave(group) => {
            let message = V2Builder { kind: V2Kind::Leave, group: ReportGroup::new(*group).ok()? };
            let datagram = wire::datagram(source, traffic_class(), message);
            let destination = MacAddr::multicast(MulticastAddr::ALL_ROUTERS);
            toyos_net_wire::ethernet::FrameBuilder { destination, source: mac }.emit(&datagram, out).ok().map(<[u8]>::len)
        }
    };
    if len.is_some() {
        cx.log.count(Counter::IgmpReportsSent);
    }
    len
}

impl Igmp {
    /// Whether a frame to `mac` is for a group joined here (RFC 1112 §6.4).
    pub fn accepts(&self, mac: MacAddr) -> bool {
        self.joined_groups().any(|group| MacAddr::multicast(group) == mac)
    }
}
