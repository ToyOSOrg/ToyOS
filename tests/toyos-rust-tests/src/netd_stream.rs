//! The guest half of the netd stream tests' agreement with the harness's host
//! server (`tests/common/tcppeer.rs`): a connection sends one [`Ask`], and
//! the host serves it.
//!
//! Each netd stream test includes this file whole and uses its own part of it.
#![allow(dead_code)]

use std::time::{Duration, Instant};

use toyos::poller::{Poller, READABLE, WRITABLE};
use toyos::{AsHandle, Pipe};
use toyos_abi::ring::RingHeader;
use toyos_abi::syscall::{self, SyscallError};

/// The host, as QEMU's slirp shows it to the guest.
pub const HOST: [u8; 4] = [10, 0, 2, 2];

/// Byte at absolute stream position `pos`. Every aligned 16-byte group carries
/// its own index, so a lost, duplicated or reordered run shows up whether it is
/// a multiple of 16 long (wrong stamp) or not (wrong filler).
pub fn stream_byte(pos: u64) -> u8 {
    let group = (pos >> 4) as u32;
    match pos & 15 {
        k @ 0..=3 => (group >> (8 * k)) as u8,
        _ => 0xC3,
    }
}

/// Fill a pipe through `write` until the kernel refuses a byte, and answer how
/// many it took: the capacity, for a fresh pipe nobody reads.
pub fn fill(write: &Pipe) -> u64 {
    let chunk = [0u8; 65536];
    let mut total = 0u64;
    loop {
        match write.write_nonblock(&chunk) {
            Ok(0) => panic!("a pipe with room took no bytes after {total}"),
            Ok(n) => total += n as u64,
            Err(SyscallError::WouldBlock) => return total,
            Err(e) => panic!("filling a pipe: {e:?} after {total} bytes"),
        }
    }
}

/// A pipe's data capacity, measured on a fresh pipe of this process's own.
pub fn ring_capacity() -> u64 {
    let (_read, write) = toyos::pipe_pair().expect("a pipe to measure");
    fill(&write)
}

/// Wait until `check` answers, re-asking it each time `handle` reports ready
/// for `flags`, and panic by name if `within` passes first.
///
/// **A readiness completion is a reason to look again, not an answer**: a
/// zero-byte write still wakes the other end's watch, and netd's liveness
/// probes are zero-byte writes.
pub fn await_until<T>(
    handle: &impl AsHandle,
    flags: u32,
    within: Duration,
    what: &str,
    mut check: impl FnMut() -> Option<T>,
) -> T {
    let deadline = Instant::now() + within;
    let poller = Poller::new(1);
    loop {
        if let Some(answer) = check() {
            return answer;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "{what}: not within {within:?}");
        poller.watch(handle, flags, 0);
        poller.wait(1, left.as_nanos() as u64, |_| {});
    }
}

/// What one connection asks the host server for.
#[derive(Clone, Copy)]
pub enum Ask {
    /// Exactly this many [`stream_byte`]s, and then the host's FIN.
    Stream(u64),
    /// The same bytes, and the connection held open with no FIN.
    Held(u64),
    /// A connection of the host's own to this guest's TCP [`FORWARDED_PORT`],
    /// written until it is refused; then this connection's FIN.
    Dial,
}

/// The guest port the harness forwards a host port to, which [`Ask::Dial`]
/// connects to.
pub const FORWARDED_PORT: u16 = 22;

/// A request on the wire: a mode byte (`tcppeer::Mode`), then the length and
/// the seed of the stream, each eight little-endian bytes.
pub fn request(mode: u8, len: u64, seed: u64) -> [u8; 17] {
    let mut request = [mode; 17];
    request[1..9].copy_from_slice(&len.to_le_bytes());
    request[9..].copy_from_slice(&seed.to_le_bytes());
    request
}

/// `what` on the wire.
pub fn ask_bytes(what: Ask) -> [u8; 17] {
    match what {
        Ask::Stream(total) => request(4, total, 0),
        Ask::Held(total) => request(7, total, 0),
        Ask::Dial => request(8, 0, 0),
    }
}

/// Send `what` on a fresh connection.
pub fn ask(tx: &Pipe, what: Ask) {
    let ask = ask_bytes(what);
    assert_eq!(tx.write(&ask), Ok(ask.len()), "telling the host what to send");
}

