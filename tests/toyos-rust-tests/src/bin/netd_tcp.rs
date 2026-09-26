//! A TCP client through std, against the harness's host peer
//! (`tests/common/tcppeer.rs`): every case a real client meets on the
//! internet, each judged by what std answered and by a SHA-256 over the bytes
//! that crossed, which the host computes over its own side of the same stream.
//!
//! `test_rs_netd_tcp <peer port> <case> [args]`. A connection opens with one
//! request (`ask`): a mode byte, then the length and the seed of the stream,
//! each eight little-endian bytes. Every case prints `netd_tcp: <case> ok ...`
//! as its only success line, and the fields the host judges on it.

use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, SocketAddrV4, TcpStream, UdpSocket};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[path = "../netd_stream.rs"]
mod netd_stream;

use sha2::{Digest, Sha256};
use toyos::ipc::{FrameRx, RxStep};
use toyos::poller::{Poller, READABLE};
use toyos_inspect::{Value, MAX_SNAPSHOT_BYTES, MSG_INSPECT, MSG_SNAPSHOT};

/// The host, as QEMU's slirp shows it to the guest.
const HOST: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 2);

/// An address on slirp's own network that nothing answers: slirp replies to
/// ARP for its own addresses alone, so a SYN to this one never leaves.
const NOBODY: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 99);

/// The host peer's modes, the other half of `tcppeer::Mode`.
const DOWNLOAD: u8 = 0;
const UPLOAD: u8 = 1;
const RESET: u8 = 2;
const HOLD: u8 = 3;
const PATTERN: u8 = 4;

/// A liveness guard on every blocking step, said by name: the host has
/// everything it needs the moment a request arrives, so a step that outlasts
/// this is a client or a netd that stopped moving.
const STEP: Duration = Duration::from_secs(60);

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let port: u16 = args.get(1).and_then(|p| p.parse().ok()).expect("usage: netd_tcp <peer port> <case> [args]");
    let peer = SocketAddr::V4(SocketAddrV4::new(HOST, port));
    let number = |i: usize| -> u64 {
        args.get(i).and_then(|a| a.parse().ok()).unwrap_or_else(|| panic!("argument {i} is a number"))
    };
    match args.get(2).map(String::as_str) {
        Some("unreachable") => unreachable(peer, number(3) as u16),
        Some("reset") => reset(peer, number(3)),
        Some("half_close") => half_close(peer, number(3)),
        Some("download") => download(peer, number(3), number(4)),
        Some("download_to_end") => download_to_end(peer, number(3)),
        Some("upload") => upload(peer, number(3), number(4)),
        Some("upload_drop") => upload_drop(peer, number(3)),
        Some("timeouts") => timeouts(peer),
        Some("many") => many(peer, number(3) as usize, number(4)),
        Some("leaves") => leaves(peer),
        Some("reader_leaves") => reader_leaves(peer),
        Some("time_wait") => time_wait(peer),
        Some("ports") => ports(peer, number(3) as usize),
        Some("child_connect") => child_connect(),
        Some("child_udp") => child_udp(),
        Some("child_receive") => child_receive(peer),
        Some("child_idle") => child_idle(peer),
        Some("orphan") => orphan(peer, number(3)),
        other => panic!("netd_tcp: no case {other:?}"),
    }
}

/// A connection to the host peer, asking for `mode` over `len` bytes of the
/// stream `seed` names.
fn ask(peer: SocketAddr, mode: u8, len: u64, seed: u64) -> TcpStream {
    let mut stream = TcpStream::connect_timeout(&peer, STEP).expect("connect to the host peer");
    let mut request = [0u8; 17];
    request[0] = mode;
    request[1..9].copy_from_slice(&len.to_le_bytes());
    request[9..].copy_from_slice(&seed.to_le_bytes());
    stream.write_all(&request).expect("send the request");
    stream
}

/// The bytes this side sends: splitmix64, eight bytes a step. Nothing on the
/// host regenerates them; the host hashes what it received.
struct Pattern(u64);

impl Pattern {
    fn fill(&mut self, buf: &mut [u8]) {
        for chunk in buf.chunks_mut(8) {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^= z >> 31;
            chunk.copy_from_slice(&z.to_le_bytes()[..chunk.len()]);
        }
    }
}

