//! One shard's TCP: the connection table and its demultiplexing, listeners and their queues,
//! TIME-WAIT, port choice, the timers, and egress: what is owed outside a connection, and each
//! connection's next segment when the caller's round serves it.
//!
//! Connections live in a slab named by index and generation, so an id from a freed slot names
//! nothing. The demux is an ordered map, which no chosen set of 4-tuples can degrade.

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::vec::Vec;
use core::net::Ipv4Addr;
use core::time::Duration;

use toyos_net_wire::addr::is_host;
use toyos_net_wire::icmp::UnreachableCode;
use toyos_net_wire::tcp::{Control, EstablishedOptions, RawWindow, TcpBuilder, TcpSegment};
use toyos_net_wire::Port;

use crate::conn::{screen, Ctx, In, Kind, Out, Rst, Screened, Sync, Tick, Ts, Verdict, NO_PAYLOAD};
use crate::counters::{Counter, Counters, Log};
use crate::open::{negotiate, refuse_syn_extras, Local, Origin, Rcvd, Sent, SynRcvd, SynSent};
use crate::rx::Rx;
use crate::seq::{Seq, Stamp};
use crate::{isn, limits, reuse_iss, siphash, ts_offset, Config, ConfigError, Endpoint, Error, Event, Exit, Failure, Hop, IcmpError, IcmpKind, Instant, NotReady, Options, Received, SoftError, State, Status, Tuple};

const EPHEMERAL_FIRST: u16 = 49_152;
const EPHEMERAL_COUNT: u16 = 16_384;
/// Answers to segments for no socket, waiting for credit or for their next hop.
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

/// What [`Tcp::serve`] did with one frame of credit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Served {
    Sent,
    /// Nothing more until it is offered again: the connection leaves the round.
    Done,
    /// The sink refused its frame.
    Refused,
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
    pub high_rxt: Option<Seq>,
    pub rescue_rxt: Option<Seq>,
    pub pipe: u32,
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
    /// A child that `accept` has not returned, and the listener that returns it.
    Child { listener: u32 },
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
    /// Offered to the caller's round and not yet answered [`Served::Done`].
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
    last_unsolicited: Option<Instant>,
}

enum Entry {
    Conn(u32),
    TimeWait(TimeWait),
    /// An RST owed under pull egress; segments for the 4-tuple are dropped until it leaves, and
    /// then the TIME-WAIT a reset child had reopened resumes.
    Stub(Rst, Option<TimeWait>),
}

struct Answer {
    tuple: Tuple,
    rst: Rst,
}

/// What is owed and not offered: under a remote address, what waits for its next hop until
/// [`Tcp::wake`] names it; within one [`Tcp::transmit_owed`], what a refused frame left owed until
/// the call ends. Either then goes back ahead of anything queued since, and a connection is
/// offered to the round again.
#[derive(Default)]
struct Parked {
    stubs: Vec<Tuple>,
    answers: Vec<Answer>,
    time_waits: Vec<Tuple>,
    conns: Vec<u32>,
}

impl Parked {
    fn is_empty(&self) -> bool {
        self.stubs.is_empty() && self.answers.is_empty() && self.time_waits.is_empty() && self.conns.is_empty()
    }
}

/// One flow's way out of a transmit opportunity: `hop` answers for its 4-tuple, and `sink` takes
/// the frame or refuses it.
struct Way<'a, H, S> {
    tuple: Tuple,
    hop: &'a mut H,
    sink: &'a mut S,
    scratch: &'a mut Vec<u8>,
}

