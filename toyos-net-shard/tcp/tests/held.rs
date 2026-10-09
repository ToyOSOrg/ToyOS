//! What [tcp] says it holds, for a caller that bounds it or keeps a copy: `orphans`, `ready` and
//! `options`. No scenario ids: the specifications hold none of the calls; the connections
//! counted are `close.rs`'s and `listen.rs`'s, by their scenarios' own segments.

mod common;

use common::*;
use toyos_net_tcp::{limits, Error, Options, State};

#[test]
fn an_orphan_is_counted_until_both_fins() {
    let mut h = fixture_e();
    assert_eq!(h.tcp.orphans(), 0, "a held connection is its user's");
    h.close(0);
    assert_eq!(h.tcp.orphans(), 1);
    h.input(10, seg(5001).ack(1002).fin());
    assert_eq!((h.tcp.orphans(), h.tcp.time_wait_count()), (0, 1), "TIME-WAIT is no connection");
}

#[test]
fn an_orphan_is_counted_until_its_idle_reset() {
    let mut h = fixture_e();
    h.close(0);
    h.input(10, seg(5001).ack(1002));
    h.at(60_009);
    assert_eq!(h.tcp.orphans(), 1);
    h.at(60_010);
    assert_eq!(h.tcp.orphans(), 0);
}

#[test]
fn a_close_before_the_handshake_ends_leaves_an_orphan_until_it_gives_up() {
    let mut h = H::new(65_535);
    h.start(0);
    h.conn = Some(h.tcp.connect(h.now(), A, Some(port(49152)), ep(B, 80)).unwrap());
    h.transmit();
    h.input(5, seg(5000).syn().mss(1460));
    assert_eq!(h.status().state, State::SynReceived);
    h.close(6);
    assert_eq!(h.tcp.orphans(), 1);
    h.at(2 * i64::try_from(limits::SYN_GIVE_UP.as_millis()).unwrap());
    assert_eq!(h.tcp.orphans(), 0);
}

#[test]
fn a_close_that_resets_leaves_no_orphan() {
    let mut h = fixture_e();
    h.input(0, seg(5001).ack(1001).len(100));
    h.close(1);
    assert_eq!(h.tcp.orphans(), 0, "unread text makes the close a reset");
}

#[test]
fn ready_counts_the_connections_accept_has_yet_to_return() {
    let mut h = listening();
    let listener = h.listener.unwrap();
    h.input(0, seg(5000).syn().mss(1460));
    assert_eq!(h.tcp.ready(listener), Ok(0), "a handshake in progress is not ready");
    h.input(1, seg(5001).ack(1001));
    assert_eq!(h.tcp.ready(listener), Ok(1));
    assert!(h.tcp.accept(listener).unwrap().is_some());
    assert_eq!(h.tcp.ready(listener), Ok(0));
    let now = h.now();
    h.tcp.close_listener(now, listener).unwrap();
    assert_eq!(h.tcp.ready(listener), Err(toyos_net_tcp::Error::NoSuchSocket));
}

#[test]
fn options_are_the_ones_the_connection_has() {
    let mut h = listening();
    let listener = h.listener.unwrap();
    let nodelay = Options { nodelay: true, ..Options::default() };
    h.tcp.set_listener_options(listener, nodelay).unwrap();
    h.input(0, seg(5000).syn().mss(1460));
    h.input(1, seg(5001).ack(1001));
    let conn = h.tcp.accept(listener).unwrap().unwrap();
    assert_eq!(h.tcp.options(conn), Ok(nodelay), "its listener's, as its SYN found them");
    let now = h.now();
    h.tcp.set_options(now, conn, Options::default()).unwrap();
    assert_eq!(h.tcp.options(conn), Ok(Options::default()));
    h.tcp.abort(now, conn).unwrap();
    assert_eq!(h.tcp.options(conn), Err(Error::NoSuchSocket));
}
