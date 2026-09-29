//! The network stack must survive a client that stops talking.
//!
//! netd used to `accept` and then call `ipc::recv_header` on the fresh connection, and
//! `read_exact` behind it is a *blocking* read — so one client that connected
//! and wrote four bytes stopped the network stack for everyone until it
//! disconnected. Its dispatch read every payload off the connection too, so a whole
//! header followed by silence did the same on a connection that had already
//! said what it wanted. This is the compositor's closed defect, line for line,
//! in the last daemon that still had it.
//!
//! Needs netd with a NIC in front of it, which only `tests/netcase` provides —
//! it is in `RUST_SKIP` and `netd_hostile_peer` runs it there.
//!
//! **No wait here has a deadline.** A netd parked on a client never answers, and
//! the harness ceiling reds the hang.

use std::process::exit;

use toyos::endow;
use toyos::AsHandle;
use toyos::ipc::{self, RxStep};
use toyos::poller::{Poller, READABLE};
use toyos::net::{MsgType, RespType};
use toyos::Connection;
use toyos_abi::syscall;

/// A literal address, which netd parses out of the request and answers without
/// a packet — so this asks whether the daemon is *serving*, on a machine whose
/// NIC has nobody on the other end of it.
const LITERAL: &[u8] = b"192.0.2.7";
/// What netd sends back for [`LITERAL`]: one address, four bytes, the octets.
const LITERAL_REPLY: [u8; 6] = [1, 4, 192, 0, 2, 7];

/// A frame netd cannot act on, and what it must do about it.
struct Case {
    name: &'static str,
    /// Bytes written on a fresh connection. A prefix of a frame on purpose in
    /// the first three: that is what a blocking read parks on.
    bytes: Vec<u8>,
    /// Whether netd must have ruled on this connection — answered it or closed
    /// it — by the time it is asked. A partial frame is *not* a ruling: netd is
    /// entitled to hold it until its handshake deadline, and that it does so
    /// without stopping is the whole point.
    ruled: bool,
}

fn header(msg_type: u32, len: u32) -> Vec<u8> {
    let mut frame = Vec::with_capacity(8);
    frame.extend_from_slice(&msg_type.to_ne_bytes());
    frame.extend_from_slice(&len.to_ne_bytes());
    frame
}

/// `TcpBindPipedRequest` is 16 bytes, so a frame declaring fewer is a payload
/// netd asked for and did not get.
fn cases() -> Vec<Case> {
    let bind = MsgType::TcpBindPiped as u32;
    vec![
        // The three that used to park netd. First, so a red run names the
        // stall rather than timing out on a later case behind it.
        Case { name: "connected and silent", bytes: Vec::new(), ruled: false },
        Case { name: "half a header", bytes: vec![0u8; 4], ruled: false },
        Case { name: "header, then silence", bytes: header(bind, 16), ruled: false },
        // Whole frames netd can locate and must rule on.
        Case { name: "short payload", bytes: header(bind, 0), ruled: true },
        Case { name: "oversized header", bytes: header(bind, u32::MAX), ruled: true },
        Case { name: "garbage frame", bytes: header(0xDEAD_BEEF, 0x7FFF_FFFF), ruled: true },
    ]
}

fn main() {
    let cases = cases();
    for case in &cases {
        let conn = endow::service("netd")
            .unwrap_or_else(|e| panic!("[{}] netd is not serving: {e:?}", case.name));
        if !case.bytes.is_empty() {
            let written = syscall::write(conn.as_handle(), &case.bytes)
                .unwrap_or_else(|e| panic!("[{}] could not write the frame: {e:?}", case.name));
            assert_eq!(written, case.bytes.len(), "[{}] partial frame write", case.name);
        }

        // Asked while the hostile connection is still open, which is the whole
        // question: a netd parked on it answers nobody.
        if let Err(e) = still_serving() {
            eprintln!("[{}] {e}", case.name);
            exit(1);
        }

        if case.ruled {
            await_ruling(&conn);
        }
        drop(conn);
    }

    // A connection that never says anything must not be netd's to hold forever:
    // its handshake deadline is netd's own, and the close is what is waited for.
    let silent = endow::service("netd").expect("netd is not serving");
    await_close(&silent);
    drop(silent);
    if let Err(e) = still_serving() {
        eprintln!("[after the silent connection] {e}");
        exit(1);
    }

    println!(
        "netd hostile peer: {} malformed frames refused, silent one dropped, netd alive",
        cases.len(),
    );
}

/// Ask netd something it answers from the request itself, without blocking.
///
/// [`ipc::FrameRx`] is the SDK's non-blocking framing — the same type netd now
/// reads its clients with — waited on with no deadline.
fn still_serving() -> Result<(), String> {
    let conn = endow::service("netd").map_err(|e| format!("netd refused a connection: {e:?}"))?;
    conn.try_send_bytes(MsgType::DnsLookup as u32, LITERAL)
        .map_err(|e| format!("netd would not take a request: {e:?}"))?;

    let mut rx = ipc::FrameRx::<16>::new();
    let poller = Poller::new(1);
    loop {
        match rx.pump(&conn) {
            RxStep::Idle => {
                poller.watch(&conn, READABLE, 0);
                poller.wait(1, u64::MAX, |_| {});
            }
            RxStep::Eof => return Err("netd closed a request without answering it".to_string()),
            RxStep::Malformed => return Err("netd sent a frame the SDK cannot read".to_string()),
            RxStep::Frame { msg_type, payload_len } => {
                if msg_type != RespType::Result as u32 {
                    return Err(format!("netd answered with message type {msg_type}"));
                }
                let payload = rx.payload(payload_len);
                if payload != LITERAL_REPLY.as_slice() {
                    return Err(format!("netd answered a literal address with {payload:?}"));
                }
                return Ok(());
            }
        }
    }
}

/// Wait, with no deadline, until netd has either answered this connection or
/// closed it.
///
/// Both are rulings. An answer is the better one — a client learns that its
/// frame was refused — and a close is what a frame with no locatable next
/// message boundary gets, and what a connection that never finished its
/// request gets at netd's handshake deadline.
fn await_ruling(conn: &Connection) {
    let mut buf = [0u8; 8];
    let poller = Poller::new(1);
    loop {
        match conn.read_nonblock(&mut buf) {
            Err(syscall::SyscallError::WouldBlock) => {
                poller.watch(conn, READABLE, 0);
                poller.wait(1, u64::MAX, |_| {});
            }
            // Bytes, EOF, or the connection itself gone: each is netd's word.
            _ => return,
        }
    }
}

/// Wait, with no deadline, for netd to close `conn`, which never asked anything:
/// bytes on it are an answer to nothing.
fn await_close(conn: &Connection) {
    let mut buf = [0u8; 8];
    let poller = Poller::new(1);
    loop {
        match conn.read_nonblock(&mut buf) {
            Err(syscall::SyscallError::WouldBlock) => {
                poller.watch(conn, READABLE, 0);
                poller.wait(1, u64::MAX, |_| {});
            }
            Ok(n) if n > 0 => {
                eprintln!("netd answered a connection that never asked anything with {n} bytes");
                exit(1);
            }
            // EOF, or the connection itself gone.
            _ => return,
        }
    }
}
