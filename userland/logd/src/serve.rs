//! This boot's log, served to whoever asks for it: over TCP on
//! [`toyos_logstream::PORT`] through netd, and on this machine through the
//! [`toyos_logstream::SERVICE`] port, whose answer is the read end of a pipe.
//!
//! **Every reader gets the boot from its first line**, however late it
//! connects: the main loop appends each round's lines to one
//! [`toyos_logstream::Replay`] after the file has them, and a reader is a
//! thread with an offset into it. So the same text reaches `/log` and every
//! reader, in the same order, and nothing a reader does can reach the file: a
//! reader that stops taking bytes blocks its own thread and nothing else.
//!
//! **A reader that is caught up waits on the replay growing**, and nothing
//! else wakes it: there is no poll and no timer.
//!
//! **A swap of netd ([`toyos_logstream::CARRIER`]) ends every network reader's
//! connection without a word**, and a connection the netd being stopped
//! accepts ends the same way, so from init's line accepting one until its
//! word that the old netd is gone — started, failed, restored or gone — this
//! program holds no listener ([`Carrier`]). It closes the listener before it says so
//! ([`CARRIER_LEAVING`]), and netd has answered the close by then: a reader
//! that asks after reading that line is refused by the old netd or answered by
//! the next, and never admitted by the one being stopped.

use std::io::Write;
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use toyos::ipc::Connection;
use toyos::port::Acceptor;
use toyos::poller::{Poller, READABLE};
use toyos::{AsHandle, Pipe};
use toyos_abi::syscall::SyscallError;
use toyos_abi::RawHandle;
use toyos_logstream::{Next, ProgramLine, Replay, Tag, CARRIER_LEAVING, LOGD, PORT, SERVED};

/// The most bytes one reader is handed per wake: bounds how long the replay's
/// lock is held for a copy, not how far a reader may fall behind.
const CHUNK: usize = 64 * 1024;

/// Readers at once. Each is a thread, and anybody on the network may be one,
/// so the count is bounded and a reader past it is refused by name.
const MAX_READERS: usize = 8;

/// The replay, and the wake a caught-up reader waits on.
pub struct Hub {
    shared: Arc<Shared>,
    /// The network server's control pipe, where this program serves the
    /// network: one byte per [`Carrier`] word.
    carrier: Option<Pipe>,
}

/// init's word on a swap of netd, as the network server acts on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Carrier {
    /// init accepted the swap: the netd the listener is registered with is
    /// about to be stopped.
    Leaving,
    /// init has stopped it and started or restored another, or has none.
    Back,
}

impl Carrier {
    const LEAVING: u8 = b'L';
    const BACK: u8 = b'B';
}

struct Shared {
    replay: Mutex<Replay>,
    grew: Condvar,
    readers: AtomicUsize,
    /// The wall clock the boot started at, for a line a reader is owed.
    boot_local: Option<u64>,
}

impl Hub {
    /// Start serving: the network where the manifest gave this program netd,
    /// and this machine where it gave it the [`SERVICE`](toyos_logstream::SERVICE) port.
    pub fn start(cap: usize, boot_local: Option<u64>, local: Option<Acceptor>) -> Self {
        let shared = Arc::new(Shared {
            replay: Mutex::new(Replay::new(cap)),
            grew: Condvar::new(),
            readers: AtomicUsize::new(0),
            boot_local,
        });
        // A row with no `receives` gives this program no namespace, so no netd,
        // and no thread to learn so on: its exit would be a kernel record at a
        // time nothing orders, after a shutdown's last word included.
        let mut carrier = None;
        if toyos::endow::namespace().is_some() {
            let network = Arc::clone(&shared);
            let (told, tell) = toyos::pipe_pair().expect("logd: no pipe for the network server");
            carrier = Some(tell);
            std::thread::Builder::new()
                .name("log-serve-net".into())
                .spawn(move || serve_network(&network, &told))
                .expect("logd: the network server's thread could not be started");
        }
        if let Some(acceptor) = local {
            let here = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("log-serve-local".into())
                .spawn(move || serve_local(&here, &acceptor))
                .expect("logd: the local server's thread could not be started");
        }
        Self { shared, carrier }
    }

