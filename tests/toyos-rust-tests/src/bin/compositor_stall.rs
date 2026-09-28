//! The desktop must survive a client that stops talking, stops listening, or
//! never stops.
//!
//! Every one of these cases used to park the compositor's whole event loop in
//! a kernel wait with no deadline — no redraws, no input, nothing — because
//! the compositor read and wrote its clients with blocking calls. The one
//! written up in `issues/isolation/` is the second case here: a client
//! that connects and sends four bytes, met by `ipc::recv_header` on a freshly
//! accepted connection.
//!
//! Each case sets its stall up and leaves it standing, then asks the
//! compositor a question. No wait here has a deadline: a compositor frozen on a
//! client never answers, and the harness ceiling reds it. The host side asserts
//! the other half — that the desktop is still *painting*, and that every client
//! dropped along the way was named in the log.

use std::process::exit;
use std::thread;
use std::sync::atomic::{AtomicBool, Ordering};

use toyos::endow;
use toyos::AsHandle;
use toyos::{ipc, Connection};
use toyos_abi::syscall::{self, SyscallError};
use window::Window;

/// Between two looks at a connection the compositor is expected to drop. A
/// pace and never a verdict.
const POLL_NS: u64 = 10_000_000;

/// A message type no protocol here defines: the compositor's dispatch ignores
/// it, so a stream of them is pure event-loop load with nothing to draw. That
/// is what makes it a starvation case rather than a redraw case.
const UNKNOWN_MSG: u32 = 0x7FFF_0001;

/// One `MSG_GET_RESOLUTION` costs the client 8 bytes and the compositor 16, so
/// filling a client's 2,097,088-byte receive ring from the far side takes
/// 131,068 answers. This is that with margin, and the requests themselves are
/// half the bytes and fit in the client's own ring — nothing here can block
/// the *client* instead, which would prove the wrong thing.
const REQUESTS: usize = 140_000;

fn main() {
    // Held to the end of the run: a dropped `Connection` closes the handle, and
    // a closed handle is a peer that hung up rather than one that went quiet.
    let mut held: Vec<Connection> = Vec::new();

    held.push(connect("connected and silent"));
    probe("connected and silent");

    let conn = connect("half a header");
    write_raw(&conn, &[0u8; 4], "half a header");
    held.push(conn);
    probe("half a header");

    let conn = connect("header without payload");
    let payload_len = std::mem::size_of::<window::CreateWindowRequest>() as u32;
    write_raw(&conn, &header(window::MSG_CREATE_WINDOW, payload_len), "header without payload");
    held.push(conn);
    probe("header without payload");

    // The three above are handshakes that never complete. Nothing the client
    // does ends them; the compositor's own deadline does, and each one's close
    // is what is waited for.
    for conn in &held {
        await_hang_up(conn.as_handle());
    }
    probe("after the handshake deadline");

    // A window that stops in the middle of a message it already declared. The
    // stall is on an established connection rather than a fresh one, which is
    // the sibling of the accept-path defect and had the same cure.
    let stuck = Window::create(64, 64).expect("a window to stall mid-message with");
    write_handle(stuck.handle(), &header(window::MSG_CLIPBOARD_SET, 116), "window mid-message");
    write_handle(stuck.handle(), &[b'x'; 8], "window mid-message");
    probe("window stopped mid-message");

    // A window that asks faster than it reads. The compositor's answer has to
    // be a refusal, because the alternative is waiting for a client to read
    // its mail.
    let deaf = Window::create(64, 64).expect("a window to stop reading with");
    let mut requests = Vec::with_capacity(REQUESTS * 8);
    for _ in 0..REQUESTS {
        requests.extend_from_slice(&header(window::MSG_GET_RESOLUTION, 0));
    }
    write_handle(deaf.handle(), &requests, "window that will not read");
    await_hang_up(deaf.handle());
    probe("window that will not read");

    // A window with something to send on every pass. Nothing here is
    // unanswerable — the loop simply never runs out of work, and a drain that
    // ends only when nothing is ready never reaches the screen. So a second
    // window presents while it streams, and the stream runs until that present
    // is composited: a drain loop the stream starves never gets to `redraw`, and
    // the frame never comes.
    let noisy = Window::create(64, 64).expect("a window to stream from");
    let handle = noisy.handle();
    let mut watcher = Window::create(64, 64).expect("a window to composite under the stream");
    let (streaming, framed) = (AtomicBool::new(false), AtomicBool::new(false));
    let presenter = thread::current();
    thread::scope(|s| {
        s.spawn(|| {
            let frame = header(UNKNOWN_MSG, 0);
            while !framed.load(Ordering::Acquire) {
                // Fill the ring, not merely feed it. The compositor takes one
                // frame per client per pass, so a client that keeps up with only
                // that lets the drain run dry and the screen get painted — which
                // is the thing this case is supposed to prevent.
                //
                // Never a torn frame: both ends move this ring in multiples of
                // eight bytes and its capacity is one too, so a write of a header
                // either fits whole or finds no room at all.
                while matches!(syscall::write_nonblock(handle, &frame), Ok(8)) {}
                streaming.store(true, Ordering::Release);
                presenter.unpark();
                syscall::nanosleep(1_000_000);
            }
        });
        // Presented once the ring is full, so the frame is composited under
        // the stream and not ahead of it. Parked until then, never spinning: a
        // thread yielding beside the writer held it below the compositor's
        // drain rate, and the ring never filled.
        while !streaming.load(Ordering::Acquire) {
            thread::park();
        }
        println!("compositor stall: the ring is full; a second window presents under it");
        watcher.present();
        loop {
            match watcher.recv_event() {
                window::Event::Frame => break,
                window::Event::Close => fail(
                    "[window that never stops sending] the window presented under the stream \
                     was closed",
                ),
                _ => {}
            }
        }
        framed.store(true, Ordering::Release);
    });
    probe("window that never stops sending");

    println!("compositor stall: 6 stalls survived, compositor still serving");
}

