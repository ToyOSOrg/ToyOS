//! Bytes netd holds back in a socket past a full receive pipe move when the
//! reader makes room, and on nothing else.
//!
//! The host sends a pipe's capacity and 32 KiB more, which the pipe and the
//! 64 KiB socket buffer hold between them, and then holds the connection open
//! with no FIN. Once the rest has landed the peer has nothing to send and
//! nothing to probe: its bytes are all acknowledged and the window never
//! closed. So when this program reads the ring, the pipe's own room is the only
//! event that can move the 32 KiB left in the socket — a netd that does not
//! watch for it never delivers them.
//!
//! **The rest has landed before the reading starts**: the host wrote all of it
//! at once into a window that never closed, so a round trip on a second
//! connection, opened once the ring is full, completes behind it on the one
//! link.
//!
//! argv[1] is the port of the harness's host server on `HOST`.
//! `netd_held_open: ok bytes=<n>` is the only success line.

#[path = "../netd_stream.rs"]
mod netd_stream;

use std::time::Duration;

use netd_stream::{ask, await_ring_full, read_pattern, read_pattern_upto, ring_capacity, Ask, HOST};

/// Bytes the host sends past the ring's capacity: less than the socket's
/// 64 KiB buffer, so the window never closes on the peer.
const PAST_THE_RING: u64 = 32 * 1024;

/// Liveness guards, said by name: the host has all of it to send the moment
/// this connects.
const FILL_BOUND: Duration = Duration::from_secs(60);
const READ_BOUND: Duration = Duration::from_secs(20);

fn main() {
    let port: u16 = std::env::args()
        .nth(1)
        .and_then(|p| p.parse().ok())
        .expect("usage: netd_held_open <host port>");
    let capacity = ring_capacity();
    let total = capacity + PAST_THE_RING;
    println!("netd_held_open: ring capacity {capacity}, expecting {total} bytes and no FIN");

    let conn = toyos::net::tcp_connect(HOST, port, 30_000).expect("connect to the host server");
    ask(&conn.tx, Ask::Held(total));
    await_ring_full(&conn.rx, capacity, FILL_BOUND);

    let behind = toyos::net::tcp_connect(HOST, port, 30_000).expect("connect to the host server again");
    ask(&behind.tx, Ask::Stream(16));
    let got = read_pattern(&behind.rx, READ_BOUND, "netd_held_open: the round trip behind the rest");
    assert_eq!(got, 16, "the round trip behind the rest ended after {got} of 16 bytes");
    println!("netd_held_open: the ring is full at {capacity} bytes unread, the rest behind it; reading");

    let what = format!("netd_held_open (ring capacity {capacity}, the peer silent)");
    let at = read_pattern_upto(&conn.rx, total, READ_BOUND, &what);
    assert_eq!(at, total, "the stream ended after {at} of {total} bytes, every one of them right");
    println!("netd_held_open: ok bytes={at}");
}