    /// The lines the file has just taken, for every reader.
    pub fn append(&self, lines: &[u8]) {
        self.shared.replay.lock().expect("logd: the replay is poisoned").append(lines);
        self.shared.grew.notify_all();
    }

    /// init's word on a swap of netd, for the network server.
    pub fn carrier(&self, word: Carrier) {
        let Some(tell) = &self.carrier else { return };
        let byte = match word {
            Carrier::Leaving => Carrier::LEAVING,
            Carrier::Back => Carrier::BACK,
        };
        match tell.write(&[byte]) {
            Ok(1) => {}
            // The server ended with no netd left to serve through, which it
            // said: nothing is listening to be told.
            Err(SyscallError::Gone) => {}
            other => panic!("logd: the network server's pipe refused a word: {other:?}"),
        }
    }
}

/// Accept readers on [`PORT`] for the life of the process.
///
/// A machine whose manifest gives this program no netd serves nothing on the
/// network, and says nothing about it: the row is the decision. A namespace
/// without netd in it ends this thread at once.
///
/// **A listener is a registration netd holds, and a failed accept is netd no
/// longer holding it** — netd was swapped or is gone. Asked again, the same
/// listener answers the same error at once and for ever, so it is dropped and
/// bound anew: through the same port, which init keeps open across a swap.
///
/// `told` carries init's words on a swap of netd ([`Hub::carrier`]): the
/// listener is closed on [`Carrier::Leaving`] and bound again on
/// [`Carrier::Back`]. Each wait is on either being ready, and a completion a
/// closed listener left behind is answered by a non-blocking accept.
fn serve_network(shared: &Arc<Shared>, told: &Pipe) {
    const TOLD: u64 = 0;
    const READY: u64 = 1;
    let Some(first) = bind_port() else { return };
    let mut listener = Some(first);
    let poller = Poller::new(2);
    loop {
        poller.watch(told, READABLE, TOLD);
        if let Some(listener) = &listener {
            poller.watch_raw(RawHandle(listener.as_raw_fd() as u32), READABLE, READY);
        }
        let (mut words, mut ready) = (false, false);
        poller.wait(1, u64::MAX, |token| match token {
            TOLD => words = true,
            READY => ready = true,
            other => unreachable!("logd: the network server watches no token {other}"),
        });
        if words {
            let mut said = [0u8; 16];
            let n = match told.read_nonblock(&mut said) {
                Ok(n) => n,
                Err(SyscallError::WouldBlock) => 0,
                Err(e) => panic!("logd: the network server's pipe refused a read: {e:?}"),
            };
            for &word in &said[..n] {
                match word {
                    // Closed before the line, and netd has answered the close
                    // when `drop` returns: the line is the proof.
                    Carrier::LEAVING => {
                        drop(listener.take());
                        say!("{CARRIER_LEAVING}");
                    }
                    Carrier::BACK if listener.is_none() => {
                        let Some(again) = bind_port() else { return };
                        listener = Some(again);
                    }
                    Carrier::BACK => {}
                    other => unreachable!("logd: the network server was told {other}"),
                }
            }
        }
        let Some(open) = listener.as_ref().filter(|_| ready) else { continue };
        match open.accept() {
            Ok((stream, peer)) => admit(shared, format!("{peer}"), Announce::Yes, stream),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => {
                say!("logd: the log's listener on port {PORT} failed ({e}); binding it again");
                let Some(again) = bind_port() else { return };
                listener = Some(again);
            }
        }
    }
}

