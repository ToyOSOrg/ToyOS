//! The scenario harness: stack A under test, peer B scripted by the test on a moved clock.
//!
//! Scenarios are written in the specification's numbers: A's ISS is 1000 and A's TSval at time t
//! is 1000 + t. A computes its ISS and timestamp offset with keyed SipHash, so the harness learns
//! each connection's two offsets from A's own SYN or SYN-ACK and translates every A-space number
//! both ways: outgoing SEQ and TSval, incoming ACK, SACK edges, TSecr and ICMP quotes. Modular
//! arithmetic keeps every relation a scenario tests.
//!
//! Incoming segments are built byte by byte, not with the wire crate's typed builders, so a test
//! can send what no correct stack sends (SYN with FIN, MSS 0, a shift of 15, TCP-MD5); they are
//! then parsed by the wire crate exactly as netstack will. The checksum here is written apart from the
//! crate's.

#![allow(dead_code)]

pub mod net;

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::net::Ipv4Addr;
use std::time::Duration;

use toyos_net_tcp::{Config, ConnId, Counter, Endpoint, Event, Hop, Info, Instant, ListenerId, Outgoing, Secrets, Served, Status, Tcp, Tuple};
use toyos_net_wire::ipv4::{Ipv4Builder, Ipv4Packet, Ipv4Source, TrafficClass, Ttl};
use toyos_net_wire::tcp::TcpSegment;
use toyos_net_wire::Port;

pub const A: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 1);
pub const B: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 2);

pub const SYN: u8 = 0x02;
pub const FIN: u8 = 0x01;
pub const RST: u8 = 0x04;
pub const PSH: u8 = 0x08;
pub const ACK: u8 = 0x10;
pub const URG: u8 = 0x20;
pub const ECE: u8 = 0x40;
pub const CWR: u8 = 0x80;

pub fn key(first: u8) -> [u8; 16] {
    core::array::from_fn(|i| first.wrapping_add(i as u8))
}

pub fn secrets() -> Secrets {
    Secrets { isn: key(0x00), timestamp: key(0x10), port_offset: key(0x20), port_index: key(0x30), port_table: [0; 16] }
}

pub fn config(receive_buffer: u32) -> Config {
    Config { mtu: 1500, receive_buffer, send_buffer: 65_535, secrets: secrets() }
}

pub fn port(n: u16) -> Port {
    Port::new(n).unwrap()
}

pub fn ep(addr: Ipv4Addr, p: u16) -> Endpoint {
    Endpoint { addr, port: port(p) }
}

/// RFC 1071, written apart from the wire crate.
pub fn oracle_sum(chunks: &[&[u8]]) -> u16 {
    let bytes: Vec<u8> = chunks.concat();
    let mut sum: u32 = 0;
    for pair in bytes.chunks(2) {
        sum += u32::from(pair[0]) << 8 | u32::from(*pair.get(1).unwrap_or(&0));
        while sum > 0xFFFF {
            sum = (sum & 0xFFFF) + (sum >> 16);
        }
    }
    !(sum as u16)
}

#[derive(Clone, Debug)]
pub enum Opt {
    Mss(u16),
    Ws(u8),
    SackOk,
    Ts(u32, u32),
    Sack(Vec<(u32, u32)>),
    Raw(Vec<u8>),
}

/// An incoming segment in the specification's numbers.
#[derive(Clone, Debug)]
pub struct S {
    pub seq: u32,
    pub ack: Option<u32>,
    pub flags: u8,
    pub wnd: u16,
    pub urg: u16,
    pub opts: Vec<Opt>,
    pub payload: Vec<u8>,
    pub from: Option<(Ipv4Addr, u16)>,
    pub to: Option<(Ipv4Addr, u16)>,
}

pub fn seg(seq: u32) -> S {
    S { seq, ack: None, flags: 0, wnd: 65_535, urg: 0, opts: Vec::new(), payload: Vec::new(), from: None, to: None }
}

/// The byte a well-behaved peer sends at sequence number `seq`: text is checkable end to end.
pub fn pattern(seq: u32) -> u8 {
    (seq.wrapping_mul(2_654_435_761) >> 24) as u8
}

