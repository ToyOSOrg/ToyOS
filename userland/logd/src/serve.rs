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
//! else wakes it but its own departure: there is no poll and no timer. A
//! network reader's departure is a second thread's blocking read on its own
//! clone of the connection — this protocol carries nothing the client ever
//! sends, so that read's only event is the peer closing it — which is how a
//! reader that reconnects on the client's own clock (a stream surviving a
//! netd swap) is not still counted against [`MAX_READERS`] once it is gone:
//! writing to it can wait for ever on a dead connection that never errors
//! while there is nothing new to write, and only that second thread notices.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use toyos::ipc::Connection;
use toyos::port::Acceptor;
use toyos::{AsHandle, Pipe};
use toyos_logstream::{Next, ProgramLine, Replay, Tag, LOGD, PORT, SERVED};

/// The most bytes one reader is handed per wake: bounds how long the replay's
/// lock is held for a copy, not how far a reader may fall behind.
const CHUNK: usize = 64 * 1024;

/// Readers at once. Each is a thread, and anybody on the network may be one,
/// so the count is bounded and a reader past it is refused by name.
const MAX_READERS: usize = 8;

/// The replay, and the wake a caught-up reader waits on.
pub struct Hub {
    shared: Arc<Shared>,
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
        if toyos::endow::namespace().is_some() {
            let network = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("log-serve-net".into())
                .spawn(move || serve_network(&network))
                .expect("logd: the network server's thread could not be started");
        }
        if let Some(acceptor) = local {
            let here = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("log-serve-local".into())
                .spawn(move || serve_local(&here, &acceptor))
                .expect("logd: the local server's thread could not be started");
        }
        Self { shared }
    }

    /// The lines the file has just taken, for every reader.
    pub fn append(&self, lines: &[u8]) {
        self.shared.replay.lock().expect("logd: the replay is poisoned").append(lines);
        self.shared.grew.notify_all();
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
fn serve_network(shared: &Arc<Shared>) {
    let Some(mut listener) = bind_port() else { return };
    loop {
        let (stream, peer) = match listener.accept() {
            Ok(pair) => pair,
            Err(e) => {
                say!("logd: the log's listener on port {PORT} failed ({e}); binding it again");
                let Some(again) = bind_port() else { return };
                listener = again;
                continue;
            }
        };
        // A clone for the departure watcher: it only ever reads, the feed
        // thread only ever writes, and each closes its own half when it ends.
        let watch = stream.try_clone().ok();
        admit(shared, format!("{peer}"), Announce::Yes, stream, watch);
    }
}

/// The listener on [`PORT`], or `None` once this machine has none to offer.
fn bind_port() -> Option<std::net::TcpListener> {
    match std::net::TcpListener::bind(("0.0.0.0", PORT)) {
        Ok(listener) => {
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
        admit(shared, format!("local reader {}", conn.as_handle().0), Announce::No, PipeSink(pipe), None);
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

/// A reader thread, where the count allows one. `watch` is a network reader's
/// own clone of its connection, read on a thread of its own so this reader's
/// departure while idle is noticed and not left counted for ever.
fn admit(
    shared: &Arc<Shared>,
    who: String,
    announce: Announce,
    sink: impl Write + Send + 'static,
    watch: Option<std::net::TcpStream>,
) {
    if shared.readers.fetch_add(1, Ordering::SeqCst) >= MAX_READERS {
        shared.readers.fetch_sub(1, Ordering::SeqCst);
        say!("logd: refusing {who}: {MAX_READERS} readers are already served");
        return;
    }
    if announce == Announce::Yes {
        say!("logd: serving this boot's log to {who}");
    }
    let gone = Arc::new(AtomicBool::new(false));
    if let Some(mut probe) = watch {
        let (theirs, gone) = (Arc::clone(shared), Arc::clone(&gone));
        let spawned = std::thread::Builder::new().name("log-reader-watch".into()).spawn(move || {
            // This protocol carries nothing a reader ever sends, so the one
            // event a read on it can end on is the reader closing its end —
            // never data, and never a timeout this holds none of.
            let mut byte = [0u8; 1];
            let _ = probe.read(&mut byte);
            gone.store(true, Ordering::SeqCst);
            theirs.grew.notify_all();
        });
        // A watcher that could not be started leaves this reader indistinguishable
        // from a live one until it next writes, same as before this existed.
        let _ = spawned;
    }
    let theirs = Arc::clone(shared);
    let spawned = std::thread::Builder::new().name("log-reader".into()).spawn(move || {
        let sent = feed(&theirs, sink, &gone);
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
/// until the reader goes or `gone` says it already has. Answers the bytes it
/// sent.
fn feed(shared: &Shared, mut sink: impl Write, gone: &AtomicBool) -> u64 {
    let mut at = 0u64;
    loop {
        let chunk = {
            let mut replay = shared.replay.lock().expect("logd: the replay is poisoned");
            loop {
                if gone.load(Ordering::SeqCst) {
                    return at;
                }
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
