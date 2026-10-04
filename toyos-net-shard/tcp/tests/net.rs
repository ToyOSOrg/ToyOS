//! Ours against ours: a consistency control, not an independent oracle.

mod common;

use std::collections::{HashMap, HashSet};

use common::net::*;
use common::*;
use toyos_net_tcp::{Counter, Failure, Options, Seq};

const MIB: usize = 1 << 20;

fn bulk(rtt: u64) -> Net {
    let mut net = Net::new(rtt);
    net.connections(1, 80, [MIB, MIB]);
    net
}

fn finish(net: &mut Net) {
    if !net.run(600_000, Net::finished) {
        panic!("the transfer did not finish:\n{}", net.dump());
    }
    net.assert_exact();
}

/// Drops every `n`th data segment in each direction; returns the drop count per direction.
fn every_nth(n: usize) -> (Impair, std::rc::Rc<std::cell::RefCell<[usize; 2]>>) {
    let dropped = std::rc::Rc::new(std::cell::RefCell::new([0usize; 2]));
    let counts = dropped.clone();
    let mut seen = [0usize; 2];
    let impair: Impair = Box::new(move |from, o| {
        if o.payload.is_empty() {
            return Fate::Pass;
        }
        seen[from] += 1;
        if seen[from] % n == 0 {
            counts.borrow_mut()[from] += 1;
            Fate::Drop
        } else {
            Fate::Pass
        }
    });
    (impair, dropped)
}

/// Strips Timestamps, Window Scale and SACK-permitted from node 0's SYN: node 1 sees a peer that
/// offers none of them, and both ends run without them.
fn bare_syn(from: usize, o: &O) -> Option<Vec<u8>> {
    (from == 0 && o.flags & SYN != 0).then(|| {
        let s = seg(o.seq).syn().wnd(o.wnd).mss(o.mss.unwrap());
        s.bytes(o.src, o.dst, 0, 0)
    })
}

#[test]
fn s_net_002_newreno_when_one_side_has_no_sack() {
    let mut net = Net::new(10);
    net.rewrite = Some(Box::new(bare_syn));
    net.connections(1, 80, [MIB, MIB]);
    let (mut impair, dropped) = every_nth(50);
    net.impair = Box::new(move |from, o| impair(from, o));
    finish(&mut net);
    for node in 0..2 {
        assert_eq!(net.count(node, Counter::SackRecovery), 0);
        assert!(net.count(node, Counter::FastRecovery) > 0);
        let drops = dropped.borrow()[node] as u64;
        assert!(net.count(node, Counter::RetransmitBytes) <= 2 * drops * 1460, "node {node}");
    }
}

#[test]
fn s_net_005_lost_acks() {
    let mut net = Net::new(10);
    net.connections(1, 80, [MIB, 0]);
    let mut acks = 0;
    net.impair = Box::new(move |_, o| {
        if o.payload.is_empty() && o.flags & (SYN | FIN | RST) == 0 {
            acks += 1;
            if acks % 2 == 0 {
                return Fate::Drop;
            }
        }
        Fate::Pass
    });
    finish(&mut net);
    assert_eq!(net.count(0, Counter::RetransmitBytes), 0);
}

#[test]
fn s_net_007_sequence_numbers_wrap() {
    let mut net = Net::new(10);
    net.pin_isn(0xFFFF_0000, 50_000, 80);
    net.connections(1, 80, [MIB, MIB]);
    net.keep_wire = true;
    let (impair, _) = every_nth(50);
    net.impair = impair;
    finish(&mut net);
    let syns: Vec<u32> = net.wire.iter().filter(|(_, o)| o.flags & SYN != 0).map(|(_, o)| o.seq).collect();
    assert_eq!(syns, [0xFFFF_0000, 0xFFFF_0000]);
    let across = net.wire.iter().any(|(_, o)| o.sack.iter().any(|&(l, r)| Seq::new(l).before(Seq::new(r)) && l > r));
    let wrapped = net.wire.iter().any(|(_, o)| o.seq < 0x0001_0000 && !o.payload.is_empty());
    assert!(wrapped, "the transfer crossed 2^32");
    assert!(across || net.count(0, Counter::SackRecovery) > 0);
    for node in 0..2 {
        assert_eq!(net.count(node, Counter::Rto), 0);
    }
}

