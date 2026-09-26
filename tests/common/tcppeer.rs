//! The host server behind every netd stream test: a server on this host's own
//! TCP stack, which the guest reaches at `10.0.2.2` through QEMU's user
//! network. **The guest's TCP peer on the wire is slirp's**, a BSD-derived
//! stack that relays each connection onto a socket of this host's; neither
//! shares code with smoltcp. This side is the oracle the guest's client is
//! judged against: it hashes exactly the bytes its sockets carried, and a test
//! compares that with what the guest saw of the same stream.
//!
//! A connection opens with the guest's request (`netd_ask`, which both sides
//! include) and is served on a thread of its own. Every connection's outcome
//! is kept, in the order they were accepted, for [`Peer::finish`] to hand
//! back.
//!
//! **Nothing here outlives [`Peer::finish`].** `accept` is woken by a
//! connection of the server's own, every held connection is let go, and every
//! connection this side dialled is shut down under its writer; the socket
//! timeouts bound only a harness that never reaches `finish`.

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use sha2::{Digest, Sha256};

#[path = "../toyos-rust-tests/src/netd_ask.rs"]
mod netd_ask;
pub use netd_ask::{Mode, LATE_SHUTDOWN_SEED, ORPHAN_RELEASED_SEED, READER_LEAVES_SEED};
use netd_ask::{stream_byte, REQUEST_LEN};

/// How one connection went, from this host's side.
#[derive(Debug)]
pub struct Served {
    pub mode: Mode,
    pub len: u64,
    pub seed: u64,
    /// Bytes this side sent, or read, of the stream.
    pub bytes: u64,
    /// SHA-256 of those bytes, as lowercase hex.
    pub sha: String,
    /// Whether the connection ended as its mode says it should, and the
    /// error's kind where it did not.
    pub ended: Result<(), (ErrorKind, String)>,
}

/// Longest any step of a connection may stall: the guest's own run bound.
pub const STALL: Duration = Duration::from_secs(120);

/// What every connection's thread shares.
struct Shared {
    /// Dropped by [`Peer::finish`], which is what ends every held connection.
    released: Mutex<mpsc::Receiver<()>>,
    /// Seeds a `Mode::Release` has named.
    seeds: (Mutex<Vec<u64>>, Condvar),
    /// Connections this side dialled, shut down by [`Peer::finish`].
    dialled: Mutex<Vec<TcpStream>>,
    /// The host port QEMU forwards to the guest, which `Mode::Dial` dials.
    forward: Option<u16>,
    /// The wire's switch, which `Mode::Dark` throws.
    dark: Option<Arc<AtomicBool>>,
}

pub struct Peer {
    pub port: u16,
    stop: Arc<AtomicBool>,
    /// Connections accepted and not yet ended, a held one left out once it
    /// is known to be held: what [`Peer::settle`] waits on.
    open: Arc<(Mutex<usize>, Condvar)>,
    release: mpsc::Sender<()>,
    shared: Arc<Shared>,
    acceptor: JoinHandle<Vec<JoinHandle<Result<Served, String>>>>,
}

impl Peer {
    /// `forward` is the host port QEMU forwards to the guest's
    /// `FORWARDED_PORT`, for [`Mode::Dial`]; `dark` the wire's switch, for
    /// [`Mode::Dark`].
    pub fn start(forward: Option<u16>, dark: Option<Arc<AtomicBool>>) -> Result<Self, String> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| format!("bind the host peer: {e}"))?;
        let port = listener.local_addr().map_err(|e| format!("the host peer's port: {e}"))?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_seen = stop.clone();
        let (release, released) = mpsc::channel::<()>();
        let shared = Arc::new(Shared {
            released: Mutex::new(released),
            seeds: (Mutex::new(Vec::new()), Condvar::new()),
            dialled: Mutex::new(Vec::new()),
            forward,
            dark,
        });
        let open = Arc::new((Mutex::new(0usize), Condvar::new()));
        let (counted, sharing) = (open.clone(), shared.clone());
        let acceptor = thread::spawn(move || {
            let mut served = Vec::new();
            for stream in listener.incoming() {
                if stop_seen.load(Ordering::Acquire) {
                    break;
                }
                let open = Open::count(&counted);
                let shared = sharing.clone();
                served.push(thread::spawn(move || {
                    let stream = stream.map_err(|e| format!("accept: {e}"))?;
                    serve(stream, &shared, open)
                }));
            }
            served
        });
        Ok(Self { port, stop, open, release, shared, acceptor })
    }

    /// Wait until every connection but the held ones has ended, and fail by
    /// name if `within` passes first. A guest that exits the moment its last
    /// write returns has left the rest of its stream to netd, and a machine
    /// stopped before netd sends it cuts the stream short on this side.
    pub fn settle(&self, within: Duration) -> Result<(), String> {
        let (count, ended) = &*self.open;
        let count = count.lock().expect("the open count");
        let (count, waited) = ended
            .wait_timeout_while(count, within, |open| *open > 0)
            .expect("the open count");
        if waited.timed_out() {
            return Err(format!("{} host connection(s) still open after {within:?}", *count));
        }
        Ok(())
    }

    /// Stop accepting, let every held connection go, end every dialled one,
    /// and answer how each connection went, in the order they were accepted.
    pub fn finish(self) -> Vec<Result<Served, String>> {
        self.stop.store(true, Ordering::Release);
        // The wake for `accept`.
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        drop(self.release);
        let served = self.acceptor.join().expect("the host peer's acceptor panicked");
        for stream in self.shared.dialled.lock().expect("the dialled list").iter() {
            // One half at a time: once the peer has sent its FIN, macOS refuses
            // `Both` whole (`ENOTCONN`) and shuts neither, leaving a writer
            // blocked. A half refused here is one already ended.
            let _ = stream.shutdown(std::net::Shutdown::Write);
            let _ = stream.shutdown(std::net::Shutdown::Read);
        }
        served
            .into_iter()
            .map(|t| t.join().unwrap_or_else(|_| Err("a host connection's thread panicked".to_string())))
            .collect()
    }
}