impl<T, H: FnMut(&Tuple) -> Hop<T>, S: FnMut(&Outgoing<'_>, T) -> bool> Exit<T> for Way<'_, H, S> {
    fn ask(&mut self) -> Result<T, NotReady> {
        (self.hop)(&self.tuple).ready()
    }

    fn send(&mut self, via: T, segment: &Out, (first, second): (&[u8], &[u8])) -> Result<(), NotReady> {
        let Self { tuple, sink, scratch, .. } = self;
        let payload = if second.is_empty() {
            first
        } else {
            scratch.clear();
            scratch.extend_from_slice(first);
            scratch.extend_from_slice(second);
            scratch.as_slice()
        };
        if sink(&builder(segment, tuple, payload), via) {
            Ok(())
        } else {
            Err(NotReady::Unframed)
        }
    }
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
    /// Connections that became eligible for the caller's round since it last drained them.
    eligible: Vec<ConnId>,
    stubs: VecDeque<Tuple>,
    answers: VecDeque<Answer>,
    /// TIME-WAITs owing an ACK: a set, so a segment finds its entry in log n.
    tw_owed: BTreeSet<Tuple>,
    parked: BTreeMap<Ipv4Addr, Parked>,
    /// Answers in `parked`, which [`ANSWERS`] bounds with the queued ones.
    parked_answers: usize,
    port_table: [u16; 16],
    log: Log,
    scratch: Vec<u8>,
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

    /// Everything but a reopening SYN; `true` owes an ACK. An exact RST is ignored (RFC 1337 fix
    /// F1), and a retransmitted FIN restarts the wait.
    fn receive(&mut self, seg: &In<'_>, ctx: &mut Ctx<'_>) -> bool {
        match screen(seg, self.rcv_nxt, self.window, self.rcv_nxt, self.ts.as_mut(), false, ctx) {
            Screened::Pass => false,
            Screened::Drop { challenge } => challenge && ctx.unsolicited(&mut self.last_unsolicited),
            Screened::Unacceptable { old: true } => {
                if seg.fin() && seg.seq.add(seg.len()) == self.rcv_nxt {
                    self.end = ctx.now.after(limits::TIME_WAIT);
                }
                true
            }
            Screened::Unacceptable { old: false } => ctx.unsolicited(&mut self.last_unsolicited),
            Screened::Ends => {
                ctx.log.refuse(Counter::TimeWaitRstIgnored, &ctx.tuple);
                false
            }
        }
    }

    fn ack(&self, now: Instant) -> Out {
        let window = u16::try_from((self.window >> self.rcv_shift).min(u32::from(u16::MAX))).unwrap_or(u16::MAX);
        Out {
            seq: self.snd_nxt,
            kind: Kind::Ack { ack: self.rcv_nxt, push: false, fin: false },
            window,
            ts: self.ts.map(|ts| ts.option(now)),
            sack: crate::conn::NO_BLOCKS,
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
    fn ctx<'a>(&self, now: Instant, log: &'a mut Log) -> Ctx<'a> {
        Ctx { now, tuple: self.tuple, options: self.options, orphan: self.user == User::Orphan, log }
    }

    fn deadline(&self, ctx: &Ctx<'_>) -> Option<Instant> {
        match &self.state {
            Tcb::SynSent(s) => Some(s.deadline()),
            Tcb::SynRcvd(s) => Some(s.deadline()),
            Tcb::Sync(s) => s.deadline(ctx),
            Tcb::Ended(_) => None,
        }
    }
}

impl Tcp {
    pub fn new(config: Config) -> Result<Self, ConfigError> {
        let buffer = config.receive_buffer;
        if buffer > limits::RECEIVE_BUFFER_MAX {
            return Err(ConfigError::ReceiveBufferTooLarge);
        }
        let shift = (0u8..14).find(|&r| buffer.checked_shr(u32::from(r)).is_some_and(|w| u16::try_from(w).is_ok())).unwrap_or(14);
        let port_table = config.secrets.port_table;
        Ok(Self {
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
            eligible: Vec::new(),
            stubs: VecDeque::new(),
            answers: VecDeque::new(),
            tw_owed: BTreeSet::new(),
            parked: BTreeMap::new(),
            parked_answers: 0,
            port_table,
            log: Log::default(),
            scratch: Vec::new(),
        })
    }

    pub fn counters(&self) -> &Counters {
        &self.log.counters
    }

    /// Refusals and reachability advice since the last call.
    pub fn drain_events(&mut self) -> impl Iterator<Item = Event> + '_ {
        self.log.drain()
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

    // ---- slab bookkeeping ----

    /// The connection a user holds: after `close` or `abort` the id names nothing.
    fn conn(&mut self, id: ConnId) -> Result<&mut Conn, Error> {
        slot(&mut self.conns, id.index, id.generation).filter(|c| c.user == User::Held).ok_or(Error::NoSuchSocket)
    }

    /// Re-files a connection's deadline and offers it the next transmit opportunity.
    fn settle(&mut self, index: u32, now: Instant) {
        self.settle_deadline(index, now);
        self.offer(index);
    }

    /// Offers a connection to the caller's round unless it is already there.
    fn offer(&mut self, index: u32) {
        let Some(slot) = self.conns.get_mut(usize::try_from(index).unwrap_or(usize::MAX)) else { return };
        let generation = slot.generation;
        if let Some(conn) = slot.value.as_mut() {
            if !core::mem::replace(&mut conn.queued, true) {
                self.eligible.push(ConnId { index, generation });
            }
        }
    }

    fn free(&mut self, index: u32) {
        let Some(conn) = release(&mut self.conns, &mut self.free_conns, index) else { return };
        // Its index may name another connection next.
        self.unpark(conn.tuple.remote.addr, |parked| parked.conns.retain(|&i| i != index));
        if let Some(at) = conn.deadline {
            self.deadlines.remove(&(at, index));
        }
        if matches!(self.demux.get(&conn.tuple), Some(Entry::Conn(i)) if *i == index) {
            self.demux.remove(&conn.tuple);
        }
        if let User::Child { listener } = conn.user {
            if let Some(l) = value(&mut self.listeners, listener) {
                l.pending.retain(|&i| i != index);
                l.ready.retain(|&i| i != index);
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
            User::Child { .. } => {
                if failure.is_some() {
                    self.log.count(Counter::AcceptResetDropped);
                }
                self.free(index);
            }
        }
    }

    fn stub(&mut self, tuple: Tuple, rst: Rst, resume: Option<TimeWait>) {
        self.demux.insert(tuple, Entry::Stub(rst, resume));
        self.stubs.push_back(tuple);
    }

    fn enter_time_wait(&mut self, tuple: Tuple, tw: TimeWait, owed: bool) {
        if self.time_waits.len() >= limits::TIME_WAIT_MAX {
            if let Some(&(end, oldest)) = self.time_waits.first() {
                self.leave_time_wait(oldest, end);
                self.log.count(Counter::TimeWaitEvicted);
            }
        }
        if owed {
            self.tw_owed.insert(tuple);
        }
        self.time_waits.insert((tw.end, tuple));
        self.demux.insert(tuple, Entry::TimeWait(tw));
    }

    /// Ends the TIME-WAIT of `tuple`, due to end at `end`.
    fn leave_time_wait(&mut self, tuple: Tuple, end: Instant) {
        self.time_waits.remove(&(end, tuple));
        self.tw_owed.remove(&tuple);
        self.unpark(tuple.remote.addr, |parked| parked.time_waits.retain(|t| *t != tuple));
        if matches!(self.demux.get(&tuple), Some(Entry::TimeWait(_))) {
            self.demux.remove(&tuple);
        }
    }

    /// The TIME-WAIT a passive child reopened, to resume when the child ends before ESTABLISHED
    /// (RFC 9293 MAY-2 (2)).
    fn reopened(state: &mut Tcb) -> Option<TimeWait> {
        match state {
            Tcb::SynRcvd(rcvd) => match &mut rcvd.origin {
                Origin::Passive { time_wait, .. } => time_wait.take(),
                Origin::Active { .. } => None,
            },
            Tcb::SynSent(_) | Tcb::Sync(_) | Tcb::Ended(_) => None,
        }
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
                        self.log.count(Counter::NoEphemeralPort);
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

    /// Every child is reset, so its peer learns at once; a TIME-WAIT a child reopened
    /// resumes once the reset has left.
    pub fn close_listener(&mut self, now: Instant, id: ListenerId) -> Result<(), Error> {
        slot(&mut self.listeners, id.index, id.generation).ok_or(Error::NoSuchSocket)?;
        let Some(listener) = release(&mut self.listeners, &mut self.free_listeners, id.index) else { return Err(Error::NoSuchSocket) };
        self.bound.remove(&(listener.port.get(), listener.addr));
        for index in listener.pending.iter().chain(listener.ready.iter()).copied() {
            let Some(conn) = value(&mut self.conns, index) else { continue };
            let tuple = conn.tuple;
            let rst = match &conn.state {
                Tcb::SynRcvd(rcvd) => Some(rcvd.reset(now)),
                Tcb::Sync(sync) => sync.reset(now),
                Tcb::SynSent(_) | Tcb::Ended(_) => None,
            };
            let resume = Self::reopened(&mut conn.state);
            self.free(index);
            if let Some(rst) = rst {
                self.stub(tuple, rst, resume);
            }
            self.log.count(Counter::ListenerClosedReset);
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
        if !is_host(remote.addr) {
            return Err(Error::InvalidRemote);
        }
        let port = match port {
            Some(port) => port,
            None => match self.ephemeral(local, remote) {
                Some(port) => port,
                None => {
                    self.log.count(Counter::NoEphemeralPort);
                    return Err(Error::AddrInUse);
                }
            },
        };
        let tuple = Tuple { local: Endpoint { addr: local, port }, remote };
        if tuple.local == tuple.remote {
            self.log.count(Counter::SelfConnect);
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
            state: Tcb::SynSent(SynSent::new(iss, &local_, now)),
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
            self.log.refuse(Counter::SynRst, &tuple);
            return;
        }
        if seg.syn() && seg.fin() {
            self.log.refuse(Counter::SynFin, &tuple);
            return;
        }
        match self.demux.get(&tuple) {
            Some(Entry::Conn(index)) => self.for_conn(*index, &seg, now),
            Some(Entry::TimeWait(_)) => self.for_time_wait(tuple, &seg, now),
            Some(Entry::Stub(..)) => {}
            None => match self.listener_for(&tuple.local) {
                Some(listener) => self.for_listener(listener, tuple, &seg, now, reset_allowed, None),
                None => self.for_nobody(tuple, &seg, reset_allowed),
            },
        }
    }

    fn answer(&mut self, tuple: Tuple, rst: Rst, reset_allowed: impl FnOnce(Ipv4Addr) -> bool) {
        if self.answers.len().saturating_add(self.parked_answers) >= ANSWERS || !reset_allowed(tuple.remote.addr) {
            self.log.count(Counter::ClosedRstLimited);
            return;
        }
        self.answers.push_back(Answer { tuple, rst });
    }

    /// RFC 9293 §3.10.7.1.
    fn for_nobody(&mut self, tuple: Tuple, seg: &In<'_>, reset_allowed: impl FnOnce(Ipv4Addr) -> bool) {
        if seg.rst() {
            self.log.count(Counter::ClosedRst);
            return;
        }
        let rst = match seg.ack {
            Some(ack) => Rst { seq: ack, ack: None, ts: seg.answer_ts() },
            None => Rst { seq: Seq::new(0), ack: Some(seg.seq.add(seg.len())), ts: seg.answer_ts() },
        };
        self.answer(tuple, rst, reset_allowed);
    }

    /// RFC 9293 §3.10.7.2, and admission under the listener's bounds (§12.2). A TIME-WAIT the
    /// SYN would reopen ends only when the child is admitted.
    fn for_listener(&mut self, index: u32, tuple: Tuple, seg: &In<'_>, now: Instant, reset_allowed: impl FnOnce(Ipv4Addr) -> bool, time_wait: Option<TimeWait>) {
        if seg.rst() {
            self.log.count(Counter::ListenRst);
            return;
        }
        if let Some(ack) = seg.ack {
            self.answer(tuple, Rst { seq: ack, ack: None, ts: seg.answer_ts() }, reset_allowed);
            return;
        }
        if !seg.syn() {
            self.log.count(Counter::ListenNoSyn);
            return;
        }
        let Some(listener) = value(&mut self.listeners, index) else { return };
        if listener.pending.len() >= limits::LISTEN_PENDING || listener.ready.len() >= limits::LISTEN_READY {
            self.log.count(Counter::ListenOverflow);
            return;
        }
        let options = listener.options;
        if let Some(tw) = time_wait {
            self.leave_time_wait(tuple, tw.end);
            self.log.count(Counter::TimeWaitReuse);
        }
        let local = self.local(&tuple);
        let fresh = isn(&self.config.secrets.isn, &tuple, now);
        let iss = time_wait.map_or(fresh, |tw| reuse_iss(tw.snd_nxt, fresh));
        let mut ctx = Ctx { now, tuple, options, orphan: false, log: &mut self.log };
        refuse_syn_extras(seg, &mut ctx);
        if seg.flags.contains(toyos_net_wire::tcp::TcpFlags::ECE) && seg.flags.contains(toyos_net_wire::tcp::TcpFlags::CWR) {
            ctx.log.count(Counter::EcnNotNegotiated);
        }
        let negotiated = negotiate(seg, &local, 0, &mut ctx);
        let child = SynRcvd::passive(iss, seg, negotiated, time_wait, now);
        let conn = Conn {
            tuple,
            options,
            user: User::Child { listener: index },
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

    /// A SYN without ACK where a listener holds the endpoint either reopens the 4-tuple or
    /// is dropped; TIME-WAIT itself judges everything else.
    fn for_time_wait(&mut self, tuple: Tuple, seg: &In<'_>, now: Instant) {
        let listener = self.listener_for(&tuple.local).filter(|_| seg.syn() && seg.ack.is_none());
        let Some(Entry::TimeWait(tw)) = self.demux.get_mut(&tuple) else { return };
        if let Some(listener) = listener {
            let tw = *tw;
            if tw.reopens(seg) {
                self.for_listener(listener, tuple, seg, now, |_| true, Some(tw));
            }
            return;
        }
        let mut ctx = Ctx { now, tuple, options: Options::default(), orphan: true, log: &mut self.log };
        let before = tw.end;
        let parked = self.parked.get(&tuple.remote.addr).is_some_and(|p| p.time_waits.contains(&tuple));
        if tw.receive(seg, &mut ctx) && !parked {
            self.tw_owed.insert(tuple);
        }
        if tw.end != before {
            self.time_waits.remove(&(before, tuple));
            self.time_waits.insert((tw.end, tuple));
        }
    }

    fn for_conn(&mut self, index: u32, seg: &In<'_>, now: Instant) {
        let Some(conn) = value(&mut self.conns, index) else { return };
        let mut ctx = conn.ctx(now, &mut self.log);
        let tuple = conn.tuple;
        let user = conn.user;
        match core::mem::replace(&mut conn.state, Tcb::Ended(Ended { failure: None, rx: None })) {
            Tcb::SynSent(sent) => {
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
                        return self.child_gone(index);
                    }
                    Rcvd::Refused => return self.end(index, Some(Failure::Refused), None),
                    Rcvd::Established | Rcvd::Crossed => {
                        let crossed = outcome == Rcvd::Crossed;
                        if let User::Child { listener } = user {
                            let full = value(&mut self.listeners, listener).is_none_or(|l| l.ready.len() >= limits::LISTEN_READY);
                            if full {
                                self.log.count(Counter::AcceptQueueFull);
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
                let una = sync.tx.una;
                let verdict = sync.receive(seg, &mut ctx);
                if sync.tx.una != una {
                    conn.soft = None;
                }
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
                let owed = sync.rx.ack_now || sync.rx.dup_owed > 0 || sync.rx.delayed.is_some();
                self.end(index, None, Some(Box::new(sync.rx)));
                self.enter_time_wait(tuple, tw, owed);
            }
            Verdict::Abort(rst) => {
                self.end(index, Some(Failure::Reset), None);
                self.stub(tuple, rst, None);
            }
        }
    }

    /// A passive child deleted without a word; a 4-tuple it took from TIME-WAIT returns there.
    fn child_gone(&mut self, index: u32) {
        let Some(conn) = value(&mut self.conns, index) else { return };
        let tuple = conn.tuple;
        let resume = Self::reopened(&mut conn.state);
        self.free(index);
        if let Some(tw) = resume {
            self.enter_time_wait(tuple, tw, false);
        }
    }

    /// An ICMP error [ip] validated and classified (RFC 5927 §4.1).
    pub fn icmp(&mut self, now: Instant, error: IcmpError) {
        let tuple = Tuple { local: error.local, remote: error.remote };
        let index = match self.demux.get(&tuple) {
            Some(Entry::Conn(index)) => *index,
            Some(Entry::TimeWait(_)) => return self.log.count(Counter::IcmpStale),
            Some(Entry::Stub(..)) | None => return self.log.count(Counter::IcmpNoSocket),
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
        let mut ctx = conn.ctx(now, &mut self.log);
        let (valid, passive) = match &conn.state {
            Tcb::SynSent(sent) => (error.sequence == sent.iss, false),
            Tcb::SynRcvd(rcvd) => (error.sequence == rcvd.iss, rcvd.is_passive()),
            Tcb::Sync(sync) => (error.sequence.within(sync.tx.una, sync.tx.flight()), false),
            Tcb::Ended(_) => (false, false),
        };
        if !valid {
            ctx.log.count(Counter::IcmpStale);
            return;
        }
        let handshake = !matches!(conn.state, Tcb::Sync(_));
        if handshake {
            if too_big.is_some() {
                return;
            }
            if passive && (refused || prohibited) {
                return self.child_gone(index);
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
            ctx.log.refuse(Counter::IcmpHardAsSoft, &ctx.tuple);
        }
        ctx.log.count(Counter::IcmpSoft);
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
                self.free(id.index);
            }
            Tcb::SynRcvd(rcvd) => {
                rcvd.shutdown_write();
                conn.user = User::Orphan;
                self.settle(id.index, now);
            }
            Tcb::Sync(sync) if sync.rx.unread() > 0 => {
                let rst = sync.reset_always(now);
                self.log.count(Counter::CloseUnreadRst);
                self.free(id.index);
                self.stub(tuple, rst, None);
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
            Tcb::SynRcvd(rcvd) => Some(rcvd.reset(now)),
            Tcb::Sync(sync) => sync.reset(now),
            Tcb::Ended(_) => {
                if let Some(Entry::TimeWait(tw)) = self.demux.get(&tuple) {
                    let end = tw.end;
                    self.leave_time_wait(tuple, end);
                }
                None
            }
        };
        self.free(id.index);
        if let Some(rst) = rst {
            self.stub(tuple, rst, None);
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
            high_rxt: sync.sack_marks().map(|(high_rxt, _)| high_rxt),
            rescue_rxt: sync.sack_marks().and_then(|(_, rescue)| rescue),
            pipe: sync.pipe(),
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
            self.leave_time_wait(tuple, end);
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
        let mut ctx = conn.ctx(now, &mut self.log);
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
                        ctx.log.count(Counter::SynAckGiveUp);
                        return self.child_gone(index);
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
                    return self.stub(tuple, rst, None);
                }
            }
            Tcb::Ended(_) => {}
        }
        self.settle(index, now);
    }

    // ---- egress ----

    /// What is owed outside a connection, in frames of `credit`: resets for connections that are
    /// gone, answers to segments for no socket, then TIME-WAIT's ACKs (architecture §3.3 (2)).
    /// `hop` is asked for a 4-tuple once a segment for it is due and before the segment is built
    /// (`ip.md` §6.7), and the segment is then handed to `sink` with what `hop` answered, counting
    /// as sent only if `sink` framed it. What waits for its next hop spends nothing and is not asked
    /// again until [`Self::wake`]. A failed next hop drops it, counting `tcp.next-hop-failed`. A
    /// refused frame counts `tcp.frame-refused` and commits nothing: it is offered again at the next
    /// call, never in this one. Returns how many left.
    pub fn transmit_owed<T>(
        &mut self,
        now: Instant,
        credit: usize,
        mut hop: impl FnMut(&Tuple) -> Hop<T>,
        mut sink: impl FnMut(&Outgoing<'_>, T) -> bool,
    ) -> usize {
        let mut sent = 0usize;
        let mut refused = Parked::default();
        while sent < credit {
            if let Some(tuple) = self.stubs.pop_front() {
                let Some(&Entry::Stub(rst, _)) = self.demux.get(&tuple) else { continue };
                let mut way = Way { tuple, hop: &mut hop, sink: &mut sink, scratch: &mut self.scratch };
                match way.ask().and_then(|via| way.send(via, &Out::rst(&rst), NO_PAYLOAD)) {
                    Ok(()) => sent = sent.saturating_add(1),
                    Err(NotReady::Pending) => {
                        self.parked.entry(tuple.remote.addr).or_default().stubs.push(tuple);
                        continue;
                    }
                    Err(NotReady::Unreachable) => self.log.count(Counter::NextHopFailed),
                    Err(NotReady::Unframed) => {
                        self.log.count(Counter::FrameRefused);
                        refused.stubs.push(tuple);
                        continue;
                    }
                }
                if let Some(Entry::Stub(_, Some(tw))) = self.demux.remove(&tuple) {
                    self.enter_time_wait(tuple, tw, false);
                }
                continue;
            }
            if let Some(answer) = self.answers.pop_front() {
                let mut way = Way { tuple: answer.tuple, hop: &mut hop, sink: &mut sink, scratch: &mut self.scratch };
                match way.ask().and_then(|via| way.send(via, &Out::rst(&answer.rst), NO_PAYLOAD)) {
                    Ok(()) => sent = sent.saturating_add(1),
                    Err(NotReady::Pending) => {
                        self.parked.entry(answer.tuple.remote.addr).or_default().answers.push(answer);
                        self.parked_answers = self.parked_answers.saturating_add(1);
                    }
                    Err(NotReady::Unreachable) => self.log.count(Counter::NextHopFailed),
                    Err(NotReady::Unframed) => {
                        self.log.count(Counter::FrameRefused);
                        refused.answers.push(answer);
                    }
                }
                continue;
            }
            let Some(tuple) = self.tw_owed.pop_first() else { break };
            let Some(Entry::TimeWait(tw)) = self.demux.get(&tuple) else { continue };
            let ack = tw.ack(now);
            let mut way = Way { tuple, hop: &mut hop, sink: &mut sink, scratch: &mut self.scratch };
            match way.ask().and_then(|via| way.send(via, &ack, NO_PAYLOAD)) {
                Ok(()) => sent = sent.saturating_add(1),
                Err(NotReady::Pending) => self.parked.entry(tuple.remote.addr).or_default().time_waits.push(tuple),
                Err(NotReady::Unreachable) => self.log.count(Counter::NextHopFailed),
                Err(NotReady::Unframed) => {
                    self.log.count(Counter::FrameRefused);
                    refused.time_waits.push(tuple);
                }
            }
        }
        self.requeue(refused);
        sent
    }

    /// Connections offered to the caller's round since the last call, each once, in the order
    /// they became eligible: a connection is offered again only after [`Self::serve`] answered
    /// [`Served::Done`] for it.
    pub fn drain_eligible(&mut self) -> alloc::vec::Drain<'_, ConnId> {
        self.eligible.drain(..)
    }

    /// One segment of `id`, if one is due, asked for and built as in [`Self::transmit_owed`].
    /// A failed next hop fails a connect and is the soft error of any other connection
    /// (`ip.md` §9.6). [`Served::Done`] takes the connection out of the round: it has nothing
    /// due, waits for its next hop, ended, or `id` names nothing; it is offered again once it has
    /// something. [`Served::Refused`] commits nothing and leaves the connection the caller's.
    pub fn serve<T>(&mut self, now: Instant, id: ConnId, mut hop: impl FnMut(&Tuple) -> Hop<T>, mut sink: impl FnMut(&Outgoing<'_>, T) -> bool) -> Served {
        let index = id.index;
        let Some(conn) = slot(&mut self.conns, index, id.generation) else { return Served::Done };
        let tuple = conn.tuple;
        let mut way = Way { tuple, hop: &mut hop, sink: &mut sink, scratch: &mut self.scratch };
        let mut ctx = conn.ctx(now, &mut self.log);
        let next = match &mut conn.state {
            Tcb::SynSent(s) => s.next_segment(&conn.local, now, &mut way),
            Tcb::SynRcvd(s) => s.next_segment(&conn.local, now, &mut way),
            Tcb::Sync(s) => s.next_segment(&mut ctx, &mut way),
            Tcb::Ended(_) => Ok(false),
        };
        let served = match next {
            Ok(true) => Served::Sent,
            Ok(false) => {
                conn.queued = false;
                Served::Done
            }
            Err(NotReady::Pending) => {
                self.park(index, tuple.remote.addr);
                Served::Done
            }
            Err(NotReady::Unreachable) if matches!(conn.state, Tcb::SynSent(_)) => {
                self.log.count(Counter::NextHopFailed);
                self.end(index, Some(Failure::Unreachable(SoftError::Unreachable(UnreachableCode::Host))), None);
                return Served::Done;
            }
            Err(NotReady::Unreachable) => {
                conn.soft = Some(SoftError::Unreachable(UnreachableCode::Host));
                self.log.count(Counter::NextHopFailed);
                self.park(index, tuple.remote.addr);
                Served::Done
            }
            Err(NotReady::Unframed) => {
                self.log.count(Counter::FrameRefused);
                Served::Refused
            }
        };
        self.settle_deadline(index, now);
        served
    }

    /// Everything waiting for the next hop of `remote` asks again at the next opportunity: what
    /// [ip] answers for it may have changed. Nothing is told what it will answer.
    pub fn wake(&mut self, remote: Ipv4Addr) {
        if let Some(parked) = self.parked.remove(&remote) {
            self.parked_answers = self.parked_answers.saturating_sub(parked.answers.len());
            self.requeue(parked);
        }
    }

    /// Everything waiting for a next hop asks again: a route may have changed (`ip.md` §3.6).
    pub fn wake_all(&mut self) {
        for (_, parked) in core::mem::take(&mut self.parked) {
            self.parked_answers = self.parked_answers.saturating_sub(parked.answers.len());
            self.requeue(parked);
        }
    }

    fn requeue(&mut self, parked: Parked) {
        for tuple in parked.stubs.into_iter().rev() {
            self.stubs.push_front(tuple);
        }
        for answer in parked.answers.into_iter().rev() {
            self.answers.push_front(answer);
        }
        self.tw_owed.extend(parked.time_waits);
        for index in parked.conns {
            if let Some(conn) = value(&mut self.conns, index) {
                conn.queued = false;
            }
            self.offer(index);
        }
    }

    /// A connection waits for its next hop out of the round, still marked queued, so nothing but
    /// a wake offers it again.
    fn park(&mut self, index: u32, remote: Ipv4Addr) {
        self.parked.entry(remote).or_default().conns.push(index);
    }

    /// `leave` takes what waits no longer out of `remote`'s record, and a record left empty goes.
    fn unpark(&mut self, remote: Ipv4Addr, leave: impl FnOnce(&mut Parked)) {
        if let Some(parked) = self.parked.get_mut(&remote) {
            leave(parked);
            if parked.is_empty() {
                self.parked.remove(&remote);
            }
        }
    }

    fn settle_deadline(&mut self, index: u32, now: Instant) {
        let Some(conn) = value(&mut self.conns, index) else { return };
        let ctx = conn.ctx(now, &mut self.log);
        let deadline = conn.deadline(&ctx);
        if let Some(old) = core::mem::replace(&mut conn.deadline, deadline) {
            self.deadlines.remove(&(old, index));
        }
        if let Some(at) = deadline {
            self.deadlines.insert((at, index));
        }
    }
}

#[cfg(test)]
impl Tcp {
    /// Every synchronized connection, for the property tests' invariants.
    pub(crate) fn each_sync(&self) -> impl Iterator<Item = (Tuple, &Sync)> {
        self.conns.iter().filter_map(|s| s.value.as_ref()).filter_map(|c| match &c.state {
            Tcb::Sync(sync) => Some((c.tuple, &**sync)),
            _ => None,
        })
    }
}
