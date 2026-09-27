//! Options and their negotiation.

mod common;

use common::*;
use toyos_net_tcp::{limits, ConfigError, Counter, Failure, State, Tcp};

fn hex(text: &str) -> Vec<u8> {
    text.split_whitespace().map(|b| u8::from_str_radix(b, 16).unwrap()).collect()
}

fn listener() -> H {
    let mut h = H::new(65_535);
    h.peer = (B, 40_000);
    h.local = (A, 80);
    h.start(-20);
    let l = h.tcp.listen(A, Some(port(80)), || 0).unwrap();
    h.listener = Some(l);
    h
}

fn synack() -> S {
    seg(5000).ack(1001).syn().wnd(65_535)
}

#[test]
fn s_op_001_the_active_syn() {
    let mut h = H::new(65_535);
    let tuple = h.tuple();
    h.pin_ts(tuple, 0);
    let id = h.tcp.connect(h.now(), A, Some(port(49152)), ep(B, 80)).unwrap();
    h.conn = Some(id);
    let outs = h.transmit();
    expect(&outs, &["CTL=SYN WND=65535 LEN=0"]);
    assert_eq!(outs[0].options, hex("02 04 05 b4 04 02 08 0a 00 00 03 e8 00 00 00 00 01 03 03 00"));
}

#[test]
fn s_op_002_synack_to_a_bare_syn() {
    let mut h = listener();
    let outs = h.input(-20, seg(5000).syn().mss(1460));
    expect(&outs, &["CTL=SYN,ACK MSS=1460 NOSACKOK TS=- WS=-"]);
    assert_eq!(outs[0].options, hex("02 04 05 b4"));
}

#[test]
fn s_op_003_synack_to_a_full_syn() {
    let mut h = listener();
    let tuple = h.tuple();
    h.pin_ts(tuple, -20);
    let outs = h.input(-20, seg(5000).syn().mss(1460).sackok().ts(49_980, 0).ws(7));
    expect(&outs, &["CTL=SYN,ACK MSS=1460 SACKOK TS=980/49980 WS=0"]);
    let mut want = hex("02 04 05 b4 04 02 08 0a");
    want.extend_from_slice(&980u32.to_be_bytes());
    want.extend_from_slice(&49_980u32.to_be_bytes());
    want.extend_from_slice(&hex("01 03 03 00"));
    assert_eq!(outs[0].options, want);
}

#[test]
fn s_op_004_mss_zero_is_raised() {
    let mut h = client(65_535, synack().mss(0));
    assert_eq!(h.info().smss, 536);
    assert_eq!(h.count(Counter::MssBelowFloor), 1);
    let refusals = h.refusals(Counter::MssBelowFloor);
    assert_eq!(refusals.len(), 1);
    assert_eq!(refusals[0].remote, ep(B, 80));
}

#[test]
fn s_op_005_mss_100_is_raised() {
    let mut h = client(65_535, synack().mss(100));
    assert_eq!(h.info().smss, 536);
    let outs = h.send(1, 2000);
    assert!(outs.iter().all(|o| o.payload.len() <= 536));
    check(&outs[0], "LEN=536");
}

#[test]
fn s_op_006_no_mss_option() {
    let mut h = client(65_535, synack());
    let outs = h.send(1, 2000);
    expect(&outs, &["LEN=536", "LEN=536", "LEN=536"]);
}

#[test]
fn s_op_007_the_interface_bounds_the_mss() {
    let mut h = client(65_535, synack().mss(9000));
    assert_eq!(h.info().smss, 1460);
}

#[test]
fn s_op_008_timestamps_take_twelve_bytes() {
    let mut h = fixture_ef();
    let data = vec![7u8; 3000];
    let (sent, outs) = h.call(1, |tcp, now, id| tcp.send(now, id, &data));
    assert_eq!(sent, Ok(3000));
    expect(&outs, &["LEN=1448", "LEN=1448"]);
    let mut again = fixture_ef();
    let mut sizes = Vec::new();
    let (_, _) = again.call(1, |tcp, now, id| {
        tcp.send(now, id, &data).unwrap();
        tcp.transmit(now, usize::MAX, |o| sizes.push(datagram(o).len()));
    });
    assert_eq!(sizes, [1500, 1500], "20 + 32 + 1448");
}

