//! A client that out-writes a peer which has stopped reading costs netd no
//! CPU.
//!
//! The host reads this connection's ask and nothing after it. This program
//! writes until the whole path — its send pipe, netd's socket buffer, slirp and
//! the host's socket — has stopped taking bytes, and then measures how busy the
//! machine is while nothing can move. A netd that watches the send pipe
//! whether or not its socket has room is woken by the pipe's bytes on every
//! pass, which is one whole CPU for as long as the peer stays stalled.
//!
//! argv[1] is the port of the harness's host server on `HOST`.
//! `netd_stalled_peer: ok` is the only success line.

#[path = "../netd_stream.rs"]
mod netd_stream;

use std::time::{Duration, Instant};

use netd_stream::{ask, Ask, HOST};
use toyos::poller::{Poller, WRITABLE};
use toyos_abi::syscall::{self, SyscallError, SysinfoHeader};

/// How long the send pipe must stay full, with the peer reading nothing, for
/// the path to count as stalled. Policy: every hop drains in far less.
const SETTLE: Duration = Duration::from_secs(2);

/// The window the machine's busy time is measured over.
const WINDOW: Duration = Duration::from_secs(2);

/// Liveness guard on reaching the stall at all.
const STALL_BOUND: Duration = Duration::from_secs(60);

fn sysinfo() -> SysinfoHeader {
    let mut buf = [0u8; toyos::system::SYSINFO_HEADER_SIZE];
    let n = toyos::system::sysinfo(&mut buf);
    assert!(n >= toyos::system::SYSINFO_HEADER_SIZE, "sysinfo returned {n} bytes");
    SysinfoHeader::decode(&buf)
}

fn main() {
    let port: u16 = std::env::args()
        .nth(1)
        .and_then(|p| p.parse().ok())
        .expect("usage: netd_stalled_peer <host port>");
    let conn = toyos::net::tcp_connect(HOST, port, 30_000).expect("connect to the host server");
    ask(&conn.tx, Ask::Held(0));

    let chunk = [0u8; 65536];
    let poller = Poller::new(1);
    let started = Instant::now();
    let mut written = 0u64;
    loop {
        match conn.tx.write_nonblock(&chunk) {
            Ok(n) => {
                written += n as u64;
                continue;
            }
            Err(SyscallError::WouldBlock) => {}
            Err(e) => panic!("writing to the stalled peer after {written} bytes: {e:?}"),
        }
        assert!(started.elapsed() < STALL_BOUND, "the send path still took bytes after {STALL_BOUND:?}");
        // The pipe is full: room within `SETTLE` is the path still moving.
        poller.watch(&conn.tx, WRITABLE, 0);
        let mut roomed = false;
        poller.wait(1, SETTLE.as_nanos() as u64, |_| roomed = true);
        if !roomed {
            break;
        }
    }
    println!("netd_stalled_peer: the path stopped taking bytes after {written}");

    let before = sysinfo();
    // An interval, not a wait: a rate is what is measured, and nothing is
    // expected to happen in it.
    syscall::nanosleep(WINDOW.as_nanos() as u64);
    let after = sysinfo();
    let busy_ns = after.total_cpu_ns - before.total_cpu_ns;
    let wall_ns = after.uptime_ns - before.uptime_ns;
    let busy_cpus = busy_ns as f64 / wall_ns as f64;
    println!("netd_stalled_peer: {busy_cpus:.3} CPUs busy over {wall_ns} ns, of {}", after.cpus);
    // A netd polling a pipe it cannot drain is one whole CPU; half of one is
    // far above an idle machine and far below that.
    assert!(busy_cpus < 0.5, "the machine was {busy_cpus:.3} CPUs busy with every connection stalled");
    println!("netd_stalled_peer: ok, {busy_cpus:.3} CPUs busy while stalled");
}