impl S {
    pub fn ack(mut self, ack: u32) -> Self {
        self.ack = Some(ack);
        self.flags |= ACK;
        self
    }
    pub fn flags(mut self, flags: u8) -> Self {
        self.flags |= flags;
        self
    }
    pub fn syn(self) -> Self {
        self.flags(SYN)
    }
    pub fn fin(self) -> Self {
        self.flags(FIN)
    }
    pub fn rst(self) -> Self {
        self.flags(RST)
    }
    pub fn psh(self) -> Self {
        self.flags(PSH)
    }
    pub fn wnd(mut self, wnd: u16) -> Self {
        self.wnd = wnd;
        self
    }
    pub fn urg(mut self, pointer: u16) -> Self {
        self.urg = pointer;
        self.flags(URG)
    }
    pub fn len(mut self, n: usize) -> Self {
        let first = self.seq.wrapping_add(u32::from(self.flags & SYN != 0));
        self.payload = (0..n as u32).map(|i| pattern(first.wrapping_add(i))).collect();
        self
    }
    pub fn data(mut self, data: &[u8]) -> Self {
        self.payload = data.to_vec();
        self
    }
    pub fn opt(mut self, opt: Opt) -> Self {
        self.opts.push(opt);
        self
    }
    pub fn mss(self, mss: u16) -> Self {
        self.opt(Opt::Mss(mss))
    }
    pub fn ws(self, shift: u8) -> Self {
        self.opt(Opt::Ws(shift))
    }
    pub fn sackok(self) -> Self {
        self.opt(Opt::SackOk)
    }
    pub fn ts(self, value: u32, echo: u32) -> Self {
        self.opt(Opt::Ts(value, echo))
    }
    pub fn sack(self, blocks: &[(u32, u32)]) -> Self {
        self.opt(Opt::Sack(blocks.to_vec()))
    }
    pub fn from(mut self, addr: Ipv4Addr, port: u16) -> Self {
        self.from = Some((addr, port));
        self
    }
    pub fn to(mut self, addr: Ipv4Addr, port: u16) -> Self {
        self.to = Some((addr, port));
        self
    }

    /// The segment's bytes, with A-space numbers moved by `seq_delta` and `ts_delta`.
    pub fn bytes(&self, from: (Ipv4Addr, u16), to: (Ipv4Addr, u16), seq_delta: u32, ts_delta: u32) -> Vec<u8> {
        let mut options = Vec::new();
        for opt in &self.opts {
            match opt {
                Opt::Mss(mss) => options.extend_from_slice(&[2, 4, (mss >> 8) as u8, *mss as u8]),
                Opt::Ws(shift) => options.extend_from_slice(&[1, 3, 3, *shift]),
                Opt::SackOk => options.extend_from_slice(&[1, 1, 4, 2]),
                Opt::Ts(value, echo) => {
                    let echo = if self.flags & ACK != 0 { echo.wrapping_add(ts_delta) } else { *echo };
                    options.extend_from_slice(&[1, 1, 8, 10]);
                    options.extend_from_slice(&value.to_be_bytes());
                    options.extend_from_slice(&echo.to_be_bytes());
                }
                Opt::Sack(blocks) => {
                    options.extend_from_slice(&[1, 1, 5, 2 + 8 * blocks.len() as u8]);
                    for (l, r) in blocks {
                        options.extend_from_slice(&l.wrapping_add(seq_delta).to_be_bytes());
                        options.extend_from_slice(&r.wrapping_add(seq_delta).to_be_bytes());
                    }
                }
                Opt::Raw(raw) => options.extend_from_slice(raw),
            }
        }
        while options.len() % 4 != 0 {
            options.push(0);
        }
        let header_len = 20 + options.len();
        let mut tcp = Vec::new();
        tcp.extend_from_slice(&from.1.to_be_bytes());
        tcp.extend_from_slice(&to.1.to_be_bytes());
        tcp.extend_from_slice(&self.seq.to_be_bytes());
        tcp.extend_from_slice(&self.ack.map_or(0, |a| a.wrapping_add(seq_delta)).to_be_bytes());
        tcp.push(((header_len / 4) as u8) << 4);
        tcp.push(self.flags);
        tcp.extend_from_slice(&self.wnd.to_be_bytes());
        tcp.extend_from_slice(&[0, 0]);
        tcp.extend_from_slice(&self.urg.to_be_bytes());
        tcp.extend_from_slice(&options);
        tcp.extend_from_slice(&self.payload);
        let length = (tcp.len() as u16).to_be_bytes();
        let checksum = oracle_sum(&[&from.0.octets(), &to.0.octets(), &[0, 6], &length, &tcp]);
        tcp[16..18].copy_from_slice(&checksum.to_be_bytes());
        let total = (20 + tcp.len()) as u16;
        let mut ip = vec![0x45, 0, (total >> 8) as u8, total as u8, 0, 0, 0x40, 0, 64, 6, 0, 0];
        ip.extend_from_slice(&from.0.octets());
        ip.extend_from_slice(&to.0.octets());
        let checksum = oracle_sum(&[&ip]);
        ip[10..12].copy_from_slice(&checksum.to_be_bytes());
        ip.extend_from_slice(&tcp);
        ip
    }
}

