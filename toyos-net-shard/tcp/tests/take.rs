//! `recv_with`: the read of a reader that may take less than it is shown. No scenario ids: the
//! specifications hold no such call, which the track lists among stage 4's departures; their
//! receive scenarios are `receive.rs`'s, on `recv`, and these hold `recv_with` to the same buffer
//! and the same window update. From E.

mod common;

use common::*;
use toyos_net_tcp::{Error, Received, State};

/// A read whose reader answers `count`, without letting the clock move: the answer, what the
/// reader was shown, and what the read made A send.
fn take(h: &mut H, count: usize) -> (Result<Received, Error>, Vec<u8>, Vec<O>) {
    let mut shown = Vec::new();
    let (r, outs) = h.call(h.t, |tcp, now, id| {
        tcp.recv_with(now, id, |held| {
            shown = held.to_vec();
            count
        })
    });
    (r, shown, outs)
}

/// The text a well-behaved peer sends in `[from, to)`.
fn text(from: u32, to: u32) -> Vec<u8> {
    (from..to).map(pattern).collect()
}

#[test]
fn what_the_reader_did_not_take_stays_held_in_order() {
    let mut h = fixture_e();
    h.input(0, seg(5001).ack(1001).len(100));
    let (r, shown, _) = take(&mut h, 40);
    assert_eq!((r, shown), (Ok(Received::Data(40)), text(5001, 5101)));
    assert_eq!(h.status().readable, 60);
    let (r, shown, _) = take(&mut h, 0);
    assert_eq!((r, shown), (Ok(Received::Data(0)), text(5041, 5101)), "a reader that took nothing");
    assert_eq!(h.read(100), text(5041, 5101));
}

#[test]
fn with_nothing_held_the_reader_is_not_asked() {
    let mut h = fixture_e();
    let mut asked = false;
    let (r, _) = h.call(h.t, |tcp, now, id| {
        tcp.recv_with(now, id, |_| {
            asked = true;
            0
        })
    });
    assert_eq!((r, asked), (Err(Error::WouldBlock), false));
    h.input(0, seg(5001).ack(1001).fin());
    let (r, _) = h.call(h.t, |tcp, now, id| {
        tcp.recv_with(now, id, |_| {
            asked = true;
            0
        })
    });
    assert_eq!((r, asked), (Ok(Received::End), false));
}

// The window update RX-13 has a read owe: 65,535 unread bytes shut the window, and a take that
// moves the edge by 1,500 from a window of 0 is acknowledged at once.
#[test]
fn a_take_owes_the_window_update_a_read_does() {
    let mut h = fixture_e();
    let mut seq = 5001u32;
    while seq < 70_536 {
        let n = (70_536 - seq).min(1460);
        h.arrive(seg(seq).ack(1001).len(n as usize));
        seq += n;
    }
    h.at(1);
    let (r, _, outs) = take(&mut h, 1000);
    assert_eq!(r, Ok(Received::Data(1000)));
    nothing(&outs);
    let (r, _, outs) = take(&mut h, 500);
    assert_eq!(r, Ok(Received::Data(500)));
    expect(&outs, &["ACK=70536 WND=1500"]);
}

// The buffer is a ring of 65,535 bytes: text that wraps it is shown in two pieces, and a reader
// that claims more than it was shown takes the piece and no byte of the next.
#[test]
fn text_across_the_rings_wrap_is_shown_in_two_pieces_and_none_is_lost() {
    let mut h = fixture_e();
    let mut seq = 5001u32;
    while seq < 70_536 {
        let n = (70_536 - seq).min(1460);
        h.arrive(seg(seq).ack(1001).len(n as usize));
        seq += n;
    }
    h.at(1);
    let (r, _, _) = take(&mut h, 65_000);
    assert_eq!(r, Ok(Received::Data(65_000)));
    h.input(2, seg(70_536).ack(1001).len(1000));
    assert_eq!(h.status().readable, 1535);

    let mut pieces = Vec::new();
    loop {
        match take(&mut h, usize::MAX) {
            (Ok(Received::Data(n)), shown, _) => {
                assert_eq!(n, shown.len(), "the count is what was shown");
                pieces.push(shown);
            }
            (Err(Error::WouldBlock), _, _) => break,
            (other, _, _) => panic!("{other:?}"),
        }
    }
    assert_eq!(pieces.iter().map(Vec::len).collect::<Vec<_>>(), [535, 1000]);
    assert_eq!(pieces.concat(), text(70_001, 71_536));
}

// Both FINs end the connection (RFC 9293 §3.6, case 1), and what the peer sent before its own is
// still the reader's: shown, taken, and only then the end.
#[test]
fn text_unread_when_both_fins_ended_the_connection_is_still_taken() {
    let mut h = fixture_e();
    let (r, _) = h.call(0, |tcp, now, id| tcp.shutdown_write(now, id));
    r.unwrap();
    h.input(10, seg(5001).ack(1002));
    h.input(20, seg(5001).ack(1002).len(100).fin());
    assert_eq!(h.status().state, State::TimeWait);
    let (r, shown, _) = take(&mut h, 40);
    assert_eq!((r, shown), (Ok(Received::Data(40)), text(5001, 5101)));
    let (r, shown, _) = take(&mut h, usize::MAX);
    assert_eq!((r, shown), (Ok(Received::Data(60)), text(5041, 5101)));
    let (r, shown, _) = take(&mut h, usize::MAX);
    assert_eq!((r, shown), (Ok(Received::End), Vec::new()));
}
