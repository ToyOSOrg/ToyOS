//! The invariants of tcp.md §18.21 and the scenarios tagged [prop], over seeded arbitrary runs:
//! two stacks exchanging data through a lossy, reordering, duplicating link, and one stack
//! against an adversary sending arbitrary segments. Every invariant is checked after every step.

extern crate std;

use alloc::vec;
use alloc::vec::Vec;
use core::net::Ipv4Addr;
use core::time::Duration;
use std::collections::HashMap;

use toyos_net_wire::checksum::{Accumulator, PseudoHeader};
use toyos_net_wire::ipv4::{Form, Ipv4Builder, Ipv4Packet, Ipv4Source, Protocol, TrafficClass, Ttl};
use toyos_net_wire::tcp::TcpSegment;
use toyos_net_wire::Port;

use crate::seq::Seq;
use crate::{Config, ConnId, Endpoint, Error, Event, Instant, Received, Secrets, Tcp, Tuple};

const ADDR: [Ipv4Addr; 2] = [Ipv4Addr::new(192, 0, 2, 1), Ipv4Addr::new(192, 0, 2, 2)];

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
}

fn stack(seed: u8) -> Tcp {
    let key = |k: u8| -> [u8; 16] { core::array::from_fn(|i| k.wrapping_add(seed).wrapping_add(i as u8)) };
    Tcp::new(Config {
        mtu: 1500,
        receive_buffer: 65_535,
        send_buffer: 65_535,
        secrets: Secrets { isn: key(0), timestamp: key(0x10), port_offset: key(0x20), port_index: key(0x30), port_table: [0; 16] },
    })
}

fn datagram(out: &crate::Outgoing<'_>) -> Vec<u8> {
    let builder = Ipv4Builder {
        source: Ipv4Source::new(out.source).unwrap(),
        destination: out.destination,
        ttl: Ttl::DEFAULT,
        traffic_class: TrafficClass::ZERO,
        form: Form::Atomic,
        options: &[],
        payload: out.segment,
    };
    let mut buffer = vec![0u8; 2048];
    builder.emit(&mut buffer).unwrap().to_vec()
}

/// What the property checks read off a segment on the wire.
#[derive(Clone, Debug)]
struct Seen {
    seq: Seq,
    ack: Option<Seq>,
    flags: u8,
    window: u16,
    urgent: u16,
    ts: Option<(u32, u32)>,
    sack: Vec<(Seq, Seq)>,
    len: u32,
}

fn seen(bytes: &[u8]) -> Seen {
    let ip = Ipv4Packet::parse(bytes).unwrap();
    let tcp = TcpSegment::parse(&ip).unwrap();
    Seen {
        seq: tcp.sequence().into(),
        ack: tcp.acknowledgment().map(Seq::from),
        flags: tcp.flags().bits(),
        window: tcp.window().0,
        urgent: u16::from_be_bytes([tcp.header()[18], tcp.header()[19]]),
        ts: tcp.options().timestamps().map(|t| (t.value, t.echo)),
        sack: tcp.options().sack_blocks().map(|b| (b.left.into(), b.right.into())).collect(),
        len: u32::try_from(tcp.payload().len()).unwrap(),
    }
}

/// One direction's application: what it wrote and what the other end read.
#[derive(Default)]
struct Stream {
    written: Vec<u8>,
    read: Vec<u8>,
    end: Option<Result<(), Error>>,
    shut: bool,
}

struct Pair {
    rng: Rng,
    now: u64,
    tcp: [Tcp; 2],
    id: [Option<ConnId>; 2],
    link: Vec<(u64, u64, usize, Vec<u8>)>,
    order: u64,
    /// `streams[i]`: what node i writes and node 1 − i reads.
    streams: [Stream; 2],
    loss: u64,
    duplicate: u64,
    jitter: u64,
    /// Data segments are never lost or reordered: only ACKs suffer.
    only_acks: bool,
    edges: HashMap<(usize, Tuple), Seq>,
    /// Every transmission of sequence space: node, start, end, time, RTO in force.
    sends: Vec<(usize, Seq, Seq, u64, Duration)>,
    expiries: [u64; 2],
    /// The oldest unacknowledged sequence number at each node's last expiry, until it leaves again.
    expired: [Option<Seq>; 2],
    judged: u64,
    sacked: [Vec<(Seq, Seq)>; 2],
    aborted: [bool; 2],
    /// How often, in percent of steps, an application reads: a slow reader shuts windows.
    reading: u64,
    /// Segments come from an adversary too: what arrives is no longer only what the peer sent.
    adversary: bool,
    /// Each node's RTO over time, as the timer saw it.
    rtos: [Vec<(u64, Duration)>; 2],
}

