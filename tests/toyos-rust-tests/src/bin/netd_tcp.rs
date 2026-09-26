//! A TCP client through std, against the harness's host peer
//! (`tests/common/tcppeer.rs`): every case a real client meets on the
//! internet, each judged by what std answered and by a SHA-256 over the bytes
//! that crossed, which the host computes over its own side of the same stream.
//!
//! `test_rs_netd_tcp <peer port> <case> [args]`. A connection opens with one
//! request (`netd_stream::request`). Every case prints `netd_tcp: <case> ok ...`
//! as its only success line, and the fields the host judges on it.

use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, SocketAddrV4, TcpStream, UdpSocket};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc, Barrier};
use std::time::{Duration, Instant};

#[path = "../netd_stream.rs"]
mod netd_stream;
#[path = "../netd_inspect.rs"]
mod netd_inspect;

use netd_inspect::Net;
use netd_stream::request;
use sha2::{Digest, Sha256};

/// The host, as QEMU's slirp shows it to the guest.
const HOST: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 2);

/// An address on slirp's own network that nothing answers: slirp replies to
/// ARP for its own addresses alone, so a SYN to this one never leaves.
const NOBODY: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 99);

/// The on-link neighbour the harness plays on the segment
/// (`tests/common/segment.rs`'s `NEIGHBOUR`), which echoes a datagram.
const NEIGHBOUR: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 7);

/// The host peer's modes, the other half of `tcppeer::Mode`.
const DOWNLOAD: u8 = 0;
const UPLOAD: u8 = 1;
const RESET: u8 = 2;
const HOLD: u8 = 3;
const PATTERN: u8 = 4;
const LATE_UPLOAD: u8 = 5;
const RELEASE: u8 = 6;
const DARK: u8 = 9;

/// A liveness guard on every blocking step, said by name: the host has
/// everything it needs the moment a request arrives, so a step that outlasts
/// this is a client or a netd that stopped moving.
const STEP: Duration = Duration::from_secs(60);

/// How long netd may take to let go of what a client left it that owes
/// nothing: a pass, or an RST and a pass. Well under every bound netd holds a
/// stream for, so a stream held to one of those instead is caught here.
const LET_GO: Duration = Duration::from_secs(10);

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
        Some("late_shutdown") => late_shutdown(peer),
        Some("read_shut") => read_shut(peer),
        Some("download") => download(peer, number(3), number(4)),
        Some("download_to_end") => download_to_end(peer, number(3)),
        Some("upload") => upload(peer, number(3), number(4)),
        Some("upload_drop") => upload_drop(peer, number(3)),
        Some("timeouts") => timeouts(peer),
        Some("many") => many(peer, number(3) as usize, number(4)),
        Some("many_up") => many_up(peer, number(3) as usize),
        Some("leaves") => leaves(peer),
        Some("reader_leaves") => reader_leaves(peer),
        Some("receives") => receives(peer, number(3) as usize),
        Some("closing") => closing(peer, number(3) as usize),
        Some("time_wait") => time_wait(peer),
        Some("ports") => ports(peer, number(3) as usize),
        Some("neighbour") => neighbour(),
        Some("waited") => waited(),
        Some("orphan") => orphan(peer, number(3)),
        Some("orphan_owes") => orphan_owes(peer, number(3), number(4)),
        Some("vanish") => vanish(peer, number(3), number(4)),
        Some("child_connect") => child_connect(),
        Some("child_udp") => child_udp(),
        Some("child_receive") => child_receive(peer),
        Some("child_idle") => child_idle(peer),
        Some("child_upload") => child_upload(peer, number(3)),
        Some("child_receives") => child_receives(number(3) as usize),
        other => panic!("netd_tcp: no case {other:?}"),
    }
}