fn hex(digest: &[u8]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn mb_per_s(bytes: u64, took: Duration) -> String {
    format!("{:.2}", bytes as f64 / took.as_secs_f64() / 1_000_000.0)
}

/// Read `stream` to its end, answering how many bytes came and their hash.
fn drain(stream: &mut TcpStream, what: &str) -> (u64, [u8; 32]) {
    stream.set_read_timeout(Some(STEP)).expect("set the liveness guard");
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; 65536];
    let mut total = 0u64;
    loop {
        match stream.read(&mut buf) {
            Ok(0) => return (total, hash.finalize().into()),
            Ok(n) => {
                hash.update(&buf[..n]);
                total += n as u64;
            }
            Err(e) => panic!("{what}: reading after {total} bytes: {e} ({:?})", e.kind()),
        }
    }
}

/// Write `len` bytes of the pattern `seed` names, answering their hash.
fn send(stream: &mut TcpStream, len: u64, seed: u64, what: &str) -> [u8; 32] {
    let mut pattern = Pattern(seed);
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; 65536];
    let mut sent = 0u64;
    while sent < len {
        let n = buf.len().min((len - sent) as usize);
        pattern.fill(&mut buf[..n]);
        hash.update(&buf[..n]);
        stream.write_all(&buf[..n]).unwrap_or_else(|e| panic!("{what}: writing at {sent} of {len}: {e}"));
        sent += n as u64;
    }
    hash.finalize().into()
}

/// The host's answer to an upload: how many bytes it read to the end of the
/// stream, and their hash.
fn upload_answer(stream: &mut TcpStream, what: &str) -> (u64, [u8; 32]) {
    stream.set_read_timeout(Some(STEP)).expect("set the liveness guard");
    let mut answer = Vec::new();
    stream.read_to_end(&mut answer).unwrap_or_else(|e| panic!("{what}: reading the host's answer: {e}"));
    assert_eq!(answer.len(), 40, "{what}: the host's answer is {} bytes, not a count and a hash", answer.len());
    (u64::from_le_bytes(answer[..8].try_into().unwrap()), answer[8..].try_into().unwrap())
}

/// A connect nothing answers ends at its own timeout, a refused one at once,
/// and a list of addresses is tried in order until one answers.
fn unreachable(peer: SocketAddr, closed: u16) {
    const ASKED: Duration = Duration::from_millis(1500);
    let nobody = SocketAddr::V4(SocketAddrV4::new(NOBODY, 9));
    let started = Instant::now();
    let err = TcpStream::connect_timeout(&nobody, ASKED).expect_err("a connect to nobody succeeded");
    let took = started.elapsed();
    assert_eq!(err.kind(), ErrorKind::TimedOut, "a connect to nobody ended {err} ({:?})", err.kind());
    assert!(took >= ASKED, "a connect with a {ASKED:?} timeout gave up after {took:?}");
    assert!(took < ASKED + STEP, "a connect with a {ASKED:?} timeout took {took:?}");

    let refused = SocketAddr::V4(SocketAddrV4::new(HOST, closed));
    let err = TcpStream::connect(refused).expect_err("a connect to a closed port succeeded");
    assert_eq!(err.kind(), ErrorKind::ConnectionRefused, "a connect to a closed port ended {err} ({:?})", err.kind());

    let mut stream = TcpStream::connect(&[refused, peer][..]).expect("the second of two addresses answers");
    assert_eq!(stream.peer_addr().expect("its peer"), peer, "connected to the wrong address");
    stream.write_all(&[HOLD; 17]).expect("the connection that answered takes a request");
    println!("netd_tcp: unreachable ok timed_out_ms={}", took.as_millis());
}

/// Bytes the peer sent before its reset arrive, and then the reset does, as
/// `ConnectionReset` on the next read and an error on the next write.
fn reset(peer: SocketAddr, len: u64) {
    let mut stream = ask(peer, RESET, len, 7);
    stream.set_read_timeout(Some(STEP)).expect("set the liveness guard");
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; 65536];
    let mut got = 0u64;
    while got < len {
        let n = buf.len().min((len - got) as usize);
        let n = stream.read(&mut buf[..n]).unwrap_or_else(|e| panic!("reset: reading at {got} of {len}: {e}"));
        assert!(n > 0, "reset: the stream ended after {got} of {len} bytes");
        hash.update(&buf[..n]);
        got += n as u64;
    }
    stream.write_all(&[1]).expect("reset: tell the host every byte is here");
    let err = match stream.read(&mut buf) {
        Ok(n) => panic!("reset: the read after the peer's reset answered Ok({n})"),
        Err(e) => e,
    };
    assert_eq!(err.kind(), ErrorKind::ConnectionReset, "reset: the read after the reset ended {err} ({:?})", err.kind());
    let err = stream.write_all(&buf).expect_err("reset: a write after the reset succeeded");
    assert!(
        matches!(err.kind(), ErrorKind::ConnectionReset | ErrorKind::BrokenPipe),
        "reset: the write after the reset ended {err} ({:?})",
        err.kind()
    );
    println!("netd_tcp: reset ok bytes={got} sha={}", hex(&hash.finalize()));
}

