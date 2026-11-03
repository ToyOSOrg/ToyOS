//! This boot's log, served to a reader on this machine that asks for it on the
//! [`toyos_logstream::SERVICE`] port, whose answer is the read end of a pipe.
//!
//! **Every reader gets the boot from its first line**, however late it
//! connects: the main loop appends each round's lines to one
//! [`toyos_logstream::Replay`] after the file has them, and a reader is a
//! thread with an offset into it. So the same text reaches `/log` and every
//! reader, in the same order, and nothing a reader does can reach the file.
//!
//! A reader that takes none for [`STALLED`] while bytes are owed to it is let
//! go, and that is a line in the log.
//!
//! **A reader that is caught up waits on the replay growing**, and nothing
//! else wakes it: there is no poll and no timer.
//!
//! The console's carriers read the same replay (`crate::carrier`): the hub
//! keeps the newest stamp each round reaches ([`Hub::stamp_through`]), where
//! the console UART's carrier starts ([`Hub::wire_from`]), and the lines said
//! while the machine stops, which the file holds back and the UART carries.

use std::collections::VecDeque;
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use toyos::ipc::Connection;
use toyos::poller::{Poller, WRITABLE};
use toyos::{AsHandle, Pipe};
use toyos_abi::syscall::SyscallError;
use toyos::say;
use toyos_logstream::{Next, ProgramLine, Replay, Severity, Tag, LOGKEEPER, SERVED};

/// The most bytes one reader is handed per wake: bounds how long the replay's
/// lock is held for a copy, not how far a reader may fall behind.
const CHUNK: usize = 64 * 1024;

/// Readers at once. Each is a thread, so the count is bounded and a reader
/// past it is refused by name: only a program holding the
/// [`SERVICE`](toyos_logstream::SERVICE) connector is one.
const MAX_READERS: usize = 2;

/// How long a reader may take no byte at all of what it is owed before its
/// slot is let go: a wait on its sink taking bytes, bounded by this.
const STALLED: Duration = Duration::from_secs(10);

/// Rounds whose stamps the hub keeps: a carrier whose channel is further
/// behind than this speaks for none of it until it catches up.
const ROUNDS: usize = 4096;

/// The replay, and what the console's carriers read beside it.
struct Held {
    replay: Replay,
    /// Each round's end in the replay and the newest stamp of its records,
    /// oldest first, the last [`ROUNDS`] of them.
    rounds: VecDeque<(u64, u64)>,
    /// Where the console UART's carrier starts, once the first round says.
    wire_from: Option<u64>,
    /// Lines said while the machine stops, held back from the file and owed
    /// the UART's carrier.
    stopping: Vec<u8>,
}

/// The replay, and the wake a caught-up reader waits on.
pub struct Hub {
    shared: Arc<Shared>,
}

struct Shared {
    replay: Mutex<Held>,
    grew: Condvar,
    readers: AtomicUsize,
    /// The wall clock the boot started at, for a line a reader is owed.
    boot_secs: Option<u64>,
}

impl Hub {
    /// A reader is handed over by the `log` port's thread ([`Hub::read`]).
    pub fn start(cap: usize, boot_secs: Option<u64>) -> Self {
        let shared = Arc::new(Shared {
            replay: Mutex::new(Held {
                replay: Replay::new(cap),
                rounds: VecDeque::new(),
                wire_from: None,
                stopping: Vec::new(),
            }),
            grew: Condvar::new(),
            readers: AtomicUsize::new(0),
            boot_secs,
        });
        Self { shared }
    }

    /// A reader that asked for the log: it gets the read end
    /// of a pipe of its own, which the log is written into.
    pub fn read(&self, conn: &Connection) {
        let Some(pipe) = self.hand_over(conn) else { return };
        admit(&self.shared, format!("reader {}", conn.as_handle().raw().0), PipeSink::new(pipe));
    }

    /// The lines the file has just taken, for every reader, and the newest
    /// stamp among them where they are a round's.
    pub fn append(&self, lines: &[u8], newest: Option<u64>) {
        let mut held = self.lock();
        held.replay.append(lines);
        if let Some(at_ns) = newest {
            let end = held.replay.end();
            if held.rounds.len() == ROUNDS {
                held.rounds.pop_front();
            }
            held.rounds.push_back((end, at_ns));
        }
        drop(held);
        self.shared.grew.notify_all();
    }

    /// The newest stamp of the last round wholly before `at`: what a channel
    /// that has taken the replay up to `at` has spoken.
    pub fn stamp_through(&self, at: u64) -> Option<u64> {
        let held = self.lock();
        held.rounds.iter().rev().find(|&&(end, _)| end <= at).map(|&(_, at_ns)| at_ns)
    }

    /// The console UART's carrier starts at `at`, the end of the last line
    /// the kernel put on the UART itself. Once.
    pub fn set_wire_from(&self, at: u64) {
        let mut held = self.lock();
        assert!(held.wire_from.is_none(), "logkeeper: where the UART's carrier starts is said twice");
        held.wire_from = Some(at);
        drop(held);
        self.shared.grew.notify_all();
    }