/// A connection to the host peer, asking for `mode` over `len` bytes of the
/// stream `seed` names.
fn ask(peer: SocketAddr, mode: u8, len: u64, seed: u64) -> TcpStream {
    let mut stream = TcpStream::connect_timeout(&peer, STEP).expect("connect to the host peer");
    stream.write_all(&request(mode, len, seed)).expect("send the request");
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

/// Write the pattern `seed` names without blocking until the stream takes no
/// more — its send pipe full, and everything behind it — answering how many
/// bytes it took and their hash.
fn fill_until_refused(stream: &mut TcpStream, seed: u64, what: &str) -> (u64, [u8; 32]) {
    /// Past every buffer between here and the host's socket: a pipe that never
    /// fills is found here rather than by the runner.
    const BOUND: u64 = 512 * 1024 * 1024;
    stream.set_nonblocking(true).expect("non-blocking");
    let mut pattern = Pattern(seed);
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; 65536];
    let mut written = 0u64;
    let mut pending = 0..0;
    loop {
        assert!(written < BOUND, "{what}: {written} bytes written into a window held shut");
        if pending.is_empty() {
            pattern.fill(&mut buf);
            pending = 0..buf.len();
        }
        match stream.write(&buf[pending.clone()]) {
            Ok(n) => {
                hash.update(&buf[pending.start..pending.start + n]);
                pending.start += n;
                written += n as u64;
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => break,
            Err(e) => panic!("{what}: writing at {written}: {e}"),
        }
    }
    stream.set_nonblocking(false).expect("blocking");
    (written, hash.finalize().into())
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
    stream.write_all(&request(HOLD, 0, 0)).expect("the connection that answered takes a request");
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

/// `shutdown(Write)` asked with the whole send pipe still unread: the peer
/// holds its window shut until then, and every byte in the pipe still reaches
/// it, ahead of the FIN.
fn late_shutdown(peer: SocketAddr) {
    const SEED: u64 = 19;
    let mut stream = ask(peer, LATE_UPLOAD, 0, SEED);
    // From here on, every byte this side wrote that the peer has not read is
    // in the send pipe or behind it.
    let (written, sha) = fill_until_refused(&mut stream, SEED, "late_shutdown");
    stream.shutdown(Shutdown::Write).expect("late_shutdown: shut the sending half down");
    drop(ask(peer, RELEASE, 0, SEED));
    let (count, host_sha) = upload_answer(&mut stream, "late_shutdown");
    assert_eq!(count, written, "late_shutdown: the host read {count} of {written} bytes");
    assert_eq!(host_sha, sha, "late_shutdown: the host's hash of the {written} bytes differs from this side's");
    println!("netd_tcp: late_shutdown ok bytes={written} sha={}", hex(&sha));
}

/// After `shutdown(Read)`, a read answers the end of the stream at once,
/// whatever the peer is still sending; and netd refuses a shutdown that names
/// no half.
fn read_shut(peer: SocketAddr) {
    let conn = toyos::net::tcp_connect(HOST.octets(), peer.port(), 60_000).expect("connect to the host peer");
    assert_eq!(conn.tx.write(&request(HOLD, 0, 0)), Ok(17), "read_shut: the request");
    assert!(
        matches!(toyos::net::tcp_shutdown(conn.socket_id, 3), Err(toyos::net::NetError::InvalidInput)),
        "read_shut: a shutdown of half 3 was not refused as invalid"
    );
    drop(conn);
    let mut stream = ask(peer, DOWNLOAD, 1 << 20, 31);
    stream.shutdown(Shutdown::Read).expect("read_shut: shut the receiving half down");
    stream.set_read_timeout(Some(STEP)).expect("set the liveness guard");
    let mut buf = [0u8; 64];
    let n = stream.read(&mut buf).expect("read_shut: a read after shutdown(Read)");
    assert_eq!(n, 0, "read_shut: a read after shutdown(Read) answered {n} bytes");
    println!("netd_tcp: read_shut ok");
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
    let barrier = Arc::new(Barrier::new(count));
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

/// `count` connections uploading at once, and a lookup made while they all
/// have bytes to send answered within its bound: a socket made after all of
/// theirs still gets its turn. Each upload's bytes are judged by the host's
/// answer.
fn many_up(peer: SocketAddr, count: usize) {
    /// A pass sends at most a frame a busy socket, and the ring takes one a
    /// millisecond, so a pass over every upload is `count` milliseconds; the
    /// lookup needs two — its server's link address, then its query — and the
    /// wire answers at once. Ten passes, for a guest under TCG.
    const ANSWERED: Duration = Duration::from_secs(1);
    /// Each upload's bytes: few enough that the slow wire carries every
    /// stream's within seconds, and so many between them that it is still
    /// carrying them when the lookup is made.
    const BYTES: u64 = 128 * 1024;
    // Every connection open before any writes, and every one written before
    // any shuts down.
    let (open, looked_up) = (Arc::new(Barrier::new(count + 1)), Arc::new(Barrier::new(count + 1)));
    // One shutdown at a time: each is a request of netd's, and netd takes at
    // most `MAX_PENDING_CONNS` of those at once.
    let asking = Arc::new(std::sync::Mutex::new(()));
    let threads: Vec<_> = (0..count as u64)
        .map(|seed| {
            let (open, looked_up, asking) = (open.clone(), looked_up.clone(), asking.clone());
            std::thread::spawn(move || {
                let mut stream = ask(peer, UPLOAD, 0, 0);
                open.wait();
                let sha = send(&mut stream, BYTES, 2000 + seed, &format!("many_up {seed}"));
                looked_up.wait();
                {
                    let _one = asking.lock().expect("the shutdown lock");
                    stream.shutdown(Shutdown::Write).expect("many_up: shut down the sending half");
                }
                let (got, host_sha) = upload_answer(&mut stream, &format!("many_up {seed}"));
                assert_eq!(got, BYTES, "many_up {seed}: the host read {got} of {BYTES} bytes");
                assert_eq!(host_sha, sha, "many_up {seed}: the host's hash differs");
            })
        })
        .collect();
    open.wait();
    let started = Instant::now();
    let mut answers = [[0u8; 4]; 8];
    let found = toyos::net::dns_lookup("fair.test", &mut answers);
    let took = started.elapsed();
    looked_up.wait();
    let found = found.unwrap_or_else(|e| panic!("many_up: a lookup during {count} uploads ended {e:?} after {took:?}"));
    assert!(took < ANSWERED, "many_up: a lookup during {count} uploads took {took:?}");
    for thread in threads {
        thread.join().expect("an upload's thread panicked");
    }
    println!("netd_tcp: many_up ok count={count} lookup_ms={} found={found} bytes={}", took.as_millis(), BYTES * count as u64);
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
    /// Connections whose client holds a pipe, and connects waiting.
    piped: u64,
    /// Connections whose client let go of both pipes, still finishing.
    closing: u64,
    /// Connections whose client has gone, pipes held or not.
    orphans: u64,
    untabled: u64,
    /// UDP receives waiting for a datagram.
    waiting: u64,
}

fn held() -> Held {
    let net = Net::ask(STEP);
    Held {
        tcp: net.count("net.sockets.tcp"),
        udp: net.count("net.sockets.udp"),
        piped: net.count("net.piped.live"),
        closing: net.count("net.piped.closing"),
        orphans: net.count("net.piped.orphans"),
        untabled: net.count("net.sockets.untabled"),
        waiting: net.count("net.udp.waiting"),
    }
}

/// Ask netd until `want` holds of its counts, and panic by name with the last
/// answer if `within` passes first.
///
/// **A poll, because nothing announces a count**: netd frees a client's
/// sockets on its own pass, which no event reaches this process from.
fn await_held(what: &str, within: Duration, want: impl Fn(Held) -> bool) -> Held {
    await_net(what, within, held, want)
}

/// [`await_held`] over any reading of netd's.
fn await_net<T: Copy + std::fmt::Debug>(what: &str, within: Duration, read: impl Fn() -> T, want: impl Fn(T) -> bool) -> T {
    const POLL: Duration = Duration::from_millis(20);
    let started = Instant::now();
    loop {
        let now = read();
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
    // known before the connect to nobody starts asking for its own.
    let before = held();
    let mut warm = ask(peer, DOWNLOAD, 0, 0);
    drain(&mut warm, "leaves: the connection before");
    drop(warm);
    await_held("leaves: the connection before, closed", STEP, |h| h == before);
    println!("netd_tcp: leaves before {before:?}");
    let children = [spawn(peer, "child_connect"), spawn(peer, "child_udp"), spawn(peer, "child_receive")];
    // Two streams (the connect and the receive), one UDP socket waiting, and
    // two piped slots (a pending connect holds one).
    let during = await_held("leaves: the children's requests", STEP, |h| {
        h.tcp == before.tcp + 2 && h.udp == before.udp + 1 && h.piped == before.piped + 2 && h.waiting == before.waiting + 1
    });
    println!("netd_tcp: leaves during {during:?}");
    for mut child in children {
        child.kill().expect("kill a child");
        child.wait().expect("reap a child");
    }
    // The stream mid-receive had the peer's bytes unread, so netd resets it
    // at once rather than holding it as an orphan.
    let after = await_held("leaves: the killed children's sockets", LET_GO, |h| h == before);
    println!("netd_tcp: leaves ok after {after:?}");
}

/// A stream's last bytes, behind a full receive pipe when the peer's FIN
/// arrives, outlive smoltcp's TIME-WAIT, whose end clears the socket's buffer:
/// a client that shut its sending half down and then fell a whole receive pipe
/// behind reads every byte.
fn time_wait(peer: SocketAddr) {
    /// Bytes past the ring's capacity: inside the socket's own buffer, so the
    /// FIN behind them is taken, and outside the full pipe.
    const TAIL: u64 = 32 * 1024;
    let time_wait = || Net::ask(STEP).count("net.sockets.time_wait");
    let before = time_wait();
    let capacity = netd_stream::ring_capacity();
    let len = capacity + TAIL;
    let conn = toyos::net::tcp_connect(HOST.octets(), peer.port(), 60_000).expect("connect to the host peer");
    assert_eq!(conn.tx.write(&request(PATTERN, len, 0)), Ok(17), "time_wait: the request");
    toyos::net::tcp_shutdown(conn.socket_id, 1).expect("time_wait: shut the sending half down");
    netd_stream::await_ring_full(&conn.rx, capacity, STEP);
    await_net("time_wait: the stream's TIME-WAIT begun", STEP, time_wait, |n| n > before);
    await_net("time_wait: the stream's TIME-WAIT over", STEP, time_wait, |n| n == before);
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
    let asked = request(DOWNLOAD, u64::MAX, READER_LEAVES_SEED);
    assert_eq!(conn.tx.write(&asked), Ok(asked.len()), "reader_leaves: the request");
    let mut first = [0u8; 4096];
    assert!(matches!(conn.rx.read(&mut first), Ok(n) if n > 0), "reader_leaves: the stream's first bytes");
    drop(conn.rx);
    await_held("reader_leaves: the connection its reader left", LET_GO, |h| h == before);
    assert_eq!(
        conn.tx.write(&[0]),
        Err(toyos_abi::syscall::SyscallError::Gone),
        "reader_leaves: the send end of a reset connection still takes bytes"
    );
    println!("netd_tcp: reader_leaves ok");
}

/// netd holds at most `waiting` UDP receives open at once and refuses the
/// next by name, and goes on serving: a child blocks `waiting` receives, one
/// more of this program's own is answered `ResourceExhausted`, netd still
/// answers and carries a stream, and once the child is killed its receives
/// are let go of.
fn receives(peer: SocketAddr, waiting: usize) {
    let before = held();
    let mut child = spawn_with(peer, "child_receives", &[&waiting.to_string()]);
    await_held("receives: the child's receives", STEP, |h| h.waiting == before.waiting + waiting as u64);
    let one_more = toyos::net::udp_bind([0; 4], 0).expect("receives: bind one more socket");
    let socket = one_more.socket_id;
    let (answered, answer) = mpsc::channel();
    std::thread::spawn(move || answered.send(toyos::net::udp_recv_from(socket, 64)));
    let refused = answer
        .recv_timeout(LET_GO)
        .unwrap_or_else(|_| panic!("receives: receive {} of {waiting} was not refused within {LET_GO:?}", waiting + 1));
    match refused {
        Err(toyos::net::NetError::ResourceExhausted) => {}
        Ok(r) => panic!("receives: receive {} of {waiting} was answered {} bytes", waiting + 1, r.len),
        Err(e) => panic!("receives: receive {} of {waiting} was refused {e:?}", waiting + 1),
    }
    let mut stream = ask(peer, DOWNLOAD, 65536, 5);
    let (got, _) = drain(&mut stream, "receives: a stream after the refusal");
    assert_eq!(got, 65536, "receives: the stream after the refusal ended short");
    drop(stream);
    child.kill().expect("kill the child");
    child.wait().expect("reap the child");
    toyos::net::udp_close(socket).expect("receives: close the refused socket");
    let after = await_held("receives: the killed child's receives", LET_GO, |h| h.waiting == before.waiting && h.udp == before.udp);
    println!("netd_tcp: receives ok refused={} after {after:?}", waiting + 1);
}

/// Connections this client closes first, against a peer that never closes,
/// cost it nothing: `count` of them — netd's cap on live connections, and on
/// closing ones — and one more are each opened and dropped with every connect
/// answered, and netd keeps no more of them closing than its bound.
fn closing(peer: SocketAddr, count: usize) {
    let before = held();
    let bound = Net::ask(STEP).count("net.piped.max_closing");
    assert_eq!(bound, count as u64, "closing: netd's closing bound is not its live cap");
    for i in 0..=count {
        let mut stream = TcpStream::connect_timeout(&peer, STEP).unwrap_or_else(|e| {
            panic!("closing: connect {} of {} with {i} closed first: {e} ({:?})", i + 1, count + 1, e.kind())
        });
        // Held: the host never closes, so each waits in FIN-WAIT-2.
        stream.write_all(&request(HOLD, 0, 0)).expect("closing: the request");
        drop(stream);
    }
    // Every client has let go once none holds a pipe: from then on the count
    // is what netd keeps.
    let after = await_held("closing: every client let go", STEP, |h| h.piped == before.piped);
    assert_eq!(after.closing, before.closing + bound, "closing: netd keeps {after:?} closing, and {bound} is its bound");
    println!("netd_tcp: closing ok connects={} closing={}", count + 1, after.closing);
}

/// A connect to a silent on-link address, asking for its neighbour every
/// second, delays no other address's resolution: a datagram to the harness's
/// neighbour, whose address nothing has asked for yet, is echoed while the
/// silent connect still asks.
fn neighbour() {
    /// ARP's own rate limit is a second an address: a bound, said by name.
    const ANSWERED: Duration = Duration::from_secs(5);
    /// Longer than [`ANSWERED`], so the silent connect is still asking when
    /// the neighbour answers.
    const SILENT_FOR: Duration = Duration::from_secs(20);
    let before = held();
    let silent = std::thread::spawn(|| {
        TcpStream::connect_timeout(&SocketAddr::V4(SocketAddrV4::new(NOBODY, 9)), SILENT_FOR)
    });
    // Its socket made first, so smoltcp visits it first on every pass.
    await_held("neighbour: the silent connect's socket", STEP, |h| h.tcp == before.tcp + 1);
    let socket = UdpSocket::bind("0.0.0.0:0").expect("bind a UDP socket");
    let started = Instant::now();
    socket.send_to(b"neighbour", SocketAddrV4::new(NEIGHBOUR, 7)).expect("send to the neighbour");
    // Bounded on a thread of its own: std's UDP socket keeps no timeout
    // (`issues/design-debt/std-udp-socket-ignores-its-timeouts.md`).
    let (echoed, echo) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0u8; 64];
        let _ = echoed.send(socket.recv_from(&mut buf).map(|(n, from)| (buf[..n].to_vec(), from)));
    });
    let (got, from) = echo
        .recv_timeout(ANSWERED)
        .unwrap_or_else(|_| panic!("neighbour: no echo in {ANSWERED:?} beside a connect to nobody"))
        .unwrap_or_else(|e| panic!("neighbour: the echo's receive failed: {e} ({:?})", e.kind()));
    let took = started.elapsed();
    assert_eq!(got, b"neighbour", "neighbour: the echo came back changed");
    assert_eq!(from, SocketAddr::V4(SocketAddrV4::new(NEIGHBOUR, 7)), "neighbour: the echo came from elsewhere");
    assert!(!silent.is_finished(), "neighbour: the premise — the silent connect was over before the echo");
    let err = silent.join().expect("the silent connect's thread").expect_err("a connect to nobody succeeded");
    assert_eq!(err.kind(), ErrorKind::TimedOut, "neighbour: the silent connect ended {err}");
    println!("netd_tcp: neighbour ok echoed_ms={}", took.as_millis());
}

/// This program again, running `case`, once it has said it is at the call
/// netd holds open.
fn spawn(peer: SocketAddr, case: &str) -> Child {
    spawn_with(peer, case, &[])
}

fn spawn_with(peer: SocketAddr, case: &str, args: &[&str]) -> Child {
    let exe = std::env::current_exe().expect("this program's path");
    let mut child = Command::new(&exe)
        .arg(peer.port().to_string())
        .arg(case)
        .args(args)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn {case}: {e}"));
    let mut word = [0u8; 1];
    child.stdout.as_mut().expect("its stdout").read_exact(&mut word).expect("the child's word");
    child
}

/// A client killed with its connection open and quiet both ways: netd sends
/// the FIN it owes, and ends the connection itself once `limit_s` seconds —
/// netd's `FIN_WAIT_2_LIMIT` — have passed with the peer still holding it
/// open.
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

/// Two clients leave bytes behind a peer that keeps its window shut: netd
/// delivers every byte of the one whose peer opens its window after
/// `fin_wait_2_s` and a second more — the longest an orphan's peer is given
/// to close — and resets the other once `stall_s` has passed with nothing
/// acknowledged.
fn orphan_owes(peer: SocketAddr, fin_wait_2_s: u64, stall_s: u64) {
    const RELEASED: u64 = 91;
    const HELD_SHUT: u64 = 92;
    let before = held();
    let upload = |seed: u64| {
        let out = Command::new(std::env::current_exe().expect("this program's path"))
            .args([peer.port().to_string(), "child_upload".into(), seed.to_string()])
            .output()
            .expect("run a child upload");
        assert!(out.status.success(), "orphan_owes: child upload {seed} failed: {out:?}");
        String::from_utf8(out.stdout).expect("its line")
    };
    let (released, held_shut) = (upload(RELEASED), upload(HELD_SHUT));
    print!("{released}{held_shut}");
    // Both children are gone and netd has seen them go: what they wrote is
    // netd's to deliver, and its clocks on them run from here or earlier.
    await_held("orphan_owes: the children's orphans", STEP, |h| h.orphans == before.orphans + 2);
    let left = Instant::now();
    // **The one wait here is the premise, not a pace**: the orphan's peer
    // holds its window shut past the longest an orphan's peer is given to
    // close, and nothing but time passing is that.
    let premise = Duration::from_secs(fin_wait_2_s + 1);
    std::thread::sleep(premise);
    let now = held();
    assert_eq!(now.piped, before.piped + 2, "orphan_owes: an orphan held shut for {premise:?} was given up: {now:?}");
    drop(ask(peer, RELEASE, 0, RELEASED));
    await_held("orphan_owes: the released orphan delivered", STEP, |h| h.piped + h.closing == before.piped + before.closing + 1);
    let stall = Duration::from_secs(stall_s);
    await_held("orphan_owes: the orphan held shut", stall + STEP, |h| h == before);
    let took = left.elapsed();
    assert!(took + Duration::from_secs(1) >= stall, "orphan_owes: the orphan held shut was reset after {took:?}");
    // Its host connection waits on a release, which it is now given.
    drop(ask(peer, RELEASE, 0, HELD_SHUT));
    println!("netd_tcp: orphan_owes ok released_seed={RELEASED} held_shut_ms={}", took.as_millis());
}

/// The wire goes dark under three streams — one whose client is killed, one
/// whose client keeps writing, and the one that asked for the dark — and netd
/// gives up on each once `stall_s` has passed with nothing from its peer, the
/// writer told by its write failing, and lets every one go within `linger_s`
/// of its reset though no RST can leave: the peers' link address is forgotten
/// by then, and asked for in the dark.
fn vanish(peer: SocketAddr, stall_s: u64, linger_s: u64) {
    let before = held();
    let mut child = spawn(peer, "child_idle");
    let mut writer = ask(peer, HOLD, 0, 0);
    await_held("vanish: both streams", STEP, |h| h.tcp == before.tcp + 2);
    drop(ask(peer, DARK, 0, 0));
    let dark = Instant::now();
    child.kill().expect("kill the child");
    child.wait().expect("reap the child");
    let stall = Duration::from_secs(stall_s);
    let buf = vec![0u8; 65536];
    // A write that blocks past the stall limit and its step is a writer
    // never told, and times out by name.
    writer.set_write_timeout(Some(stall + STEP)).expect("vanish: bound the writer");
    let err = loop {
        if let Err(e) = writer.write(&buf) {
            break e;
        }
    };
    let told = dark.elapsed();
    assert!(
        matches!(err.kind(), ErrorKind::ConnectionReset | ErrorKind::BrokenPipe),
        "vanish: the writer was told {err} ({:?})",
        err.kind()
    );
    assert!(told >= stall, "vanish: the writer was told after {told:?}");
    drop(writer);
    let linger = Duration::from_secs(linger_s);
    let after = await_held("vanish: every stream in the dark", stall + linger + STEP, |h| h == before);
    println!("netd_tcp: vanish ok told_ms={} after {after:?} in {}ms", told.as_millis(), dark.elapsed().as_millis());
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

/// Bytes into a peer that reads nothing until it is released, written until
/// nothing more is taken, and left behind: this child's line says what.
fn child_upload(peer: SocketAddr, seed: u64) {
    let mut stream = ask(peer, LATE_UPLOAD, 0, seed);
    let (written, sha) = fill_until_refused(&mut stream, seed, "child_upload");
    println!("netd_tcp: child_upload seed={seed} bytes={written} sha={}", hex(&sha));
}

/// `count` receives, one a socket, on sockets nothing sends to.
fn child_receives(count: usize) {
    let sockets: Vec<UdpSocket> = (0..count).map(|_| UdpSocket::bind("0.0.0.0:0").expect("bind a UDP socket")).collect();
    println!("q");
    let threads: Vec<_> = sockets
        .into_iter()
        .map(|socket| {
            std::thread::spawn(move || {
                let mut buf = [0u8; 64];
                let _ = socket.recv_from(&mut buf);
                panic!("a receive nothing sends to answered");
            })
        })
        .collect();
    for thread in threads {
        let _ = thread.join();
    }
}

/// How many frames have waited for netd's transmit ring since it started: the
/// proof, for a case after it, that the ring filled.
fn waited() {
    println!("netd_tcp: waited ok frames={}", Net::ask(STEP).count("net.tx.waited"));
}
