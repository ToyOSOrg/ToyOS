//! The invariants and the scenarios tagged [prop], over seeded runs of the test
//! network in `tests/common`: two stacks exchanging data through a lossy, reordering, duplicating
//! link, and one stack against an adversary sending arbitrary segments. Every invariant is checked
//! after every arrival, every firing and every segment handed off.

#[path = "../tests/common/mod.rs"]
mod common;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use common::net::{End, Fate, Impair, Net};
use common::{datagram, parse_out, seg, Opt, O, FIN, RST, SYN, URG};

use crate::seq::Seq;
use crate::{Counter, Endpoint, Event, Failure, Hop, Instant, Tcp, Tuple};

#[derive(Clone)]
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
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

/// What the link does, in percent per datagram, and how often, in percent of steps, an
/// application reads: a slow reader shuts windows.
#[derive(Clone, Copy)]
struct Shape {
    loss: u64,
    duplicate: u64,
    /// Up to this many milliseconds late: reordering.
    jitter: u64,
    /// Data is never lost or reordered; ACKs are lost five times as often.
    only_acks: bool,
    reading: u64,
    abort: bool,
    adversary: bool,
    /// Hop questions answer at random, a frame is now and then refused, and every flow is woken
    /// now and then.
    hops: bool,
}

impl Shape {
    fn new(rng: &mut Rng) -> Self {
        let reading = 5 + rng.below(90);
        Self { loss: rng.below(8), duplicate: rng.below(5), jitter: rng.below(20), only_acks: false, reading, abort: false, adversary: false, hops: false }
    }
}

fn link(mut rng: Rng, shape: Shape) -> Impair {
    Box::new(move |_, o| {
        let data = !o.payload.is_empty();
        if shape.only_acks {
            return match (data, rng.chance(shape.loss * 5)) {
                (true, _) => Fate::Pass,
                (false, true) => Fate::Drop,
                (false, false) => Fate::Late(rng.below(shape.jitter + 1)),
            };
        }
        if rng.chance(shape.loss) {
            Fate::Drop
        } else if rng.chance(shape.duplicate) {
            Fate::Duplicate
        } else {
            Fate::Late(rng.below(shape.jitter + 1))
        }
    })
}

/// What the checks remember between events.
struct Checker {
    rng: Rng,
    shape: Shape,
    edges: HashMap<(usize, Tuple), Seq>,
    /// The end of the highest sequence space each connection's segments carried out.
    left: HashMap<(usize, Tuple), Seq>,
    expiries: [u64; 2],
    /// The oldest unacknowledged sequence number at each node's last expiry, until it leaves again.
    expired: [Option<Seq>; 2],
    /// Every transmission of sequence space: node, start, end, time, RTO in force.
    sends: Vec<(usize, Seq, Seq, Instant, Duration)>,
    /// Each node's RTO over time, as the timer saw it.
    rtos: [Vec<(Instant, Duration)>; 2],
    judged: u64,
    /// The SACK blocks each node sent.
    sacked: [Vec<(Seq, Seq)>; 2],
}

impl Checker {
    fn check(&mut self, node: usize, tcp: &mut Tcp, now: Instant, seg: Option<&O>) {
        match seg {
            Some(o) => self.segment(node, tcp, now, o),
            None => {
                self.state(node, tcp, now);
                self.nothing_before_the_deadline(tcp, now);
            }
        }
    }

