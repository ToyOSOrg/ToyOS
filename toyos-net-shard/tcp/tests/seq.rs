//! Sequence arithmetic.

mod common;

use common::*;
use toyos_net_tcp::Seq;

#[test]
fn s_seq_001_offsets() {
    assert_eq!(Seq::new(0x0000_0001).since(Seq::new(0xFFFF_FFFF)), 2);
    assert_eq!(Seq::new(3).since(Seq::new(5)), 0xFFFF_FFFE);
}

#[test]
fn s_seq_002_ordering_across_the_wrap() {
    let (low, high) = (Seq::new(0xFFFF_FFF0), Seq::new(0x0000_0010));
    assert_eq!(high.since(low), 0x20);
    assert!(low.before(high));
    assert!(high.after(low));
    assert!(Seq::new(7).at_or_before(Seq::new(7)));
}

#[test]
fn s_seq_003_half_the_space_is_unordered() {
    let (a, b) = (Seq::new(0), Seq::new(0x8000_0000));
    assert!(!a.before(b) && !a.after(b) && !b.before(a) && !b.after(a) && a != b);
}

#[test]
fn s_seq_004_ack_range_check() {
    let (una, nxt) = (Seq::new(0xFFFF_FF00), Seq::new(0x0000_0100));
    let in_range = |ack: u32| {
        let off = Seq::new(ack).since(una);
        (1..=nxt.since(una)).contains(&off)
    };
    assert!(!in_range(0xFFFF_FF00));
    assert!(in_range(0xFFFF_FF01));
    assert!(in_range(0x0000_0100));
    assert!(!in_range(0x0000_0101));
    assert!(!in_range(0x7FFF_FF00));
}

#[test]
fn s_seq_005_segment_length() {
    let mut h = fixture_e();
    // SEG.LEN is what RCV.NXT advances by: a FIN with ten bytes takes eleven.
    h.input(10, seg(5001).ack(1001).fin().len(10));
    assert_eq!(h.info().rcv_nxt.since(Seq::new(5001)), 11);
    let mut h = fixture_e();
    h.input(10, seg(5001).ack(1001));
    assert_eq!(h.info().rcv_nxt.get(), 5001, "a bare ACK takes none");
    let mut listener = H::new(65_535);
    listener.peer = (B, 40_000);
    listener.local = (A, 80);
    listener.tcp.listen(A, Some(port(80)), || 0).unwrap();
    let outs = listener.input(0, seg(5000).syn().len(10));
    expect(&outs, &["CTL=SYN,ACK ACK=5001"]);
    let outs = listener.input(1, seg(6000).syn().to(A, 80).from(B, 40_001));
    expect(&outs, &["CTL=SYN,ACK ACK=6001"]);
}

#[test]
fn s_seq_006_acceptability_across_the_wrap() {
    let mut h = client(0x20, seg(0xFFFF_FFEF).ack(1001).syn().wnd(65_535).mss(1460));
    assert_eq!(h.info().rcv_nxt.get(), 0xFFFF_FFF0);
    assert_eq!(h.info().rcv_edge.since(h.info().rcv_nxt), 0x20);
    h.input(1, seg(0x0000_0008).ack(1001).len(4));
    assert_eq!(h.info().ooo_ranges, 1, "0x00000008 is inside the window");
    let outs = h.input(600, seg(0x0000_0010).ack(1001).len(4));
    expect(&outs, &["ACK=0xFFFFFFF0"]);
    assert_eq!(h.info().ooo_ranges, 1, "0x00000010 starts at the right edge");
    h.input(1200, seg(0xFFFF_FFEF).ack(1001).len(2));
    assert_eq!(h.info().rcv_nxt.get(), 0xFFFF_FFF1, "trimmed to the one byte at RCV.NXT");
}

#[test]
fn s_seq_007_timestamps_compare_modularly() {
    use toyos_net_tcp::Seq as S;
    // PAWS rejects a TSval before TS.Recent; the stack's Stamp order is Seq's.
    let recent = S::new(0xFFFF_FFFE);
    assert!(S::new(0x0000_0001).after(recent));
    let half = S::new(0x7FFF_FFFE);
    assert!(!half.before(recent), "not before: passes PAWS");
    assert!(!half.at_or_after(recent), "not at or after: no update");
    let mut h = client(65_535, seg(5000).ack(1001).syn().wnd(65_535).mss(1460).sackok().ts(0xFFFF_FFFE, 990).ws(7));
    h.input(10, seg(5001).ack(1001).wnd(512).ts(0x0000_0001, 1000).len(10));
    assert_eq!(h.info().ts_recent, Some(1), "after: passes and updates");
    h.input(20, seg(5011).ack(1001).wnd(512).ts(0x8000_0001, 1000).len(10));
    assert_eq!(h.info().rcv_nxt.get(), 5021, "2^31 away: not before, so accepted");
    assert_eq!(h.info().ts_recent, Some(1), "and not at or after, so no update");
}

#[test]
fn s_seq_008_sending_across_the_wrap() {
    let mut h = H::new(65_535);
    let tuple = h.tuple();
    h.pin(0xFFFF_FFF0, tuple, -10);
    let id = h.tcp.connect(h.now(), A, Some(port(49152)), ep(B, 80)).unwrap();
    h.conn = Some(id);
    h.transmit();
    h.input(0, seg(0xFFFF_FF00).ack(0xFFFF_FFF1).syn().mss(1460));
    assert_eq!(h.info().snd_una.get(), 0xFFFF_FFF1, "pinned: the stack's own numbers");
    let outs = h.send(0, 32);
    expect(&outs, &["SEQ=0xFFFFFFF1 LEN=32"]);
    h.input(10, seg(0xFFFF_FF01).ack(0x0000_0011));
    let info = h.info();
    assert_eq!(info.snd_una.get(), 0x11);
    assert_eq!(info.snd_una, info.snd_nxt);
    assert_eq!(info.queued, 0);
}
