//! soundserver plays on while the log reads nothing of what it says.
//!
//! Booted on `tests/logstallcase`, where `logkeeper` leaves soundserver's log ring unread
//! until this program says [`RELEASE`]. The ring is filled first, by soundserver's
//! own words: this program takes every control connection soundserver will hold and
//! then connects [`FLOOD`] more times, and soundserver refuses each with a line. Then
//! it plays the tone with the ring full, so every line soundserver says while it
//! plays — the mix thread's into its lane, the control thread's into the ring —
//! is said where nobody reads. A mix thread that waits on its output stops
//! there, and the tone with it.
//!
//! The verdict is `/log`'s: once `logkeeper` reads again soundserver's lines account for
//! every connection — each refusal said, or counted by `logkeeper` among the records
//! that found the ring full.

#[path = "../tone.rs"]
mod tone;

use std::time::{Duration, Instant};

use toyos::audio::{MSG_STREAM_CLOSE, MSG_STREAM_ERROR};
use toyos::endow::{self, EndowError};
use toyos::poller::{Poller, READABLE};
use toyos::ipc::{IpcError, IpcHeader};
use toyos::Connection;
use toyos_abi::syscall::SyscallError;

/// What ends `logkeeper`'s stall. `tests/logstallcase` names the same words.
const RELEASE: &str = "soundserver_log_stall: the tone has played, and logkeeper may read soundserver again";

/// soundserver's `MAX_CONTROL_CLIENTS`: past it, a connection is refused with a line.
const HELD: usize = 63;

/// Connections refused with a line each, closed as they are made; one more is
/// kept open to hear its refusal. Each refusal is one record, and the flood is
/// several times the records one ring holds, so the ring fills and the rest
/// are counted.
const FLOOD: usize = 8_192;

/// A flood that has not been refused in this long has met a soundserver that stopped
/// accepting: its control thread is waiting on something.
const PATIENCE: Duration = Duration::from_secs(180);

fn main() {
    let start = Instant::now();
    // Queued before any connection of the flood, and a port accepts in the
    // order it was connected to: these are the connections soundserver holds.
    let held: Vec<Connection> = (0..HELD).map(|_| connect(start)).collect();

    for _ in 0..FLOOD {
        // Closed at once: soundserver's refusal then finds no reader and costs no
        // ring page, and the only thing the connection leaves is the line.
        drop(connect(start));
    }
    // The one refusal this program waits to hear. soundserver says its line before
    // it answers, and accepts in order, so every connection before this one has
    // been refused with a line by then — none is still queued to be accepted
    // once the held ones are gone, as a client that says nothing.
    let last = connect(start);
    match answer(&last, start) {
        Ok(header) if header.msg_type == MSG_STREAM_ERROR => {}
        other => panic!("soundserver answered the flood's last connection with {:?}", other.map(|h| h.msg_type)),
    }
    println!("flooded soundserver with {} refusals in {:.2?}", FLOOD + 1, start.elapsed());
    drop(last);
    drop(held);

    // The tone's connection has to find a free slot, and soundserver frees one when
    // it reads a held connection's end, which it may do after it accepts the
    // next. A probe that breaks the protocol tells the two apart: soundserver
    // refuses it with an error while it is full, and drops it unanswered once
    // it has room. Each refusal is one more line soundserver says.
    let mut refused = 0;
    loop {
        let probe = connect(start);
        probe.signal(MSG_STREAM_CLOSE).expect("a probe's frame to soundserver");
        match answer(&probe, start) {
            Ok(header) if header.msg_type == MSG_STREAM_ERROR => {
                refused += 1;
                std::thread::yield_now();
            }
            Err(IpcError::Disconnected) => break,
            other => panic!("soundserver answered a probe with {:?}", other.map(|h| h.msg_type)),
        }
    }

    println!("soundserver refused {refused} probe(s) before it had room");
    tone::play_tone();
    println!("{RELEASE}");
}

/// soundserver's first frame on `conn`, which it sends within [`PATIENCE`] of `start`
/// or never.
fn answer(conn: &Connection, start: Instant) -> Result<IpcHeader, IpcError> {
    let poller = Poller::new(1);
    poller.watch(conn, READABLE, 0);
    let mut answered = false;
    let left = PATIENCE.saturating_sub(start.elapsed());
    poller.wait(1, left.as_nanos().max(1) as u64, |_| answered = true);
    assert!(answered, "soundserver did not answer a connection within {PATIENCE:?} of the start");
    conn.recv_header()
}

/// One connection to soundserver, waiting out a full queue of connections soundserver has
/// not accepted yet.
fn connect(start: Instant) -> Connection {
    loop {
        match endow::service("soundserver") {
            Ok(conn) => return conn,
            Err(EndowError::Refused(SyscallError::ResourceExhausted)) => {
                assert!(
                    start.elapsed() < PATIENCE,
                    "soundserver stopped accepting connections: its control thread is waiting on \
                     something"
                );
                std::thread::yield_now();
            }
            Err(e) => panic!("soundserver refused a connection: {e:?}"),
        }
    }
}
