//! The receive side. From E unless stated.

mod common;

use common::*;
use toyos_net_tcp::{Counter, Error, Failure, Received, State};

fn recv(h: &mut H, n: usize) -> Result<Received, Error> {
    let mut buf = vec![0u8; n];
    h.call(h.t, |tcp, now, id| tcp.recv(now, id, &mut buf)).0
}

/// Reads `n` bytes without letting the clock move; returns what the read made A send.
fn read_now(h: &mut H, n: usize) -> Vec<O> {
    let (r, outs) = h.call(h.t, |tcp, now, id| tcp.recv(now, id, &mut vec![0u8; n]));
    assert_eq!(r, Ok(Received::Data(n)));
    outs
}

#[test]
fn s_rx_001_every_second_segment() {
    let mut h = fixture_e();
    nothing(&h.input(0, seg(5001).ack(1001).len(100)));
    expect(&h.input(10, seg(5101).ack(1001).len(100)), &["ACK=5201"]);
    nothing(&h.at(40));
}

#[test]
fn s_rx_002_the_delayed_ack_is_40_ms() {
    let mut h = fixture_e();
    h.input(0, seg(5001).ack(1001).len(100));
    nothing(&h.at(39));
    let outs = h.at(40);
    expect(&outs, &["ACK=5101"]);
    assert_eq!(outs[0].t, 40);
}

#[test]
fn s_rx_003_out_of_order_is_acknowledged_at_once() {
    let mut h = fixture_e();
    expect(&h.input(1, seg(6001).ack(1001).len(100)), &["SEQ=1001 ACK=5001 WND=65535"]);
    assert_eq!(h.info().ooo_ranges, 1);
}

#[test]
fn s_rx_004_duplicate_acks_are_not_coalesced() {
    let mut h = fixture_e();
    h.at(1);
    for seq in [6001, 6201, 6401] {
        h.arrive(seg(seq).ack(1001).len(100));
    }
    expect(&h.transmit(), &["ACK=5001 LEN=0", "ACK=5001 LEN=0", "ACK=5001 LEN=0"]);
}

#[test]
fn s_rx_005_a_filled_hole_is_acknowledged_at_once() {
    let mut h = fixture_e();
    h.input(1, seg(6001).ack(1001).len(100));
    expect(&h.input(2, seg(5001).ack(1001).len(1000)), &["ACK=6101"]);
    assert_eq!(h.info().rcv_nxt.get(), 6101);
}

#[test]
fn s_rx_006_a_later_range_remains() {
    let mut h = fixture_e();
    h.input(1, seg(6001).ack(1001).len(100));
    h.input(2, seg(7001).ack(1001).len(100));
    expect(&h.input(3, seg(5001).ack(1001).len(1000)), &["ACK=6101"]);
    assert_eq!(h.info().ooo_ranges, 1);
}

#[test]
fn s_rx_007_stored_bytes_win() {
    let mut h = fixture_e();
    h.input(1, seg(5011).ack(1001).data(b"WXYZ"));
    h.input(2, seg(5001).ack(1001).data(b"abcdefghijklmnopqrst"));
    assert_eq!(h.info().rcv_nxt.get(), 5021);
    assert_eq!(h.read(100), b"abcdefghijWXYZopqrst");
}

#[test]
fn s_rx_008_at_most_32_ranges() {
    let mut h = fixture_e();
    for k in 0..32u32 {
        h.input(1, seg(6001 + 200 * k).ack(1001).len(100));
    }
    assert_eq!(h.info().ooo_ranges, 32);
    expect(&h.input(2, seg(20_001).ack(1001).len(100)), &["ACK=5001"]);
    assert_eq!(h.info().ooo_ranges, 32);
    assert_eq!(h.count(Counter::OooRangeLimit), 1);
    h.input(3, seg(6101).ack(1001).len(100));
    assert_eq!(h.info().ooo_ranges, 31);
}

#[test]
fn s_rx_009_a_fin_beyond_a_hole() {
    let mut h = fixture_e();
    expect(&h.input(1, seg(5101).ack(1001).fin()), &["ACK=5001"]);
    expect(&h.input(2, seg(5001).ack(1001).len(100)), &["ACK=5102"]);
    assert_eq!(h.info().state, State::CloseWait);
    assert_eq!(recv(&mut h, 1000), Ok(Received::Data(100)));
    assert_eq!(recv(&mut h, 1000), Ok(Received::End));
}