    /// PROP-01, 04, 05, 06 and 07 on every synchronized connection, and every refusal counted.
    fn state(&mut self, node: usize, tcp: &mut Tcp, now: Instant) {
        for (tuple, sync) in tcp.each_sync() {
            let expiries = tcp.counters().get(Counter::Rto);
            if expiries > core::mem::replace(&mut self.expiries[node], expiries) {
                self.expired[node] = Some(sync.tx.una);
            }
            if self.expired[node].is_some_and(|x| x.before(sync.tx.una)) {
                self.expired[node] = None;
            }
            let rto = sync.rtt.rto();
            if self.rtos[node].last().is_none_or(|&(_, r)| r != rto) {
                self.rtos[node].push((now, rto));
            }
            let tx = &sync.tx;
            assert!(tx.una.at_or_before(tx.nxt), "PROP-01: SND.UNA past SND.NXT");
            if let Some(&left) = self.left.get(&(node, tuple)) {
                assert!(tx.nxt.at_or_before(left), "§11.3: SND.NXT {:?} past what left, {left:?}", tx.nxt);
            }
            let edge = sync.rx.edge();
            if let Some(previous) = self.edges.insert((node, tuple), edge) {
                assert!(previous.at_or_before(edge), "PROP-04: the right edge retreated");
            }
            assert!(tx.sacked().iter().all(|&(start, _)| start.at_or_after(tx.una)), "PROP-05: SACKed below SND.UNA: {:?}", tx.sacked());
            assert!(sync.rx.ranges() <= 32 && tx.sacked().len() <= 64 && sync.rx.dup_owed <= 3, "PROP-06: a bound");
            if let Some((high_rxt, _)) = sync.sack_marks() {
                let resent = if high_rxt.after(tx.una) { high_rxt.since(tx.una) } else { 0 };
                assert!(sync.pipe() <= tx.flight().saturating_add(resent), "PROP-07: pipe {} above FlightSize {} + {resent}", sync.pipe(), tx.flight());
            }
        }
        let events: Vec<Event> = tcp.drain_events().collect();
        for event in events {
            if let Event::Refused(r) = event {
                assert!(tcp.counters().get(r.rule) > 0, "PROP-06: a refusal not counted");
            }
        }
        assert_eq!(tcp.counters().get(Counter::RtoUnsent), 0, "an RTO fired for a segment that had not left");
    }

    /// PROP-08: a call before the deadline changes nothing.
    fn nothing_before_the_deadline(&mut self, tcp: &mut Tcp, now: Instant) {
        let Some(deadline) = tcp.next_deadline().filter(|d| d.nanos() > now.nanos() + 1) else { return };
        let before = Instant::from_nanos(now.nanos() + 1 + self.rng.below(deadline.nanos() - now.nanos() - 1));
        let snapshot = |tcp: &Tcp| {
            let syncs: Vec<_> = tcp.each_sync().map(|(_, s)| (s.tx.una, s.tx.nxt, s.rx.next, s.rtx_timer, s.rtt.rto(), s.cc.cwnd, s.rx.ack_now)).collect();
            (syncs, tcp.counters().iter().map(|(_, v)| v).collect::<Vec<_>>(), tcp.next_deadline(), tcp.time_wait_count())
        };
        let was = snapshot(tcp);
        tcp.fire(before);
        assert_eq!(was, snapshot(tcp), "PROP-08: firing before the deadline changed state");
    }

    /// MOD-04, OP-25 and PROP-04 on each segment as it leaves, and RT-17's record.
    fn segment(&mut self, node: usize, tcp: &mut Tcp, now: Instant, o: &O) {
        if o.flags & RST == 0 {
            let endpoint = |(addr, port): (std::net::Ipv4Addr, u16)| Endpoint { addr, port: toyos_net_wire::Port::new(port).unwrap() };
            let end = Seq::new(o.seq).add(o.len());
            let left = self.left.entry((node, Tuple { local: endpoint(o.src), remote: endpoint(o.dst) })).or_insert(end);
            *left = left.later(end);
        }
        assert_eq!(o.flags & URG, 0, "MOD-04: URG set");
        assert_eq!(o.urg, 0, "MOD-04: an urgent pointer");
        let Some((_, sync)) = tcp.each_sync().next() else { return };
        if sync.ts.is_some() && o.flags & (SYN | RST) == 0 {
            assert!(o.ts.is_some(), "OP-25: a segment without timestamps on a timestamped connection");
        }
        if let (Some(ack), 0) = (o.ack, o.flags & (SYN | RST)) {
            let shift = u32::from(sync.rx.shift);
            let advertised = Seq::new(ack).add(u32::from(o.wnd) << shift);
            let edge = sync.rx.edge();
            assert!(advertised.at_or_before(edge) && edge.since(advertised) < 1 << shift, "PROP-04: advertised {advertised:?}, edge {edge:?}");
        }
        if o.flags & SYN == 0 {
            self.sacked[node].extend(o.sack.iter().map(|&(l, r)| (Seq::new(l), Seq::new(r))));
        }
        if !o.payload.is_empty() || o.flags & (SYN | FIN) != 0 {
            self.record_send(node, sync.rtt.rto(), now, o);
        }
    }

