//! The far end of `netd_tcp`'s connections: a server on this host's own TCP
//! stack, which the guest reaches at `10.0.2.2` through QEMU's user network.
//! **The guest's TCP peer on the wire is slirp's**, a BSD-derived stack that
//! relays each connection onto a socket of this host's; neither shares code
//! with smoltcp. This side is the oracle the guest's client is judged against:
//! it hashes exactly the bytes its sockets carried, and a test compares that
//! with the hash the guest printed over its side of the stream.
//!
//! A connection opens with the guest's request — a mode byte, then the length
//! and the seed of the stream, each eight little-endian bytes — and is served
//! on a thread of its own. Every connection's outcome is kept, in the order
//! they were accepted, for [`Peer::finish`] to hand back.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use sha2::{Digest, Sha256};

/// The modes a request names, the other half of `netd_tcp`'s constants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Send `len` bytes of the stream `seed` names, then FIN.
    Download,
    /// Read to the end of the stream, then answer its length and hash, then FIN.
    Upload,
    /// Send `len` bytes, wait for the guest's one byte saying they arrived,
    /// then end the connection with a reset.
    Reset,
    /// Read nothing and send nothing until [`Peer::finish`].
    Hold,
    /// Send `len` bytes of [`stream_byte`]'s pattern, then FIN: a stream the
    /// guest can recognise in its receive ring before it reads it.
    Pattern,
}

impl Mode {
    fn of(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Self::Download),
            1 => Some(Self::Upload),
            2 => Some(Self::Reset),
            3 => Some(Self::Hold),
            4 => Some(Self::Pattern),
            _ => None,
        }
    }
}

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
    /// Whether the connection ended as its mode says it should.
    pub ended: Result<(), String>,
}

/// Longest any step of a connection may stall: the guest's own run bound.
pub const STALL: Duration = Duration::from_secs(120);

pub struct Peer {
    pub port: u16,
    stop: Arc<AtomicBool>,
    /// Connections accepted and not yet ended, a held one left out once it
    /// is known to be held: what [`Peer::settle`] waits on.
    open: Arc<(Mutex<usize>, Condvar)>,
    /// Dropped by [`Peer::finish`], which is what ends every held connection.
    release: mpsc::Sender<()>,
    acceptor: JoinHandle<Vec<JoinHandle<Result<Served, String>>>>,
}

impl Peer {
    pub fn start() -> Result<Self, String> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| format!("bind the host peer: {e}"))?;
        let port = listener.local_addr().map_err(|e| format!("the host peer's port: {e}"))?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_seen = stop.clone();
        let (release, released) = mpsc::channel::<()>();
        let released = Arc::new(Mutex::new(released));
        let open = Arc::new((Mutex::new(0usize), Condvar::new()));
        let counted = open.clone();
        let acceptor = thread::spawn(move || {
            let mut served = Vec::new();
            for stream in listener.incoming() {
                if stop_seen.load(Ordering::Acquire) {
                    break;
                }
                let released = released.clone();
                let open = Open::count(&counted);
                served.push(thread::spawn(move || {
                    let stream = stream.map_err(|e| format!("accept: {e}"))?;
                    serve(stream, &released, open)
                }));
            }
            served
        });
        Ok(Self { port, stop, open, release, acceptor })
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

    /// Stop accepting, let every held connection go, and answer how each
    /// connection went, in the order they were accepted. Waits for every
    /// connection to reach its end, each bounded by [`STALL`].
    pub fn finish(self) -> Vec<Result<Served, String>> {
        self.stop.store(true, Ordering::Release);
        // The wake for `accept`.
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        drop(self.release);
        let served = self.acceptor.join().expect("the host peer's acceptor panicked");
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

/// Byte at absolute stream position `pos`, the guest's `netd_stream::stream_byte`:
/// every aligned 16-byte group carries its own index.
fn stream_byte(pos: u64) -> u8 {
    let group = (pos >> 4) as u32;
    match pos & 15 {
        k @ 0..=3 => (group >> (8 * k)) as u8,
        _ => 0xC3,
    }
}

fn hex(digest: &[u8]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
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

fn serve(mut stream: TcpStream, released: &Mutex<mpsc::Receiver<()>>, mut open: Open) -> Result<Served, String> {
    stream.set_read_timeout(Some(STALL)).map_err(|e| format!("read timeout: {e}"))?;
    stream.set_write_timeout(Some(STALL)).map_err(|e| format!("write timeout: {e}"))?;
    let mut request = [0u8; 17];
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
        Mode::Download | Mode::Reset | Mode::Pattern => {
            let mut source = Stream::new(seed);
            let mut buf = vec![0u8; 65536];
            while served.bytes < len {
                let n = buf.len().min((len - served.bytes) as usize);
                match mode {
                    Mode::Pattern => {
                        for (i, b) in buf[..n].iter_mut().enumerate() {
                            *b = stream_byte(served.bytes + i as u64);
                        }
                    }
                    _ => source.fill(&mut buf[..n]),
                }
                if let Err(e) = stream.write_all(&buf[..n]) {
                    served.ended = Err(format!("send at {} of {len}: {e}", served.bytes));
                    break;
                }
                hash.update(&buf[..n]);
                served.bytes += n as u64;
            }
            if served.ended.is_ok() && mode == Mode::Reset {
                served.ended = reset_after_ack(stream);
            } else if served.ended.is_ok() {
                served.ended = stream.shutdown(std::net::Shutdown::Write).map_err(|e| format!("FIN: {e}"));
            }
        }
        Mode::Upload => {
            let mut buf = vec![0u8; 65536];
            loop {
                match stream.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        hash.update(&buf[..n]);
                        served.bytes += n as u64;
                    }
                    Err(e) => {
                        served.ended = Err(format!("read at {}: {e}", served.bytes));
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
        Mode::Hold => {
            open.end();
            // Held until `finish` drops the sender; a poisoned lock is another
            // held connection's panic, which that one reports.
            let _ = released.lock().map(|r| r.recv());
        }
    }
    served.sha = hex(&hash.finalize());
    Ok(served)
}

/// Wait for the guest's byte saying every byte arrived, then close with
/// `SO_LINGER` at zero, which ends the connection with a reset rather than a
/// FIN (RFC 793 §3.5's ABORT). Waiting first keeps the reset from discarding
/// stream bytes still in this host's send buffer.
fn reset_after_ack(mut stream: TcpStream) -> Result<(), String> {
    use std::os::fd::AsRawFd;
    let mut ack = [0u8; 1];
    stream.read_exact(&mut ack).map_err(|e| format!("read the guest's acknowledgement: {e}"))?;
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
        return Err(format!("SO_LINGER: {}", std::io::Error::last_os_error()));
    }
    drop(stream);
    Ok(())
}
