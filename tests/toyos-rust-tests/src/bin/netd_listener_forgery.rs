//! netd must not tear a piped listener down on a flag its client forged. The
//! client keeps the notify pipe's reader; netd used to abort the listener when
//! `RingHeader::is_reader_closed()` read set — a bit the client can forge in the
//! writable ring page. netd now probes the kernel (a zero-byte `write_nonblock`,
//! refused only when the reader is really gone). Here the flag is forged with
//! the reader alive: the listener must survive, observed as a kernel fact —
//! netd drops its notify writer when it aborts, so the client's `read_nonblock`
//! sees EOF, and `WouldBlock` while the listener lives. Runs on `tests/netcase`.

use std::sync::atomic::{AtomicU32, Ordering};

use toyos::net::NetdConn;
use toyos::AsHandle;
use toyos_abi::ring::RING_READER_CLOSED;
use toyos_abi::syscall::{self, SyscallError};

const PORT: u16 = 8080;
/// netd runs `cleanup_dead_listeners` on every wake, and with no traffic it
/// wakes on a new IPC connection — so poke it, then read the notify verdict.
const POKES: usize = 12;
const POKE_PAUSE_NANOS: u64 = 20_000_000; // 20 ms

/// A plain store into the mapped ring header, as the client would forge it.
fn forge(page: *mut u8, bit: u32) {
    let flags = unsafe { &*(page as *const AtomicU32) };
    flags.fetch_or(bit, Ordering::Release);
}

/// One netd wake: a bare connection dropped at once, so its next pass runs cleanup.
fn poke() {
    if let Ok(conn) = NetdConn::connect() {
        drop(conn);
    }
}

fn main() {
    let bound = toyos::net::tcp_bind([0, 0, 0, 0], PORT).expect("bind a piped listener");

    // Forge "my reader is gone" with the reader still open.
    let page = bound.notify.pipe_map().expect("map the notify pipe") as *mut u8;
    forge(page, RING_READER_CLOSED);

    for _ in 0..POKES {
        poke();
        syscall::nanosleep(POKE_PAUSE_NANOS);
    }

    // A believed flag aborts the listener and drops netd's notify writer, so
    // this reads EOF; a survivor's writer is alive and there is nothing to read.
    let mut buf = [0u8; 4];
    match syscall::read_nonblock(bound.notify.as_handle(), &mut buf) {
        Err(SyscallError::WouldBlock) => {
            println!("netd listener forgery: listener survived a forged reader-closed flag");
        }
        Ok(0) => panic!(
            "netd tore the listener down on a flag the client forged — its notify writer is \
             gone while the client's reader was never closed"
        ),
        other => panic!("unexpected read of the notify pipe: {other:?}"),
    }
}
