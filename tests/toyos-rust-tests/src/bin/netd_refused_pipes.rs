//! A client's pipe that refuses netd costs that client its connection, and
//! never netd.
//!
//! A client moves netd the far ends of its data pipes, and nothing about the
//! handles it moves is checked: whatever they are, netd writes and reads them.
//! Each case below hands netd one it cannot use, or takes one away under it,
//! and is followed by an ordinary connection that must round-trip — the proof
//! that netd is still there to serve it:
//!
//! 1. `to_client` is a pipe's **read** end. netd's writes are refused.
//! 2. `from_client` is a pipe's **write** end. netd's reads are refused.
//! 3. A listener's notify handle is a pipe's read end.
//! 4. The client drops its receive end unread while its ring is full and the
//!    host is still sending, so netd's next write finds no reader.
//!
//! Cases 1 and 3 hand netd the read end of a pipe this program has filled
//! and keeps writing into: the pipe has room again only once every reader is
//! gone, so its write end turning writable is netd letting go of the handle —
//! an event, not a guess about when netd got round to it.
//!
//! argv[1] is the port of the harness's host server on `HOST`.
//! `netd_refused_pipes: ok` is the only success line.

#[path = "../netd_stream.rs"]
mod netd_stream;

use std::time::{Duration, Instant};

use netd_stream::{ask, await_ring_full, await_until, fill, read_pattern, ring_capacity, HOST};
use toyos::net::{
    MsgType, NetdConn, TcpBindPipedRequest, TcpBindResponse, TcpConnectPipedRequest,
    TcpConnectResponse,
};
use toyos::poller::{READABLE, WRITABLE};
use toyos::Pipe;
use toyos_abi::syscall::SyscallError;

/// How long netd may take to act on something it has been handed. Orders of
/// magnitude over a pass; a bound, said by name, not a pace.
const WITHIN: Duration = Duration::from_secs(20);

/// Bytes a round trip asks for: more than one pipe write and one TCP segment,
/// far less than a ring.
const ROUND_TRIP: u64 = 256 * 1024;

/// Bytes the host sends past the ring's capacity in case 4, so the socket still
/// holds some when the receive end goes.
const PAST_THE_RING: u64 = 1024 * 1024;

fn main() {
    let port: u16 = std::env::args()
        .nth(1)
        .and_then(|p| p.parse().ok())
        .expect("usage: netd_refused_pipes <host port>");
    let capacity = ring_capacity();

    let cases: [(&str, &dyn Fn()); 4] = [
        ("a receive end netd cannot write", &|| receive_end_is_a_read_end(port)),
        ("a send end netd cannot read", &|| send_end_is_a_write_end(port)),
        ("a notify end netd cannot write", &|| notify_end_is_a_read_end()),
        ("a receive end dropped while netd held bytes for it", &|| {
            receive_end_dropped_while_held(port, capacity)
        }),
    ];
    for (case, run) in cases {
        let started = Instant::now();
        run();
        round_trip(port, &format!("after {case}"));
        println!("netd_refused_pipes: {case}, and a round trip after it, in {} ms", started.elapsed().as_millis());
    }
    println!("netd_refused_pipes: ok");
}

/// Ask netd for a connection to the host, handing it `to_client` and
/// `from_client` as they are.
fn connect_with(port: u16, to_client: Pipe, from_client: Pipe) {
    let resp: TcpConnectResponse = NetdConn::connect()
        .expect("netd is serving")
        .request_with_handles(
            &[to_client.into_raw(), from_client.into_raw()],
            MsgType::TcpConnectPiped,
            &TcpConnectPipedRequest { addr: HOST, port, _pad: 0, timeout_ms: 30_000 },
        )
        .expect("netd takes the request")
        .response()
        .expect("netd connects to the host");
    println!("netd_refused_pipes: connected, socket {}", resp.socket_id);
}

/// A pipe whose read end is for netd, and whose write end, filled, is this
/// program's watch on netd letting go of it.
fn watched_read_end() -> (Pipe, Pipe) {
    let (read, write) = toyos::pipe_pair().expect("a pipe");
    fill(&write);
    (read, write)
}

/// netd has closed the read end of `write`'s pipe: a zero-byte write names the
/// reader gone, looked at again each time the filled pipe reports room.
fn await_released(write: &Pipe, what: &str) {
    await_until(write, WRITABLE, WITHIN, what, || match write.write_nonblock(&[]) {
        Err(SyscallError::Gone) => Some(()),
        Err(SyscallError::WouldBlock) => None,
        other => panic!("{what}: a zero-byte write into a full pipe answered {other:?}"),
    });
}

fn receive_end_is_a_read_end(port: u16) {
    let (read_end, watch) = watched_read_end();
    let (from_client, tx) = toyos::pipe_pair().expect("a pipe");
    connect_with(port, read_end, from_client);
    // Something for netd to try to deliver, unless netd has already refused
    // the receive end on its own and closed this pipe's far end with it.
    let asked = 64u64.to_le_bytes();
    match tx.write(&asked) {
        Ok(n) => assert_eq!(n, asked.len(), "telling the host how much to send"),
        Err(e) => assert_eq!(e, SyscallError::Gone, "telling the host how much to send"),
    }
    await_released(&watch, "a read end handed over as the receive pipe");
}

fn send_end_is_a_write_end(port: u16) {
    let (rx, to_client) = toyos::pipe_pair().expect("a pipe");
    let (_kept, write_end) = toyos::pipe_pair().expect("a pipe");
    connect_with(port, to_client, write_end);
    // netd ends the connection by closing the receive pipe's write end, which
    // this program reads as EOF.
    let what = "a write end handed over as the send pipe";
    let mut buf = [0u8; 64];
    let got = await_until(&rx, READABLE, WITHIN, what, || match rx.read_nonblock(&mut buf) {
        Err(SyscallError::WouldBlock) => None,
        other => Some(other),
    });
    assert_eq!(got, Ok(0), "{what}: bytes or a refusal, not EOF");
}

fn notify_end_is_a_read_end() {
    let (read_end, watch) = watched_read_end();
    let resp: TcpBindResponse = NetdConn::connect()
        .expect("netd is serving")
        .request_with_handles(
            &[read_end.into_raw()],
            MsgType::TcpBindPiped,
            &TcpBindPipedRequest { addr: [0; 4], port: 0, _pad: 0 },
        )
        .expect("netd takes the request")
        .response()
        .expect("netd binds");
    println!("netd_refused_pipes: bound, socket {}", resp.socket_id);
    await_released(&watch, "a read end handed over as the notify pipe");
}

fn receive_end_dropped_while_held(port: u16, capacity: u64) {
    let conn = toyos::net::tcp_connect(HOST, port, 30_000).expect("connect to the host server");
    ask(&conn.tx, capacity + PAST_THE_RING);
    await_ring_full(&conn.rx, capacity, WITHIN);
    drop(conn.rx);
    println!("netd_refused_pipes: dropped a full receive end with the host still sending");
}

fn round_trip(port: u16, what: &str) {
    let conn = toyos::net::tcp_connect(HOST, port, 30_000)
        .unwrap_or_else(|e| panic!("{what}: netd did not connect: {e:?}"));
    ask(&conn.tx, ROUND_TRIP);
    let got = read_pattern(&conn.rx, WITHIN, what);
    assert_eq!(got, ROUND_TRIP, "{what}: the stream ended after {got} of {ROUND_TRIP} bytes");
    println!("netd_refused_pipes: round trip {what}: {got} bytes");
}
