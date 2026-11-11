//! This boot's log, served to a reader on this machine that asks for it on the
//! [`toyos_logstream::SERVICE`] port, whose answer is the read end of a pipe.
//!
//! **Every reader gets the boot from its first line**, however late it
//! connects: the main loop appends each round's lines to one
//! [`toyos_logstream::Replay`], which the file (`crate::file`) takes its
//! batches from. A reader is a thread with an offset into it. So the same
//! text reaches `/log` and every reader, in the same order, and nothing a
//! reader does can reach the file.
//!
//! A reader that takes none for [`STALLED`] while bytes are owed to it is let
//! go, and that is a line in the log.
//!
//! **A reader that is caught up waits on the replay growing or on its reader
//! leaving**, whichever comes first: a reader that leaves while the log is
//! quiet gives back its thread and its slot then. There is no timer.
//!
//! The console's carriers read the same replay (`crate::carrier`): the hub
//! keeps the kernel's read positions each round is written through
//! ([`Hub::positions_through`]), where the console UART's carrier starts with
//! what it owes of the rounds before ([`Hub::wire_start`]), and the lines said
//! while the machine stops, which the file holds back and the UART carries.

use std::collections::VecDeque;
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use toyos::ipc::Connection;
use toyos::poller::{Poller, OTHER_END_GONE, READABLE, WRITABLE};
use toyos::wake::{self, Bell, Waker};
use toyos::{AsHandle, Pipe};
use toyos_abi::syscall::SyscallError;
use toyos::say;
use toyos_logstream::{Next, ProgramLine, Replay, Severity, Tag, LOGKEEPER, SERVED};

use crate::store::Owed;

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

/// Rounds whose positions the hub keeps: a carrier whose channel is further
/// behind than this speaks for none of it until it catches up.
const ROUNDS: usize = 4096;

/// The kernel's read positions a round is written through, one per shard.
pub type Positions = Arc<[u64]>;

