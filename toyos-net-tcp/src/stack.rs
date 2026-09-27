//! One shard's TCP: the connection table and its demultiplexing, listeners and their queues,
//! TIME-WAIT, port choice, the timers, and the transmit opportunity.
//!
//! Connections live in a slab named by index and generation, so an id from a freed slot names
//! nothing. The demux is an ordered map, which no chosen set of 4-tuples can degrade.

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::vec::Vec;
use core::net::Ipv4Addr;
use core::time::Duration;

use toyos_net_wire::icmp::UnreachableCode;
use toyos_net_wire::tcp::{Control, EstablishedOptions, RawWindow, TcpBuilder, TcpSegment, Timestamps};
use toyos_net_wire::Port;

use crate::conn::{Ctx, In, Kind, Out, Rst, Sync, Tick, Ts, Verdict};
use crate::counters::{Counter, Counters, Refusal};
use crate::open::{negotiate, Local, Origin, Rcvd, Sent, SynRcvd, SynSent};
use crate::rx::Rx;
use crate::seq::{Seq, Stamp};
use crate::{isn, limits, reuse_iss, siphash, ts_offset, Config, Endpoint, Error, Event, Failure, IcmpError, IcmpKind, Instant, Options, Received, SoftError, State, Status, Tuple};

const EPHEMERAL_FIRST: u16 = 49_152;
const EPHEMERAL_COUNT: u16 = 16_384;
/// Answers to segments for no socket, waiting for credit.
const ANSWERS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ConnId {
    index: u32,
    generation: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ListenerId {
    index: u32,
    generation: u32,
}

/// A segment at the moment it leaves: headers from the state of now, payload borrowed from the
/// send buffer.
#[derive(Clone, Copy, Debug)]
pub struct Outgoing<'a> {
    pub source: Ipv4Addr,
    pub destination: Ipv4Addr,
    pub segment: TcpBuilder<'a>,
}

/// A connection's variables, for inspect and tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Info {
    pub state: State,
    pub snd_una: Seq,
    pub snd_nxt: Seq,
    pub snd_wnd: u32,
    pub snd_wl1: Seq,
    pub max_snd_wnd: u32,
    pub rcv_nxt: Seq,
    pub rcv_edge: Seq,
    pub srtt: Option<Duration>,
    pub rttvar: Option<Duration>,
    pub rto: Duration,
    pub cwnd: u32,
    pub ssthresh: u32,
    pub smss: u32,
    pub snd_shift: u8,
    pub rcv_shift: u8,
    pub sack: bool,
    pub ts_recent: Option<u32>,
    pub sacked_ranges: usize,
    pub in_recovery: bool,
    pub ooo_ranges: usize,
    pub queued: usize,
    pub unread: usize,
    pub rtx_timer: Option<Instant>,
    pub deadline: Option<Instant>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum User {
    Held,
    Orphan,
    /// A listener's child that `accept` has not returned.
    Child,
}

struct Ended {
    failure: Option<Failure>,
    /// After both FINs: what the user has still to read.
    rx: Option<Box<Rx>>,
}

enum Tcb {
    SynSent(SynSent),
    SynRcvd(Box<SynRcvd>),
    Sync(Box<Sync>),
    Ended(Ended),
}

struct Conn {
    tuple: Tuple,
    options: Options,
    user: User,
    soft: Option<SoftError>,
    local: Local,
    state: Tcb,
    queued: bool,
    deadline: Option<Instant>,
}

struct Listener {
    addr: Ipv4Addr,
    port: Port,
    options: Options,
    pending: Vec<u32>,
    ready: VecDeque<u32>,
}

struct Slot<T> {
    generation: u32,
    value: Option<T>,
}

/// What TIME-WAIT keeps: enough to answer a retransmitted FIN and to judge a reusing SYN.
#[derive(Clone, Copy, Debug)]
pub struct TimeWait {
    snd_nxt: Seq,
    rcv_nxt: Seq,
    ts: Option<Ts>,
    window: u32,
    rcv_shift: u8,
    end: Instant,
    ack_owed: bool,
    last_unsolicited: Option<Instant>,
}

enum Entry {
    Conn(u32),
    TimeWait(TimeWait),
    /// An RST owed under pull egress; segments for the 4-tuple are dropped until it leaves.
    Stub(Rst),
}

struct Answer {
    tuple: Tuple,
    rst: Rst,
}

pub struct Tcp {
    config: Config,
    shift: u8,
    conns: Vec<Slot<Conn>>,
    free_conns: Vec<u32>,
    listeners: Vec<Slot<Listener>>,
    free_listeners: Vec<u32>,
    bound: BTreeMap<(u16, Ipv4Addr), u32>,
    demux: BTreeMap<Tuple, Entry>,
    time_waits: BTreeSet<(Instant, Tuple)>,
    deadlines: BTreeSet<(Instant, u32)>,
    active: VecDeque<u32>,
    stubs: VecDeque<Tuple>,
    answers: VecDeque<Answer>,
    tw_owed: VecDeque<Tuple>,
    port_table: [u16; 16],
    counters: Counters,
    events: Vec<Event>,
    scratch: Vec<u8>,
}

fn unicast(addr: Ipv4Addr) -> bool {
    !(addr.is_unspecified() || addr.is_broadcast() || addr.is_multicast() || addr.octets()[0] >= 240)
}

fn slot<T>(slots: &mut [Slot<T>], index: u32, generation: u32) -> Option<&mut T> {
    slots.get_mut(usize::try_from(index).ok()?).filter(|s| s.generation == generation).and_then(|s| s.value.as_mut())
}

fn value<T>(slots: &mut [Slot<T>], index: u32) -> Option<&mut T> {
    slots.get_mut(usize::try_from(index).ok()?).and_then(|s| s.value.as_mut())
}

fn insert<T>(slots: &mut Vec<Slot<T>>, free: &mut Vec<u32>, value: T) -> (u32, u32) {
    if let Some(index) = free.pop() {
        if let Some(slot) = slots.get_mut(usize::try_from(index).unwrap_or(usize::MAX)) {
            slot.value = Some(value);
            return (index, slot.generation);
        }
    }
    let index = u32::try_from(slots.len()).unwrap_or(u32::MAX);
    slots.push(Slot { generation: 0, value: Some(value) });
    (index, 0)
}