fn ms(n: u64) -> u64 {
    n * 1_000_000
}

impl Pair {
    fn new(seed: u64) -> Self {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let reading = 5 + rng.below(90);
        let loss = rng.below(8);
        let duplicate = rng.below(5);
        let jitter = rng.below(20);
        let mut pair = Self {
            rng,
            now: ms(3_600_000),
            tcp: [stack(0), stack(0x80)],
            id: [None, None],
            link: Vec::new(),
            order: 0,
            streams: [Stream::default(), Stream::default()],
            loss,
            duplicate,
            jitter,
            only_acks: false,
            edges: HashMap::new(),
            sends: Vec::new(),
            expiries: [0, 0],
            expired: [None, None],
            judged: 0,
            sacked: [Vec::new(), Vec::new()],
            aborted: [false, false],
            reading,
            adversary: false,
            rtos: [Vec::new(), Vec::new()],
        };
        let listener = pair.tcp[1].listen(ADDR[1], Port::new(80), || 0).unwrap();
        let now = pair.instant();
        pair.id[0] = Some(pair.tcp[0].connect(now, ADDR[0], None, Endpoint { addr: ADDR[1], port: Port::new(80).unwrap() }).unwrap());
        for _ in 0..2000 {
            pair.step_time(ms(5));
            if let Some(id) = pair.tcp[1].accept(listener).unwrap() {
                pair.id[1] = Some(id);
                break;
            }
        }
        assert!(pair.id[1].is_some(), "the handshake completed");
        pair
    }

    fn instant(&self) -> Instant {
        Instant::from_nanos(self.now)
    }

    fn transmit(&mut self, node: usize, credit: usize) {
        let now = self.instant();
        let mut out = Vec::new();
        self.tcp[node].transmit(now, credit, |o| out.push(datagram(o)));
        for bytes in out {
            let s = seen(&bytes);
            self.check_segment(node, &s);
            let data = s.len > 0;
            if data || s.flags & 0x03 != 0 {
                self.record_send(node, &s);
            }
            let drop = if self.only_acks { !data && self.rng.chance(self.loss * 5) } else { self.rng.chance(self.loss) };
            if drop {
                continue;
            }
            let copies = if !self.only_acks && self.rng.chance(self.duplicate) { 2 } else { 1 };
            for _ in 0..copies {
                let jitter = if self.only_acks && data { 0 } else { self.rng.below(self.jitter + 1) };
                self.order += 1;
                self.link.push((self.now + ms(5 + jitter), self.order, 1 - node, bytes.clone()));
            }
        }
    }

    /// RT-17 judges what the timer retransmits: the segment that was oldest when it expired
    /// (RFC 6298 (5.4)). What follows it is §8.8's resending of the rest as cwnd allows, which
    /// after a spurious expiry resends segments sooner than an RTO after they left; telling a
    /// spurious expiry is stage 8's (RFC 3522, 5682).
    fn record_send(&mut self, node: usize, s: &Seen) {
        let Some((_, sync)) = self.tcp[node].each_sync().next() else { return };
        let rto = sync.rtt.rto();
        let end = s.seq.add(s.len).add(u32::from(s.flags & 0x01 != 0));
        if let Some(oldest) = self.expired[node].filter(|&x| x.within(s.seq, end.since(s.seq))) {
            self.expired[node] = None;
            let previous = self.sends.iter().rev().find(|&&(n, a, b, _, _)| n == node && oldest.within(a, b.since(a)));
            if let Some(&(_, _, _, at, rto)) = previous.filter(|_| self.only_acks) {
                let gap = Duration::from_nanos(self.now - at);
                let lowest = self.rtos[node].iter().filter(|&&(t, _)| t >= at).map(|&(_, r)| r).fold(rto, Duration::min);
                assert!(gap >= lowest, "RT-17: retransmitted {gap:?} after the previous transmission, the RTO never below {lowest:?}");
                self.judged += 1;
            }
        }
        self.sends.push((node, s.seq, end, self.now, rto));
    }