/// A segment A sent, parsed back by the wire crate, in the specification's numbers.
#[derive(Clone, Debug, PartialEq)]
pub struct O {
    pub t: i64,
    pub src: (Ipv4Addr, u16),
    pub dst: (Ipv4Addr, u16),
    pub seq: u32,
    pub ack: Option<u32>,
    pub flags: u8,
    pub wnd: u16,
    pub urg: u16,
    pub tos: u8,
    pub df: bool,
    pub options: Vec<u8>,
    pub mss: Option<u16>,
    pub ws: Option<u8>,
    pub sackok: bool,
    pub ts: Option<(u32, u32)>,
    pub sack: Vec<(u32, u32)>,
    pub payload: Vec<u8>,
}

impl O {
    pub fn len(&self) -> u32 {
        self.payload.len() as u32 + u32::from(self.flags & SYN != 0) + u32::from(self.flags & FIN != 0)
    }
    pub fn is(&self, flags: u8) -> bool {
        self.flags & (SYN | ACK | FIN | RST | PSH | URG | ECE | CWR) == flags
    }
}

fn flag_names(flags: u8) -> String {
    let names = [(SYN, "SYN"), (FIN, "FIN"), (RST, "RST"), (PSH, "PSH"), (ACK, "ACK"), (URG, "URG"), (ECE, "ECE"), (CWR, "CWR")];
    names.iter().filter(|(f, _)| flags & f != 0).map(|(_, n)| *n).collect::<Vec<_>>().join(",")
}

impl fmt::Display for O {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "t={} <SEQ={}>", self.t, self.seq)?;
        if let Some(ack) = self.ack {
            write!(f, "<ACK={ack}>")?;
        }
        write!(f, "<CTL={}><WND={}><LEN={}>", flag_names(self.flags), self.wnd, self.payload.len())?;
        if let Some(mss) = self.mss {
            write!(f, "[MSS {mss}]")?;
        }
        if self.sackok {
            write!(f, "[SACKOK]")?;
        }
        if let Some((v, e)) = self.ts {
            write!(f, "[TS {v}/{e}]")?;
        }
        if let Some(ws) = self.ws {
            write!(f, "[WS {ws}]")?;
        }
        if !self.sack.is_empty() {
            let blocks: Vec<String> = self.sack.iter().map(|(l, r)| format!("{l}-{r}")).collect();
            write!(f, "[SACK {}]", blocks.join(", "))?;
        }
        Ok(())
    }
}

fn number(text: &str) -> u32 {
    match text.strip_prefix("0x") {
        Some(hex) => u32::from_str_radix(hex, 16).unwrap(),
        None => text.parse::<i64>().map(|n| n as u32).unwrap_or_else(|_| panic!("number {text:?}")),
    }
}