/// The replay, and what the console's carriers read beside it.
struct Held {
    replay: Replay,
    /// Each round's end in the replay and the positions it is written
    /// through, oldest first, the last [`ROUNDS`] of them.
    rounds: VecDeque<(u64, Positions)>,
    /// Where the console UART's carrier starts, and what it owes of the
    /// rounds before: their lines but the kernel's records it carried itself.
    wire_from: Option<(u64, Arc<[u8]>)>,
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
    /// A wake per reader, rung as the replay grows: a reader waits on its
    /// own end beside it, which a condition variable cannot.
    reader_wakes: Mutex<Vec<Waker>>,
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
            reader_wakes: Mutex::new(Vec::new()),
            boot_secs,
        });
        Self { shared }
    }

    /// A reader that asked for the log: it gets the read end
    /// of a pipe of its own, which the log is written into.
    pub fn read(&self, conn: &Connection) {
        let Some(pipe) = self.hand_over(conn) else { return };
        admit(&self.shared, format!("reader {}", conn.as_handle().raw().0), pipe);
    }

    /// The lines the file has just taken, for every reader, and the kernel's
    /// read positions they are written through where they are a round's.
    pub fn append(&self, lines: &[u8], through: Option<Positions>) {
        let mut held = self.lock();
        held.replay.append(lines);
        if let Some(positions) = through {
            let end = held.replay.end();
            if held.rounds.len() == ROUNDS {
                held.rounds.pop_front();
            }
            held.rounds.push_back((end, positions));
        }
        drop(held);
        self.shared.grew.notify_all();
        self.shared.reader_wakes.lock().expect("logkeeper: a reader's wake is poisoned").retain(|w| w.ring().is_ok());
    }

    /// The positions of the last round wholly before `at`: what a channel
    /// that has taken the replay up to `at` has spoken of the kernel's log.
    pub fn positions_through(&self, at: u64) -> Option<Positions> {
        let held = self.lock();
        held.rounds.iter().rev().find(|(end, _)| *end <= at).map(|(_, positions)| Arc::clone(positions))
    }

    /// The console UART's carrier starts at `at`, owing `owed` of the rounds
    /// before. Once.
    pub fn set_wire_from(&self, at: u64, owed: Vec<u8>) {
        let mut held = self.lock();
        assert!(held.wire_from.is_none(), "logkeeper: where the UART's carrier starts is said twice");
        held.wire_from = Some((at, owed.into()));
        drop(held);
        self.shared.grew.notify_all();
    }

    /// Where a carrier of the UART starts, once the rounds have said, given
    /// the replay offset `resumed` its channel had taken or passed over
    /// before: there, or where the kernel left the UART with what the rounds
    /// before that owe it, which a channel that took less is handed again.
    pub fn wire_start(&self, resumed: u64) -> (u64, Option<Arc<[u8]>>) {
        let mut held = self.lock();
        loop {
            if let Some((from, owed)) = &held.wire_from {
                return if resumed >= *from { (resumed, None) } else { (*from, Some(Arc::clone(owed))) };
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

    /// What a file at the replay's offset `at` is owed next: at most `max`
    /// bytes of whole lines, or one line longer than that, or the gap the
    /// replay let go of. Never waits.
    pub fn owed(&self, at: u64, max: usize) -> Owed {
        match self.lock().replay.next(at, max) {
            Next::Bytes(bytes) => Owed::Text(bytes.to_vec()),
            Next::Evicted { lost, at } => Owed::Gap { lost, at },
            Next::CaughtUp => Owed::Nothing,
        }
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
                    return Fed::Gap(evicted(self.shared.boot_secs, lost));
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
    /// The line that says what the replay no longer holds, which the carrier
    /// is past: its channel speaks for none of what follows, whose rounds the
    /// skipped bytes are part of.
    Gap(Vec<u8>),
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
fn admit(shared: &Arc<Shared>, who: String, pipe: Pipe) {
    let count = &shared.readers;
    if count.fetch_add(1, Ordering::SeqCst) >= MAX_READERS {
        count.fetch_sub(1, Ordering::SeqCst);
        say!("logkeeper: refusing {who}: {MAX_READERS} readers on this machine are already served");
        return;
    }
    let (waker, bell) = match wake::pair() {
        Ok(ends) => ends,
        Err(e) => {
            count.fetch_sub(1, Ordering::SeqCst);
            say!("logkeeper: no wake for {who}: {e:?}");
            return;
        }
    };
    // Before the thread reads the replay, so no growth after its first look
    // goes unrung.
    shared.reader_wakes.lock().expect("logkeeper: a reader's wake is poisoned").push(waker);
    let theirs = Arc::clone(shared);
    let spawned = std::thread::Builder::new().name("log-reader".into()).spawn(move || {
        let (sent, left) = feed(&theirs, PipeSink::new(pipe, bell));
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
fn feed(shared: &Shared, mut sink: PipeSink) -> (u64, Left) {
    let mut at = 0u64;
    let mut sent = 0u64;
    loop {
        let Some(chunk) = next(shared, &mut at) else {
            if !sink.await_more() {
                return (sent, Left::Gone);
            }
            continue;
        };
        match sink.write_all(&chunk) {
            Ok(()) => sent += chunk.len() as u64,
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => return (sent, Left::Stalled),
            Err(_) => return (sent, Left::Gone),
        }
    }
}

/// The bytes after `at`, at most [`CHUNK`] of them, or `None` where there are
/// none yet; `at` moves past them. A reader the replay no longer holds the
/// bytes of is handed the line that says so instead.
fn next(shared: &Shared, at: &mut u64) -> Option<Vec<u8>> {
    match shared.replay.lock().expect("logkeeper: the replay is poisoned").replay.next(*at, CHUNK) {
        Next::Bytes(bytes) => {
            *at += bytes.len() as u64;
            Some(bytes.to_vec())
        }
        Next::Evicted { lost, at: resume } => {
            *at = resume;
            Some(evicted(shared.boot_secs, lost))
        }
        Next::CaughtUp => None,
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

/// The tokens of a reader's watches: room in its pipe, the replay grown, and
/// its reader gone.
const ROOM: u64 = 0;
const MORE: u64 = 1;
const GONE: u64 = 2;

/// A reader's pipe as a byte sink: one `write` takes what fits, and a full
/// pipe is waited on for [`STALLED`] and no longer. Its watches are one-shot,
/// so each is armed again only once it has answered.
struct PipeSink {
    pipe: Pipe,
    /// Rung as the replay grows.
    more: Bell,
    poller: Poller,
    armed: [bool; 3],
    gone: bool,
}

impl PipeSink {
    fn new(pipe: Pipe, more: Bell) -> Self {
        Self { pipe, more, poller: Poller::new(3), armed: [false; 3], gone: false }
    }

    fn arm(&mut self, token: u64) {
        if !std::mem::replace(&mut self.armed[token as usize], true) {
            match token {
                ROOM => self.poller.watch(&self.pipe, WRITABLE, ROOM),
                MORE => self.poller.watch(&self.more, READABLE, MORE),
                _ => self.poller.watch(&self.pipe, OTHER_END_GONE, GONE),
            }
        }
    }

    /// Wait at most `timeout_nanos` for any armed watch; the tokens answered.
    fn wait(&mut self, timeout_nanos: u64) -> Vec<u64> {
        self.arm(GONE);
        let mut answered = Vec::new();
        self.poller.wait(1, timeout_nanos, |token| answered.push(token));
        for &token in &answered {
            self.armed[token as usize] = false;
            self.gone |= token == GONE;
        }
        answered
    }

    /// Wait until the replay may have grown: `false` once the reader is gone.
    fn await_more(&mut self) -> bool {
        loop {
            if self.gone {
                return false;
            }
            if self.more.take() {
                return true;
            }
            self.arm(MORE);
            self.wait(u64::MAX);
        }
    }
}

impl Write for PipeSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let until = std::time::Instant::now() + STALLED;
        loop {
            match self.pipe.write_nonblock(buf) {
                Ok(n) => return Ok(n),
                Err(SyscallError::WouldBlock) => {}
                Err(e) => return Err(std::io::Error::other(format!("{e:?}"))),
            }
            let left = until.saturating_duration_since(std::time::Instant::now());
            if self.gone {
                return Err(std::io::ErrorKind::BrokenPipe.into());
            }
            if left.is_zero() {
                return Err(std::io::ErrorKind::TimedOut.into());
            }
            self.arm(ROOM);
            self.wait(left.as_nanos() as u64);
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
            Fed::Said(_) | Fed::Gap(_) => panic!("lines that are not the replay's"),
        }
    }

    fn at(positions: &[u64]) -> Option<Positions> {
        Some(positions.into())
    }

    /// A channel that has taken the replay up to an offset has spoken the
    /// kernel's log through the positions of the last round wholly before it,
    /// and nothing of a round it has only begun.
    #[test]
    fn what_a_channel_took_speaks_for_the_rounds_it_took_whole() {
        let hub = Hub::start(1 << 20, None);
        hub.append(b"a\nb\n", at(&[3, 1]));
        hub.append(b"c\n", at(&[3, 2]));
        hub.resume(b"held\n");
        assert_eq!(hub.positions_through(0), None);
        assert_eq!(hub.positions_through(3), None, "half the first round is none of it");
        assert_eq!(hub.positions_through(4), at(&[3, 1]));
        assert_eq!(hub.positions_through(6), at(&[3, 2]));
        assert_eq!(hub.positions_through(11), at(&[3, 2]), "lines with no round speak for nothing");
    }

    /// The UART's carrier starts where the kernel left the UART, owing the
    /// rounds before what the kernel did not put on it, and is handed what
    /// was said while the machine stopped once it has the replay; of a refused
    /// stop's lines, those it took as they were said reach it again through
    /// the replay the file takes them into
    /// (`issues/the-console-loses-what-its-carrier-had-not-delivered.md`).
    #[test]
    fn the_wire_starts_after_the_kernel_and_takes_the_stops_lines_last() {
        let hub = Hub::start(1 << 20, None);
        hub.append(b"kernel 1\nprogram 1\nkernel 2\n", at(&[2]));
        hub.set_wire_from(28, b"program 1\n".to_vec());
        hub.append(b"program 2\n", at(&[2]));
        let (mut from, owed) = hub.wire_start(0);
        assert_eq!((from, owed.as_deref()), (28, Some(&b"program 1\n"[..])));
        assert_eq!(replay(hub.next_for_a_carrier(&mut from, true)), b"program 2\n");
        hub.append_stopping(b"stopping\n");
        match hub.next_for_a_carrier(&mut from, true) {
            Fed::Said(bytes) => assert_eq!(bytes, b"stopping\n"),
            _ => panic!("the replay where the stop's lines were owed"),
        }
        hub.append_stopping(b"again\n");
        hub.resume(b"stopping\nagain\n");
        assert_eq!(replay(hub.next_for_a_carrier(&mut from, true)), b"stopping\nagain\n");
        let mut port = 0;
        assert_eq!(replay(hub.next_for_a_carrier(&mut port, false)).len(), 53, "the port is fed from the first line");
    }

    /// **A UART carrier started again resumes where its channel left off**,
    /// never from the hand-over: a channel that had taken the rounds the
    /// kernel left it owes nothing of them again, and one that had not is
    /// handed them again.
    #[test]
    fn a_uart_carrier_started_again_resumes_where_its_channel_left_off() {
        let hub = Hub::start(1 << 20, None);
        hub.append(b"kernel\nprogram\n", at(&[1]));
        hub.set_wire_from(15, b"program\n".to_vec());
        hub.append(b"later\n", at(&[1]));
        assert_eq!(hub.wire_start(21), (21, None), "a channel that took the rounds after resumes after them");
        assert_eq!(hub.wire_start(15), (15, None));
        assert_eq!(hub.wire_start(3).1.as_deref(), Some(&b"program\n"[..]), "one that took less owes them again");
    }
}