fn release<T>(slots: &mut [Slot<T>], free: &mut Vec<u32>, index: u32) -> Option<T> {
    let slot = slots.get_mut(usize::try_from(index).ok()?)?;
    let value = slot.value.take()?;
    slot.generation = slot.generation.wrapping_add(1);
    free.push(index);
    Some(value)
}

fn failure_of(soft: Option<SoftError>) -> Failure {
    soft.map_or(Failure::TimedOut, Failure::Unreachable)
}

impl TimeWait {
    fn from_sync(sync: &Sync, now: Instant) -> Self {
        Self {
            snd_nxt: sync.tx.nxt,
            rcv_nxt: sync.rx.next,
            ts: sync.ts,
            window: sync.rx.last_window,
            rcv_shift: sync.rx.shift,
            end: now.after(limits::TIME_WAIT),
            ack_owed: sync.rx.owes_ack() || sync.rx.dup_owed > 0 || sync.rx.delayed.is_some(),
            last_unsolicited: None,
        }
    }

    /// RFC 6191 §2 and RFC 9293 §3.6.1: whether a SYN may reopen this 4-tuple early.
    fn reopens(&self, seg: &In<'_>) -> bool {
        let later = seg.seq.at_or_after(self.rcv_nxt);
        match (self.ts, seg.options.timestamps()) {
            (Some(ts), Some(t)) => Stamp(ts.recent).before(Stamp(t.value)) || (t.value == ts.recent && later),
            (Some(_), None) => later,
            (None, Some(_)) => true,
            (None, None) => later,
        }
    }

    fn unsolicited(&mut self, ctx: &mut Ctx<'_>) {
        if self.last_unsolicited.is_some_and(|at| ctx.now.since(at) < limits::UNSOLICITED_ACK) {
            ctx.count(Counter::UnsolicitedAckLimited);
            return;
        }
        self.last_unsolicited = Some(ctx.now);
        self.ack_owed = true;
    }

    /// Everything but a reopening SYN. RSTs are ignored (RFC 1337 fix F1).
    fn receive(&mut self, seg: &In<'_>, ctx: &mut Ctx<'_>) {
        if let Some(ts) = self.ts.filter(|_| !seg.rst()) {
            let Some(t) = seg.options.timestamps() else {
                ctx.refuse(Counter::TsMissing);
                return;
            };
            if Stamp(t.value).before(Stamp(ts.recent)) {
                ctx.count(Counter::PawsReject);
                self.unsolicited(ctx);
                return;
            }
        }
        let len = seg.len();
        let offset = seg.seq.since(self.rcv_nxt);
        let acceptable = match (len, self.window) {
            (0, 0) => offset == 0,
            (0, w) => offset < w,
            (_, 0) => false,
            (_, w) => offset < w || seg.seq.add(len.saturating_sub(1)).since(self.rcv_nxt) < w,
        };
        if !acceptable {
            if seg.rst() {
                return;
            }
            if len > 0 && seg.seq.add(len).at_or_before(self.rcv_nxt) {
                if seg.fin() && seg.seq.add(len) == self.rcv_nxt {
                    self.end = ctx.now.after(limits::TIME_WAIT);
                }
                self.ack_owed = true;
            } else {
                self.unsolicited(ctx);
            }
            return;
        }
        if seg.rst() {
            if seg.seq == self.rcv_nxt {
                ctx.refuse(Counter::TimeWaitRstIgnored);
            } else {
                ctx.refuse(Counter::RstChallenged);
                self.unsolicited(ctx);
            }
            return;
        }
        if seg.syn() {
            ctx.refuse(Counter::SynChallenged);
            self.unsolicited(ctx);
        }
    }

    fn ack(&self, now: Instant) -> Out {
        let window = u16::try_from((self.window >> self.rcv_shift).min(u32::from(u16::MAX))).unwrap_or(u16::MAX);
        let ts = self.ts.map(|ts| Timestamps { value: ts.clock(now), echo: ts.recent });
        Out {
            seq: self.snd_nxt,
            kind: Kind::Ack { ack: self.rcv_nxt, push: false, fin: false },
            window,
            ts,
            sack: crate::conn::NO_BLOCKS,
            data: (0, 0),
        }
    }
}

fn builder<'a>(out: &'a Out, tuple: &Tuple, data: &'a [u8]) -> Outgoing<'a> {
    let sack = out.sack.0.get(..out.sack.1).unwrap_or_default();
    let control = match out.kind {
        Kind::Syn(options) => Control::Syn(options),
        Kind::SynAck(ack, options) => Control::SynAck { acknowledgment: ack.into(), options },
        Kind::Ack { ack, push, fin } => Control::Ack { acknowledgment: ack.into(), push, fin, options: EstablishedOptions { timestamps: out.ts, sack } },
        Kind::Rst(ack) => Control::Rst { acknowledgment: ack.map(Into::into), options: EstablishedOptions { timestamps: out.ts, sack: &[] } },
    };
    Outgoing {
        source: tuple.local.addr,
        destination: tuple.remote.addr,
        segment: TcpBuilder {
            source: tuple.local.port,
            destination: tuple.remote.port,
            sequence: out.seq.into(),
            control,
            window: RawWindow(out.window),
            data,
        },
    }
}

impl Conn {
    fn ctx<'a>(&self, now: Instant, counters: &'a mut Counters, events: &'a mut Vec<Event>) -> Ctx<'a> {
        Ctx { now, tuple: self.tuple, options: self.options, orphan: self.user == User::Orphan, counters, events }
    }

    fn deadline(&self, ctx: &Ctx<'_>) -> Option<Instant> {
        match &self.state {
            Tcb::SynSent(s) => s.deadline(),
            Tcb::SynRcvd(s) => s.deadline(),
            Tcb::Sync(s) => s.deadline(ctx),
            Tcb::Ended(_) => None,
        }
    }
}

impl Tcp {
    pub fn new(config: Config) -> Self {
        let buffer = config.receive_buffer;
        let shift = (0u8..14).find(|&r| buffer.checked_shr(u32::from(r)).is_some_and(|w| w <= u32::from(u16::MAX))).unwrap_or(14);
        let port_table = config.secrets.port_table;
        Self {
            config,
            shift,
            conns: Vec::new(),
            free_conns: Vec::new(),
            listeners: Vec::new(),
            free_listeners: Vec::new(),
            bound: BTreeMap::new(),
            demux: BTreeMap::new(),
            time_waits: BTreeSet::new(),
            deadlines: BTreeSet::new(),
            active: VecDeque::new(),
            stubs: VecDeque::new(),
            answers: VecDeque::new(),
            tw_owed: VecDeque::new(),
            port_table,
            counters: Counters::default(),
            events: Vec::new(),
            scratch: Vec::new(),
        }
    }