/// Checks only the fields `spec` names: `SEQ=` `ACK=` (`-` for none) `CTL=` (the exact flag
/// set) `WND=` `LEN=` `TS=v/e` (`*` for any) `SACK=l-r,l-r` (`-` for none) `MSS=` `WS=` (`-`
/// for none) `SACKOK` `NOSACKOK` `NOOPT`.
pub fn check(out: &O, spec: &str) {
    for token in spec.split_whitespace() {
        let (key, value) = token.split_once('=').unwrap_or((token, ""));
        let ok = match key {
            "SEQ" => out.seq == number(value),
            "ACK" if value == "-" => out.ack.is_none(),
            "ACK" => out.ack == Some(number(value)),
            "CTL" => {
                let want = value.split(',').fold(0u8, |f, n| {
                    f | match n {
                        "SYN" => SYN,
                        "ACK" => ACK,
                        "FIN" => FIN,
                        "RST" => RST,
                        "PSH" => PSH,
                        "URG" => URG,
                        other => panic!("flag {other}"),
                    }
                });
                out.is(want)
            }
            "WND" => u32::from(out.wnd) == number(value),
            "LEN" => out.payload.len() as u32 == number(value),
            "TS" if value == "-" => out.ts.is_none(),
            "TS" => {
                let (v, e) = value.split_once('/').unwrap();
                out.ts.is_some_and(|(tv, te)| (v == "*" || tv == number(v)) && (e == "*" || te == number(e)))
            }
            "SACK" if value == "-" => out.sack.is_empty(),
            "SACK" => {
                let want: Vec<(u32, u32)> = value
                    .split(',')
                    .map(|b| {
                        let (l, r) = b.split_once('-').unwrap();
                        (number(l), number(r))
                    })
                    .collect();
                out.sack == want
            }
            "MSS" => out.mss == Some(number(value) as u16),
            "WS" if value == "-" => out.ws.is_none(),
            "WS" => out.ws == Some(number(value) as u8),
            "SACKOK" => out.sackok,
            "NOSACKOK" => !out.sackok,
            "NOOPT" => out.options.is_empty(),
            other => panic!("unknown check {other}"),
        };
        assert!(ok, "{token} fails on {out}");
    }
}

/// Exactly `specs.len()` segments, each checked in order.
#[track_caller]
pub fn expect(outs: &[O], specs: &[&str]) {
    let shown: Vec<String> = outs.iter().map(ToString::to_string).collect();
    assert_eq!(outs.len(), specs.len(), "segments out: {shown:#?}");
    for (out, spec) in outs.iter().zip(specs) {
        check(out, spec);
    }
}

#[track_caller]
pub fn nothing(outs: &[O]) {
    expect(outs, &[]);
}

/// The IPv4 datagram the shell would build around a segment: DF, DSCP 0, ECN 0.
pub fn datagram(out: &Outgoing<'_>) -> Vec<u8> {
    let builder = Ipv4Builder {
        source: Ipv4Source::new(out.source).unwrap(),
        destination: out.destination,
        ttl: Ttl::DEFAULT,
        traffic_class: TrafficClass::ZERO,
        options: &[],
        payload: out.segment,
    };
    let mut buffer = vec![0u8; 65_536];
    builder.emit(&mut buffer).unwrap().to_vec()
}

pub fn parse_out(bytes: &[u8], t: i64) -> O {
    let ip = Ipv4Packet::parse(bytes).unwrap();
    let tcp = TcpSegment::parse(&ip).unwrap();
    let options = tcp.options();
    O {
        t,
        src: (ip.source(), tcp.source_port().get()),
        dst: (ip.destination(), tcp.destination_port().get()),
        seq: tcp.sequence().get(),
        ack: tcp.acknowledgment().map(|a| a.get()),
        flags: tcp.flags().bits(),
        wnd: tcp.window().0,
        urg: u16::from_be_bytes([tcp.header()[18], tcp.header()[19]]),
        tos: ip.traffic_class().byte(),
        df: ip.dont_fragment(),
        options: tcp.options_bytes().to_vec(),
        mss: options.mss(),
        ws: options.window_scale().map(|w| w.raw()),
        sackok: options.sack_permitted(),
        ts: options.timestamps().map(|t| (t.value, t.echo)),
        sack: options.sack_blocks().map(|b| (b.left.get(), b.right.get())).collect(),
        payload: tcp.payload().to_vec(),
    }
}

/// The spec's clock: t in milliseconds, possibly negative, an hour after the stack's origin.
pub const BASE_MS: u64 = 3_600_000;


#[derive(Clone, Copy, Debug, Default)]
struct Delta {
    seq: Option<u32>,
    ts: Option<u32>,
}

/// The answer to the hop question for a 4-tuple at spec time t (`ip.md` §6.7).
pub type Hops = Box<dyn FnMut(i64, &Tuple) -> Hop<()>>;

