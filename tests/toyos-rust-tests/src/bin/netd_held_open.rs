//! Bytes netd holds back in a socket past a full receive pipe move when the
//! reader makes room, and on nothing else.
//!
//! The host sends a pipe's capacity and 32 KiB more, which the pipe and the
//! 64 KiB socket buffer hold between them, and then holds the connection open
//! with no FIN: its bytes are all acknowledged and the window never closes, so
//! the peer has nothing to send and nothing to probe. Once the ring is full
//! this program makes room a [`STEP`] at a time, and after each waits for the
//! next `STEP` of what the socket held to reach the ring. The pipe's room is
//! the only event each step makes: a netd that does not watch for it is moved
//! at most by a timer it already had pending, which is spent by the first step
//! it rescues.
//!
//! argv[1] is the port of the harness's host server on `HOST`.
//! `netd_held_open: ok bytes=<n>` is the only success line.

#[path = "../netd_stream.rs"]
mod netd_stream;

use std::time::{Duration, Instant};

use netd_stream::{ask, await_ring_full, await_ring_holds, read_pattern_from, ring_capacity, Ask, HOST};

/// Bytes the host sends past the ring's capacity: less than the socket's
/// 64 KiB buffer, so the window never closes on the peer.
const PAST_THE_RING: u64 = 32 * 1024;

/// Room made at a time, and the bytes each step waits to see arrive.
const STEP: u64 = 1024;

/// Liveness guards, said by name: the host has all of it to send the moment
/// this connects, and each step is one pass of netd's.
const FILL_BOUND: Duration = Duration::from_secs(60);
const STEP_BOUND: Duration = Duration::from_secs(20);

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
    println!("netd_held_open: the ring is full at {capacity} bytes unread; making room a step at a time");

    let what = format!("netd_held_open (ring capacity {capacity}, the peer silent)");
    let started = Instant::now();
    let mut at = 0;
    while at < PAST_THE_RING {
        at = read_pattern_from(&conn.rx, at, at + STEP, STEP_BOUND, &what);
        await_ring_holds(&conn.rx, capacity, capacity + at - 16, STEP_BOUND);
    }
    let stepped = started.elapsed();
    let at = read_pattern_from(&conn.rx, at, total, STEP_BOUND, &what);
    assert_eq!(at, total, "the stream ended after {at} of {total} bytes, every one of them right");
    println!(
        "netd_held_open: ok bytes={at}, the last {PAST_THE_RING} moved a {STEP}-byte step at a time in {} ms",
        stepped.as_millis()
    );
}
