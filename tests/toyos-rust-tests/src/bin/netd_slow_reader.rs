//! A TCP receiver that falls a whole pipe behind still gets every byte, once.
//!
//! netd moves a connection's received bytes out of its TCP socket into the
//! client's receive pipe. A pipe is a ring of fixed capacity, so a client that
//! stops reading fills it, and from then on the only lawful home for the
//! peer's further bytes is the socket's own buffer, whose window then closes.
//!
//! This program is that client: it connects to the harness's host server
//! (argv[1] is its port on [`HOST`]), which sends [`stream_byte`]s and
//! closes, and reads **nothing** until the ring holds a whole capacity — seen
//! through its own `SYS_PIPE_MAP` window of the receive pipe, where the last
//! byte of the data region carries the stamp of stream position
//! `capacity - 1`. Only then does it read the stream to its end and compare
//! each byte with the pattern.
//!
//! The capacity is measured, never assumed: a fresh pipe of this process's own
//! is written until the kernel refuses a byte.
//!
//! `netd_slow_reader: ok bytes=<n>` is the only success line; a stream that
//! differs names its first differing offset and exits non-zero.

use toyos::AsHandle;
use toyos_abi::ring::RingHeader;
use toyos_abi::syscall::{self, SyscallError};

/// The host, as QEMU's slirp shows it to the guest.
const HOST: [u8; 4] = [10, 0, 2, 2];

/// Bytes the host sends past the ring's capacity. Anything over the socket's
/// own buffer makes a pipe that drops bytes when full drop some.
const PAST_THE_RING: u64 = 1024 * 1024;

/// A liveness guard on the ring filling: the host has more than a capacity to
/// send the moment this connects, so a ring that never fills is a netd that
/// stopped moving bytes, and it is said by name rather than left to the
/// runner's ceiling.
const FILL_BOUND_NANOS: u64 = 60_000_000_000;
const FILL_POLL_NANOS: u64 = 1_000_000;

/// Byte at absolute stream position `pos`. Every aligned 16-byte group carries
/// its own index, so a lost, duplicated or reordered run shows up whether it is
/// a multiple of 16 long (wrong stamp) or not (wrong filler). The host server
/// in `tests/toyos.rs` sends exactly this.
fn stream_byte(pos: u64) -> u8 {
    let group = (pos >> 4) as u32;
    match pos & 15 {
        k @ 0..=3 => (group >> (8 * k)) as u8,
        _ => 0xC3,
    }
}

/// A pipe's data capacity, measured: a fresh pipe written without a reader
/// taking anything until the kernel says it is full.
fn ring_capacity() -> u64 {
    let (_read, write) = toyos::pipe_pair().expect("a pipe to measure");
    let chunk = [0u8; 65536];
    let mut total = 0u64;
    loop {
        match write.write_nonblock(&chunk) {
            Ok(0) => panic!("a pipe with room took no bytes after {total}"),
            Ok(n) => total += n as u64,
            Err(SyscallError::WouldBlock) => return total,
            Err(e) => panic!("measuring a pipe's capacity: {e:?} after {total} bytes"),
        }
    }
}

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
    let ask = total.to_le_bytes();
    assert_eq!(conn.tx.write(&ask), Ok(ask.len()), "telling the host how much to send");

    let page = conn.rx.pipe_map().expect("map the receive pipe") as *const u8;
    // SAFETY: `pipe_map` returned the base of this pipe's mapped page, whose
    // data region starts after the header and is `capacity` bytes long; the
    // window lives as long as `conn.rx`, which outlives every read below.
    let last = unsafe { page.add(core::mem::size_of::<RingHeader>() + capacity as usize - 1) };
    let want = stream_byte(capacity - 1);
    // The stamp alone could be a stale byte of a reused page; the filler
    // around it is checked too by reading the whole last group.
    let group_start = capacity - 16;
    let full = || {
        (0..16).all(|i| {
            // SAFETY: inside the data region, as `last` above.
            let at = unsafe { last.sub(15 - i as usize).read_volatile() };
            at == stream_byte(group_start + i)
        })
    };
    let mut waited = 0u64;
    while !full() {
        assert!(
            waited < FILL_BOUND_NANOS,
            "the receive ring never filled in {}s: its last byte is {:#04x}, want {want:#04x}",
            FILL_BOUND_NANOS / 1_000_000_000,
            // SAFETY: as above.
            unsafe { last.read_volatile() },
        );
        syscall::nanosleep(FILL_POLL_NANOS);
        waited += FILL_POLL_NANOS;
    }
    println!("netd_slow_reader: the ring is full at {capacity} bytes unread; reading");

    let mut buf = vec![0u8; 65536];
    let mut at = 0u64;
    loop {
        let n = match syscall::read(conn.rx.as_handle(), &mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => panic!("reading the stream at {at}: {e:?}"),
        };
        if let Some(i) = (0..n).find(|&i| buf[i] != stream_byte(at + i as u64)) {
            let off = at + i as u64;
            panic!(
                "netd_slow_reader: stream byte {off} came back {:#04x}, want {:#04x} \
                 (ring capacity {capacity}, {} past it)",
                buf[i],
                stream_byte(off),
                off as i64 - capacity as i64,
            );
        }
        at += n as u64;
    }
    assert_eq!(at, total, "the stream ended after {at} of {total} bytes, every one of them right");
    println!("netd_slow_reader: ok bytes={at}");
}
