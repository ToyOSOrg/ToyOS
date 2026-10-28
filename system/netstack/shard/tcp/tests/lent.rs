//! Lent buffers: storage the user holds and reads and writes itself, which [tcp] copies into and
//! out of. No scenario ids: the specifications hold no such call. What lending must keep is
//! RFC 9293's: text past a hole is held for later processing and a FIN only once every byte before
//! it is in (§3.10.7.4), a read owes the window update a read owes (§3.8.6.2.2), and the text a
//! segment carries is the user's, in order (§3.9.1.2).

mod common;

use std::cell::RefCell;
use std::rc::Rc;

use common::*;
use toyos_net_tcp::{Error, Lent, State};

/// A buffer the test holds beside [tcp]: the user's view of its bytes.
#[derive(Clone, Debug)]
struct Shared(Rc<RefCell<Vec<u8>>>);

impl Shared {
    fn new(len: usize) -> Self {
        Self(Rc::new(RefCell::new(vec![0; len])))
    }

    /// `len` bytes from `at`, wrapping at the end.
    fn bytes(&self, at: usize, len: usize) -> Vec<u8> {
        let bytes = self.0.borrow();
        (0..len).map(|i| bytes[(at + i) % bytes.len()]).collect()
    }

    /// The user writes `data` at `at`, wrapping at the end.
    fn write(&self, at: usize, data: &[u8]) {
        let mut bytes = self.0.borrow_mut();
        let len = bytes.len();
        for (i, b) in data.iter().enumerate() {
            bytes[(at + i) % len] = *b;
        }
    }
}

impl Lent for Shared {
    fn len(&self) -> usize {
        self.0.borrow().len()
    }

    fn most(&self) -> usize {
        self.0.borrow().len()
    }

    fn want(&mut self, _: usize) {}

    /// A fixed ring: a position is its offset below the length.
    fn put(&mut self, at: usize, data: &[u8]) {
        let mut bytes = self.0.borrow_mut();
        let len = bytes.len();
        for (i, b) in data.iter().enumerate() {
            bytes[(at + i) % len] = *b;
        }
    }

    fn take(&self, at: usize, out: &mut [u8]) {
        let bytes = self.0.borrow();
        for (i, b) in out.iter_mut().enumerate() {
            *b = bytes[(at + i) % bytes.len()];
        }
    }
}

/// The connection's buffers lent: two of `len` bytes, the test's views of them.
fn lend(h: &mut H, len: usize) -> (Shared, Shared) {
    let (rx, tx) = (Shared::new(len), Shared::new(len));
    let (lent, _) = h.call(h.t, |tcp, _, id| tcp.lend(id, Box::new(rx.clone()), Box::new(tx.clone())));
    assert_eq!(lent, Ok(()));
    (rx, tx)
}

/// The text a well-behaved peer sends in `[from, to)`.
fn text(from: u32, to: u32) -> Vec<u8> {
    (from..to).map(pattern).collect()
}

// A child holds text in order, text past a hole and a FIN behind that before it is lent (RFC 9293
// §3.10.7.4: kept for later processing, the FIN not yet taken): lent, every byte is at its
// offset, and the hole filled after it makes the text whole and takes the FIN.
#[test]
fn what_a_connection_held_moves_to_its_lent_ring_at_its_offsets() {
    let mut h = listening();
    h.input(0, seg(5000).syn().wnd(65_535).mss(1460));
    h.input(10, seg(5001).ack(1001).len(100));
    h.input(20, seg(5201).ack(1001).len(100).fin());
    h.conn = Some(h.tcp.accept(h.listener.unwrap()).unwrap().expect("a ready child"));
    let (rx, _) = lend(&mut h, 65_535 * 2);
    assert_eq!(h.status().readable, 100);
    assert_eq!(rx.bytes(0, 100), text(5001, 5101), "the text in order");
    assert_eq!(rx.bytes(200, 100), text(5201, 5301), "the text past the hole, at its offset");
    let outs = h.input(30, seg(5101).ack(1001).len(100));
    assert_eq!((h.status().readable, h.status().state), (300, State::CloseWait), "the hole filled, and the FIN taken");
    expect(&outs, &["ACK=5302"]);
    assert_eq!(rx.bytes(0, 300), text(5001, 5301));
}