fn header(msg_type: u32, len: u32) -> [u8; 8] {
    let mut frame = [0u8; 8];
    frame[..4].copy_from_slice(&msg_type.to_ne_bytes());
    frame[4..].copy_from_slice(&len.to_ne_bytes());
    frame
}

fn connect(what: &str) -> Connection {
    endow::service("compositor")
        .unwrap_or_else(|e| fail(&format!("[{what}] the compositor is not serving: {e:?}")))
}

fn write_raw(conn: &Connection, bytes: &[u8], what: &str) {
    write_handle(conn.as_handle(), bytes, what);
}

/// Every write here fits in the pipe it goes into, so a blocking `write` can
/// only be the compositor's problem, never this binary's.
fn write_handle(handle: toyos_abi::RawHandle, bytes: &[u8], what: &str) {
    let mut offset = 0;
    while offset < bytes.len() {
        match syscall::write(handle, &bytes[offset..]) {
            Ok(n) => offset += n,
            Err(e) => fail(&format!("[{what}] write failed after {offset} bytes: {e:?}")),
        }
    }
}

/// Wait, with no deadline, until nothing holds the other end of `handle`.
///
/// **Without draining a byte**, which is the whole difficulty: this client's
/// receive ring has to stay full for the compositor to reach the end of it,
/// so the answer cannot be read from the ring. An empty `write_nonblock`
/// writes nothing and still asks the one question that matters — is anything
/// still holding the read end — so the refusal is observed rather than slept
/// through. A compositor parked in `write` instead has its handle open and
/// answers `Ok` here forever.
fn await_hang_up(handle: toyos_abi::RawHandle) {
    while syscall::write_nonblock(handle, &[]) != Err(SyscallError::Gone) {
        syscall::nanosleep(POLL_NS);
    }
}

/// Ask the compositor something it always answers from its dispatch, so a reply
/// proves the event loop reached the end of a pass. No deadline: a compositor
/// parked on a client never answers, and the harness ceiling reds it.
fn probe(what: &str) {
    let conn = connect(what);
    if let Err(e) = ipc::signal(conn.as_handle(), window::MSG_GET_RESOLUTION) {
        fail(&format!("[{what}] could not ask the compositor for its resolution: {e:?}"));
    }
    let mut buf = [0u8; 16];
    let mut got = 0;
    while got < buf.len() {
        match syscall::read(conn.as_handle(), &mut buf[got..]) {
            Ok(0) => fail(&format!("[{what}] the compositor closed the probe unanswered")),
            Ok(n) => got += n,
            Err(e) => fail(&format!("[{what}] the probe could not be read: {e:?}")),
        }
    }
}

fn fail(msg: &str) -> ! {
    eprintln!("compositor stall: {msg}");
    exit(1);
}