#[test]
fn s_net_008_timestamps_wrap() {
    let mut net = Net::new(10);
    net.pin_tsval(u32::MAX - 100, 50_000, 80);
    net.connections(1, 80, [MIB, MIB]);
    net.keep_wire = true;
    finish(&mut net);
    let values: Vec<u32> = net.wire.iter().filter_map(|(_, o)| o.ts.map(|(v, _)| v)).collect();
    assert!(values.iter().any(|&v| v > u32::MAX - 200) && values.iter().any(|&v| v < 1000), "TSvals crossed 2^32");
    for node in 0..2 {
        assert_eq!(net.count(node, Counter::PawsReject), 0);
    }
}

#[test]
fn s_net_009_the_last_segment_of_each_burst() {
    let mut net = Net::new(10);
    net.connections(1, 80, [MIB, MIB]);
    let mut first_seq: [Option<u32>; 2] = [None, None];
    let mut dropped: HashSet<(usize, u32)> = HashSet::new();
    net.impair = Box::new(move |from, o| {
        if o.flags & SYN != 0 {
            first_seq[from] = Some(o.seq.wrapping_add(1));
            return Fate::Pass;
        }
        let Some(start) = first_seq[from] else { return Fate::Pass };
        let offset = o.seq.wrapping_sub(start) as usize;
        let end = offset + o.payload.len();
        if !o.payload.is_empty() && end / 65_536 != offset / 65_536 && dropped.insert((from, o.seq)) {
            return Fate::Drop;
        }
        Fate::Pass
    });
    finish(&mut net);
}

#[test]
fn s_net_010_a_lost_retransmission() {
    let mut net = bulk(10);
    let mut seen: HashMap<(usize, u32), usize> = HashMap::new();
    let mut data = [0usize; 2];
    let mut victim: [Option<u32>; 2] = [None, None];
    net.impair = Box::new(move |from, o| {
        if o.payload.is_empty() {
            return Fate::Pass;
        }
        data[from] += 1;
        if data[from] == 100 {
            victim[from] = Some(o.seq);
        }
        if victim[from] == Some(o.seq) {
            let times = seen.entry((from, o.seq)).or_default();
            *times += 1;
            if *times <= 2 {
                return Fate::Drop;
            }
        }
        Fate::Pass
    });
    finish(&mut net);
    for node in 0..2 {
        assert!(net.count(node, Counter::Rto) >= 1, "node {node}");
    }
}

#[test]
fn s_net_011_the_fin_overtakes_the_last_data() {
    let mut net = Net::new(10);
    net.connections(1, 80, [100_000, 0]);
    let mut held = false;
    net.impair = Box::new(move |from, o| {
        if from == 0 && !held && !o.payload.is_empty() && o.flags & FIN == 0 && o.flags & PSH != 0 {
            held = true;
            return Fate::Hold;
        }
        Fate::Pass
    });
    finish(&mut net);
}

/// Receiver silly-window avoidance reopens the window after two 1 KiB reads, inside one RTO, so
/// every tenth read is skipped: a window then stays shut long enough to be probed.
#[test]
fn s_net_012_a_slow_reader() {
    let mut net = Net::new(10);
    net.connections(1, 80, [640 * 1024, 0]);
    net.keep_wire = true;
    net.run(100, |n| n.apps.len() == 2);
    let reader = net.apps.iter().position(|a| a.node == 1).unwrap();
    let start = net.now;
    for read in 0.. {
        net.apps[reader].read_limit = Some(1024);
        net.apps[reader].reading = read % 10 != 9;
        net.advance(100);
        if net.apps.iter().all(|a| a.end.is_some()) {
            break;
        }
        assert!(net.now - start < ns(400_000), "the reader never finished");
    }
    net.assert_exact();
    assert!(net.wire.iter().any(|(f, o)| *f == 1 && o.wnd == 0), "the window shut");
    assert!(net.count(0, Counter::PersistProbe) > 0);
    assert_eq!(net.apps.iter().filter(|a| a.end == Some(End::Fin)).count(), 2, "no give-up");
    assert!(net.now - start >= ns(55_000), "about 60 s of reading");
}

