//! netd must not tear a listener down on a flag its client forged.
//!
//! A piped listener hands netd the write end of a notify pipe and the client
//! keeps the read end. netd used to poll `RingHeader::is_reader_closed()` on
//! that pipe and abort the listener when it read set — but that bit lives in
//! the page `SYS_PIPE_MAP` maps writable, so the client can set it while its
//! reader is wide open — a peer forging a fact about itself.
//! netd now asks the kernel instead: a zero-byte `write_nonblock` to the notify
//! pipe is refused by name only when the reader is really gone.
//!
//! This stages the exact disagreement: forge the flag with the reader alive,
//! and the listener must survive. The observation is the kernel's, not another
//! forgeable bit — when netd tears a listener down it drops its end of the
//! notify pipe, so the client's own `read_nonblock` sees EOF (`Ok(0)`); while
//! the listener lives, that read is `WouldBlock`.
//!
//! Needs netd with a NIC. `netd_listener_forgery` runs it on `tests/netcase`.

use std::sync::atomic::{AtomicU32, Ordering};

use toyos::net::NetdConn;
use toyos::AsHandle;
use toyos_abi::ring::RING_READER_CLOSED;
use toyos_abi::syscall::{self, SyscallError};

const PORT: u16 = 8080;
/// netd runs `cleanup_dead_listeners` at the top of every loop pass, and a pass
/// happens whenever it wakes. With no traffic it wakes on a new IPC connection,
/// so this pokes it and then reads the verdict off the notify pipe.
const POKES: usize = 12;
const POKE_PAUSE_NANOS: u64 = 20_000_000; // 20 ms

/// Forge a flag bit into a mapped ring page — a plain store into memory the
/// client owns, exactly `RingHeader::close_reader` would make.
fn forge(page: *mut u8, bit: u32) {
    let flags = unsafe { &*(page as *const AtomicU32) };
    flags.fetch_or(bit, Ordering::Release);
}

/// One netd wake: a bare connection, dropped at once. Its accept is what runs
/// netd's next `cleanup_dead_listeners`.
fn poke() {
    if let Ok(conn) = NetdConn::connect() {
        drop(conn);
    }
}

fn main() {
    let bound = toyos::net::tcp_bind([0, 0, 0, 0], PORT).expect("bind a piped listener");

    // Forge "my reader is gone" with the reader — this process's own notify end
    // — still open.
    let page = bound.notify.pipe_map().expect("map the notify pipe") as *mut u8;
    forge(page, RING_READER_CLOSED);

    // Give netd several passes to act on the forged flag if it is going to.
    for _ in 0..POKES {
        poke();
        syscall::nanosleep(POKE_PAUSE_NANOS);
    }

    // The verdict, read as a kernel fact. If netd believed the flag it aborted
    // the listener and dropped its notify writer, so this reads EOF; if it
    // asked the kernel instead, the writer is alive and there is nothing to
    // read.
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