/// Stack A and its scripted peer.
pub struct H {
    pub tcp: Tcp,
    pub t: i64,
    now: Instant,
    base: u64,
    deltas: HashMap<(u16, Ipv4Addr, u16), Delta>,
    /// The ISS the next learned connection is written with in the test.
    pub iss: u32,
    pub log: Vec<O>,
    /// `None`: every opportunity has unlimited credit.
    pub credit: Option<usize>,
    pub reset_budget: bool,
    /// B's side of the fixture's connection, and A's.
    pub peer: (Ipv4Addr, u16),
    pub local: (Ipv4Addr, u16),
    pub conn: Option<ConnId>,
    pub listener: Option<ListenerId>,
    pub events: Vec<Event>,
    pub hop: Hops,
    /// Hop questions asked so far.
    pub asked: usize,
    /// The connections [`pull`] takes turns among.
    pub round: VecDeque<ConnId>,
}

impl H {
    pub fn new(receive_buffer: u32) -> Self {
        Self::with(config(receive_buffer))
    }

    pub fn with(config: Config) -> Self {
        Self {
            tcp: Tcp::new(config).unwrap(),
            t: 0,
            now: Instant::from_nanos(0),
            base: 0,
            deltas: HashMap::new(),
            iss: 1000,
            log: Vec::new(),
            credit: None,
            reset_budget: true,
            peer: (B, 80),
            local: (A, 49152),
            conn: None,
            listener: None,
            events: Vec::new(),
            hop: Box::new(|_, _| Hop::Ready(())),
            asked: 0,
            round: VecDeque::new(),
        }
    }

    pub fn now(&self) -> Instant {
        self.now
    }

    pub fn instant(&self, t: i64) -> Instant {
        Instant::from_nanos(self.base + (BASE_MS as i64 + t) as u64 * 1_000_000)
    }

    pub fn spec_t(&self, at: Instant) -> i64 {
        self.spec_time(at)
    }

    fn spec_time(&self, now: Instant) -> i64 {
        ((now.nanos() - self.base) / 1_000_000) as i64 - BASE_MS as i64
    }

    /// Moves the clock's origin so that A's ISN for `tuple` at spec time `t` is `iss` (RFC 6528:
    /// M + F, with M the 4 µs clock): the one way to make A's own sequence space wrap in a test.
    pub fn pin(&mut self, iss: u32, tuple: Tuple, t: i64) {
        let f = toyos_net_tcp::isn(&secrets().isn, &tuple, Instant::from_nanos(0)).get();
        let m = u64::from(iss.wrapping_sub(f));
        let period = (1u64 << 32) * 4_000;
        let at = (BASE_MS as i64 + t) as u64 * 1_000_000;
        self.base = (m * 4_000 + period - at % period) % period;
        self.iss = iss;
        self.start(t);
    }

    /// Moves the clock's origin so that A's TSval for `tuple` at spec time `t` is 1000 + t.
    pub fn pin_ts(&mut self, tuple: Tuple, t: i64) {
        let offset = toyos_net_tcp::ts_offset(&secrets().timestamp, &tuple);
        let base_ms = 1000u32.wrapping_sub(offset).wrapping_sub(BASE_MS as u32);
        self.base = u64::from(base_ms) * 1_000_000;
        self.start(t);
    }

    /// Sets the clock without firing anything: fixtures start before 0.
    pub fn start(&mut self, t: i64) {
        self.t = t;
        self.now = self.instant(t);
    }

    fn delta(&self, local_port: u16, remote: (Ipv4Addr, u16)) -> Delta {
        self.deltas.get(&(local_port, remote.0, remote.1)).copied().unwrap_or_default()
    }

    fn learn(&mut self, bytes: &[u8]) -> O {
        let mut out = parse_out(bytes, self.t);
        let key = (out.src.1, out.dst.0, out.dst.1);
        if out.flags & SYN != 0 && out.flags & RST == 0 {
            let seq = out.seq.wrapping_sub(self.iss);
            let entry = self.deltas.entry(key).or_default();
            entry.seq.get_or_insert(seq);
        }
        if let Some((value, _)) = out.ts {
            let t = self.t as u32;
            let entry = self.deltas.entry(key).or_default();
            if entry.ts.is_none() && value != 0 {
                entry.ts = Some(value.wrapping_sub(1000u32.wrapping_add(t)));
            }
        }
        let delta = self.delta(out.src.1, out.dst);
        out.seq = out.seq.wrapping_sub(delta.seq.unwrap_or(0));
        if let Some((value, echo)) = out.ts.as_mut() {
            if *value != 0 {
                *value = value.wrapping_sub(delta.ts.unwrap_or(0));
            }
            let _ = echo;
        }
        out
    }

