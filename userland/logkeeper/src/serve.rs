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

use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use toyos::ipc::{Connection, TrySendError};
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

/// The replay, and the wake a caught-up reader waits on.
pub struct Hub {
    shared: Arc<Shared>,
}

struct Shared {
    replay: Mutex<Replay>,
    grew: Condvar,
    readers: AtomicUsize,
    /// The wall clock the boot started at, for a line a reader is owed.
    boot_secs: Option<u64>,
}

impl Hub {
    /// A reader is handed over by the `log` port's thread ([`Hub::read`]).
    pub fn start(cap: usize, boot_secs: Option<u64>) -> Self {
        let shared = Arc::new(Shared {
            replay: Mutex::new(Replay::new(cap)),
            grew: Condvar::new(),
            readers: AtomicUsize::new(0),
            boot_secs,
        });
        Self { shared }
    }

    /// A reader that asked for the log: it gets the read end
    /// of a pipe of its own, which the log is written into.
    pub fn read(&self, conn: &Connection) {
        let end = self.shared.replay.lock().expect("logkeeper: the replay is poisoned").end();
        let Some(pipe) = hand_over(conn, end) else { return };
        admit(&self.shared, format!("reader {}", conn.as_handle().0), PipeSink::new(pipe));
    }

    /// The lines the file has just taken, for every reader.
    pub fn append(&self, lines: &[u8]) {
        self.shared.replay.lock().expect("logkeeper: the replay is poisoned").append(lines);
        self.shared.grew.notify_all();
    }
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
    let sent = conn.send_handles([read.into()]).map_err(TrySendError::Syscall);
    match sent.and_then(|()| conn.try_send(SERVED, &end)) {
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
        let chunk = {
            let mut replay = shared.replay.lock().expect("logkeeper: the replay is poisoned");
            loop {
                match replay.next(at, CHUNK) {
                    Next::Bytes(bytes) => {
                        at += bytes.len() as u64;
                        break bytes.to_vec();
                    }
                    Next::Evicted { lost, at: resume } => {
                        at = resume;
                        break evicted(shared.boot_secs, lost);
                    }
                    Next::CaughtUp => {
                        replay = shared.grew.wait(replay).expect("logkeeper: the replay is poisoned");
                    }
                }
            }
        };
        match sink.write_all(&chunk) {
            Ok(()) => sent += chunk.len() as u64,
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => return (sent, Left::Stalled),
            Err(_) => return (sent, Left::Gone),
        }
    }
}

/// The line a reader gets in place of what the replay no longer holds.
fn evicted(boot_secs: Option<u64>, lost: u64) -> Vec<u8> {
    let at_ns = toyos_abi::clock::stamp_ns();
    let text = format!("logkeeper: the first {lost} bytes of this boot are no longer held here; /log has them");
    let tag = Tag::new(LOGKEEPER).expect("logkeeper's own name is a tag");
    let stamp = crate::stamp(boot_secs, Some(at_ns));
    let line = ProgramLine {
        stamp: &stamp,
        at_ns,
        severity: Severity::Warn,
        tid: 0,
        pid: None,
        tag,
        text: text.as_bytes(),
    };
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