    /// MOD-04, OP-25 and PROP-04 on each segment as it leaves.
    fn check_segment(&mut self, node: usize, s: &Seen) {
        assert_eq!(s.flags & 0x20, 0, "MOD-04: URG set");
        assert_eq!(s.urgent, 0, "MOD-04: an urgent pointer");
        let Some((tuple, sync)) = self.tcp[node].each_sync().next() else { return };
        if sync.ts.is_some() && s.flags & 0x06 == 0 {
            assert!(s.ts.is_some(), "OP-25: a segment without timestamps on a timestamped connection");
        }
        if let (Some(ack), true) = (s.ack, s.flags & 0x06 == 0) {
            let shift = u32::from(sync.rx.shift);
            let advertised = ack.add(u32::from(s.window) << shift);
            let edge = sync.rx.edge();
            assert!(advertised.at_or_before(edge) && edge.since(advertised) < 1 << shift, "PROP-04: advertised {advertised:?}, edge {edge:?}");
            let _ = tuple;
        }
        if s.flags & 0x04 == 0 {
            self.sacked[node].extend(s.sack.iter().copied());
        }
    }

    fn deliver_due(&mut self) {
        loop {
            let due = self.link.iter().enumerate().filter(|(_, f)| f.0 <= self.now).min_by_key(|(_, f)| (f.0, f.1)).map(|(i, _)| i);
            let Some(i) = due else { break };
            let (_, _, to, bytes) = self.link.swap_remove(i);
            let ip = Ipv4Packet::parse(&bytes).unwrap();
            let tcp = TcpSegment::parse(&ip).unwrap();
            let now = self.instant();
            self.tcp[to].receive(now, ip.source(), ip.destination(), &tcp, |_| true);
            self.check();
            self.transmit(to, usize::MAX);
        }
    }

    /// PROP-08: a call before the deadline changes nothing; then time moves to it.
    fn step_time(&mut self, limit: u64) {
        let target = self.now + limit;
        loop {
            let link = self.link.iter().map(|f| f.0).min();
            let timers = self.tcp.iter().filter_map(|t| t.next_deadline().map(|d| d.nanos())).min();
            let Some(next) = [link, timers].into_iter().flatten().min().filter(|&n| n <= target) else { break };
            if let Some(deadline) = timers.filter(|&d| d > self.now + 1) {
                let before = self.now + self.rng.below(deadline - self.now - 1) + 1;
                for node in 0..2 {
                    let snapshot = self.snapshot(node);
                    self.tcp[node].fire(Instant::from_nanos(before.min(deadline - 1)));
                    assert_eq!(snapshot, self.snapshot(node), "PROP-08: firing before the deadline changed state");
                }
            }
            self.now = next.max(self.now);
            for node in 0..2 {
                let now = self.instant();
                self.tcp[node].fire(now);
            }
            self.check();
            self.deliver_due();
            for node in 0..2 {
                self.transmit(node, usize::MAX);
            }
        }
        self.now = target;
        for node in 0..2 {
            let now = self.instant();
            self.tcp[node].fire(now);
            self.transmit(node, usize::MAX);
        }
    }

    fn snapshot(&mut self, node: usize) -> (Vec<Option<crate::Info>>, Vec<u64>, Option<Instant>) {
        let infos = self.id.iter().flatten().map(|&id| self.tcp[node].info(id)).collect();
        let counters = self.tcp[node].counters().iter().map(|(_, v)| v).collect();
        (infos, counters, self.tcp[node].next_deadline())
    }