    /// A transmit opportunity now; returns what left.
    pub fn transmit(&mut self) -> Vec<O> {
        let credit = self.credit.unwrap_or(usize::MAX);
        let mut raw = Vec::new();
        let (now, t, hop, asked, round) = (self.now, self.t, &mut self.hop, &mut self.asked, &mut self.round);
        let ask = |tuple: &Tuple| {
            *asked += 1;
            hop(t, tuple)
        };
        pull(&mut self.tcp, round, now, credit, ask, |out, ()| raw.push(datagram(out)));
        if let Some(c) = self.credit.as_mut() {
            *c = c.saturating_sub(raw.len());
        }
        let outs: Vec<O> = raw.iter().map(|b| self.learn(b)).collect();
        self.events.extend(self.tcp.drain_events());
        self.log.extend(outs.iter().cloned());
        outs
    }

    /// Moves the clock to `t`, firing each timer at its own instant with a transmit opportunity.
    pub fn at(&mut self, t: i64) -> Vec<O> {
        assert!(self.instant(t) >= self.now, "the clock moves forward: {:?} to {t}", self.now);
        let mut outs = Vec::new();
        while let Some(deadline) = self.tcp.next_deadline().filter(|&d| d <= self.instant(t)) {
            self.now = deadline.max(self.now);
            self.t = self.spec_time(self.now);
            self.tcp.fire(self.now);
            outs.extend(self.transmit());
            let next = self.tcp.next_deadline();
            assert!(next.is_none_or(|n| n > self.now), "the timer due at {deadline:?} did not fire");
        }
        self.t = t;
        self.now = self.instant(t);
        self.tcp.fire(self.now);
        outs.extend(self.transmit());
        outs
    }

    /// Delivers `s` at `t` from the fixture's peer (or `s.from`), then offers an opportunity.
    pub fn input(&mut self, t: i64, s: S) -> Vec<O> {
        let mut outs = self.at(t);
        outs.extend(self.deliver(s));
        outs
    }

    /// Delivers without a transmit opportunity: several arrivals in one receive pass.
    pub fn arrive(&mut self, s: S) {
        let from = s.from.unwrap_or(self.peer);
        let to = s.to.unwrap_or(self.local);
        let delta = self.delta(to.1, from);
        let bytes = s.bytes(from, to, delta.seq.unwrap_or(0), delta.ts.unwrap_or(0));
        let ip = Ipv4Packet::parse(&bytes).unwrap();
        let tcp = TcpSegment::parse(&ip).unwrap();
        let budget = self.reset_budget;
        self.tcp.receive(self.now(), from.0, to.0, &tcp, |_| budget);
    }

    pub fn deliver(&mut self, s: S) -> Vec<O> {
        self.arrive(s);
        self.transmit()
    }

    /// Forgets the fixture 4-tuple's offsets: what A sends next for it belongs to no incarnation.
    pub fn forget(&mut self) {
        self.deltas.remove(&(self.local.1, self.peer.0, self.peer.1));
    }

    pub fn id(&self) -> ConnId {
        self.conn.expect("the fixture's connection")
    }

    /// A user call at `t` on the fixture's connection, then an opportunity.
    pub fn call<R>(&mut self, t: i64, f: impl FnOnce(&mut Tcp, Instant, ConnId) -> R) -> (R, Vec<O>) {
        let mut outs = self.at(t);
        let now = self.now();
        let id = self.id();
        let r = f(&mut self.tcp, now, id);
        outs.extend(self.transmit());
        (r, outs)
    }

    pub fn send(&mut self, t: i64, n: usize) -> Vec<O> {
        let data: Vec<u8> = (0..n).map(|i| i as u8).collect();
        let (r, outs) = self.call(t, |tcp, now, id| tcp.send(now, id, &data));
        assert_eq!(r, Ok(n), "send({n})");
        outs
    }