#[test]
fn s_net_013_a_hundred_connections() {
    let mut net = Net::new(10);
    net.connections(100, 80, [128 * 1024, 128 * 1024]);
    finish(&mut net);
    assert_eq!(net.apps.len(), 200);
}

#[test]
fn s_net_014_an_abort_is_a_reset() {
    let mut net = bulk(10);
    let target = net.now + ns(50);
    net.run(100, |n| n.now >= target);
    let victim = net.apps.iter().position(|a| a.node == 0).unwrap();
    let now = net.instant(0);
    net.nodes[0].tcp.abort(now, net.apps[victim].id).unwrap();
    net.apps[victim].end = Some(End::Failed(Failure::Reset));
    assert!(net.run(10_000, |n| n.apps.iter().all(|a| a.end.is_some())));
    let other = net.apps.iter().find(|a| a.node == 1).unwrap();
    assert_eq!(other.end, Some(End::Failed(Failure::Reset)));
}

/// Request/response with each message written as two halves: Nagle holds the second half until
/// the first is acknowledged, and the delayed ACK makes that wait 40 ms.
fn exchanges(nodelay: bool) -> u64 {
    let mut net = Net::new(10);
    net.auto_shut = false;
    net.connections(1, 80, [0, 0]);
    assert!(net.run(100, |n| n.apps.len() == 2));
    if nodelay {
        net.set_options(0, Options { nodelay: true, ..Options::default() });
        net.set_options(1, Options { nodelay: true, ..Options::default() });
    }
    let start = net.now;
    for _ in 0..1000 {
        for side in [0usize, 1] {
            let app = net.apps.iter().position(|a| a.node == side).unwrap();
            let peer = 1 - app;
            let now = net.instant(side);
            let id = net.apps[app].id;
            for half in [[1u8; 50], [2u8; 50]] {
                net.nodes[side].tcp.send(now, id, &half).unwrap();
                net.apps[app].sent.feed(&half);
                net.pass();
            }
            let want = net.apps[app].sent.1;
            assert!(net.run(10_000, |n| n.apps[peer].received.1 >= want));
        }
    }
    (net.now - start) / 1_000_000
}

#[test]
fn s_net_015_nagle_and_the_delayed_ack() {
    let with_nagle = exchanges(false);
    let without = exchanges(true);
    assert!(with_nagle >= 1000 * 40, "{with_nagle} ms for 1000 exchanges");
    assert!(without <= 1000 * 12, "{without} ms for 1000 exchanges");
}

#[test]
fn s_net_016_a_peer_without_options() {
    let mut net = Net::new(10);
    net.rewrite = Some(Box::new(bare_syn));
    net.connections(1, 80, [MIB, MIB]);
    net.keep_wire = true;
    finish(&mut net);
    assert!(net.wire.iter().filter(|(_, o)| o.flags & SYN == 0).all(|(_, o)| o.ts.is_none() && o.sack.is_empty()));
}

#[test]
fn s_ls_012_a_syn_flood() {
    let mut h = H::new(65_535);
    h.peer = (B, 40_000);
    h.local = (A, 80);
    h.start(0);
    let listener = h.tcp.listen(A, Some(port(80)), || 0).unwrap();
    let mut answered = HashSet::new();
    for i in 0..10_000u32 {
        let from = (std::net::Ipv4Addr::from(0xC633_6400 + i), 1024 + (i % 60_000) as u16);
        let outs = h.input(i64::from(i / 10), seg(i).syn().mss(1460).from(from.0, from.1));
        answered.extend(outs.iter().map(|o| o.dst));
    }
    assert_eq!(answered.len(), toyos_net_tcp::limits::LISTEN_PENDING);
    assert_eq!(h.count(Counter::ListenOverflow), 10_000 - 256);
    h.at(63_999);
    assert_eq!(h.tcp.next_deadline(), None, "every child is gone");
    expect(&h.input(64_000, seg(5000).syn().mss(1460)), &["CTL=SYN,ACK"]);
    h.input(64_010, seg(5001).ack(1001));
    assert!(h.tcp.accept(listener).unwrap().is_some());
}
