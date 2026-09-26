//! A client's handle that refuses netd costs that client its connection, and
//! never netd.
//!
//! A client moves netd the far ends of its data pipes and its listener's
//! notify pipe, and nothing about the handles it moves is checked: whatever
//! they are, netd writes and reads them. Each case below hands netd one it
//! cannot use, or takes one away under it, and is followed by an ordinary
//! connection that must round-trip — the proof that netd is still there to
//! serve it:
//!
//! 1. `to_client` is a pipe's **read** end. netd's writes are refused.
//! 2. `from_client` is a pipe's **write** end. netd's reads are refused.
//! 3. A listener's notify handle is a pipe's read end.
//! 4. The client drops its receive end unread while its ring is full and the
//!    host is still sending, so netd's next write finds no reader.
//! 5. `to_client` is a file seeked to exactly the kernel's size limit. A
//!    zero-byte write there is taken, so netd's liveness probe passes, and
//!    every write of a byte is refused: only the write of the peer's bytes
//!    meets the refusal.
//! 6. A listener's notify handle is that same kind of file, and the host
//!    connects to the listener, so the refusal meets the wake netd owes.
//!
//! netd letting go of a handle is observed as an event, never waited out: a
//! pipe this program keeps full regains room once every reader is gone, and a
//! listener netd closes turns the host's connection into it away.
//!
//! argv[1] is the port of the harness's host server on `HOST`, and the harness
//! forwards a host port to this guest's `FORWARDED_PORT`.
//! `netd_refused_pipes: ok` is the only success line.

#[path = "../netd_stream.rs"]
mod netd_stream;

use std::time::{Duration, Instant};

use netd_stream::{
    ask, ask_bytes, await_ring_full, await_until, keep_full_until_released, read_pattern, ring_capacity, Ask,
    FORWARDED_PORT, HOST,
};
use toyos::net::{
    MsgType, NetError, NetdConn, TcpBindPipedRequest, TcpBindResponse, TcpConnectPipedRequest,
    TcpConnectResponse, TcpSocketId,
};
use toyos::poller::READABLE;
use toyos::Pipe;
use toyos_abi::syscall::{self, OpenFlags, SeekFrom, SyscallError};
use toyos_abi::RawHandle;

/// How long netd may take to act on something it has been handed. Orders of
/// magnitude over a pass; a bound, said by name, not a pace.
const WITHIN: Duration = Duration::from_secs(20);

/// Bytes a round trip asks for: more than one pipe write and one TCP segment,
/// far less than a ring.
const ROUND_TRIP: u64 = 256 * 1024;

/// Bytes the host sends past the ring's capacity in case 4, so the socket still
/// holds some when the receive end goes.
const PAST_THE_RING: u64 = 1024 * 1024;

/// The kernel's largest file: 2^32 pages of 4 KiB. Asserted where it is used,
/// by a seek one byte past it being refused.
const FILE_LIMIT: u64 = (u32::MAX as u64 + 1) * 4096;

/// The file cases 5 and 6 hand netd, one at a time.
const FILE_PATH: &str = "/tmp/netd_refused_pipes";