    pub fn close(&mut self, t: i64) -> Vec<O> {
        let (r, outs) = self.call(t, |tcp, now, id| tcp.close(now, id));
        r.unwrap();
        outs
    }

    pub fn read(&mut self, n: usize) -> Vec<u8> {
        let mut buf = vec![0u8; n];
        let now = self.now();
        let id = self.id();
        match self.tcp.recv(now, id, &mut buf) {
            Ok(toyos_net_tcp::Received::Data(k)) => buf.truncate(k),
            other => panic!("recv: {other:?}"),
        }
        buf
    }

    /// The connection's variables, A's sequence numbers in the test's numbers.
    pub fn info(&mut self) -> Info {
        let id = self.id();
        let mut info = self.tcp.info(id).expect("a synchronized connection");
        info.snd_una = toyos_net_tcp::Seq::new(self.spec(info.snd_una.get()));
        info.snd_nxt = toyos_net_tcp::Seq::new(self.spec(info.snd_nxt.get()));
        info.high_rxt = info.high_rxt.map(|s| toyos_net_tcp::Seq::new(self.spec(s.get())));
        info.rescue_rxt = info.rescue_rxt.map(|s| toyos_net_tcp::Seq::new(self.spec(s.get())));
        info
    }

    pub fn status(&mut self) -> Status {
        let id = self.id();
        self.tcp.status(id).unwrap()
    }

    pub fn count(&self, counter: Counter) -> u64 {
        self.tcp.counters().get(counter)
    }

    /// Refusal events for `rule` so far.
    pub fn refusals(&self, rule: Counter) -> Vec<toyos_net_tcp::Refusal> {
        self.events
            .iter()
            .filter_map(|e| match e {
                Event::Refused(r) if r.rule == rule => Some(*r),
                _ => None,
            })
            .collect()
    }

    pub fn tuple(&self) -> Tuple {
        Tuple { local: ep(self.local.0, self.local.1), remote: ep(self.peer.0, self.peer.1) }
    }

    /// A number in A's sequence space, as the stack holds it.
    pub fn real(&self, spec: u32) -> u32 {
        spec.wrapping_add(self.delta(self.local.1, self.peer).seq.unwrap_or(0))
    }

    /// A number the stack holds, as the test writes it.
    pub fn spec(&self, real: u32) -> u32 {
        real.wrapping_sub(self.delta(self.local.1, self.peer).seq.unwrap_or(0))
    }
}

pub fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

// ---- fixtures ----

/// A connects from 49152 at t = −10; B's SYN-ACK arrives at 0 with `options`.
pub fn client(receive_buffer: u32, synack: S) -> H {
    let mut h = H::new(receive_buffer);
    h.start(-10);
    let id = h.tcp.connect(h.now(), A, Some(port(49152)), ep(B, 80)).unwrap();
    h.conn = Some(id);
    h.transmit();
    h.input(0, synack);
    h
}

/// Fixture E: established, bare, A is the client.
pub fn fixture_e() -> H {
    client(65_535, seg(5000).ack(1001).syn().wnd(65_535).mss(1460))
}

/// Fixture EF: established, full options, A is the client.
pub fn fixture_ef() -> H {
    client(65_535, seg(5000).ack(1001).syn().wnd(65_535).mss(1460).sackok().ts(50_000, 990).ws(7))
}

/// A listens on 80 at t = 0, for B's SYNs from 40000.
pub fn listening() -> H {
    let mut h = H::new(65_535);
    h.peer = (B, 40_000);
    h.local = (A, 80);
    h.start(0);
    h.listener = Some(h.tcp.listen(A, Some(port(80)), || 0).unwrap());
    h
}

/// A listens on 80; B's SYN from 40000 at −20, its ACK at 0; the user accepts.
pub fn server(syn: S, ack: S) -> H {
    let mut h = H::new(65_535);
    h.peer = (B, 40_000);
    h.local = (A, 80);
    h.start(-20);
    let listener = h.tcp.listen(A, Some(port(80)), || 0).unwrap();
    h.listener = Some(listener);
    h.input(-20, syn);
    h.input(0, ack);
    h.conn = Some(h.tcp.accept(listener).unwrap().expect("a ready child"));
    h
}