    /// PROP-01, 04, 05, 06 and 07 on every synchronized connection.
    fn check(&mut self) {
        for node in 0..2 {
            for (tuple, sync) in self.tcp[node].each_sync() {
                let expiries = self.tcp[node].counters().get(crate::Counter::Rto);
                if expiries > core::mem::replace(&mut self.expiries[node], expiries) {
                    self.expired[node] = Some(sync.tx.una);
                }
                if self.expired[node].is_some_and(|x| x.before(sync.tx.una)) {
                    self.expired[node] = None;
                }
                let rto = sync.rtt.rto();
                if self.rtos[node].last().is_none_or(|&(_, r)| r != rto) {
                    self.rtos[node].push((self.now, rto));
                }
                let tx = &sync.tx;
                assert!(tx.una.at_or_before(tx.nxt), "PROP-01: SND.UNA past SND.NXT");
                let edge = sync.rx.edge();
                if let Some(previous) = self.edges.insert((node, tuple), edge) {
                    assert!(previous.at_or_before(edge), "PROP-04: the right edge retreated");
                }
                assert!(tx.sacked().iter().all(|&(start, _)| start.at_or_after(tx.una)), "PROP-05: SACKed below SND.UNA: una {:?} nxt {:?} ranges {:?}", tx.una, tx.nxt, tx.sacked());
                assert!(sync.rx.ranges() <= 32 && tx.sacked().len() <= 64 && sync.rx.dup_owed <= 3, "PROP-06: a bound");
                if let Some(high_rxt) = sync.high_rxt() {
                    let pipe = tx.pipe(high_rxt, sync.smss());
                    let resent = if high_rxt.after(tx.una) { high_rxt.since(tx.una) } else { 0 };
                    assert!(pipe <= tx.flight().saturating_add(resent), "PROP-07: pipe {pipe} above FlightSize {} + {resent}", tx.flight());
                }
            }
            let events: Vec<Event> = self.tcp[node].drain_events().collect();
            for event in events {
                if let Event::Refused(r) = event {
                    assert!(self.tcp[node].counters().get(r.rule) > 0, "PROP-06: a refusal not counted");
                }
            }
        }
    }

    /// The applications: write, read, shut, now and then abort.
    fn apps(&mut self, abort: bool) {
        for node in 0..2 {
            let Some(id) = self.id[node] else { continue };
            let now = self.instant();
            if self.streams[node].written.len() < 200_000 && !self.streams[node].shut && self.rng.chance(60) {
                let n = 1 + self.rng.below(8000) as usize;
                let data: Vec<u8> = (0..n).map(|_| self.rng.next() as u8).collect();
                if let Ok(k) = self.tcp[node].send(now, id, &data) {
                    self.streams[node].written.extend_from_slice(&data[..k]);
                }
            }
            if !self.streams[node].shut && (self.streams[node].written.len() >= 200_000 || self.rng.chance(1)) && self.tcp[node].shutdown_write(now, id).is_ok() {
                self.streams[node].shut = true;
            }
            if abort && !self.aborted[node] && self.rng.chance(1) {
                self.tcp[node].abort(now, id).unwrap();
                self.aborted[node] = true;
                self.id[node] = None;
                continue;
            }
            let other = 1 - node;
            if self.streams[other].end.is_none() && self.rng.chance(self.reading) {
                let mut buf = vec![0u8; 1 + self.rng.below(20_000) as usize];
                match self.tcp[node].recv(now, id, &mut buf) {
                    Ok(Received::Data(n)) => {
                        self.streams[other].read.extend_from_slice(&buf[..n]);
                        let stream = &self.streams[other];
                        assert!(self.adversary || stream.written.starts_with(&stream.read), "PROP-02: delivered data is not a prefix of what was sent");
                    }
                    Ok(Received::End) => {
                        let stream = &self.streams[other];
                        assert!(self.adversary || (stream.shut && stream.read == stream.written), "PROP-03: end of stream before every byte");
                        self.streams[other].end = Some(Ok(()));
                    }
                    Err(Error::WouldBlock) => {}
                    Err(e) => {
                        assert!(self.aborted[other] || matches!(e, Error::Failed(_)), "{e:?}");
                        self.streams[other].end = Some(Err(e));
                    }
                }
            }
        }
    }

    fn run(&mut self, steps: usize, abort: bool) {
        for _ in 0..steps {
            self.apps(abort);
            match self.rng.below(4) {
                0 => {
                    let node = self.rng.below(2) as usize;
                    let credit = self.rng.below(6) as usize;
                    self.transmit(node, credit);
                }
                _ => {
                    let limit = ms(self.rng.below(60));
                    self.step_time(limit);
                }
            }
            self.check();
        }
    }