#[test]
fn s_op_009_window_scaling() {
    let mut h = fixture_ef();
    h.input(1, seg(5001).ack(1001).wnd(512).ts(50_001, 1000));
    assert_eq!(h.info().snd_wnd, 65_536);
}

#[test]
fn s_op_010_shift_above_14_is_clamped() {
    let mut h = client(65_535, synack().mss(1460).ws(15));
    assert_eq!(h.info().snd_shift, 14);
    assert_eq!(h.count(Counter::WscaleClamped), 1);
    assert_eq!(h.refusals(Counter::WscaleClamped).len(), 1);
}

#[test]
fn s_op_011_unscaled_when_the_peer_offers_none() {
    let mut h = client(65_536, synack().mss(1460));
    let info = h.info();
    assert_eq!((info.snd_shift, info.rcv_shift), (0, 0));
    h.input(1, seg(5001).ack(1001).wnd(512));
    assert_eq!(h.info().snd_wnd, 512);
    let outs = h.input(2, seg(5001).ack(1001).wnd(512).len(100));
    let outs = [outs, h.at(50)].concat();
    expect(&outs, &["WND=65435"]);
}

#[test]
fn s_op_012_window_scale_outside_a_syn_is_ignored() {
    let mut h = fixture_ef();
    h.input(1, seg(5001).ack(1001).wnd(512).ts(50_001, 1000).ws(3));
    assert_eq!(h.info().snd_wnd, 65_536);
}

#[test]
fn s_op_013_a_synack_window_is_never_scaled() {
    let mut h = client(65_535, seg(5000).ack(1001).syn().wnd(1000).mss(1460).ws(7));
    assert_eq!(h.info().snd_wnd, 1000);
}

#[test]
fn s_op_014_sack_unnegotiated() {
    let mut h = client(65_535, synack().mss(1460));
    assert!(!h.info().sack);
    ten_out(&mut h);
    h.input(10, seg(5001).ack(1001).sack(&[(2461, 3921)]));
    assert_eq!(h.count(Counter::SackUnnegotiated), 1);
    h.input(20, seg(6001).ack(1001).len(100));
    assert!(h.log.iter().all(|o| o.sack.is_empty()));
}

#[test]
fn s_op_015_timestamps_unnegotiated() {
    let mut h = client(65_535, synack().mss(1460).sackok().ws(7));
    h.send(1, 100);
    h.close(2);
    assert!(h.log.iter().filter(|o| o.flags & SYN == 0).all(|o| o.ts.is_none()));
    assert!(h.log.len() >= 3);
}

#[test]
fn s_op_016_timestamps_on_an_unnegotiated_connection() {
    let mut h = fixture_e();
    h.input(1, seg(5001).ack(1001).len(10).ts(7, 0));
    assert_eq!(h.info().rcv_nxt.get(), 5011);
    assert_eq!(h.info().ts_recent, None);
}

#[test]
fn s_op_017_ts_recent_needs_seq_at_or_before_last_ack_sent() {
    let mut h = fixture_ef();
    h.input(1, seg(5001).ack(1001).wnd(512).len(100).ts(50_100, 1000));
    assert_eq!(h.info().ts_recent, Some(50_100));
    h.arrive(seg(5201).ack(1001).wnd(512).len(100).ts(50_200, 1000));
    assert_eq!(h.info().ts_recent, Some(50_100));
}

#[test]
fn s_op_018_delayed_ack_echoes_the_earliest() {
    let mut h = fixture_ef();
    nothing(&h.input(100, seg(5001).ack(1001).wnd(512).len(100).ts(50_100, 1000)));
    let outs = h.input(110, seg(5101).ack(1001).wnd(512).len(100).ts(50_110, 1000));
    expect(&outs, &["ACK=5201 TS=1110/50100"]);
}