/// Keep the pipe `write` feeds full until its reader is gone, looking again
/// each time the pipe reports room, and panic by name if `within` passes
/// first.
///
/// **A reader's departure is an event only for a full pipe**: one with room is
/// writable already, so its watch would complete at once, every time.
pub fn keep_full_until_released(write: &Pipe, within: Duration, what: &str) {
    let chunk = [0u8; 65536];
    await_until(write, WRITABLE, within, what, || loop {
        match write.write_nonblock(&chunk) {
            Ok(_) => {}
            Err(SyscallError::WouldBlock) => return None,
            Err(SyscallError::Gone) => return Some(()),
            Err(e) => panic!("{what}: filling the pipe: {e:?}"),
        }
    });
}

/// Wait until `rx`'s ring holds a whole `capacity` of unread stream: its last
/// 16-byte group, read through this process's own `SYS_PIPE_MAP` window, is
/// the pattern's group at `capacity - 16`.
///
/// **A poll, because nothing announces a full ring to its reader**: readiness
/// fires on the first byte. `within` is a liveness guard, said by name.
pub fn await_ring_full(rx: &Pipe, capacity: u64, within: Duration) {
    await_ring_holds(rx, capacity, capacity - 16, within)
}

/// Wait until `rx`'s ring holds the pattern's 16-byte group at stream position
/// `group_start`, in the slot a ring of `capacity` bytes puts it: a group of an
/// earlier lap in that slot carries another stamp. A poll, as
/// [`await_ring_full`] is.
pub fn await_ring_holds(rx: &Pipe, capacity: u64, group_start: u64, within: Duration) {
    const POLL: Duration = Duration::from_millis(1);
    let page = rx.pipe_map().expect("map the receive pipe") as *const u8;
    // SAFETY: `pipe_map` returned the base of this pipe's mapped page, whose
    // data region starts after the header and is `capacity` bytes long; the
    // window lives as long as `rx`, which outlives this function.
    let group = unsafe { page.add(core::mem::size_of::<RingHeader>() + (group_start % capacity) as usize) };
    // SAFETY: inside the data region, as `group` above.
    let at = |i: u64| unsafe { group.add(i as usize).read_volatile() };
    let started = Instant::now();
    while !(0..16).all(|i| at(i) == stream_byte(group_start + i)) {
        assert!(
            started.elapsed() < within,
            "the receive ring never held stream byte {group_start} in {within:?}: its group reads {:02x?}, want {:02x?}",
            (0..16).map(at).collect::<Vec<_>>(),
            (0..16).map(|i| stream_byte(group_start + i)).collect::<Vec<_>>(),
        );
        syscall::nanosleep(POLL.as_nanos() as u64);
    }
}


/// Read `rx` to its end, each wait for more bounded by `within`, and panic by
/// name at the first byte that is not the pattern's. Answers how many bytes
/// came.
pub fn read_pattern(rx: &Pipe, within: Duration, what: &str) -> u64 {
    read_pattern_from(rx, 0, u64::MAX, within, what)
}

/// [`read_pattern`] from stream position `from`, ending at position `until`.
/// Answers the position reached.
pub fn read_pattern_from(rx: &Pipe, from: u64, until: u64, within: Duration, what: &str) -> u64 {
    let mut buf = vec![0u8; 65536];
    let mut at = from;
    while at < until {
        let want = buf.len().min((until - at) as usize);
        let waiting = format!("{what}: the stream after byte {at}");
        let n = await_until(rx, READABLE, within, &waiting, || match rx.read_nonblock(&mut buf[..want]) {
            Ok(n) => Some(n),
            Err(SyscallError::WouldBlock) => None,
            Err(e) => panic!("{what}: reading the stream at {at}: {e:?}"),
        });
        if n == 0 {
            return at;
        }
        if let Some(i) = (0..n).find(|&i| buf[i] != stream_byte(at + i as u64)) {
            let off = at + i as u64;
            panic!(
                "{what}: stream byte {off} came back {:#04x}, want {:#04x}",
                buf[i],
                stream_byte(off),
            );
        }
        at += n as u64;
    }
    at
}