fn main() {
    let port: u16 = std::env::args()
        .nth(1)
        .and_then(|p| p.parse().ok())
        .expect("usage: netd_refused_pipes <host port>");
    let capacity = ring_capacity();

    let cases: [(&str, &dyn Fn()); 6] = [
        ("a receive end netd cannot write", &|| receive_end_is_a_read_end(port)),
        ("a send end netd cannot read", &|| send_end_is_a_write_end(port)),
        ("a notify end netd cannot write", &|| notify_end_is_a_read_end()),
        ("a receive end dropped while netd held bytes for it", &|| {
            receive_end_dropped_while_held(port, capacity)
        }),
        ("a receive end at its size limit", &|| receive_end_is_a_full_file(port)),
        ("a notify end at its size limit, owed a wake", &|| notify_end_is_a_full_file(port)),
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
fn connect_with(port: u16, to_client: RawHandle, from_client: Pipe) {
    let resp: TcpConnectResponse = NetdConn::connect()
        .expect("netd is serving")
        .request_with_handles(
            &[to_client, from_client.into_raw()],
            MsgType::TcpConnectPiped,
            &TcpConnectPipedRequest { addr: HOST, port, _pad: 0, timeout_ms: 30_000 },
        )
        .expect("netd takes the request")
        .response()
        .expect("netd connects to the host");
    println!("netd_refused_pipes: connected, socket {}", resp.socket_id);
}

/// Ask netd for a listener on `port`, handing it `notify` as it is.
fn bind_with(port: u16, notify: RawHandle) -> TcpSocketId {
    let resp: TcpBindResponse = NetdConn::connect()
        .expect("netd is serving")
        .request_with_handles(
            &[notify],
            MsgType::TcpBindPiped,
            &TcpBindPipedRequest { addr: [0; 4], port, _pad: 0 },
        )
        .expect("netd takes the request")
        .response()
        .expect("netd binds");
    println!("netd_refused_pipes: bound, socket {} on port {}", resp.socket_id, resp.bound_port);
    TcpSocketId(resp.socket_id)
}

/// A file netd can write zero bytes to and not one more: seeked to exactly
/// [`FILE_LIMIT`], each half of that checked on this program's own handle.
fn file_at_its_limit() -> RawHandle {
    let file = syscall::open(FILE_PATH.as_bytes(), OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE)
        .expect("create a file in /tmp");
    assert_eq!(
        syscall::seek(file, SeekFrom::Start(FILE_LIMIT + 1)),
        Err(SyscallError::InvalidArgument),
        "a seek one byte past the size limit",
    );
    assert_eq!(syscall::seek(file, SeekFrom::Start(FILE_LIMIT)), Ok(FILE_LIMIT), "a seek to the size limit");
    assert_eq!(syscall::write_nonblock(file, &[]), Ok(0), "a zero-byte write at the size limit");
    assert_eq!(
        syscall::write_nonblock(file, &[0]),
        Err(SyscallError::InvalidArgument),
        "a one-byte write at the size limit",
    );
    file
}

fn receive_end_is_a_read_end(port: u16) {
    let (read_end, watch) = toyos::pipe_pair().expect("a pipe");
    let (from_client, tx) = toyos::pipe_pair().expect("a pipe");
    connect_with(port, read_end.into_raw(), from_client);
    // Something for netd to try to deliver, unless netd has already refused
    // the receive end on its own and closed this pipe's far end with it.
    let asked = ask_bytes(Ask::Stream(64));
    match tx.write(&asked) {
        Ok(n) => assert_eq!(n, asked.len(), "telling the host what to send"),
        Err(e) => assert_eq!(e, SyscallError::Gone, "telling the host what to send"),
    }
    keep_full_until_released(&watch, WITHIN, "a read end handed over as the receive pipe");
}

fn send_end_is_a_write_end(port: u16) {
    let (rx, to_client) = toyos::pipe_pair().expect("a pipe");
    let (_kept, write_end) = toyos::pipe_pair().expect("a pipe");
    connect_with(port, to_client.into_raw(), write_end);
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
    let (read_end, watch) = toyos::pipe_pair().expect("a pipe");
    bind_with(0, read_end.into_raw());
    keep_full_until_released(&watch, WITHIN, "a read end handed over as the notify pipe");
}

fn receive_end_dropped_while_held(port: u16, capacity: u64) {
    let conn = toyos::net::tcp_connect(HOST, port, 30_000).expect("connect to the host server");
    ask(&conn.tx, Ask::Stream(capacity + PAST_THE_RING));
    await_ring_full(&conn.rx, capacity, WITHIN);
    drop(conn.rx);
    println!("netd_refused_pipes: dropped a full receive end with the host still sending");
}

fn receive_end_is_a_full_file(port: u16) {
    let (from_client, tx) = toyos::pipe_pair().expect("a pipe");
    connect_with(port, file_at_its_limit(), from_client);
    ask(&tx, Ask::Stream(64));
    // netd ends the connection by closing the send pipe's read end.
    keep_full_until_released(&tx, WITHIN, "a file at its size limit handed over as the receive pipe");
    std::fs::remove_file(FILE_PATH).expect("remove the file");
}

fn notify_end_is_a_full_file(port: u16) {
    let listener = bind_with(FORWARDED_PORT, file_at_its_limit());
    // The host dials the listener and writes into the connection until it is
    // refused, which a listener netd has closed does at the host's next
    // segment; it then ends this connection.
    let dial = toyos::net::tcp_connect(HOST, port, 30_000).expect("connect to the host server");
    ask(&dial.tx, Ask::Dial);
    let what = "a file at its size limit handed over as the notify pipe, and the host dialling in";
    assert_eq!(read_pattern(&dial.rx, WITHIN, what), 0, "{what}: the host sent bytes, not its FIN");
    assert_eq!(
        toyos::net::tcp_accept(listener).err(),
        Some(NetError::NotConnected),
        "{what}: the listener is still there to accept from",
    );
    std::fs::remove_file(FILE_PATH).expect("remove the file");
}

fn round_trip(port: u16, what: &str) {
    let conn = toyos::net::tcp_connect(HOST, port, 30_000)
        .unwrap_or_else(|e| panic!("{what}: netd did not connect: {e:?}"));
    ask(&conn.tx, Ask::Stream(ROUND_TRIP));
    let got = read_pattern(&conn.rx, WITHIN, what);
    assert_eq!(got, ROUND_TRIP, "{what}: the stream ended after {got} of {ROUND_TRIP} bytes");
    println!("netd_refused_pipes: round trip {what}: {got} bytes");
}