/// This side's bytes: xorshift64*, eight bytes a step, from `seed`. Nothing in
/// the guest regenerates them; the guest hashes what it received.
struct Stream(u64);

impl Stream {
    fn new(seed: u64) -> Self {
        Self(seed ^ 0x2545_F491_4F6C_DD1D | 1)
    }

    fn fill(&mut self, buf: &mut [u8]) {
        for chunk in buf.chunks_mut(8) {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            let z = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D);
            chunk.copy_from_slice(&z.to_le_bytes()[..chunk.len()]);
        }
    }
}

fn hex(digest: &[u8]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn failed(what: &str, e: std::io::Error) -> (ErrorKind, String) {
    (e.kind(), format!("{what}: {e}"))
}

/// One connection in [`Peer::settle`]'s count, until it is dropped or known
/// to be held.
struct Open(Option<Arc<(Mutex<usize>, Condvar)>>);

impl Open {
    fn count(open: &Arc<(Mutex<usize>, Condvar)>) -> Self {
        *open.0.lock().expect("the open count") += 1;
        Self(Some(open.clone()))
    }

    fn end(&mut self) {
        if let Some(open) = self.0.take() {
            *open.0.lock().expect("the open count") -= 1;
            open.1.notify_all();
        }
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        self.end();
    }
}

fn serve(mut stream: TcpStream, shared: &Shared, mut open: Open) -> Result<Served, String> {
    stream.set_read_timeout(Some(STALL)).map_err(|e| format!("read timeout: {e}"))?;
    stream.set_write_timeout(Some(STALL)).map_err(|e| format!("write timeout: {e}"))?;
    let mut request = [0u8; REQUEST_LEN];
    if let Err(e) = stream.read_exact(&mut request) {
        // The acceptor's own wake, and nothing else, connects and says nothing.
        return Err(format!("read the guest's request: {e}"));
    }
    let mode = Mode::of(request[0]).ok_or_else(|| format!("the guest asked for mode {}", request[0]))?;
    let len = u64::from_le_bytes(request[1..9].try_into().expect("eight bytes"));
    let seed = u64::from_le_bytes(request[9..].try_into().expect("eight bytes"));
    let mut served = Served { mode, len, seed, bytes: 0, sha: String::new(), ended: Ok(()) };
    let mut hash = Sha256::new();
    match mode {
        Mode::Download | Mode::Reset | Mode::Pattern | Mode::PatternHeld => {
            let mut source = Stream::new(seed);
            let mut buf = vec![0u8; 65536];
            while served.bytes < len {
                let n = buf.len().min((len - served.bytes) as usize);
                match mode {
                    Mode::Pattern | Mode::PatternHeld => {
                        for (i, b) in buf[..n].iter_mut().enumerate() {
                            *b = stream_byte(served.bytes + i as u64);
                        }
                    }
                    _ => source.fill(&mut buf[..n]),
                }
                if let Err(e) = stream.write_all(&buf[..n]) {
                    served.ended = Err(failed(&format!("send at {} of {len}", served.bytes), e));
                    break;
                }
                hash.update(&buf[..n]);
                served.bytes += n as u64;
            }
            if served.ended.is_ok() {
                served.ended = match mode {
                    Mode::Reset => reset_after_ack(stream),
                    Mode::PatternHeld => {
                        hold(shared, &mut open);
                        Ok(())
                    }
                    _ => stream.shutdown(std::net::Shutdown::Write).map_err(|e| failed("FIN", e)),
                };
            }
        }
        Mode::Release => {
            shared.seeds.0.lock().expect("the released seeds").push(seed);
            shared.seeds.1.notify_all();
        }
        Mode::Dark => {
            shared.dark.as_ref().ok_or("the guest asked for the dark and this boot has no wire")?.store(true, Ordering::Release);
        }
        Mode::Dial => {
            let forward = shared.forward.ok_or("the guest asked for a dial and this boot forwards no port")?;
            served.bytes = dial(forward, shared)?;
            served.ended = stream.shutdown(std::net::Shutdown::Write).map_err(|e| failed("FIN", e));
        }
        Mode::Upload | Mode::LateUpload => {
            if mode == Mode::LateUpload {
                let (named, waited) = shared
                    .seeds
                    .1
                    .wait_timeout_while(shared.seeds.0.lock().expect("the released seeds"), STALL, |s| {
                        !s.contains(&seed)
                    })
                    .expect("the released seeds");
                drop(named);
                if waited.timed_out() {
                    served.ended = Err((ErrorKind::TimedOut, format!("no release named seed {seed} within {STALL:?}")));
                    served.sha = hex(&hash.finalize());
                    return Ok(served);
                }
            }
            let mut buf = vec![0u8; 65536];
            loop {
                match stream.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        hash.update(&buf[..n]);
                        served.bytes += n as u64;
                    }
                    Err(e) => {
                        served.ended = Err(failed(&format!("read at {}", served.bytes), e));
                        break;
                    }
                }
            }
            let sha: [u8; 32] = hash.clone().finalize().into();
            if served.ended.is_ok() {
                let mut answer = served.bytes.to_le_bytes().to_vec();
                answer.extend_from_slice(&sha);
                // Refused where the guest dropped its end unread, which is not
                // this connection's verdict: what it read is.
                let _ = stream.write_all(&answer).and_then(|()| stream.shutdown(std::net::Shutdown::Write));
            }
        }
        Mode::Hold => hold(shared, &mut open),
    }
    served.sha = hex(&hash.finalize());
    Ok(served)
}