// RX-13's window update, owed by the user's read out of the lent ring as by `recv`: 65,535
// unread bytes shut the window, and a read that moves the edge by 1,500 from a window of 0 is
// acknowledged at once.
#[test]
fn a_read_out_of_the_lent_ring_owes_the_window_update_a_recv_does() {
    let mut h = fixture_e();
    lend(&mut h, 65_535 * 2);
    let mut seq = 5001u32;
    while seq < 70_536 {
        let n = (70_536 - seq).min(1460);
        h.arrive(seg(seq).ack(1001).len(n as usize));
        seq += n;
    }
    h.at(1);
    assert!(h.status().starved, "a shut window");
    let (r, outs) = h.call(1, |tcp, now, id| tcp.consume(now, id, 1000));
    assert_eq!(r, Ok(1000));
    nothing(&outs);
    let (r, outs) = h.call(1, |tcp, now, id| tcp.consume(now, id, 500));
    assert_eq!(r, Ok(500));
    expect(&outs, &["ACK=70536 WND=1500"]);
}

// What the user queued in the lent send ring is what the segments carry, in order (RFC 9293
// §3.9.1.2), across the ring's end, and an acknowledgment lets go of what it covers.
#[test]
fn the_lent_send_ring_is_the_text_segments_carry_across_its_end() {
    let mut h = fixture_e();
    let (_, tx) = lend(&mut h, 4000);
    let data: Vec<u8> = (0..2920u32).map(|i| (i % 251) as u8).collect();
    tx.write(0, &data);
    let (r, outs) = h.call(0, |tcp, now, id| tcp.queue(now, id, 2920));
    assert_eq!(r, Ok(2920));
    expect(&outs, &["SEQ=1001 LEN=1460", "SEQ=2461 LEN=1460"]);
    assert_eq!([&outs[0].payload[..], &outs[1].payload[..]].concat(), data);
    h.input(10, seg(5001).ack(3921));
    assert_eq!(h.status().queued, 0, "both acknowledged");
    let wrapped: Vec<u8> = (0..2920u32).map(|i| (i % 13) as u8).collect();
    tx.write(2920, &wrapped);
    let (r, outs) = h.call(20, |tcp, now, id| tcp.queue(now, id, 2920));
    assert_eq!(r, Ok(2920));
    expect(&outs, &["SEQ=3921 LEN=1460", "SEQ=5381 LEN=1460"]);
    assert_eq!([&outs[0].payload[..], &outs[1].payload[..]].concat(), wrapped, "the text across the ring's end");
    assert_eq!(h.status().queued, 2920);
}

// A buffer is lent once, and only a synchronized connection's.
#[test]
fn a_connection_is_lent_once_and_only_once_synchronized() {
    let mut h = H::new(65_535);
    h.start(-10);
    let id = h.tcp.connect(h.now(), A, Some(port(49152)), ep(B, 80)).unwrap();
    h.conn = Some(id);
    let (r, _) = h.call(-10, |tcp, _, id| tcp.lend(id, Box::new(Shared::new(10)), Box::new(Shared::new(10))));
    assert_eq!(r, Err(Error::NotConnected), "a connection in SYN-SENT");
    h.input(0, seg(5000).ack(1001).syn().wnd(65_535).mss(1460));
    lend(&mut h, 65_535);
    let (r, _) = h.call(0, |tcp, _, id| tcp.lend(id, Box::new(Shared::new(65_535)), Box::new(Shared::new(65_535))));
    assert_eq!(r, Err(Error::NotConnected), "lent already");
}