    /// RT-17 judges what the timer retransmits: the segment that was oldest when it expired
    /// (RFC 6298 (5.4)). What follows it is resending of the rest as cwnd allows, which
    /// after a spurious expiry resends segments sooner than an RTO after they left.
    fn record_send(&mut self, node: usize, rto: Duration, now: Instant, o: &O) {
        let start = Seq::new(o.seq);
        let end = start.add(o.len());
        if let Some(oldest) = self.expired[node].filter(|&x| x.within(start, end.since(start))) {
            self.expired[node] = None;
            let previous = self.sends.iter().rev().find(|&&(n, a, b, _, _)| n == node && oldest.within(a, b.since(a)));
            if let Some(&(_, _, _, at, rto)) = previous.filter(|_| self.shape.only_acks) {
                let gap = now.since(at);
                let lowest = self.rtos[node].iter().filter(|&&(t, _)| t >= at).map(|&(_, r)| r).fold(rto, Duration::min);
                assert!(gap >= lowest, "RT-17: retransmitted {gap:?} after the previous transmission, the RTO never below {lowest:?}");
                self.judged += 1;
            }
        }
        self.sends.push((node, start, end, now, rto));
    }
}

const RUNS: u64 = 40;
const STREAM: u64 = 200_000;

/// One connection on a shaped link, every event checked.
struct Run {
    net: Net,
    rng: Rng,
    shape: Shape,
    checker: Rc<RefCell<Checker>>,
}

impl Run {
    fn new(seed: u64, shape: impl FnOnce(&mut Shape)) -> Self {
        let mut rng = Rng::new(seed);
        let mut s = Shape::new(&mut rng);
        shape(&mut s);
        let checker = Rc::new(RefCell::new(Checker {
            rng: Rng::new(seed ^ 0x5555),
            shape: s,
            edges: HashMap::new(),
            left: HashMap::new(),
            expiries: [0, 0],
            expired: [None, None],
            sends: Vec::new(),
            rtos: [Vec::new(), Vec::new()],
            judged: 0,
            sacked: [Vec::new(), Vec::new()],
        }));
        let mut net = Net::new(10);
        net.keep_streams = true;
        net.impair = link(Rng::new(seed ^ 0xaaaa), s);
        let shared = Rc::clone(&checker);
        net.check = Some(Box::new(move |node, tcp, now, o| shared.borrow_mut().check(node, tcp, now, o)));
        let len = [(STREAM / 4 + rng.below(STREAM)) as usize, (STREAM / 4 + rng.below(STREAM)) as usize];
        net.connections(1, 80, len);
        assert!(net.run(10_000, |n| n.apps.len() == 2), "the handshake completed");
        Self { net, rng, shape: s, checker }
    }

    /// The applications write, read and now and then abort; the device's credit comes and goes.
    fn run(&mut self, steps: usize) {
        if self.shape.hops {
            let mut rng = self.rng.clone();
            self.net.hop = Some(Box::new(move |_| match rng.below(20) {
                0 => Hop::Unreachable,
                1..=3 => Hop::Pending,
                _ => Hop::Ready(()),
            }));
            let mut rng = Rng::new(self.rng.next());
            self.net.framed = Some(Box::new(move |_| rng.chance(95)));
        }
        for _ in 0..steps {
            if self.shape.hops && self.rng.chance(30) {
                self.net.nodes.iter_mut().for_each(|n| n.tcp.wake_all());
            }
            for app in &mut self.net.apps {
                app.write_limit = Some(if self.rng.chance(60) { 1 + self.rng.below(8000) as usize } else { 0 });
                app.reading = self.rng.chance(self.shape.reading);
                app.read_limit = Some(1 + self.rng.below(20_000) as usize);
            }
            if self.shape.abort && self.rng.chance(1) {
                let node = self.rng.below(2) as usize;
                self.abort(node);
            }
            for node in &mut self.net.nodes {
                node.credit_per_ms = self.rng.chance(25).then(|| self.rng.below(6) as usize);
            }
            if self.shape.adversary && self.rng.chance(70) {
                self.inject();
            }
            let ms = self.rng.below(60);
            self.net.advance(ms);
        }
    }