/// Hold the connection until [`Peer::finish`], out of [`Peer::settle`]'s
/// count. A poisoned lock is another held connection's panic, which that one
/// reports.
fn hold(shared: &Shared, open: &mut Open) {
    open.end();
    let _ = shared.released.lock().map(|r| r.recv());
}

/// Connect to the guest through `forward` and write until the connection is
/// refused, answering how many bytes it took first.
fn dial(forward: u16, shared: &Shared) -> Result<u64, String> {
    let mut dialled =
        TcpStream::connect(("127.0.0.1", forward)).map_err(|e| format!("dial the guest's forwarded port: {e}"))?;
    dialled.set_write_timeout(Some(STALL)).map_err(|e| format!("write timeout: {e}"))?;
    let kept = dialled.try_clone().map_err(|e| format!("keep the dialled connection: {e}"))?;
    shared.dialled.lock().expect("the dialled list").push(kept);
    let chunk = [0u8; 4096];
    let mut written = 0u64;
    loop {
        match dialled.write(&chunk) {
            Ok(0) => return Ok(written),
            Ok(n) => written += n as u64,
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                return Err(format!("the dialled connection took {written} bytes and then nothing for {STALL:?}"));
            }
            Err(_) => return Ok(written),
        }
    }
}

/// Wait for the guest's byte saying every byte arrived, then close with
/// `SO_LINGER` at zero, which ends the connection with a reset rather than a
/// FIN (RFC 793 §3.5's ABORT). Waiting first keeps the reset from discarding
/// stream bytes still in this host's send buffer.
fn reset_after_ack(mut stream: TcpStream) -> Result<(), (ErrorKind, String)> {
    use std::os::fd::AsRawFd;
    let mut ack = [0u8; 1];
    stream.read_exact(&mut ack).map_err(|e| failed("read the guest's acknowledgement", e))?;
    let linger = libc::linger { l_onoff: 1, l_linger: 0 };
    // SAFETY: a valid socket descriptor this function owns, and a pointer to
    // a live `linger` of the size passed.
    let set = unsafe {
        libc::setsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_LINGER,
            (&linger as *const libc::linger).cast(),
            std::mem::size_of::<libc::linger>() as libc::socklen_t,
        )
    };
    if set != 0 {
        return Err(failed("SO_LINGER", std::io::Error::last_os_error()));
    }
    drop(stream);
    Ok(())
}
