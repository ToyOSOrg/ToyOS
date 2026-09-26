//! A TCP receiver that falls a whole pipe behind still gets every byte, once.
//!
//! netd moves a connection's received bytes out of its TCP socket into the
//! client's receive pipe. A pipe is a ring of fixed capacity, so a client that
//! stops reading fills it, and from then on the only lawful home for the
//! peer's further bytes is the socket's own buffer, whose window then closes.
//!
//! This program is that client: it connects to the harness's host server
//! (argv[1] is its port on `HOST`), which sends `stream_byte`s and closes, and
//! reads **nothing** until the ring holds a whole capacity — seen through its
//! own `SYS_PIPE_MAP` window of the receive pipe. Only then does it read the
//! stream to its end and compare each byte with the pattern.
//!
//! The capacity is measured, never assumed: a fresh pipe of this process's own
//! is written until the kernel refuses a byte.
//!
//! `netd_slow_reader: ok bytes=<n>` is the only success line; a stream that
//! differs names its first differing offset and exits non-zero.

#[path = "../netd_stream.rs"]
mod netd_stream;

use std::time::Duration;

use netd_stream::{ask, await_ring_full, read_pattern, ring_capacity, HOST};

/// Bytes the host sends past the ring's capacity. Anything over the socket's
/// own buffer makes a pipe that drops bytes when full drop some.
const PAST_THE_RING: u64 = 1024 * 1024;

/// Liveness guards, said by name rather than left to the runner's ceiling: the
/// host has more than a capacity to send the moment this connects, so a ring
/// that never fills or a stream that stops is a netd that stopped moving bytes.
const FILL_BOUND: Duration = Duration::from_secs(60);
const READ_BOUND: Duration = Duration::from_secs(30);

fn main() {
    let port: u16 = std::env::args()
        .nth(1)
        .and_then(|p| p.parse().ok())
        .expect("usage: netd_slow_reader <host port>");
    let capacity = ring_capacity();
    let total = capacity + PAST_THE_RING;
    println!("netd_slow_reader: ring capacity {capacity}, expecting {total} bytes");

    let conn = toyos::net::tcp_connect(HOST, port, 30_000).expect("connect to the host server");
    // The host learns how much to send from here, before it sends anything,
    // so the two ends cannot disagree about the length being judged.
    ask(&conn.tx, total);
    await_ring_full(&conn.rx, capacity, FILL_BOUND);
    println!("netd_slow_reader: the ring is full at {capacity} bytes unread; reading");

    let what = format!("netd_slow_reader (ring capacity {capacity})");
    let at = read_pattern(&conn.rx, READ_BOUND, &what);
    assert_eq!(at, total, "the stream ended after {at} of {total} bytes, every one of them right");
    println!("netd_slow_reader: ok bytes={at}");
}