#[test]
fn s_rx_010_nothing_after_a_remembered_fin() {
    let mut h = fixture_e();
    h.input(1, seg(5101).ack(1001).fin());
    h.input(2, seg(5101).ack(1001).len(10));
    assert_eq!(h.count(Counter::DataAfterFin), 1);
    assert_eq!(h.info().ooo_ranges, 0);
    h.input(3, seg(5201).ack(1001).fin());
    assert_eq!(h.count(Counter::FinConflict), 1);
    h.input(4, seg(5001).ack(1001).len(100));
    assert_eq!(h.info().state, State::CloseWait);
    assert_eq!(h.info().rcv_nxt.get(), 5102);
}

fn unread_1000() -> H {
    let mut h = fixture_e();
    h.input(0, seg(5001).ack(1001).len(1000));
    expect(&h.at(40), &["ACK=6001 WND=64535"]);
    h
}

#[test]
fn s_rx_011_unread_bytes_close_the_window() {
    unread_1000();
}

#[test]
fn s_rx_012_receiver_silly_window_avoidance() {
    let mut h = unread_1000();
    nothing(&read_now(&mut h, 1000));
    assert_eq!(h.info().rcv_edge.get(), 70_536);
    h.input(50, seg(6001).ack(1001).len(500));
    expect(&h.at(90), &["ACK=6501 WND=64035"]);
    nothing(&read_now(&mut h, 500));
    expect(&h.send(100, 1), &["ACK=6501 WND=65535"]);
}

#[test]
fn s_rx_013_a_window_update_when_the_window_was_small() {
    let mut h = fixture_e();
    let mut seq = 5001u32;
    while seq < 70_536 {
        let n = (70_536 - seq).min(1460);
        h.arrive(seg(seq).ack(1001).len(n as usize));
        seq += n;
    }
    h.at(1);
    nothing(&read_now(&mut h, 1000));
    expect(&read_now(&mut h, 500), &["ACK=70536 WND=1500"]);
}

#[test]
fn s_rx_014_the_edge_holds_while_a_hole_is_open() {
    let mut h = fixture_e();
    h.input(0, seg(5001).ack(1001).len(2000));
    expect(&h.input(1, seg(8001).ack(1001).len(100)), &["ACK=7001 WND=63535"]);
    nothing(&read_now(&mut h, 2000));
    expect(&h.input(2, seg(8101).ack(1001).len(100)), &["ACK=7001 WND=63535"]);
    expect(&h.input(3, seg(7001).ack(1001).len(1000)), &["ACK=8201 WND=64335"]);
    assert_eq!(h.info().rcv_edge.get(), 72_536);
}

#[test]
fn s_rx_015_the_offered_edge_is_honoured_under_scaling() {
    let mut h = client(65_536, seg(5000).ack(1001).syn().mss(1460).ws(7));
    assert_eq!(h.info().rcv_shift, 1);
    let outs: Vec<O> = h.log.iter().filter(|o| o.flags & SYN == 0).cloned().collect();
    expect(&outs, &["WND=32767"]);
    h.arrive(seg(5001).ack(1001).wnd(512).len(40_000));
    h.arrive(seg(45_001).ack(1001).wnd(512).len(25_535));
    h.at(1);
    assert_eq!(h.info().rcv_nxt.get(), 70_536);
    assert_eq!(h.info().unread, 65_535);
}

#[test]
fn s_rx_016_the_ack_rides_on_data() {
    let mut h = fixture_e();
    h.input(0, seg(5001).ack(1001).len(100));
    expect(&h.send(5, 10), &["SEQ=1001 ACK=5101 LEN=10"]);
    nothing(&h.at(40));
}

#[test]
fn s_rx_017_sack_blocks() {
    let mut h = fixture_ef();
    expect(&h.input_full(1, seg(6001).ack(1001).len(1000)), &["ACK=5001 TS=*/* SACK=6001-7001"]);
    expect(&h.input_full(2, seg(8001).ack(1001).len(1000)), &["ACK=5001 SACK=8001-9001,6001-7001"]);
}

#[test]
fn s_rx_018_the_triggering_range_first() {
    let mut h = fixture_ef();
    h.input_full(1, seg(6001).ack(1001).len(1000));
    h.input_full(2, seg(8001).ack(1001).len(1000));
    expect(&h.input_full(3, seg(7001).ack(1001).len(500)), &["SACK=6001-7501,8001-9001"]);
}