    pub fn counters(&self) -> &Counters {
        &self.counters
    }

    /// Refusals and reachability advice since the last call.
    pub fn drain_events(&mut self) -> impl Iterator<Item = Event> + '_ {
        self.events.drain(..)
    }

    pub fn time_wait_count(&self) -> usize {
        self.time_waits.len()
    }

    fn local(&self, tuple: &Tuple) -> Local {
        let mtu = self.config.mtu;
        Local {
            mss: mtu.saturating_sub(40),
            shift: self.shift,
            window: u16::try_from(self.config.receive_buffer.min(u32::from(u16::MAX))).unwrap_or(u16::MAX),
            receive_buffer: usize::try_from(self.config.receive_buffer).unwrap_or(0),
            send_buffer: usize::try_from(self.config.send_buffer).unwrap_or(0),
            mtu,
            ts_offset: ts_offset(&self.config.secrets.timestamp, tuple),
        }
    }

    fn refuse(&mut self, rule: Counter, tuple: &Tuple) {
        self.counters.add(rule, 1);
        if rule.logged() {
            self.events.push(Event::Refused(Refusal { rule, local: tuple.local, remote: tuple.remote }));
        }
    }

    // ---- slab bookkeeping ----

    /// The connection a user holds: after `close` or `abort` the id names nothing.
    fn conn(&mut self, id: ConnId) -> Result<&mut Conn, Error> {
        slot(&mut self.conns, id.index, id.generation).filter(|c| c.user == User::Held).ok_or(Error::NoSuchSocket)
    }

    /// Re-files a connection's deadline and offers it the next transmit opportunity.
    fn settle(&mut self, index: u32, now: Instant) {
        let Some(conn) = value(&mut self.conns, index) else { return };
        let ctx = conn.ctx(now, &mut self.counters, &mut self.events);
        let deadline = conn.deadline(&ctx);
        if let Some(old) = core::mem::replace(&mut conn.deadline, deadline) {
            self.deadlines.remove(&(old, index));
        }
        if let Some(at) = deadline {
            self.deadlines.insert((at, index));
        }
        if !core::mem::replace(&mut conn.queued, true) {
            self.active.push_back(index);
        }
    }

    fn free(&mut self, index: u32) {
        let Some(conn) = release(&mut self.conns, &mut self.free_conns, index) else { return };
        if let Some(at) = conn.deadline {
            self.deadlines.remove(&(at, index));
        }
        if matches!(self.demux.get(&conn.tuple), Some(Entry::Conn(i)) if *i == index) {
            self.demux.remove(&conn.tuple);
        }
        if let Tcb::SynRcvd(rcvd) = &conn.state {
            if let Origin::Passive { listener, .. } = rcvd.origin {
                if let Some(l) = value(&mut self.listeners, listener) {
                    l.pending.retain(|&i| i != index);
                }
            }
        }
    }

    /// A connection's end: the user who holds it keeps the socket to learn why (and to read what
    /// both FINs left); anyone else's is freed.
    fn end(&mut self, index: u32, failure: Option<Failure>, rx: Option<Box<Rx>>) {
        let Some(conn) = value(&mut self.conns, index) else { return };
        match conn.user {
            User::Held => {
                if let Some(at) = conn.deadline.take() {
                    self.deadlines.remove(&(at, index));
                }
                if matches!(self.demux.get(&conn.tuple), Some(Entry::Conn(i)) if *i == index) {
                    self.demux.remove(&conn.tuple);
                }
                conn.state = Tcb::Ended(Ended { failure, rx });
            }
            User::Orphan => self.free(index),
            User::Child => {
                let listener = self.listeners.iter_mut().filter_map(|s| s.value.as_mut()).find(|l| l.ready.contains(&index));
                if let Some(l) = listener {
                    l.ready.retain(|&i| i != index);
                    if failure.is_some() {
                        self.counters.add(Counter::AcceptResetDropped, 1);
                    }
                }
                self.free(index);
            }
        }
    }

    fn stub(&mut self, tuple: Tuple, rst: Rst) {
        self.demux.insert(tuple, Entry::Stub(rst));
        self.stubs.push_back(tuple);
    }

    fn enter_time_wait(&mut self, tuple: Tuple, tw: TimeWait) {
        if self.time_waits.len() >= limits::TIME_WAIT_MAX {
            if let Some((_, oldest)) = self.time_waits.pop_first() {
                self.demux.remove(&oldest);
                self.counters.add(Counter::TimeWaitEvicted, 1);
            }
        }
        if tw.ack_owed {
            self.tw_owed.push_back(tuple);
        }
        self.time_waits.insert((tw.end, tuple));
        self.demux.insert(tuple, Entry::TimeWait(tw));
    }

    // ---- listening ----

    fn listener_for(&self, local: &Endpoint) -> Option<u32> {
        let port = local.port.get();
        self.bound.get(&(port, local.addr)).or_else(|| self.bound.get(&(port, Ipv4Addr::UNSPECIFIED))).copied()
    }

    fn port_listened(&self, port: u16) -> bool {
        self.bound.range((port, Ipv4Addr::UNSPECIFIED)..=(port, Ipv4Addr::BROADCAST)).next().is_some()
    }

    /// `addr` is the local address to listen on, UNSPECIFIED for any. Port 0 draws RFC 6056
    /// Algorithm 2's candidates from `random`.
    pub fn listen(&mut self, addr: Ipv4Addr, port: Option<Port>, mut random: impl FnMut() -> u16) -> Result<ListenerId, Error> {
        let port = match port {
            Some(port) if self.bound.contains_key(&(port.get(), addr)) => return Err(Error::AddrInUse),
            Some(port) => port,
            None => {
                let found = (0..EPHEMERAL_COUNT)
                    .map(|_| EPHEMERAL_FIRST.saturating_add(random() % EPHEMERAL_COUNT))
                    .find(|&p| !self.port_listened(p))
                    .and_then(Port::new);
                match found {
                    Some(port) => port,
                    None => {
                        self.counters.add(Counter::NoEphemeralPort, 1);
                        return Err(Error::AddrInUse);
                    }
                }
            }
        };
        let listener = Listener { addr, port, options: Options::default(), pending: Vec::new(), ready: VecDeque::new() };
        let (index, generation) = insert(&mut self.listeners, &mut self.free_listeners, listener);
        self.bound.insert((port.get(), addr), index);
        Ok(ListenerId { index, generation })
    }

    pub fn listener_port(&mut self, id: ListenerId) -> Result<Port, Error> {
        slot(&mut self.listeners, id.index, id.generation).map(|l| l.port).ok_or(Error::NoSuchSocket)
    }

    pub fn set_listener_options(&mut self, id: ListenerId, options: Options) -> Result<(), Error> {
        slot(&mut self.listeners, id.index, id.generation).map(|l| l.options = options).ok_or(Error::NoSuchSocket)
    }

    /// The oldest child that completed its handshake.
    pub fn accept(&mut self, id: ListenerId) -> Result<Option<ConnId>, Error> {
        let listener = slot(&mut self.listeners, id.index, id.generation).ok_or(Error::NoSuchSocket)?;
        let Some(index) = listener.ready.pop_front() else { return Ok(None) };
        let generation = self.conns.get(usize::try_from(index).unwrap_or(usize::MAX)).map_or(0, |s| s.generation);
        if let Some(conn) = value(&mut self.conns, index) {
            conn.user = User::Held;
        }
        Ok(Some(ConnId { index, generation }))
    }

    /// Every child is reset, so its peer learns at once (§12.4).
    pub fn close_listener(&mut self, now: Instant, id: ListenerId) -> Result<(), Error> {
        slot(&mut self.listeners, id.index, id.generation).ok_or(Error::NoSuchSocket)?;
        let Some(listener) = release(&mut self.listeners, &mut self.free_listeners, id.index) else { return Err(Error::NoSuchSocket) };
        self.bound.remove(&(listener.port.get(), listener.addr));
        for index in listener.pending.iter().chain(listener.ready.iter()).copied() {
            let Some(conn) = value(&mut self.conns, index) else { continue };
            let tuple = conn.tuple;
            let rst = match &conn.state {
                Tcb::SynRcvd(rcvd) => Some(rcvd.reset(now, &conn.local)),
                Tcb::Sync(sync) => sync.reset(now),
                Tcb::SynSent(_) | Tcb::Ended(_) => None,
            };
            conn.user = User::Orphan;
            self.free(index);
            if let Some(rst) = rst {
                self.stub(tuple, rst);
            }
            self.counters.add(Counter::ListenerClosedReset, 1);
        }
        Ok(())
    }

    // ---- opening ----

    fn ephemeral(&mut self, local: Ipv4Addr, remote: Endpoint) -> Option<Port> {
        let [a, b, c, d] = local.octets();
        let [e, f, g, h] = remote.addr.octets();
        let [i, j] = remote.port.get().to_be_bytes();
        let input = [a, b, c, d, e, f, g, h, i, j];
        let offset = siphash::low32(&self.config.secrets.port_offset, &input);
        let slot = usize::try_from(siphash::low32(&self.config.secrets.port_index, &input) & 15).unwrap_or(0);
        for _ in 0..EPHEMERAL_COUNT {
            let counter = self.port_table.get_mut(slot)?;
            let candidate = offset.wrapping_add(u32::from(*counter)) & u32::from(EPHEMERAL_COUNT - 1);
            *counter = counter.wrapping_add(1);
            let port = u16::try_from(candidate).ok().map(|c| EPHEMERAL_FIRST.saturating_add(c)).and_then(Port::new)?;
            let tuple = Tuple { local: Endpoint { addr: local, port }, remote };
            if !self.demux.contains_key(&tuple) && !self.port_listened(port.get()) && tuple.local != tuple.remote {
                return Some(port);
            }
        }
        None
    }

    /// An active open from `local` (chosen by [ip]'s route lookup), from `port` or an ephemeral one.
    pub fn connect(&mut self, now: Instant, local: Ipv4Addr, port: Option<Port>, remote: Endpoint) -> Result<ConnId, Error> {
        if !unicast(remote.addr) {
            return Err(Error::InvalidRemote);
        }
        let port = match port {
            Some(port) => port,
            None => match self.ephemeral(local, remote) {
                Some(port) => port,
                None => {
                    self.counters.add(Counter::NoEphemeralPort, 1);
                    return Err(Error::AddrInUse);
                }
            },
        };
        let tuple = Tuple { local: Endpoint { addr: local, port }, remote };
        if tuple.local == tuple.remote {
            self.counters.add(Counter::SelfConnect, 1);
            return Err(Error::InvalidRemote);
        }
        if self.demux.contains_key(&tuple) {
            return Err(Error::AddrInUse);
        }
        let local_ = self.local(&tuple);
        let iss = isn(&self.config.secrets.isn, &tuple, now);
        let conn = Conn {
            tuple,
            options: Options::default(),
            user: User::Held,
            soft: None,
            local: local_,
            state: Tcb::SynSent(SynSent::new(iss, &local_)),
            queued: false,
            deadline: None,
        };
        let (index, generation) = insert(&mut self.conns, &mut self.free_conns, conn);
        self.demux.insert(tuple, Entry::Conn(index));
        self.settle(index, now);
        Ok(ConnId { index, generation })
    }

    // ---- arrival ----

    /// A segment [ip] delivered: to one of our unicast addresses, from a valid unicast source.
    /// `reset_allowed` is the limiter RSTs for no socket draw from.
    pub fn receive(&mut self, now: Instant, from: Ipv4Addr, to: Ipv4Addr, segment: &TcpSegment<'_>, reset_allowed: impl FnOnce(Ipv4Addr) -> bool) {
        let seg = In::new(segment);
        let tuple = Tuple { local: Endpoint { addr: to, port: segment.destination_port() }, remote: Endpoint { addr: from, port: segment.source_port() } };
        if seg.syn() && seg.rst() {
            self.refuse(Counter::SynRst, &tuple);
            return;
        }
        if seg.syn() && seg.fin() {
            self.refuse(Counter::SynFin, &tuple);
            return;
        }
        match self.demux.get(&tuple) {
            Some(Entry::Conn(index)) => self.for_conn(*index, &seg, now),
            Some(Entry::TimeWait(_)) => self.for_time_wait(tuple, &seg, now),
            Some(Entry::Stub(_)) => {}
            None => match self.listener_for(&tuple.local) {
                Some(listener) => self.for_listener(listener, tuple, &seg, now, reset_allowed, None),
                None => self.for_nobody(tuple, &seg, reset_allowed),
            },
        }
    }

    fn answer(&mut self, tuple: Tuple, rst: Rst, reset_allowed: impl FnOnce(Ipv4Addr) -> bool) {
        if self.answers.len() >= ANSWERS || !reset_allowed(tuple.remote.addr) {
            self.counters.add(Counter::ClosedRstLimited, 1);
            return;
        }
        self.answers.push_back(Answer { tuple, rst });
    }

    /// RFC 9293 §3.10.7.1.
    fn for_nobody(&mut self, tuple: Tuple, seg: &In<'_>, reset_allowed: impl FnOnce(Ipv4Addr) -> bool) {
        if seg.rst() {
            self.counters.add(Counter::ClosedRst, 1);
            return;
        }
        let rst = match seg.ack {
            Some(ack) => Rst { seq: ack, ack: None, ts: seg.answer_ts() },
            None => Rst { seq: Seq::new(0), ack: Some(seg.seq.add(seg.len())), ts: seg.answer_ts() },
        };
        self.answer(tuple, rst, reset_allowed);
    }

    /// Counts what a SYN asks for that this stack does not do.
    fn syn_refusals(&mut self, seg: &In<'_>, tuple: &Tuple) {
        let (md5, fast_open) = seg.unimplemented_options();
        if md5 {
            self.refuse(Counter::OptionMd5, tuple);
        }
        if fast_open {
            self.refuse(Counter::OptionFastOpen, tuple);
        }
        if !seg.payload.is_empty() {
            self.refuse(Counter::SynDataDiscarded, tuple);
        }
    }

    /// RFC 9293 §3.10.7.2, and admission under the listener's bounds (§12.2).
    fn for_listener(&mut self, index: u32, tuple: Tuple, seg: &In<'_>, now: Instant, reset_allowed: impl FnOnce(Ipv4Addr) -> bool, time_wait: Option<TimeWait>) {
        if seg.rst() {
            self.counters.add(Counter::ListenRst, 1);
            return;
        }
        if let Some(ack) = seg.ack {
            self.answer(tuple, Rst { seq: ack, ack: None, ts: seg.answer_ts() }, reset_allowed);
            return;
        }
        if !seg.syn() {
            self.counters.add(Counter::ListenNoSyn, 1);
            return;
        }
        let Some(listener) = value(&mut self.listeners, index) else { return };
        if listener.pending.len() >= limits::LISTEN_PENDING || listener.ready.len() >= limits::LISTEN_READY {
            self.counters.add(Counter::ListenOverflow, 1);
            return;
        }
        let options = listener.options;
        self.syn_refusals(seg, &tuple);
        if seg.flags.contains(toyos_net_wire::tcp::TcpFlags::ECE) && seg.flags.contains(toyos_net_wire::tcp::TcpFlags::CWR) {
            self.counters.add(Counter::EcnNotNegotiated, 1);
        }
        let local = self.local(&tuple);
        let fresh = isn(&self.config.secrets.isn, &tuple, now);
        let iss = time_wait.map_or(fresh, |tw| reuse_iss(tw.snd_nxt, fresh));
        let mut ctx = Ctx { now, tuple, options, orphan: false, counters: &mut self.counters, events: &mut self.events };
        let negotiated = negotiate(seg, &local, 0, &mut ctx);
        let child = SynRcvd::passive(iss, seg, negotiated, index, time_wait.map(|tw| (tuple, tw)));
        let conn = Conn {
            tuple,
            options,
            user: User::Child,
            soft: None,
            local,
            state: Tcb::SynRcvd(Box::new(child)),
            queued: false,
            deadline: None,
        };
        let (child, _) = insert(&mut self.conns, &mut self.free_conns, conn);
        self.demux.insert(tuple, Entry::Conn(child));
        if let Some(listener) = value(&mut self.listeners, index) {
            listener.pending.push(child);
        }
        self.settle(child, now);
    }

    fn for_time_wait(&mut self, tuple: Tuple, seg: &In<'_>, now: Instant) {
        if seg.syn() && seg.ack.is_none() {
            if let Some(listener) = self.listener_for(&tuple.local) {
                let Some(Entry::TimeWait(tw)) = self.demux.get(&tuple) else { return };
                let tw = *tw;
                if tw.reopens(seg) {
                    self.demux.remove(&tuple);
                    self.time_waits.remove(&(tw.end, tuple));
                    self.counters.add(Counter::TimeWaitReuse, 1);
                    self.for_listener(listener, tuple, seg, now, |_| true, Some(tw));
                }
                return;
            }
        }
        let Some(Entry::TimeWait(tw)) = self.demux.get_mut(&tuple) else { return };
        let mut ctx = Ctx { now, tuple, options: Options::default(), orphan: true, counters: &mut self.counters, events: &mut self.events };
        let before = tw.end;
        tw.receive(seg, &mut ctx);
        let (after, owed) = (tw.end, tw.ack_owed);
        if after != before {
            self.time_waits.remove(&(before, tuple));
            self.time_waits.insert((after, tuple));
        }
        if owed && !self.tw_owed.contains(&tuple) {
            self.tw_owed.push_back(tuple);
        }
    }

    fn for_conn(&mut self, index: u32, seg: &In<'_>, now: Instant) {
        let Some(conn) = value(&mut self.conns, index) else { return };
        let mut ctx = conn.ctx(now, &mut self.counters, &mut self.events);
        let tuple = conn.tuple;
        match core::mem::replace(&mut conn.state, Tcb::Ended(Ended { failure: None, rx: None })) {
            Tcb::SynSent(sent) => {
                if seg.syn() {
                    let (md5, fast_open) = seg.unimplemented_options();
                    if md5 {
                        ctx.refuse(Counter::OptionMd5);
                    }
                    if fast_open {
                        ctx.refuse(Counter::OptionFastOpen);
                    }
                }
                let (kept, outcome) = sent.receive(seg, &conn.local, &mut ctx);
                if let Some(sent) = kept {
                    conn.state = Tcb::SynSent(sent);
                }
                match outcome {
                    Sent::Keep => {}
                    Sent::Refused => return self.end(index, Some(Failure::Refused), None),
                    Sent::Established(sync) => conn.state = Tcb::Sync(sync),
                    Sent::Simultaneous(rcvd) => conn.state = Tcb::SynRcvd(rcvd),
                }
            }
            Tcb::SynRcvd(mut rcvd) => {
                let outcome = rcvd.receive(seg, &conn.local, &mut ctx);
                match outcome {
                    Rcvd::Keep => conn.state = Tcb::SynRcvd(rcvd),
                    Rcvd::Gone => {
                        conn.state = Tcb::SynRcvd(rcvd);
                        return self.child_gone(index, now);
                    }
                    Rcvd::Refused => return self.end(index, Some(Failure::Refused), None),
                    Rcvd::Established | Rcvd::Crossed => {
                        let crossed = outcome == Rcvd::Crossed;
                        if let Origin::Passive { listener, .. } = rcvd.origin {
                            let full = value(&mut self.listeners, listener).is_none_or(|l| l.ready.len() >= limits::LISTEN_READY);
                            if full {
                                self.counters.add(Counter::AcceptQueueFull, 1);
                                if let Some(conn) = value(&mut self.conns, index) {
                                    conn.state = Tcb::SynRcvd(rcvd);
                                }
                                return self.settle(index, now);
                            }
                            if let Some(l) = value(&mut self.listeners, listener) {
                                l.pending.retain(|&i| i != index);
                                l.ready.push_back(index);
                            }
                        }
                        let Some(conn) = value(&mut self.conns, index) else { return };
                        let mut sync = rcvd.establish(seg, crossed, &conn.local, now);
                        if conn.user == User::Orphan {
                            sync.orphan(now);
                        }
                        conn.state = Tcb::Sync(Box::new(sync));
                        if !crossed {
                            return self.for_conn(index, seg, now);
                        }
                    }
                }
            }
            Tcb::Sync(mut sync) => {
                let verdict = sync.receive(seg, &mut ctx);
                return self.after_sync(index, tuple, sync, verdict, now);
            }
            ended @ Tcb::Ended(_) => conn.state = ended,
        }
        self.settle(index, now);
    }

    fn after_sync(&mut self, index: u32, tuple: Tuple, sync: Box<Sync>, verdict: Verdict, now: Instant) {
        match verdict {
            Verdict::Keep => {
                if let Some(conn) = value(&mut self.conns, index) {
                    conn.state = Tcb::Sync(sync);
                }
                self.settle(index, now);
            }
            Verdict::Closed => self.end(index, None, Some(Box::new(sync.rx))),
            Verdict::Reset => self.end(index, Some(Failure::Reset), None),
            Verdict::TimeWait => {
                let tw = TimeWait::from_sync(&sync, now);
                self.end(index, None, Some(Box::new(sync.rx)));
                self.enter_time_wait(tuple, tw);
            }
            Verdict::Abort(rst) => {
                self.end(index, Some(Failure::Reset), None);
                self.stub(tuple, rst);
            }
        }
    }

    /// A passive child deleted without a word; a 4-tuple it took from TIME-WAIT returns there.
    fn child_gone(&mut self, index: u32, _now: Instant) {
        let Some(conn) = value(&mut self.conns, index) else { return };
        let restore = match &mut conn.state {
            Tcb::SynRcvd(rcvd) => match &mut rcvd.origin {
                Origin::Passive { time_wait, .. } => time_wait.take(),
                Origin::Active { .. } => None,
            },
            _ => None,
        };
        self.free(index);
        if let Some((tuple, tw)) = restore {
            self.enter_time_wait(tuple, TimeWait { ack_owed: false, ..tw });
        }
    }

    /// An ICMP error [ip] validated and classified (RFC 5927 §4.1, tcp.md §13).
    pub fn icmp(&mut self, now: Instant, error: IcmpError) {
        let tuple = Tuple { local: error.local, remote: error.remote };
        let index = match self.demux.get(&tuple) {
            Some(Entry::Conn(index)) => *index,
            Some(Entry::TimeWait(_)) => return self.counters.add(Counter::IcmpStale, 1),
            Some(Entry::Stub(_)) | None => return self.counters.add(Counter::IcmpNoSocket, 1),
        };
        let Some(conn) = value(&mut self.conns, index) else { return };
        let soft = match error.kind {
            IcmpKind::Unreachable(code) => Some(SoftError::Unreachable(code)),
            IcmpKind::TimeExceeded => Some(SoftError::TimeExceeded),
            IcmpKind::ParameterProblem => Some(SoftError::ParameterProblem),
            IcmpKind::PacketTooBig { .. } => None,
        };
        let refused = matches!(error.kind, IcmpKind::Unreachable(UnreachableCode::Protocol | UnreachableCode::Port));
        let prohibited = matches!(
            error.kind,
            IcmpKind::Unreachable(UnreachableCode::NetProhibited | UnreachableCode::HostProhibited | UnreachableCode::CommunicationProhibited)
        );
        let too_big = match error.kind {
            IcmpKind::PacketTooBig { next_hop_mtu, quoted_length } => Some((next_hop_mtu, quoted_length)),
            IcmpKind::Unreachable(UnreachableCode::FragmentationNeeded) => Some((None, 0)),
            IcmpKind::Unreachable(_) | IcmpKind::TimeExceeded | IcmpKind::ParameterProblem => None,
        };
        let mut ctx = conn.ctx(now, &mut self.counters, &mut self.events);
        let (valid, passive) = match &conn.state {
            Tcb::SynSent(sent) => (error.sequence == sent.iss, false),
            Tcb::SynRcvd(rcvd) => (error.sequence == rcvd.iss, rcvd.is_passive()),
            Tcb::Sync(sync) => (error.sequence.within(sync.tx.una, sync.tx.flight()), false),
            Tcb::Ended(_) => (false, false),
        };
        if !valid {
            ctx.count(Counter::IcmpStale);
            return;
        }
        let handshake = !matches!(conn.state, Tcb::Sync(_));
        if handshake {
            if too_big.is_some() {
                return;
            }
            if passive && (refused || prohibited) {
                return self.child_gone(index, now);
            }
            if refused {
                return self.end(index, Some(Failure::Refused), None);
            }
            if prohibited {
                return self.end(index, Some(Failure::Prohibited), None);
            }
        } else if let (Some((mtu, quoted)), Tcb::Sync(sync)) = (too_big, &mut conn.state) {
            sync.packet_too_big(mtu, quoted, error.sequence, &mut ctx);
            return self.settle(index, now);
        } else if refused {
            ctx.refuse(Counter::IcmpHardAsSoft);
        }
        ctx.count(Counter::IcmpSoft);
        conn.soft = soft;
    }

    // ---- user calls ----

    pub fn set_options(&mut self, now: Instant, id: ConnId, options: Options) -> Result<(), Error> {
        self.conn(id)?.options = options;
        self.settle(id.index, now);
        Ok(())
    }

    pub fn send(&mut self, now: Instant, id: ConnId, data: &[u8]) -> Result<usize, Error> {
        let conn = self.conn(id)?;
        let result = match &mut conn.state {
            Tcb::SynSent(sent) => match sent.buf.push(data) {
                0 if !data.is_empty() => Err(Error::WouldBlock),
                n => Ok(n),
            },
            Tcb::SynRcvd(rcvd) => match rcvd.send(data) {
                Some(0) if !data.is_empty() => Err(Error::WouldBlock),
                Some(n) => Ok(n),
                None => Err(Error::Closing),
            },
            Tcb::Sync(sync) => sync.send(data, now),
            Tcb::Ended(ended) => Err(ended.failure.map_or(Error::Closing, Error::Failed)),
        };
        self.settle(id.index, now);
        result
    }

    pub fn recv(&mut self, now: Instant, id: ConnId, out: &mut [u8]) -> Result<Received, Error> {
        let conn = self.conn(id)?;
        let result = match &mut conn.state {
            Tcb::SynSent(_) | Tcb::SynRcvd(_) => Err(Error::WouldBlock),
            Tcb::Sync(sync) => sync.recv(out),
            Tcb::Ended(Ended { failure: Some(failure), .. }) => Err(Error::Failed(*failure)),
            Tcb::Ended(Ended { failure: None, rx }) => match rx.as_mut().map(|rx| rx.read(out)) {
                Some(n) if n > 0 => Ok(Received::Data(n)),
                _ => Ok(Received::End),
            },
        };
        self.settle(id.index, now);
        result
    }

    pub fn shutdown_write(&mut self, now: Instant, id: ConnId) -> Result<(), Error> {
        let conn = self.conn(id)?;
        let result = match &mut conn.state {
            Tcb::SynSent(_) => Err(Error::NotConnected),
            Tcb::SynRcvd(rcvd) => {
                rcvd.shutdown_write();
                Ok(())
            }
            Tcb::Sync(sync) => {
                sync.shutdown_write(now);
                Ok(())
            }
            Tcb::Ended(ended) => ended.failure.map_or(Ok(()), |f| Err(Error::Failed(f))),
        };
        self.settle(id.index, now);
        result
    }

    /// `how = 0`: what is held is dropped, and later data is acknowledged and dropped as if read.
    pub fn shutdown_read(&mut self, now: Instant, id: ConnId) -> Result<(), Error> {
        if let Tcb::Sync(sync) = &mut self.conn(id)?.state {
            sync.rx.stop_reading();
        }
        self.settle(id.index, now);
        Ok(())
    }

    /// The user lets go. Unread data makes it a reset, which shows the peer data was lost
    /// (RFC 9293 §3.6.1 SHLD-3); otherwise the connection finishes alone.
    pub fn close(&mut self, now: Instant, id: ConnId) -> Result<(), Error> {
        let conn = self.conn(id)?;
        let tuple = conn.tuple;
        match &mut conn.state {
            Tcb::SynSent(_) | Tcb::Ended(_) => {
                conn.user = User::Orphan;
                self.free(id.index);
            }
            Tcb::SynRcvd(rcvd) => {
                rcvd.shutdown_write();
                conn.user = User::Orphan;
                self.settle(id.index, now);
            }
            Tcb::Sync(sync) if sync.rx.unread() > 0 => {
                let rst = sync.reset_always(now);
                conn.user = User::Orphan;
                self.counters.add(Counter::CloseUnreadRst, 1);
                self.free(id.index);
                self.stub(tuple, rst);
            }
            Tcb::Sync(sync) => {
                sync.shutdown_write(now);
                sync.orphan(now);
                conn.user = User::Orphan;
                self.settle(id.index, now);
            }
        }
        Ok(())
    }

    /// RFC 9293 §3.10.5.
    pub fn abort(&mut self, now: Instant, id: ConnId) -> Result<(), Error> {
        let conn = self.conn(id)?;
        let tuple = conn.tuple;
        let rst = match &conn.state {
            Tcb::SynSent(_) => None,
            Tcb::SynRcvd(rcvd) => Some(rcvd.reset(now, &conn.local)),
            Tcb::Sync(sync) => sync.reset(now),
            Tcb::Ended(_) => {
                if let Some(Entry::TimeWait(tw)) = self.demux.get(&tuple) {
                    let end = tw.end;
                    self.time_waits.remove(&(end, tuple));
                    self.demux.remove(&tuple);
                }
                None
            }
        };
        if let Some(conn) = value(&mut self.conns, id.index) {
            conn.user = User::Orphan;
        }
        self.free(id.index);
        if let Some(rst) = rst {
            self.stub(tuple, rst);
        }
        Ok(())
    }

    pub fn status(&mut self, id: ConnId) -> Result<Status, Error> {
        let in_time_wait = |demux: &BTreeMap<Tuple, Entry>, tuple| matches!(demux.get(tuple), Some(Entry::TimeWait(_)));
        let conn = slot(&mut self.conns, id.index, id.generation).filter(|c| c.user == User::Held).ok_or(Error::NoSuchSocket)?;
        let (state, readable, writable, delivery_problem, failure) = match &conn.state {
            Tcb::SynSent(sent) => (State::SynSent, 0, sent.buf.room(), sent.timer.stalled >= 3, None),
            Tcb::SynRcvd(rcvd) => (State::SynReceived, 0, 0, rcvd.timer.stalled >= 3, None),
            Tcb::Sync(sync) => {
                let writable = if sync.tx.fin.is_some() { 0 } else { sync.tx.buf.room() };
                (sync.state(), sync.rx.unread(), writable, sync.delivery_problem, None)
            }
            Tcb::Ended(ended) => {
                let state = if in_time_wait(&self.demux, &conn.tuple) { State::TimeWait } else { State::Closed };
                (state, ended.rx.as_ref().map_or(0, |rx| rx.unread()), 0, false, ended.failure)
            }
        };
        Ok(Status { state, readable, writable, failure, soft_error: conn.soft, delivery_problem })
    }

    /// Any live connection, orphans included: what inspect lists.
    pub fn info(&mut self, id: ConnId) -> Option<Info> {
        let conn = slot(&mut self.conns, id.index, id.generation)?;
        let Tcb::Sync(sync) = &conn.state else { return None };
        Some(Info {
            state: sync.state(),
            snd_una: sync.tx.una,
            snd_nxt: sync.tx.nxt,
            snd_wnd: sync.tx.wnd,
            snd_wl1: sync.tx.wl1,
            max_snd_wnd: sync.tx.max_wnd,
            rcv_nxt: sync.rx.next,
            rcv_edge: sync.rx.edge(),
            srtt: sync.rtt.srtt(),
            rttvar: sync.rtt.rttvar(),
            rto: sync.rtt.rto(),
            cwnd: sync.cc.cwnd,
            ssthresh: sync.cc.ssthresh,
            smss: sync.smss(),
            snd_shift: sync.tx.shift,
            rcv_shift: sync.rx.shift,
            sack: sync.sack_ok,
            ts_recent: sync.ts.map(|ts| ts.recent),
            sacked_ranges: sync.tx.sacked().len(),
            in_recovery: sync.in_recovery(),
            ooo_ranges: sync.rx.ranges(),
            queued: sync.tx.buf.len(),
            unread: sync.rx.unread(),
            rtx_timer: sync.rtx_timer,
            deadline: conn.deadline,
        })
    }

    pub fn tuple(&mut self, id: ConnId) -> Result<Tuple, Error> {
        Ok(self.conn(id)?.tuple)
    }

    // ---- time ----

    pub fn next_deadline(&self) -> Option<Instant> {
        let conn = self.deadlines.first().map(|&(at, _)| at);
        let time_wait = self.time_waits.first().map(|&(at, _)| at);
        conn.into_iter().chain(time_wait).min()
    }

    /// Processes every timer due at `now`; the work they make due waits for [`transmit`](Self::transmit).
    pub fn fire(&mut self, now: Instant) {
        while let Some(&(end, tuple)) = self.time_waits.first() {
            if end > now {
                break;
            }
            self.time_waits.pop_first();
            if matches!(self.demux.get(&tuple), Some(Entry::TimeWait(_))) {
                self.demux.remove(&tuple);
            }
        }
        while let Some(&(at, index)) = self.deadlines.first() {
            if at > now {
                break;
            }
            self.deadlines.pop_first();
            if let Some(conn) = value(&mut self.conns, index) {
                conn.deadline = None;
            }
            self.tick(index, now);
        }
    }

    fn tick(&mut self, index: u32, now: Instant) {
        let Some(conn) = value(&mut self.conns, index) else { return };
        let mut ctx = conn.ctx(now, &mut self.counters, &mut self.events);
        let tuple = conn.tuple;
        let soft = conn.soft;
        match &mut conn.state {
            Tcb::SynSent(sent) => {
                if sent.tick(&mut ctx) {
                    return self.end(index, Some(failure_of(soft)), None);
                }
            }
            Tcb::SynRcvd(rcvd) => {
                if rcvd.tick(&mut ctx) {
                    if rcvd.is_passive() {
                        ctx.count(Counter::SynAckGiveUp);
                        return self.child_gone(index, now);
                    }
                    return self.end(index, Some(failure_of(soft)), None);
                }
            }
            Tcb::Sync(sync) => {
                let failure = match sync.tick(&mut ctx) {
                    Tick::Keep => None,
                    Tick::GiveUp => Some(failure_of(soft)),
                    Tick::TimedOut | Tick::Orphan => Some(Failure::TimedOut),
                };
                if let Some(failure) = failure {
                    let rst = sync.reset_always(now);
                    self.end(index, Some(failure), None);
                    return self.stub(tuple, rst);
                }
            }
            Tcb::Ended(_) => {}
        }
        self.settle(index, now);
    }

    // ---- egress ----

    /// A transmit opportunity with room for `credit` frames: resets first, then each connection
    /// in turn one segment at a time. Each segment is built now and handed to `sink`, and only
    /// then counts as sent. Returns how many left.
    pub fn transmit(&mut self, now: Instant, credit: usize, mut sink: impl FnMut(&Outgoing<'_>)) -> usize {
        let mut sent = 0usize;
        while sent < credit {
            if let Some(tuple) = self.stubs.pop_front() {
                if let Some(Entry::Stub(rst)) = self.demux.remove(&tuple) {
                    sink(&builder(&Out::rst(&rst), &tuple, &[]));
                    sent = sent.saturating_add(1);
                }
                continue;
            }
            if let Some(answer) = self.answers.pop_front() {
                sink(&builder(&Out::rst(&answer.rst), &answer.tuple, &[]));
                sent = sent.saturating_add(1);
                continue;
            }
            if let Some(tuple) = self.tw_owed.pop_front() {
                if let Some(Entry::TimeWait(tw)) = self.demux.get_mut(&tuple) {
                    if core::mem::replace(&mut tw.ack_owed, false) {
                        sink(&builder(&tw.ack(now), &tuple, &[]));
                        sent = sent.saturating_add(1);
                    }
                }
                continue;
            }
            let Some(index) = self.active.pop_front() else { break };
            let Some(conn) = value(&mut self.conns, index) else { continue };
            let mut ctx = conn.ctx(now, &mut self.counters, &mut self.events);
            let out = match &mut conn.state {
                Tcb::SynSent(s) => s.next_segment(&conn.local, now),
                Tcb::SynRcvd(s) => s.next_segment(&conn.local, now),
                Tcb::Sync(s) => s.next_segment(&mut ctx),
                Tcb::Ended(_) => None,
            };
            let Some(out) = out else {
                conn.queued = false;
                self.settle_deadline(index, now);
                continue;
            };
            let payload: &[u8] = match &conn.state {
                Tcb::Sync(sync) if out.data.1 > 0 => {
                    let (first, second) = sync.tx.buf.slices(out.data.0, out.data.1);
                    if second.is_empty() {
                        first
                    } else {
                        self.scratch.clear();
                        self.scratch.extend_from_slice(first);
                        self.scratch.extend_from_slice(second);
                        &self.scratch
                    }
                }
                _ => &[],
            };
            sink(&builder(&out, &conn.tuple, payload));
            sent = sent.saturating_add(1);
            self.active.push_back(index);
            self.settle_deadline(index, now);
        }
        sent
    }

    fn settle_deadline(&mut self, index: u32, now: Instant) {
        let Some(conn) = value(&mut self.conns, index) else { return };
        let ctx = conn.ctx(now, &mut self.counters, &mut self.events);
        let deadline = conn.deadline(&ctx);
        if let Some(old) = core::mem::replace(&mut conn.deadline, deadline) {
            self.deadlines.remove(&(old, index));
        }
        if let Some(at) = deadline {
            self.deadlines.insert((at, index));
        }
    }
}
