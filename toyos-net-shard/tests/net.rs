//! The test network's scenarios at frame level: two shards — [ip], ARP and TCP composed — on one
//! segment, with the impairment each scenario names. Ours against ours:
//! a consistency control, not an independent oracle.

mod common;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use common::*;
use toyos_net_tcp::{Counter, Seq};
use toyos_net_testnet::{Credit, Fate, Net};

const MIB: u64 = 1 << 20;

/// A and B, a round trip of `rtt_ms`, B serving port 80 and A opening `n` connections to it,
/// each side writing its `len`.
fn bulk(rtt_ms: u64, n: usize, len: [u64; 2]) -> (Net, usize, usize) {
    let (mut net, a, b) = segment();
    for (from, to) in [(a, b), (b, a)] {
        net.link(from, to).delay = Duration::from_micros(rtt_ms * 500);
    }
    net.serve(b, 80, len[1]);
    for _ in 0..n {
        net.open(a, ep(B, 80), len[0]);
    }
    (net, a, b)
}

fn finish(net: &mut Net) {
    let start = net.now();
    assert!(net.run_until(Duration::from_secs(600), |net| net.finished()), "the transfer did not finish in 600 s from {start:?}");
    net.assert_exact();
}

/// Applies `fate` to each data frame `pick` chooses by its index among the data frames on the
/// link, and counts them.
fn on_data(net: &mut Net, from: usize, to: usize, fate: Fate, mut pick: impl FnMut(usize) -> bool + 'static) -> Rc<RefCell<u64>> {
    let count = Rc::new(RefCell::new(0u64));
    let counted = count.clone();
    let mut seen = 0usize;
    net.link(from, to).rule = Some(Box::new(move |frame| {
        if data(frame).is_none() {
            return Fate::Pass;
        }
        seen += 1;
        if pick(seen) {
            *counted.borrow_mut() += 1;
            fate
        } else {
            Fate::Pass
        }
    }));
    count
}

fn count(net: &Net, node: usize, counter: Counter) -> u64 {
    net.nodes[node].shard.tcp_counters().get(counter)
}

#[test]
fn s_net_001_sack_repairs_every_50th() {
    let (mut net, a, b) = bulk(10, 1, [MIB, MIB]);
    let dropped = [on_data(&mut net, a, b, Fate::Drop, |n| n % 50 == 0), on_data(&mut net, b, a, Fate::Drop, |n| n % 50 == 0)];
    finish(&mut net);
    for node in [a, b] {
        assert_eq!(count(&net, node, Counter::Rto), 0, "node {node}");
        let recoveries = count(&net, node, Counter::SackRecovery);
        assert!(recoveries > 0, "node {node}");
        let bound = (*dropped[node].borrow() + recoveries) * 1448;
        assert!(count(&net, node, Counter::RetransmitBytes) <= bound, "node {node}: one retransmission per drop and one rescue per recovery");
    }
}

#[test]
fn s_net_003_reordering_below_the_threshold() {
    let (mut net, a, b) = bulk(10, 1, [MIB, MIB]);
    for (from, to) in [(a, b), (b, a)] {
        on_data(&mut net, from, to, Fate::Hold, |n| n % 20 == 10);
    }
    finish(&mut net);
    for node in [a, b] {
        assert_eq!(count(&net, node, Counter::RetransmitBytes), 0, "node {node}");
    }
}

#[test]
fn s_net_004_duplicates_are_reported_by_dsack() {
    let (mut net, a, b) = bulk(10, 1, [MIB, MIB]);
    let duplicated = [on_data(&mut net, a, b, Fate::Duplicate, |_| true), on_data(&mut net, b, a, Fate::Duplicate, |_| true)];
    finish(&mut net);
    for node in [a, b] {
        assert_eq!(count(&net, node, Counter::RetransmitBytes), 0, "node {node}");
        let reported = net.wire().iter().filter(|c| c.from != node && dsack(&c.frame)).count() as u64;
        // The duplicate of the last segment can land after its receiver left the synchronized
        // states, and TIME-WAIT's ACK carries no SACK.
        let duplicates = *duplicated[node].borrow();
        assert!(reported <= duplicates && reported + 1 >= duplicates, "node {node}: {reported} D-SACKs for {duplicates} duplicates");
    }
}

#[test]
fn s_net_006_a_dark_link() {
    let (mut net, a, b) = bulk(10, 1, [MIB, MIB]);
    let dark = (net.now().after(Duration::from_millis(50)), net.now().after(Duration::from_millis(5_050)));
    for (from, to) in [(a, b), (b, a)] {
        net.link(from, to).dark = Some(dark);
    }
    finish(&mut net);
    for node in [a, b] {
        assert!(count(&net, node, Counter::Rto) >= 3, "node {node}: {} expiries", count(&net, node, Counter::Rto));
    }
}

/// 100 connections from A each write 64 KiB at once through a device taking `frames` a
/// millisecond. Returns expiries and retransmissions handed off.
fn many_up(frames: usize) -> (u64, u64) {
    let (mut net, a, _) = bulk(10, 100, [64 * 1024, 0]);
    net.nodes[a].credit = Credit::PerMs(frames);
    let start = net.wire().len();
    finish(&mut net);
    assert_eq!(count(&net, a, Counter::RtoUnsent), 0, "an RTO fired for a segment that had not left");
    let mut highest: HashMap<u16, u32> = HashMap::new();
    let mut retransmissions = 0u64;
    for carried in &net.wire()[start..] {
        let Some((port, end, _)) = data(&carried.frame).filter(|_| carried.from == a) else { continue };
        match highest.get(&port) {
            Some(&top) if !Seq::new(end).after(Seq::new(top)) => retransmissions += 1,
            _ => {
                highest.insert(port, end);
            }
        }
    }
    let expiries = count(&net, a, Counter::Rto);
    assert!(expiries <= retransmissions, "{expiries} expiries, {retransmissions} retransmissions");
    (expiries, retransmissions)
}

#[test]
fn s_pl_011_many_up() {
    many_up(16);
    many_up(4);
}