    /// Lets the connection finish on a clean link and checks what both ends delivered.
    fn finish(&mut self) {
        self.loss = 0;
        self.duplicate = 0;
        for _ in 0..20_000 {
            if self.streams.iter().all(|s| s.end.is_some()) {
                break;
            }
            for node in 0..2 {
                if let Some(id) = self.id[node] {
                    if !self.streams[node].shut {
                        let now = self.instant();
                        self.tcp[node].shutdown_write(now, id).unwrap();
                        self.streams[node].shut = true;
                    }
                }
            }
            self.apps(false);
            self.step_time(ms(50));
        }
        for (node, stream) in self.streams.iter().enumerate() {
            assert_eq!(stream.end, Some(Ok(())), "stream {node} did not end cleanly");
            assert_eq!(stream.read, stream.written);
            let delivered = u32::try_from(stream.read.len()).unwrap();
            let receiver = 1 - node;
            let base = self.sends.iter().find(|s| s.0 == node).map(|s| s.1).unwrap();
            for &(left, right) in &self.sacked[receiver] {
                if left.since(base) < 1 << 30 {
                    assert!(right.since(base) <= delivered.saturating_add(2), "RX-26: SACKed bytes never delivered");
                }
            }
        }
    }
}

const RUNS: u64 = 40;

/// Every invariant is checked after every step of every run; each test names the one its link
/// and applications are shaped to stress.
fn runs(salt: u64, shape: impl Fn(&mut Pair), abort: bool) {
    for seed in 0..RUNS {
        let mut pair = Pair::new(salt ^ seed);
        shape(&mut pair);
        pair.run(1500, abort);
        if !abort {
            pair.finish();
        }
    }
}

#[test]
fn s_prop_001_snd_una_at_or_before_snd_nxt() {
    runs(0x9e37_79b9, |p| p.loss = 15, false);
}

#[test]
fn s_prop_002_delivered_data_is_a_prefix() {
    runs(0x7f4a_7c15, |p| (p.duplicate, p.jitter) = (20, 40), false);
}

/// Half the runs abort: a reset is never taken for an end of stream.
#[test]
fn s_prop_003_end_of_stream_after_every_byte() {
    runs(0x51_7cc1, |_| {}, false);
    runs(0x51_7cc2, |_| {}, true);
}

#[test]
fn s_prop_004_the_right_edge_never_retreats() {
    runs(0x2545_f491, |p| p.reading = 3, false);
}

#[test]
fn s_prop_005_the_scoreboard_stays_above_snd_una() {
    runs(0x6c62_272e, |p| (p.loss, p.jitter) = (10, 30), false);
}

#[test]
fn s_prop_007_pipe_is_bounded() {
    runs(0x5be0_cd19, |p| p.loss = 20, false);
}

#[test]
fn s_prop_008_nothing_happens_before_a_deadline() {
    runs(0x1f83_d9ab, |_| {}, false);
}

#[test]
fn s_mod_004_prop_urgent_is_never_sent() {
    runs(0x3c6e_f371, |p| p.loss = 5, false);
}

#[test]
fn s_rx_026_prop_sacked_bytes_are_delivered() {
    for seed in 0..RUNS {
        let mut pair = Pair::new(0x2545_f491 ^ seed);
        pair.loss = 10;
        pair.jitter = 30;
        pair.run(1000, false);
        pair.finish();
        assert!(pair.sacked.iter().any(|s| !s.is_empty()) || seed > 0, "the runs exercise SACK");
    }
}

#[test]
fn s_rt_017_prop_no_retransmission_before_an_rto() {
    let mut judged = 0;
    for seed in 0..RUNS {
        let mut pair = Pair::new(0x6a09_e667 ^ seed);
        pair.only_acks = true;
        pair.loss = 8;
        pair.jitter = 60;
        pair.run(1500, false);
        judged += pair.judged;
    }
    assert!(judged >= 10, "only {judged} timer retransmissions judged");
}

#[test]
fn s_op_025_prop_timestamps_on_every_segment() {
    for seed in 0..RUNS {
        let mut pair = Pair::new(0xbb67_ae85 ^ seed);
        pair.run(500, false);
        let node = (seed % 2) as usize;
        let Some(id) = pair.id[node] else { continue };
        let Some((_, sync)) = pair.tcp[node].each_sync().next() else { continue };
        let ts = sync.ts.expect("both ends offer timestamps");
        let (clock, recent) = (ts.clock(pair.instant()), ts.recent);
        let now = pair.instant();
        pair.tcp[node].abort(now, id).unwrap();
        let mut rst = Vec::new();
        pair.tcp[node].transmit(now, usize::MAX, |o| rst.push(datagram(o)));
        let rst: Vec<Seen> = rst.iter().map(|b| seen(b)).filter(|s| s.flags & 0x04 != 0).collect();
        if let Some(rst) = rst.first() {
            assert_eq!(rst.ts, Some((clock, recent)), "OP-25: the abort's RST");
        }
    }
}