/// Everything written before `shutdown(Write)` reaches the host, which
/// answers only once it has read to the end of the stream; the answer then
/// arrives on the half still open.
fn half_close(peer: SocketAddr, len: u64) {
    let mut stream = ask(peer, UPLOAD, len, 0);
    let sha = send(&mut stream, len, 11, "half_close");
    stream.shutdown(Shutdown::Write).expect("half_close: shut down the sending half");
    let err = stream.write(&[0]).expect_err("half_close: a write after shutdown(Write) succeeded");
    assert_eq!(err.kind(), ErrorKind::BrokenPipe, "half_close: the write after shutdown ended {err}");
    let (count, host_sha) = upload_answer(&mut stream, "half_close");
    assert_eq!(count, len, "half_close: the host read {count} of {len} bytes");
    assert_eq!(host_sha, sha, "half_close: the host's hash of the {len} bytes differs from this side's");
    println!("netd_tcp: half_close ok bytes={len} sha={}", hex(&sha));
}

fn download(peer: SocketAddr, len: u64, seed: u64) {
    let started = Instant::now();
    let mut stream = ask(peer, DOWNLOAD, len, seed);
    let (got, sha) = drain(&mut stream, "download");
    let took = started.elapsed();
    assert_eq!(got, len, "download: the stream ended after {got} of {len} bytes");
    println!("netd_tcp: download ok bytes={got} sha={} mbps={} ms={}", hex(&sha), mb_per_s(got, took), took.as_millis());
}

/// The same download through `read_to_end`, which reads into the spare
/// capacity of a growing buffer.
fn download_to_end(peer: SocketAddr, len: u64) {
    let started = Instant::now();
    let mut stream = ask(peer, DOWNLOAD, len, 3);
    stream.set_read_timeout(Some(STEP)).expect("set the liveness guard");
    let mut all = Vec::new();
    stream.read_to_end(&mut all).expect("download_to_end: read to the end");
    let took = started.elapsed();
    assert_eq!(all.len() as u64, len, "download_to_end: the stream ended after {} of {len} bytes", all.len());
    println!(
        "netd_tcp: download_to_end ok bytes={} sha={} mbps={} ms={}",
        all.len(),
        hex(&Sha256::digest(&all)),
        mb_per_s(len, took),
        took.as_millis()
    );
}

fn upload(peer: SocketAddr, len: u64, seed: u64) {
    let started = Instant::now();
    let mut stream = ask(peer, UPLOAD, len, 0);
    let sha = send(&mut stream, len, seed, "upload");
    stream.shutdown(Shutdown::Write).expect("upload: shut down the sending half");
    let (count, host_sha) = upload_answer(&mut stream, "upload");
    let took = started.elapsed();
    assert_eq!(count, len, "upload: the host read {count} of {len} bytes");
    assert_eq!(host_sha, sha, "upload: the host's hash of the {len} bytes differs from this side's");
    println!("netd_tcp: upload ok bytes={len} sha={} mbps={} ms={}", hex(&sha), mb_per_s(len, took), took.as_millis());
}

/// Write the whole stream and drop the connection with no shutdown and no
/// read: every byte is still owed to the host, which judges what it got.
fn upload_drop(peer: SocketAddr, len: u64) {
    let mut stream = ask(peer, UPLOAD, len, 0);
    let sha = send(&mut stream, len, 13, "upload_drop");
    drop(stream);
    println!("netd_tcp: upload_drop ok bytes={len} sha={}", hex(&sha));
}