#[test]
fn s_rx_019_three_blocks_with_timestamps() {
    let mut h = fixture_ef();
    for (t, seq) in [(1, 6001), (2, 7001), (3, 8001)] {
        h.input_full(t, seg(seq).ack(1001).len(100));
    }
    expect(&h.input_full(4, seg(9001).ack(1001).len(100)), &["SACK=9001-9101,8001-8101,7001-7101"]);
}

#[test]
fn s_rx_020_dsack_below_rcv_nxt() {
    let mut h = fixture_ef();
    h.input_full(1, seg(5001).ack(1001).len(100));
    expect(&h.input_full(2, seg(5001).ack(1001).len(100)), &["ACK=5101 SACK=5001-5101"]);
}

#[test]
fn s_rx_021_dsack_inside_a_range() {
    let mut h = fixture_ef();
    h.input_full(1, seg(6001).ack(1001).len(1000));
    expect(&h.input_full(2, seg(6001).ack(1001).len(500)), &["ACK=5001 SACK=6001-6501,6001-7001"]);
}

#[test]
fn s_rx_022_no_sack_unnegotiated() {
    let mut h = fixture_e();
    expect(&h.input(1, seg(6001).ack(1001).len(100)), &["ACK=5001 SACK=- NOOPT"]);
}

#[test]
fn s_rx_023_a_reset_is_never_end_of_stream() {
    let mut h = fixture_e();
    h.input(1, seg(5001).ack(1001).len(100));
    h.input(2, seg(5101).rst());
    assert_eq!(recv(&mut h, 1000), Err(Error::Failed(Failure::Reset)));
}

#[test]
fn s_rx_024_data_then_end() {
    let mut h = fixture_e();
    h.input(1, seg(5001).ack(1001).fin().len(100));
    assert_eq!(recv(&mut h, 50), Ok(Received::Data(50)));
    assert_eq!(recv(&mut h, 50), Ok(Received::Data(50)));
    assert_eq!(recv(&mut h, 50), Ok(Received::End));
}

#[test]
fn s_rx_025_push_is_not_a_boundary() {
    let run = |push: bool| {
        let mut h = fixture_e();
        for (t, seq) in [(1, 5001), (2, 5101), (3, 5201)] {
            let s = seg(seq).ack(1001).len(100);
            h.input(t, if push { s.psh() } else { s });
        }
        let mut reads = Vec::new();
        while let Ok(Received::Data(n)) = recv(&mut h, 70) {
            reads.push(n);
        }
        (reads, h.read(0))
    };
    assert_eq!(run(true), run(false));
}

#[test]
fn s_rx_027_half_close_keeps_receiving() {
    let mut h = fixture_e();
    let (r, _) = h.call(0, |tcp, now, id| tcp.shutdown_write(now, id));
    r.unwrap();
    h.input(10, seg(5001).ack(1002));
    assert_eq!(h.info().state, State::FinWait2);
    let mut seq = 5001u32;
    for minute in 1..=10 {
        h.input(minute * 60_000, seg(seq).ack(1002).len(1000));
        seq += 1000;
        h.read(1000);
    }
    h.at(10 * 60_000 + 100);
    let last = h.log.iter().rev().find(|o| o.ack.is_some()).unwrap().clone();
    check(&last, "ACK=15001");
    assert_eq!(h.info().state, State::FinWait2);
}

/// At shift 7 a window below one unit reads as zero, and is offered as one unit only where the
/// buffer has that unit free: with 100 bytes free and a hole open, a whole unit would let the peer
/// send past the capacity, so the edge holds and the field stays 0.
#[test]
fn a_sub_unit_window_rounds_up_only_into_free_room() {
    let mut h = client(4 << 20, seg(5000).ack(1001).syn().wnd(65_535).mss(1460).sackok().ws(7));
    assert_eq!(h.info().rcv_shift, 7);
    assert_eq!(h.info().rcv_capacity, 65_535);
    h.input(1, seg(5001).ack(1001).len(65_435));
    let edge = h.info().rcv_edge;
    assert_eq!(edge.since(h.info().rcv_nxt), 100);
    expect(&h.input(2, seg(70_486).ack(1001).len(50)), &["ACK=70436 WND=0"]);
    let info = h.info();
    assert_eq!((info.rcv_edge, info.ooo_ranges, info.unread), (edge, 1, 65_435));
}
