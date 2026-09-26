//! The guest half of the netd stream tests' agreement with the harness's host
//! server (`PatternServer` in `tests/toyos.rs`): a connection sends eight
//! little-endian bytes naming a length, and the host sends exactly that many
//! [`stream_byte`]s and closes its side.

use std::time::{Duration, Instant};

use toyos::poller::{Poller, READABLE};
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

/// Tell the host how many bytes to send on this connection.
pub fn ask(tx: &Pipe, total: u64) {
    let ask = total.to_le_bytes();
    assert_eq!(tx.write(&ask), Ok(ask.len()), "telling the host how much to send");
}

/// Wait until `rx`'s ring holds a whole `capacity` of unread stream: its last
/// 16-byte group, read through this process's own `SYS_PIPE_MAP` window, is
/// the pattern's group at `capacity - 16`.
///
/// **A poll, because nothing announces a full ring to its reader**: readiness
/// fires on the first byte. `within` is a liveness guard, said by name.
pub fn await_ring_full(rx: &Pipe, capacity: u64, within: Duration) {
    const POLL: Duration = Duration::from_millis(1);
    let page = rx.pipe_map().expect("map the receive pipe") as *const u8;
    let group_start = capacity - 16;
    // SAFETY: `pipe_map` returned the base of this pipe's mapped page, whose
    // data region starts after the header and is `capacity` bytes long; the
    // window lives as long as `rx`, which outlives this function.
    let group = unsafe { page.add(core::mem::size_of::<RingHeader>() + group_start as usize) };
    // SAFETY: inside the data region, as `group` above.
    let at = |i: u64| unsafe { group.add(i as usize).read_volatile() };
    let started = Instant::now();
    while !(0..16).all(|i| at(i) == stream_byte(group_start + i)) {
        assert!(
            started.elapsed() < within,
            "the receive ring never filled in {within:?}: its last group reads {:02x?}, want {:02x?}",
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
    let mut buf = vec![0u8; 65536];
    let mut at = 0u64;
    loop {
        let waiting = format!("{what}: the stream after byte {at}");
        let n = await_until(rx, READABLE, within, &waiting, || match rx.read_nonblock(&mut buf) {
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
}