/// A read with nothing to read and a write with no room each end at their
/// timeout, against a peer that holds the connection open and reads nothing.
fn timeouts(peer: SocketAddr) {
    const ASKED: Duration = Duration::from_millis(300);
    /// Far past every buffer between here and the host's socket: a write
    /// timeout that never fires is found here rather than by the runner.
    const BOUND: u64 = 512 * 1024 * 1024;
    let mut stream = ask(peer, HOLD, 0, 0);
    stream.set_read_timeout(Some(ASKED)).expect("set the read timeout");
    assert_eq!(stream.read_timeout().expect("read it back"), Some(ASKED));
    let started = Instant::now();
    let mut buf = vec![0u8; 65536];
    let err = stream.read(&mut buf).expect_err("timeouts: a read of a silent peer answered");
    let read_took = started.elapsed();
    assert!(
        matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut),
        "timeouts: the read ended {err} ({:?})",
        err.kind()
    );
    assert!(read_took >= ASKED, "timeouts: a read with a {ASKED:?} timeout gave up after {read_took:?}");
    assert!(read_took < ASKED + STEP, "timeouts: a read with a {ASKED:?} timeout took {read_took:?}");

    stream.set_write_timeout(Some(ASKED)).expect("set the write timeout");
    let mut written = 0u64;
    let (err, write_took) = loop {
        assert!(written < BOUND, "timeouts: {written} bytes written into a peer that reads nothing");
        let started = Instant::now();
        match stream.write(&buf) {
            Ok(n) => written += n as u64,
            Err(e) => break (e, started.elapsed()),
        }
    };
    assert!(
        matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut),
        "timeouts: the write ended {err} ({:?}) after {written} bytes",
        err.kind()
    );
    assert!(write_took >= ASKED, "timeouts: a write with a {ASKED:?} timeout gave up after {write_took:?}");
    assert!(write_took < ASKED + STEP, "timeouts: a write with a {ASKED:?} timeout took {write_took:?}");
    println!(
        "netd_tcp: timeouts ok read_ms={} write_ms={} written={written}",
        read_took.as_millis(),
        write_took.as_millis()
    );
}

/// `count` connections open at once, each downloading `len` bytes of its own
/// stream; each prints its hash for the host to judge.
fn many(peer: SocketAddr, count: usize, len: u64) {
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(count));
    let threads: Vec<_> = (0..count as u64)
        .map(|seed| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut stream = ask(peer, DOWNLOAD, len, 1000 + seed);
                // Every connection is open before any is read.
                barrier.wait();
                drain(&mut stream, &format!("many {seed}"))
            })
        })
        .collect();
    for (seed, thread) in threads.into_iter().enumerate() {
        let (got, sha) = thread.join().expect("a connection's thread panicked");
        assert_eq!(got, len, "many: connection {seed} ended after {got} of {len} bytes");
        println!("netd_tcp: many seed={} sha={}", 1000 + seed, hex(&sha));
    }
    println!("netd_tcp: many ok count={count} bytes={len}");
}

/// Local ports of `count` connections held open at once.
fn ports(peer: SocketAddr, count: usize) {
    let streams: Vec<TcpStream> = (0..count).map(|_| ask(peer, HOLD, 0, 0)).collect();
    let ports: Vec<u16> = streams.iter().map(|s| s.local_addr().expect("its local address").port()).collect();
    let listed: Vec<String> = ports.iter().map(u16::to_string).collect();
    println!("netd_tcp: ports ok ports={}", listed.join(","));
}

/// The counts netd's `inspect` answers for its sockets and what it is waiting
/// on.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
struct Held {
    tcp: u64,
    udp: u64,
    piped: u64,
    untabled: u64,
}

fn held() -> Held {
    let conn = toyos::endow::service("netd").expect("a connection to netd");
    conn.signal(MSG_INSPECT).expect("netd takes an inspect request");
    let poller = Poller::new(1);
    let mut rx: Box<FrameRx<MAX_SNAPSHOT_BYTES>> = Box::new(FrameRx::new());
    let deadline = Instant::now() + STEP;
    loop {
        match rx.pump(&conn) {
            RxStep::Frame { msg_type: MSG_SNAPSHOT, payload_len } => {
                let snap = toyos_inspect::decode(rx.payload(payload_len), toyos_inspect::NET)
                    .unwrap_or_else(|why| panic!("netd's snapshot: {why}"));
                let get = |key: &str| match snap.get(key) {
                    Some(Value::U64(n)) => *n,
                    other => panic!("netd's snapshot has {key} as {other:?}"),
                };
                return Held {
                    tcp: get("net.sockets.tcp"),
                    udp: get("net.sockets.udp"),
                    piped: get("net.piped.live"),
                    untabled: get("net.sockets.untabled"),
                };
            }
            RxStep::Idle => {}
            other => panic!("netd answered inspect with {other:?}, not a snapshot"),
        }
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "netd did not answer inspect within {STEP:?}");
        poller.watch(&conn, READABLE, 0);
        poller.wait(1, left.as_nanos() as u64, |_| {});
    }
}