    /// Where the console UART's carrier starts, once the first round said.
    pub fn wire_from(&self) -> u64 {
        let mut held = self.lock();
        loop {
            if let Some(at) = held.wire_from {
                return at;
            }
            held = self.shared.grew.wait(held).expect("logkeeper: the replay is poisoned");
        }
    }

    /// Lines said while the machine stops, for the UART's carrier alone.
    pub fn append_stopping(&self, lines: &[u8]) {
        self.lock().stopping.extend_from_slice(lines);
        self.shared.grew.notify_all();
    }

    /// The stop was refused: what was said while it stood, which the file
    /// takes now, for every reader, and none of it owed the UART beside.
    pub fn resume(&self, lines: &[u8]) {
        self.lock().stopping.clear();
        self.append(lines, None);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Held> {
        self.shared.replay.lock().expect("logkeeper: the replay is poisoned")
    }

    /// How long the boot is so far.
    pub fn end(&self) -> u64 {
        self.lock().replay.end()
    }

    /// The pipe a reader on `conn` is handed, and the boot's length it starts
    /// with: `None` where the reader is already gone.
    pub fn hand_over(&self, conn: &Connection) -> Option<Pipe> {
        hand_over(conn, self.end())
    }

    /// What a carrier at `at` is fed next, once there is any: at most
    /// [`CHUNK`] bytes of the replay, and `at` past them, or the line that
    /// says what the replay no longer holds; or, for the UART's carrier
    /// caught up with the replay, the lines said while the machine stops.
    pub fn next_for_a_carrier(&self, at: &mut u64, wire: bool) -> Fed {
        let mut held = self.lock();
        loop {
            match held.replay.next(*at, CHUNK) {
                Next::Bytes(bytes) => {
                    *at += bytes.len() as u64;
                    return Fed::Replay(bytes.to_vec());
                }
                Next::Evicted { lost, at: resume } => {
                    *at = resume;
                    return Fed::Said(evicted(self.shared.boot_secs, lost));
                }
                Next::CaughtUp if wire && !held.stopping.is_empty() => {
                    return Fed::Said(std::mem::take(&mut held.stopping));
                }
                Next::CaughtUp => held = self.shared.grew.wait(held).expect("logkeeper: the replay is poisoned"),
            }
        }
    }

    /// One of this program's own lines, as the log renders it, which no ring
    /// carries: what a carrier is told on its own channel.
    pub fn own_line(&self, severity: Severity, text: &str) -> Vec<u8> {
        own_line(self.shared.boot_secs, severity, text)
    }
}

/// What a carrier is fed.
pub enum Fed {
    /// Bytes of the replay, which a channel taking them speaks for.
    Replay(Vec<u8>),
    /// Lines that are not the replay's.
    Said(Vec<u8>),
}

/// The read end goes to the reader with the boot's length so far, the write
/// end stays here; `None` where the reader is already gone.
fn hand_over(conn: &Connection, end: u64) -> Option<Pipe> {
    let (read, write) = match toyos::pipe_pair() {
        Ok(ends) => ends,
        Err(e) => {
            say!("logkeeper: no pipe for a reader: {e:?}");
            return None;
        }
    };
    match conn.try_send_with(SERVED, &end, [read.into()]) {
        Ok(()) => Some(write),
        Err(e) => {
            say!("logkeeper: a reader went before it was answered: {e:?}");
            None
        }
    }
}

/// Why a reader's thread ended.
enum Left {
    /// The reader closed, or its sink refused a write.
    Gone,
    /// Its sink took no byte for [`STALLED`] while bytes were owed.
    Stalled,
}

/// A reader thread, where its count allows one.
fn admit(shared: &Arc<Shared>, who: String, sink: impl Write + Send + 'static) {
    let count = &shared.readers;
    if count.fetch_add(1, Ordering::SeqCst) >= MAX_READERS {
        count.fetch_sub(1, Ordering::SeqCst);
        say!("logkeeper: refusing {who}: {MAX_READERS} readers on this machine are already served");
        return;
    }
    let theirs = Arc::clone(shared);
    let spawned = std::thread::Builder::new().name("log-reader".into()).spawn(move || {
        let (sent, left) = feed(&theirs, sink);
        theirs.readers.fetch_sub(1, Ordering::SeqCst);
        match left {
            Left::Stalled => say!(
                "logkeeper: letting {who} go after {sent} bytes: it took none of what it is owed for {} s",
                STALLED.as_secs()
            ),
            Left::Gone => {}
        }
    });
    if let Err(e) = spawned {
        count.fetch_sub(1, Ordering::SeqCst);
        say!("logkeeper: no thread for a reader: {e}");
    }
}

/// Write the boot to `sink` from its first byte, then each round as it lands,
/// until the reader goes or stalls. Answers the bytes it sent and why it ended.
fn feed(shared: &Shared, mut sink: impl Write) -> (u64, Left) {
    let mut at = 0u64;
    let mut sent = 0u64;
    loop {
        let chunk = next(shared, &mut at);
        match sink.write_all(&chunk) {
            Ok(()) => sent += chunk.len() as u64,
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => return (sent, Left::Stalled),
            Err(_) => return (sent, Left::Gone),
        }
    }
}

/// The bytes after `at`, at most [`CHUNK`] of them, once there are any; `at`
/// moves past them. A reader the replay no longer holds the bytes of is
/// handed the line that says so instead.
fn next(shared: &Shared, at: &mut u64) -> Vec<u8> {
    let mut held = shared.replay.lock().expect("logkeeper: the replay is poisoned");
    loop {
        match held.replay.next(*at, CHUNK) {
            Next::Bytes(bytes) => {
                *at += bytes.len() as u64;
                return bytes.to_vec();
            }
            Next::Evicted { lost, at: resume } => {
                *at = resume;
                return evicted(shared.boot_secs, lost);
            }
            Next::CaughtUp => held = shared.grew.wait(held).expect("logkeeper: the replay is poisoned"),
        }
    }
}

/// The line a reader gets in place of what the replay no longer holds.
fn evicted(boot_secs: Option<u64>, lost: u64) -> Vec<u8> {
    let text = format!("logkeeper: the first {lost} bytes of this boot are no longer held here; /log has them");
    own_line(boot_secs, Severity::Warn, &text)
}

/// One of this program's lines, stamped now, rendered as the log renders it.
fn own_line(boot_secs: Option<u64>, severity: Severity, text: &str) -> Vec<u8> {
    let at_ns = toyos_abi::clock::stamp_ns();
    let tag = Tag::new(LOGKEEPER).expect("logkeeper's own name is a tag");
    let stamp = crate::stamp(boot_secs, Some(at_ns));
    let line = ProgramLine { stamp: &stamp, at_ns, severity, tid: 0, pid: None, tag, text: text.as_bytes() };
    format!("{line}\n").into_bytes()
}

/// A pipe as a byte sink: one `write` takes what fits, and a full pipe is
/// waited on for [`STALLED`] and no longer.
struct PipeSink {
    pipe: Pipe,
    poller: Poller,
}

impl PipeSink {
    fn new(pipe: Pipe) -> Self {
        Self { pipe, poller: Poller::new(1) }
    }
}

impl Write for PipeSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        loop {
            match self.pipe.write_nonblock(buf) {
                Ok(n) => return Ok(n),
                Err(SyscallError::WouldBlock) => {}
                Err(e) => return Err(std::io::Error::other(format!("{e:?}"))),
            }
            self.poller.watch(&self.pipe, WRITABLE, 0);
            let mut room = false;
            self.poller.wait(1, STALLED.as_nanos() as u64, |_| room = true);
            if !room {
                return Err(std::io::ErrorKind::TimedOut.into());
            }
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn replay(fed: Fed) -> Vec<u8> {
        match fed {
            Fed::Replay(bytes) => bytes,
            Fed::Said(_) => panic!("lines that are not the replay's"),
        }
    }