    fn abort(&mut self, node: usize) {
        let now = self.net.instant(node);
        let Some(app) = self.net.apps.iter_mut().find(|a| a.node == node && a.end.is_none()) else { return };
        self.net.nodes[node].tcp.abort(now, app.id).unwrap();
        app.end = Some(End::Failed(Failure::Reset));
    }

    /// Lets the connection finish on a clean link, and checks each direction arrived whole.
    fn finish(&mut self) {
        (self.net.hop, self.net.framed) = (None, None);
        self.net.nodes.iter_mut().for_each(|n| n.tcp.wake_all());
        self.net.impair = link(self.rng.clone(), Shape { loss: 0, duplicate: 0, ..self.shape });
        for app in &mut self.net.apps {
            (app.write_limit, app.read_limit, app.reading) = (None, None, true);
        }
        for node in &mut self.net.nodes {
            node.credit_per_ms = None;
        }
        assert!(self.net.run(600_000, Net::finished), "{}", self.net.dump());
        self.net.assert_exact();
    }

    /// PROP-02 and PROP-03 when a run may abort: each end read a prefix of what the other wrote,
    /// and saw its end of stream only after every byte.
    fn delivered_prefixes(&self) {
        for app in &self.net.apps {
            let peer = self.net.peer_of(app);
            let (read, written) = (app.received.2.as_ref().unwrap(), peer.sent.2.as_ref().unwrap());
            assert!(written.starts_with(read), "PROP-02: delivered data is not a prefix of what was sent");
            if app.end == Some(End::Fin) {
                assert!(peer.shut && read == written, "PROP-03: end of stream before every byte");
            }
        }
    }

    /// One segment an adversary makes up for either end: near the connection's numbers and far
    /// from them, any flags, any window, SACK blocks anywhere.
    fn inject(&mut self) {
        let target = self.rng.below(2) as usize;
        let rng = &mut self.rng;
        let Some((tuple, rcv_nxt, una, nxt)) = self.net.nodes[target].tcp.each_sync().next().map(|(t, s)| (t, s.rx.next, s.tx.una, s.tx.nxt)) else { return };
        fn near(rng: &mut Rng, base: Seq) -> Seq {
            match rng.below(4) {
                0 => Seq::new(rng.next() as u32),
                _ => base.add(rng.below(140_000) as u32).sub(70_000),
            }
        }
        let mut s = seg(near(rng, rcv_nxt).get()).flags(rng.next() as u8).wnd(rng.next() as u16);
        s.ack = Some(if rng.chance(50) { una.add(rng.below(u64::from(nxt.since(una)) + 1) as u32) } else { near(rng, una) }.get());
        if rng.chance(50) {
            s = s.opt(Opt::Ts(rng.next() as u32, rng.next() as u32));
        }
        if rng.chance(50) {
            let blocks = (0..1 + rng.below(3)).map(|_| {
                let left = near(rng, una);
                (left.get(), left.add(rng.below(20_000) as u32).get())
            });
            s = s.sack(&blocks.collect::<Vec<_>>());
        }
        let s = s.data(&(0..rng.below(1400)).map(|_| rng.next() as u8).collect::<Vec<_>>());
        let bytes = s.bytes((tuple.remote.addr, tuple.remote.port.get()), (tuple.local.addr, tuple.local.port.get()), 0, 0);
        self.net.inject(target, bytes);
    }
}

fn runs(salt: u64, shape: impl Fn(&mut Shape)) {
    for seed in 0..RUNS {
        let mut run = Run::new(salt ^ seed, &shape);
        run.run(1500);
        if run.shape.abort {
            run.delivered_prefixes();
        } else {
            run.finish();
        }
    }
}

#[test]
fn s_prop_001_snd_una_at_or_before_snd_nxt() {
    runs(0x9e37_79b9, |s| s.loss = 15);
}

/// No RTO for a segment that never left, and SND.UNA never past SND.NXT, whatever the next hop
/// answers and whether or not the device frames each segment.
#[test]
fn s_prop_001_snd_una_at_or_before_snd_nxt_whatever_the_next_hop_answers() {
    let (mut failed, mut refused) = (0, 0);
    for seed in 0..RUNS {
        let mut run = Run::new(0x2b99_2ddf ^ seed, |s| (s.loss, s.hops) = (5, true));
        run.run(1500);
        for node in &run.net.nodes {
            failed += node.tcp.counters().get(Counter::NextHopFailed);
            refused += node.tcp.counters().get(Counter::FrameRefused);
        }
        run.finish();
    }
    assert!(failed > 0 && refused > 0, "the runs meet both: {failed} unreachable, {refused} refused");
}