/// Ask netd until `want` holds of its counts, and panic by name with the last
/// answer if `within` passes first.
///
/// **A poll, because nothing announces a count**: netd frees a client's
/// sockets on its own pass, which no event reaches this process from.
fn await_held(what: &str, within: Duration, want: impl Fn(Held) -> bool) -> Held {
    const POLL: Duration = Duration::from_millis(20);
    let started = Instant::now();
    loop {
        let now = held();
        if want(now) {
            return now;
        }
        assert!(started.elapsed() < within, "{what}: netd still holds {now:?} after {within:?}");
        std::thread::sleep(POLL);
    }
}

/// Clients killed while netd holds a request of theirs open — a connect
/// nobody answers, a UDP receive nothing is sent to, a stream mid-receive —
/// leave netd holding nothing of theirs.
fn leaves(peer: SocketAddr) {
    // A connection of this process's own first, so the host's link address is
    // known before the connect to nobody starts asking for its own: smoltcp
    // rate-limits every address's ARP request by one clock, which the connect
    // that is never answered then holds.
    let before = held();
    let mut warm = ask(peer, DOWNLOAD, 0, 0);
    drain(&mut warm, "leaves: the connection before");
    drop(warm);
    await_held("leaves: the connection before, closed", STEP, |h| h == before);
    println!("netd_tcp: leaves before {before:?}");
    let children = [spawn(peer, "child_connect"), spawn(peer, "child_udp"), spawn(peer, "child_receive")];
    // Two streams (the connect and the receive), one UDP socket, and two
    // piped slots (a pending connect holds one).
    let during = await_held("leaves: the children's requests", STEP, |h| {
        h.tcp == before.tcp + 2 && h.udp == before.udp + 1 && h.piped == before.piped + 2
    });
    println!("netd_tcp: leaves during {during:?}");
    for mut child in children {
        child.kill().expect("kill a child");
        child.wait().expect("reap a child");
    }
    // The stream mid-receive had the peer's bytes unread, so netd resets it
    // at once rather than waiting out a close.
    let after = await_held("leaves: the killed children's sockets", STEP, |h| h == before);
    println!("netd_tcp: leaves ok after {after:?}");
}

/// The peer's last bytes outlive smoltcp's TIME-WAIT: a client that shut its
/// sending half down and then fell a whole receive pipe behind reads every
/// byte of a stream whose FIN arrived with part of it still in the socket.
///
/// **The one wait here is the premise, not a pace**: nothing tells a client
/// that its socket's TIME-WAIT has ended, and smoltcp's is a fixed ten seconds
/// (`CLOSE_DELAY`), after which it clears the socket's buffer.
fn time_wait(peer: SocketAddr) {
    /// Bytes past the ring's capacity: inside the socket's own buffer, so the
    /// FIN behind them is taken, and outside the full pipe.
    const TAIL: u64 = 32 * 1024;
    const TIME_WAIT: Duration = Duration::from_secs(10);
    let capacity = netd_stream::ring_capacity();
    let len = capacity + TAIL;
    let conn = toyos::net::tcp_connect(HOST.octets(), peer.port(), 60_000).expect("connect to the host peer");
    let mut request = [0u8; 17];
    request[0] = PATTERN;
    request[1..9].copy_from_slice(&len.to_le_bytes());
    assert_eq!(conn.tx.write(&request), Ok(request.len()), "time_wait: the request");
    toyos::net::tcp_shutdown(conn.socket_id, 1).expect("time_wait: shut the sending half down");
    netd_stream::await_ring_full(&conn.rx, capacity, STEP);
    std::thread::sleep(TIME_WAIT + Duration::from_secs(2));
    let at = netd_stream::read_pattern(&conn.rx, STEP, "time_wait");
    assert_eq!(at, len, "time_wait: the stream ended after {at} of {len} bytes");
    println!("netd_tcp: time_wait ok bytes={len}");
}