    /// A channel that has taken the replay up to an offset has spoken the
    /// newest stamp of the last round wholly before it, and nothing of a round
    /// it has only begun.
    #[test]
    fn what_a_channel_took_speaks_for_the_rounds_it_took_whole() {
        let hub = Hub::start(1 << 20, None);
        hub.append(b"a\nb\n", Some(10));
        hub.append(b"c\n", Some(20));
        hub.resume(b"held\n");
        assert_eq!(hub.stamp_through(0), None);
        assert_eq!(hub.stamp_through(3), None, "half the first round is none of it");
        assert_eq!(hub.stamp_through(4), Some(10));
        assert_eq!(hub.stamp_through(6), Some(20));
        assert_eq!(hub.stamp_through(11), Some(20), "lines with no round speak for nothing");
    }

    /// The UART's carrier starts where the kernel left the UART, and is handed
    /// what was said while the machine stopped once it has the replay; a
    /// refused stop's lines reach it through the replay, once.
    #[test]
    fn the_wire_starts_after_the_kernel_and_takes_the_stops_lines_last() {
        let hub = Hub::start(1 << 20, None);
        hub.append(b"kernel 1\nkernel 2\nprogram\n", Some(30));
        hub.set_wire_from(18);
        let mut at = hub.wire_from();
        assert_eq!(replay(hub.next_for_a_carrier(&mut at, true)), b"program\n");
        hub.append_stopping(b"stopping\n");
        match hub.next_for_a_carrier(&mut at, true) {
            Fed::Said(bytes) => assert_eq!(bytes, b"stopping\n"),
            Fed::Replay(bytes) => panic!("the replay's {bytes:?} where the stop's lines were owed"),
        }
        hub.append_stopping(b"again\n");
        hub.resume(b"stopping\nagain\n");
        assert_eq!(replay(hub.next_for_a_carrier(&mut at, true)), b"stopping\nagain\n");
        let mut port = 0;
        assert_eq!(replay(hub.next_for_a_carrier(&mut port, false)).len(), 41, "the port is fed from the first line");
    }
}