#[test]
fn s_op_019_ts_across_a_hole() {
    let mut h = fixture_ef();
    nothing(&h.input(1, seg(5001).ack(1001).wnd(512).len(100).ts(50_100, 1000)));
    let outs = h.input(2, seg(5201).ack(1001).wnd(512).len(100).ts(50_120, 1000));
    expect(&outs, &["ACK=5101 TS=*/50100"]);
    let outs = h.input(3, seg(5101).ack(1001).wnd(512).len(100).ts(50_130, 1000));
    expect(&outs, &["ACK=5301 TS=*/50130"]);
}

fn recent_50100() -> H {
    let mut h = fixture_ef();
    h.input(1, seg(5001).ack(1001).wnd(512).ts(50_100, 1000));
    assert_eq!(h.info().ts_recent, Some(50_100));
    h
}

/// A passive child whose SYN carried TSval 49,980.
fn timestamped_child() -> H {
    let mut h = listener();
    expect(&h.input(-20, seg(5000).syn().mss(1460).sackok().ts(49_980, 0).ws(7)), &["CTL=SYN,ACK TS=*/49980"]);
    h
}

/// ESF closed by A into TIME-WAIT with TS.Recent 50,020, ending at 60,020.
fn timestamped_time_wait() -> H {
    let mut h = fixture_esf();
    h.close(0);
    h.input(10, seg(5001).ack(1002).wnd(512).ts(50_010, 1000));
    h.input(20, seg(5001).ack(1002).wnd(512).fin().ts(50_020, 1000));
    assert_eq!(h.tcp.time_wait_count(), 1);
    h
}

fn accepted(h: &mut H) -> bool {
    h.tcp.accept(h.listener.unwrap()).unwrap().is_some()
}

/// In ESTABLISHED, SYN-RECEIVED and TIME-WAIT.
#[test]
fn s_op_020_paws() {
    let mut h = recent_50100();
    let outs = h.input(2, seg(5001).ack(1001).wnd(512).len(100).ts(50_050, 1000));
    expect(&outs, &["SEQ=1001 ACK=5001 TS=*/50100"]);
    assert_eq!(h.info().rcv_nxt.get(), 5001);
    assert_eq!(h.count(Counter::PawsReject), 1);
    let mut h = timestamped_child();
    expect(&h.input(-10, seg(5001).ack(1001).ts(49_000, 1000)), &["SEQ=1001 ACK=5001 CTL=ACK TS=*/49980"]);
    assert_eq!(h.count(Counter::PawsReject), 1);
    assert!(!accepted(&mut h));
    h.input(-5, seg(5001).ack(1001).ts(49_990, 1000));
    assert!(accepted(&mut h), "the child stayed in SYN-RECEIVED");
    let mut h = timestamped_time_wait();
    expect(&h.input(1000, seg(5001).ack(1002).wnd(512).fin().ts(40_000, 1000)), &["SEQ=1002 ACK=5002 CTL=ACK TS=*/50020"]);
    assert_eq!(h.count(Counter::PawsReject), 1);
    h.at(60_020);
    assert_eq!(h.tcp.time_wait_count(), 0, "an old FIN rejected by PAWS does not restart TIME-WAIT");
}

/// T-4, decided the modern way: a segment without timestamps on a timestamped connection is
/// dropped (RFC 7323 §3.2), counted and logged; in ESTABLISHED, SYN-RECEIVED and TIME-WAIT.
#[test]
fn s_op_021_missing_timestamps_are_dropped() {
    let mut h = fixture_ef();
    nothing(&h.input(1, seg(5001).ack(1001).wnd(512).len(100)));
    assert_eq!(h.info().rcv_nxt.get(), 5001);
    assert_eq!(h.info().unread, 0);
    assert_eq!(h.count(Counter::TsMissing), 1);
    assert_eq!(h.refusals(Counter::TsMissing).len(), 1);
    assert_eq!(h.info().state, State::Established);
    let mut h = timestamped_child();
    nothing(&h.input(-10, seg(5001).ack(1001)));
    assert_eq!((h.count(Counter::TsMissing), h.refusals(Counter::TsMissing).len()), (1, 1));
    assert!(!accepted(&mut h));
    h.input(-5, seg(5001).ack(1001).ts(49_990, 1000));
    assert!(accepted(&mut h), "the child stayed in SYN-RECEIVED");
    let mut h = timestamped_time_wait();
    nothing(&h.input(1000, seg(5001).ack(1002).wnd(512).fin()));
    assert_eq!((h.count(Counter::TsMissing), h.refusals(Counter::TsMissing).len()), (1, 1));
    h.at(60_020);
    assert_eq!(h.tcp.time_wait_count(), 0, "a FIN without timestamps does not restart TIME-WAIT");
}