/// The seed of [`reader_leaves`]' stream, by which the host's record of it is
/// found.
const READER_LEAVES_SEED: u64 = 77;

/// A client that closes its receive end with the peer still sending and keeps
/// its send end: netd resets the connection at once rather than holding the
/// peer's bytes for a reader that is gone, closes the send end under it, and
/// lets the connection go. The host's sender seeing the reset is the harness's
/// half of the verdict.
fn reader_leaves(peer: SocketAddr) {
    let before = held();
    let conn = toyos::net::tcp_connect(HOST.octets(), peer.port(), 60_000).expect("connect to the host peer");
    let mut request = [0u8; 17];
    request[0] = DOWNLOAD;
    request[1..9].copy_from_slice(&u64::MAX.to_le_bytes());
    request[9..].copy_from_slice(&READER_LEAVES_SEED.to_le_bytes());
    assert_eq!(conn.tx.write(&request), Ok(request.len()), "reader_leaves: the request");
    let mut first = [0u8; 4096];
    assert!(matches!(conn.rx.read(&mut first), Ok(n) if n > 0), "reader_leaves: the stream's first bytes");
    drop(conn.rx);
    await_held("reader_leaves: the connection its reader left", STEP, |h| h == before);
    assert_eq!(
        conn.tx.write(&[0]),
        Err(toyos_abi::syscall::SyscallError::Gone),
        "reader_leaves: the send end of a reset connection still takes bytes"
    );
    println!("netd_tcp: reader_leaves ok");
}

/// This program again, running `case`, once it has said it is at the call
/// netd holds open.
fn spawn(peer: SocketAddr, case: &str) -> Child {
    let exe = std::env::current_exe().expect("this program's path");
    let mut child = Command::new(&exe)
        .arg(peer.port().to_string())
        .arg(case)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn {case}: {e}"));
    let mut word = [0u8; 1];
    child.stdout.as_mut().expect("its stdout").read_exact(&mut word).expect("the child's word");
    child
}

/// A client killed with its connection open and quiet both ways: netd sends
/// the FIN it owes, and ends the connection itself once `limit_s` seconds —
/// netd's `ORPHAN_LIMIT` — have passed with the peer still holding it open.
fn orphan(peer: SocketAddr, limit_s: u64) {
    let before = held();
    let mut child = spawn(peer, "child_idle");
    await_held("orphan: the child's stream", STEP, |h| h.tcp == before.tcp + 1 && h.piped == before.piped + 1);
    // Before the kill, so netd's clock on the orphan starts after this one.
    let killed = Instant::now();
    child.kill().expect("kill the child");
    child.wait().expect("reap the child");
    let limit = Duration::from_secs(limit_s);
    let after = await_held("orphan: the killed child's stream", limit + STEP, |h| h == before);
    let took = killed.elapsed();
    assert!(took >= limit, "orphan: netd let go of a stream its peer still held open after {took:?}");
    println!("netd_tcp: orphan ok after {after:?} in {}ms", took.as_millis());
}

/// A connect with no timeout to an address that never answers.
fn child_connect() {
    println!("c");
    let _ = toyos::net::tcp_connect(NOBODY.octets(), 9, 0);
    panic!("a connect to nobody with no timeout answered");
}

/// A receive on a socket nothing sends to.
fn child_udp() {
    let socket = UdpSocket::bind("0.0.0.0:0").expect("bind a UDP socket");
    println!("u");
    let mut buf = [0u8; 64];
    let _ = socket.recv_from(&mut buf);
    panic!("a receive nothing sends to answered");
}

/// A stream longer than this child lives, read until it is killed.
fn child_receive(peer: SocketAddr) {
    let mut stream = ask(peer, DOWNLOAD, u64::MAX, 0);
    let mut buf = vec![0u8; 65536];
    stream.read_exact(&mut buf).expect("the first of the stream");
    println!("r");
    loop {
        stream.read_exact(&mut buf).expect("the rest of the stream");
    }
}

/// A stream the host keeps open and silent, read until it is killed.
fn child_idle(peer: SocketAddr) {
    let mut stream = ask(peer, HOLD, 0, 0);
    println!("i");
    let mut buf = [0u8; 64];
    let _ = stream.read(&mut buf);
    panic!("a read of a silent peer answered");
}