#[test]
fn s_prop_002_delivered_data_is_a_prefix() {
    runs(0x7f4a_7c15, |s| (s.duplicate, s.jitter) = (20, 40));
}

/// Half the runs abort: a reset is never taken for an end of stream.
#[test]
fn s_prop_003_end_of_stream_after_every_byte() {
    runs(0x51_7cc1, |_| {});
    runs(0x51_7cc2, |s| s.abort = true);
}

#[test]
fn s_prop_004_the_right_edge_never_retreats() {
    runs(0x2545_f491, |s| s.reading = 3);
}

#[test]
fn s_prop_005_the_scoreboard_stays_above_snd_una() {
    runs(0x6c62_272e, |s| (s.loss, s.jitter) = (10, 30));
}

#[test]
fn s_prop_007_pipe_is_bounded() {
    runs(0x5be0_cd19, |s| s.loss = 20);
}

#[test]
fn s_prop_008_nothing_happens_before_a_deadline() {
    runs(0x1f83_d9ab, |_| {});
}

#[test]
fn s_mod_004_prop_urgent_is_never_sent() {
    runs(0x3c6e_f371, |s| s.loss = 5);
}

#[test]
fn s_rx_026_prop_sacked_bytes_are_delivered() {
    let mut sacking = false;
    for seed in 0..RUNS {
        let mut run = Run::new(0x2545_f491 ^ seed, |s| (s.loss, s.jitter) = (10, 30));
        run.run(1000);
        run.finish();
        let checker = run.checker.borrow();
        for app in &run.net.apps {
            let delivered = app.sent.1 as u32;
            let base = checker.sends.iter().find(|s| s.0 == app.node).map(|s| s.1).unwrap();
            for &(left, right) in &checker.sacked[1 - app.node] {
                sacking = true;
                if left.since(base) < 1 << 30 {
                    assert!(right.since(base) <= delivered.saturating_add(2), "RX-26: SACKed bytes never delivered");
                }
            }
        }
    }
    assert!(sacking, "the runs exercise SACK");
}

#[test]
fn s_rt_017_prop_no_retransmission_before_an_rto() {
    let mut judged = 0;
    for seed in 0..RUNS {
        let mut run = Run::new(0x6a09_e667 ^ seed, |s| (s.only_acks, s.loss, s.jitter) = (true, 8, 60));
        run.run(1500);
        judged += run.checker.borrow().judged;
    }
    assert!(judged >= 10, "only {judged} timer retransmissions judged");
}

#[test]
fn s_op_025_prop_timestamps_on_every_segment() {
    for seed in 0..RUNS {
        let mut run = Run::new(0xbb67_ae85 ^ seed, |_| {});
        run.run(500);
        let node = (seed % 2) as usize;
        let now = run.net.instant(node);
        let tcp = &mut run.net.nodes[node].tcp;
        let Some((_, sync)) = tcp.each_sync().next() else { continue };
        let ts = sync.ts.expect("both ends offer timestamps").option(now);
        let id = run.net.apps.iter().find(|a| a.node == node).unwrap().id;
        let tcp = &mut run.net.nodes[node].tcp;
        tcp.abort(now, id).unwrap();
        let mut out = Vec::new();
        tcp.transmit(now, usize::MAX, |_| Hop::Ready(()), |o, ()| {
            out.push(parse_out(&datagram(o), 0));
            true
        });
        if let Some(rst) = out.iter().find(|o| o.flags & RST != 0) {
            assert_eq!(rst.ts, Some((ts.value, ts.echo)), "OP-25: the abort's RST");
        }
    }
}

/// One stack against segments an adversary makes up. Nothing panics, every bound holds.
#[test]
fn s_prop_006_totality_against_an_adversary() {
    for seed in 0..RUNS {
        let mut run = Run::new(0x3c6e_f372 ^ seed, |s| s.adversary = true);
        run.run(3000);
    }
}