/// Fixture ES.
pub fn fixture_es() -> H {
    server(seg(5000).syn().wnd(65_535).mss(1460), seg(5001).ack(1001).wnd(65_535))
}

/// Fixture ESF.
pub fn fixture_esf() -> H {
    server(seg(5000).syn().wnd(65_535).mss(1460).sackok().ts(49_980, 0).ws(7), seg(5001).ack(1001).wnd(512).ts(50_000, 980))
}

/// "Ten segments outstanding": the user wrote exactly one initial window at t = 0.
pub fn ten_out(h: &mut H) -> Vec<O> {
    let smss = h.info().smss as usize;
    let outs = h.send(0, 10 * smss);
    assert_eq!(outs.len(), 10, "ten segments out");
    outs
}

/// B's segments in EF carry `<WND=512>` and `[TS 50000+t/latest]`.
pub fn b_full(h: &H, s: S) -> S {
    let latest = h.log.iter().rev().find_map(|o| o.ts.map(|(v, _)| v)).unwrap_or(0);
    s.wnd(512).ts(50_000u32.wrapping_add(h.t as u32), latest)
}

impl H {
    /// `input` for a timestamped peer: B's segment as fixture EF sends it (`b_full`).
    pub fn input_full(&mut self, t: i64, s: S) -> Vec<O> {
        let mut outs = self.at(t);
        let s = b_full(self, s);
        outs.extend(self.deliver(s));
        outs
    }

    /// `arrive` for a timestamped peer.
    pub fn arrive_full(&mut self, s: S) {
        let s = b_full(self, s);
        self.arrive(s);
    }
}

impl H {
    /// An ICMP error about the fixture's connection, quoting `seq` in the test's numbers.
    pub fn icmp(&mut self, t: i64, kind: toyos_net_tcp::IcmpKind, seq: u32) -> Vec<O> {
        let mut outs = self.at(t);
        let error = toyos_net_tcp::IcmpError {
            local: ep(self.local.0, self.local.1),
            remote: ep(self.peer.0, self.peer.1),
            sequence: toyos_net_tcp::Seq::new(self.real(seq)),
            kind,
        };
        let now = self.now();
        self.tcp.icmp(now, error);
        outs.extend(self.transmit());
        outs
    }
}

/// A destination-unreachable code as the wire crate parses it off a real message.
pub fn unreachable(code: u8) -> toyos_net_wire::icmp::UnreachableCode {
    let mut quote = vec![0x45, 0, 0, 40, 0, 0, 0x40, 0, 64, 6, 0, 0];
    quote.extend_from_slice(&A.octets());
    quote.extend_from_slice(&B.octets());
    quote.extend_from_slice(&[0; 8]);
    let mut icmp = vec![3, code, 0, 0, 0, 0, 0, 0];
    icmp.extend_from_slice(&quote);
    let sum = oracle_sum(&[&icmp]);
    icmp[2..4].copy_from_slice(&sum.to_be_bytes());
    match toyos_net_wire::icmp::IcmpPacket::parse(&icmp).unwrap().message() {
        toyos_net_wire::icmp::IcmpMessage::DestinationUnreachable { code, .. } => code,
        other => panic!("{other:?}"),
    }
}

/// A transmit opportunity as a shard composes one, with one segment per turn in place of its
/// byte round: what [tcp] owes outside a connection first, then each connection of `round` in
/// turn, `round` keeping its order between opportunities. Returns how many left.
pub fn pull<T>(
    tcp: &mut Tcp,
    round: &mut VecDeque<ConnId>,
    now: Instant,
    credit: usize,
    mut hop: impl FnMut(&Tuple) -> Hop<T>,
    mut sink: impl FnMut(&Outgoing<'_>, T),
) -> usize {
    let mut sent = tcp.transmit_owed(now, credit, &mut hop, &mut sink);
    for gone in tcp.drain_gone() {
        round.retain(|id| *id != gone);
    }
    round.extend(tcp.drain_eligible());
    while sent < credit {
        let Some(id) = round.pop_front() else { break };
        match tcp.serve(now, id, &mut hop, &mut sink) {
            Served::Sent => {
                sent += 1;
                round.push_back(id);
            }
            Served::Done => {}
        }
    }
    sent
}