/// The listener on [`PORT`], or `None` once this machine has none to offer.
fn bind_port() -> Option<std::net::TcpListener> {
    match std::net::TcpListener::bind(("0.0.0.0", PORT)) {
        Ok(listener) => {
            listener.set_nonblocking(true).expect("logd: a listener that cannot be asked without waiting");
            say!("logd: serving this boot's log on port {PORT}");
            Some(listener)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotConnected => None,
        Err(e) => {
            say!("logd: cannot serve this boot's log on port {PORT}: {e}");
            None
        }
    }
}

/// Accept readers on this machine: each gets the read end of a pipe of its
/// own, which the log is written into.
fn serve_local(shared: &Arc<Shared>, acceptor: &Acceptor) {
    loop {
        let conn = match acceptor.accept() {
            Ok(conn) => conn,
            Err(e) => {
                say!("logd: the local log service ended: {e:?}");
                return;
            }
        };
        let end = shared.replay.lock().expect("logd: the replay is poisoned").end();
        let Some(pipe) = hand_over(&conn, end) else { continue };
        admit(shared, format!("local reader {}", conn.as_handle().0), Announce::No, PipeSink(pipe));
    }
}

/// The read end goes to the reader with the boot's length so far, the write
/// end stays here; `None` where the reader is already gone.
fn hand_over(conn: &Connection, end: u64) -> Option<Pipe> {
    let (read, write) = match toyos::pipe_pair() {
        Ok(ends) => ends,
        Err(e) => {
            say!("logd: no pipe for a local reader: {e:?}");
            return None;
        }
    };
    match conn.try_send_with_handles(&[read.into_raw()], SERVED, &end) {
        Ok(()) => Some(write),
        Err(e) => {
            say!("logd: a local reader went before it was answered: {e:?}");
            None
        }
    }
}

/// Whether a reader's coming and going is a line in the log: a peer on the
/// network is somebody the machine's owner may want named, and a reader on
/// this machine is the console, which would draw the line about itself
/// beside its own prompt.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Announce {
    Yes,
    No,
}

/// A reader thread, where the count allows one.
fn admit(shared: &Arc<Shared>, who: String, announce: Announce, sink: impl Write + Send + 'static) {
    if shared.readers.fetch_add(1, Ordering::SeqCst) >= MAX_READERS {
        shared.readers.fetch_sub(1, Ordering::SeqCst);
        say!("logd: refusing {who}: {MAX_READERS} readers are already served");
        return;
    }
    if announce == Announce::Yes {
        say!("logd: serving this boot's log to {who}");
    }
    let theirs = Arc::clone(shared);
    let spawned = std::thread::Builder::new().name("log-reader".into()).spawn(move || {
        let sent = feed(&theirs, sink);
        theirs.readers.fetch_sub(1, Ordering::SeqCst);
        if announce == Announce::Yes {
            say!("logd: {who} stopped reading after {sent} bytes");
        }
    });
    if let Err(e) = spawned {
        shared.readers.fetch_sub(1, Ordering::SeqCst);
        say!("logd: no thread for a reader: {e}");
    }
}

/// Write the boot to `sink` from its first byte, then each round as it lands,
/// until the reader goes. Answers the bytes it sent.
fn feed(shared: &Shared, mut sink: impl Write) -> u64 {
    let mut at = 0u64;
    loop {
        let chunk = {
            let mut replay = shared.replay.lock().expect("logd: the replay is poisoned");
            loop {
                let next = match replay.next(at, CHUNK) {
                    Next::Bytes(bytes) => Some(Ok(bytes.to_vec())),
                    Next::Evicted { lost, at: resume } => Some(Err((lost, resume))),
                    Next::CaughtUp => None,
                };
                match next {
                    Some(Ok(bytes)) => break bytes,
                    Some(Err((lost, resume))) => {
                        at = resume;
                        break evicted(shared.boot_local, lost);
                    }
                    None => {
                        replay = shared.grew.wait(replay).expect("logd: the replay is poisoned");
                    }
                }
            }
        };
        if sink.write_all(&chunk).and_then(|()| sink.flush()).is_err() {
            return at;
        }
        at += chunk.len() as u64;
    }
}

/// The line a reader gets in place of what the replay no longer holds.
fn evicted(boot_local: Option<u64>, lost: u64) -> Vec<u8> {
    let at_ns = toyos_abi::syscall::clock_nanos();
    let text = format!("logd: the first {lost} bytes of this boot are no longer held here; /log has them");
    let tag = Tag::new(LOGD).expect("logd's own name is a tag");
    let stamp = crate::stamp(boot_local, at_ns);
    format!("{}\n", ProgramLine { stamp: &stamp, at_ns, tag, text: text.as_bytes() }).into_bytes()
}

/// A pipe as a byte sink: one `write` may take part of what it is handed.
struct PipeSink(Pipe);

impl Write for PipeSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf).map_err(|e| std::io::Error::other(format!("{e:?}")))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