#[test]
fn s_op_022_no_paws_on_an_rst() {
    let mut h = recent_50100();
    nothing(&h.input(2, seg(5001).rst().ts(10, 0)));
    assert_eq!(h.status().failure, Some(Failure::Reset));
}

#[test]
fn s_op_023_ts_recent_expires_after_24_days() {
    let mut h = recent_50100();
    let days = 25 * 24 * 3600 * 1000;
    h.input(days, seg(5001).ack(1001).wnd(512).len(10).ts(20, 1000));
    assert_eq!(h.info().rcv_nxt.get(), 5011);
    assert_eq!(h.info().ts_recent, Some(20));
}

#[test]
fn s_op_024_rst_for_no_socket_echoes_the_timestamp() {
    let mut h = H::new(65_535);
    let outs = h.input(0, seg(9).ack(77).ts(77, 5).to(A, 81));
    expect(&outs, &["SEQ=77 ACK=- CTL=RST"]);
    assert_eq!(outs[0].options, hex("01 01 08 0a 00 00 00 00 00 00 00 4d"));
}

#[test]
fn s_op_026_mss_outside_a_syn_is_ignored() {
    let mut h = fixture_e();
    h.input(1, seg(5001).ack(1001).mss(100));
    assert_eq!(h.info().smss, 1460);
}

#[test]
fn s_op_027_ecn_is_not_negotiated() {
    let mut h = listener();
    let outs = h.input(-20, seg(5000).syn().flags(ECE | CWR).mss(1460));
    expect(&outs, &["CTL=SYN,ACK"]);
    assert_eq!(h.count(Counter::EcnNotNegotiated), 1);
    h.input(0, seg(5001).ack(1001));
    let id = h.tcp.accept(h.listener.unwrap()).unwrap().unwrap();
    h.conn = Some(id);
    h.input(1, seg(5001).ack(1001).flags(ECE).len(10));
    assert_eq!(h.read(100).len(), 10);
}

#[test]
fn s_op_028_sack_blocks_in_a_syn_are_ignored() {
    let mut h = listener();
    let outs = h.input(-20, seg(5000).syn().mss(1460).sack(&[(1, 2)]));
    expect(&outs, &["CTL=SYN,ACK ACK=5001"]);
    h.input(0, seg(5001).ack(1001));
    assert!(h.tcp.accept(h.listener.unwrap()).unwrap().is_some());
}

#[test]
fn s_op_029_the_last_mss_wins() {
    let mut h = client(65_535, synack().mss(1460).mss(536));
    assert_eq!(h.info().smss, 536);
}

#[test]
fn s_op_031_the_shift_fits_the_buffer() {
    let mut h = H::new(65_536);
    h.start(0);
    let id = h.tcp.connect(h.now(), A, Some(port(49152)), ep(B, 80)).unwrap();
    h.conn = Some(id);
    expect(&h.transmit(), &["CTL=SYN WS=1"]);
    assert!(Tcp::new(config(limits::RECEIVE_BUFFER_MAX)).is_ok());
    assert_eq!(Tcp::new(config(limits::RECEIVE_BUFFER_MAX + 1)).err(), Some(ConfigError::ReceiveBufferTooLarge));
}

#[test]
fn s_op_032_tsecr_without_ack_is_ignored() {
    let mut h = listener();
    let tuple = h.tuple();
    h.pin_ts(tuple, 0);
    let outs = h.input(0, seg(5000).syn().mss(1460).ts(49_980, 77));
    expect(&outs, &["CTL=SYN,ACK TS=1000/49980"]);
}