/// One stack against segments an adversary makes up: near the connection's numbers and far from
/// them, any flags, any window, SACK blocks anywhere. Nothing panics, every bound holds.
#[test]
fn s_prop_006_totality_against_an_adversary() {
    for seed in 0..RUNS {
        let mut pair = Pair::new(0x3c6e_f372 ^ seed);
        pair.adversary = true;
        for _ in 0..3000 {
            pair.apps(false);
            if pair.rng.chance(70) {
                pair.inject();
            } else {
                let limit = ms(pair.rng.below(100));
                pair.step_time(limit);
            }
            pair.check();
        }
    }
}

impl Pair {
    fn inject(&mut self) {
        let target = self.rng.below(2) as usize;
        let Some((tuple, sync)) = self.tcp[target].each_sync().next().map(|(t, s)| (t, (s.rx.next, s.tx.una, s.tx.nxt))) else { return };
        let (rcv_nxt, una, nxt) = sync;
        let near = |rng: &mut Rng, base: Seq| -> Seq {
            match rng.below(4) {
                0 => Seq::new(rng.next() as u32),
                _ => base.add(rng.below(140_000) as u32).sub(70_000),
            }
        };
        let seq = near(&mut self.rng, rcv_nxt);
        let ack = if self.rng.chance(50) { una.add(self.rng.below(u64::from(nxt.since(una)) + 1) as u32) } else { near(&mut self.rng, una) };
        let flags = self.rng.next() as u8;
        let window = self.rng.next() as u16;
        let mut options = Vec::new();
        if self.rng.chance(50) {
            let v = self.rng.next() as u32;
            options.extend_from_slice(&[1, 1, 8, 10]);
            options.extend_from_slice(&v.to_be_bytes());
            options.extend_from_slice(&(self.rng.next() as u32).to_be_bytes());
        }
        if self.rng.chance(50) {
            let n = 1 + self.rng.below(3) as usize;
            options.extend_from_slice(&[1, 1, 5, 2 + 8 * n as u8]);
            for _ in 0..n {
                let left = near(&mut self.rng, una);
                let right = left.add(self.rng.below(20_000) as u32);
                options.extend_from_slice(&left.get().to_be_bytes());
                options.extend_from_slice(&right.get().to_be_bytes());
            }
        }
        let payload: Vec<u8> = (0..self.rng.below(1400)).map(|_| self.rng.next() as u8).collect();
        let mut tcp = Vec::new();
        tcp.extend_from_slice(&tuple.remote.port.get().to_be_bytes());
        tcp.extend_from_slice(&tuple.local.port.get().to_be_bytes());
        tcp.extend_from_slice(&seq.get().to_be_bytes());
        tcp.extend_from_slice(&ack.get().to_be_bytes());
        tcp.push((((20 + options.len()) / 4) as u8) << 4);
        tcp.push(flags);
        tcp.extend_from_slice(&window.to_be_bytes());
        tcp.extend_from_slice(&[0, 0, 0, 0]);
        tcp.extend_from_slice(&options);
        tcp.extend_from_slice(&payload);
        let pseudo = PseudoHeader { source: tuple.remote.addr, destination: tuple.local.addr, protocol: Protocol::Tcp, length: tcp.len() as u16 };
        let sum = pseudo.accumulator().feed(&tcp).sum().checksum().to_be_bytes();
        tcp[16..18].copy_from_slice(&sum);
        let mut ip = vec![0x45, 0, 0, 0, 0, 0, 0x40, 0, 64, 6, 0, 0];
        ip[2..4].copy_from_slice(&((20 + tcp.len()) as u16).to_be_bytes());
        ip.extend_from_slice(&tuple.remote.addr.octets());
        ip.extend_from_slice(&tuple.local.addr.octets());
        let sum = Accumulator::new().feed(&ip).sum().checksum().to_be_bytes();
        ip[10..12].copy_from_slice(&sum);
        ip.extend_from_slice(&tcp);
        let packet = Ipv4Packet::parse(&ip).unwrap();
        let segment = TcpSegment::parse(&packet).unwrap();
        let now = self.instant();
        self.tcp[target].receive(now, tuple.remote.addr, tuple.local.addr, &segment, |_| true);
        self.check();
        self.transmit(target, usize::MAX);
    }
}
