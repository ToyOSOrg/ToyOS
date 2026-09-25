//! The `log` port: `inspect`'s answer about where this boot's log is going.
//!
//! **On a thread of its own, so no reader can reach the loop that writes the
//! file.** The loop publishes what it knows once a round — a few relaxed
//! stores, and a lock only when the file it writes changes — and this thread
//! owns the acceptor, reads each reader's request without ever blocking on it,
//! answers in one non-blocking write and closes. A reader that connects and
//! says nothing costs a slot and [`HANDSHAKE_TIMEOUT`], never the log.

use std::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use toyos::ipc::{self, FrameRx, RxStep};
use toyos::poller::{Poller, READABLE};
use toyos::port::Acceptor;
use toyos::{AsHandle, Connection};
use toyos_inspect::Snapshot;

use crate::store::Volume;

const _: () = assert!(
    toyos_inspect::MAX_SNAPSHOT_BYTES == ipc::MAX_FRAME_LEN as usize,
    "a snapshot is one frame"
);

/// Readers accepted and not yet answered.
///
/// Policy: a reader asks once and goes, so this is how many may be mid-request
/// at once. Past it a connection is refused by name rather than queued.
const MAX_PENDING: usize = 8;

/// How long an accepted reader may take to send its request.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);

/// The poll token of the acceptor, clear of every handle's own value.
const ACCEPT: u64 = u64::MAX;

/// Where the file side of the loop stands.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum State {
    /// Every round written, flushed and published.
    Writing = 0,
    /// Every round answered, and slower than a log is worth.
    Degraded = 1,
    /// A round the volume would not start, carried into the next.
    Retrying = 2,
    /// No volume, or one given up on: the log is on the console only.
    ConsoleOnly = 3,
}

impl State {
    fn word(raw: u8) -> &'static str {
        match raw {
            0 => "writing",
            1 => "degraded",
            2 => "retrying",
            3 => "console-only",
            other => unreachable!("logd: volume state {other} was never published"),
        }
    }
}

/// The loop's side of the answer. One writer, the loop; one reader, this
/// module's thread.
pub struct Published {
    state: AtomicU8,
    /// The part being written, or 0 for none. Only ever changes with `path`.
    part: AtomicU32,
    path: Mutex<Option<String>>,
    bytes: AtomicU64,
    lost: AtomicU64,
    stream: bool,
}

impl Published {
    /// `stream` is whether this boot was asked to stream its log at all.
    pub fn new(stream: bool) -> Self {
        Self {
            state: AtomicU8::new(State::ConsoleOnly as u8),
            part: AtomicU32::new(0),
            path: Mutex::new(None),
            bytes: AtomicU64::new(0),
            lost: AtomicU64::new(0),
            stream,
        }
    }

    /// What the loop knows at the top of a round. `lost` is the records the
    /// kernel overwrote before this reader reached them.
    pub fn publish(&self, volume: Option<&Volume>, state: State, lost: u64) {
        self.state.store(state as u8, Ordering::Relaxed);
        self.lost.store(lost, Ordering::Relaxed);
        let part = volume.map_or(0, Volume::part);
        self.bytes.store(volume.map_or(0, Volume::bytes), Ordering::Relaxed);
        // The lock is taken when the file changes and at no other round.
        if self.part.swap(part, Ordering::Relaxed) != part {
            *self.path.lock().expect("the inspect thread does not panic holding this") =
                volume.map(Volume::path);
        }
    }

    fn snapshot(&self) -> Vec<u8> {
        let mut snap = Snapshot::new(toyos_inspect::LOG);
        snap.put("volume.state", State::word(self.state.load(Ordering::Relaxed)));
        let path = self.path.lock().expect("the loop does not panic holding this").clone();
        if let Some(path) = path {
            snap.put("volume.path", path);
            snap.put("volume.part", self.part.load(Ordering::Relaxed));
            snap.put("volume.bytes", self.bytes.load(Ordering::Relaxed));
        }
        snap.put("records.lost", self.lost.load(Ordering::Relaxed));
        snap.put("stream", if self.stream { "on" } else { "off" });
        snap.encode().unwrap_or_else(|why| panic!("logd: its snapshot: {why}"))
    }
}

/// A reader accepted and not yet answered.
struct Pending {
    conn: Connection,
    /// One byte kept, so a request that carries a payload is seen to.
    rx: FrameRx<1>,
    since: Instant,
}

/// Serve `inspect` on `acceptor` for the life of the process.
pub fn serve(acceptor: Acceptor, published: Arc<Published>) {
    std::thread::Builder::new()
        .name("log-inspect".into())
        .spawn(move || run(&acceptor, &published))
        .expect("logd: the inspect thread could not be started");
}

fn run(acceptor: &Acceptor, published: &Published) -> ! {
    let poller = Poller::new(1 + MAX_PENDING as u32);
    let mut pending: Vec<Pending> = Vec::new();
    loop {
        poller.watch(acceptor, READABLE, ACCEPT);
        for p in &pending {
            poller.watch(&p.conn, READABLE, u64::from(p.conn.as_handle().0));
        }
        let timeout =
            if pending.is_empty() { u64::MAX } else { HANDSHAKE_TIMEOUT.as_nanos() as u64 };
        let mut ready: Vec<u64> = Vec::new();
        poller.wait(1, timeout, |token| ready.push(token));

        let now = Instant::now();
        pending.retain(|p| {
            let alive = now.duration_since(p.since) < HANDSHAKE_TIMEOUT;
            if !alive {
                say!("logd: dropping inspect reader {} — it never sent its request", p.conn.as_handle().0);
            }
            alive
        });

        if ready.contains(&ACCEPT) {
            match acceptor.accept() {
                Ok(conn) if pending.len() >= MAX_PENDING => say!(
                    "logd: refusing inspect reader {} — {MAX_PENDING} are already waiting",
                    conn.as_handle().0
                ),
                Ok(conn) => pending.push(Pending { conn, rx: FrameRx::new(), since: now }),
                Err(e) => say!("logd: an inspect reader could not be accepted ({e:?})"),
            }
        }

        pending.retain_mut(|p| {
            if !ready.contains(&u64::from(p.conn.as_handle().0)) {
                return true;
            }
            match p.rx.pump(&p.conn) {
                RxStep::Idle => true,
                RxStep::Eof => false,
                RxStep::Frame { msg_type: toyos_inspect::MSG_INSPECT, payload_len: 0 } => {
                    if let Err(e) =
                        p.conn.try_send_bytes(toyos_inspect::MSG_SNAPSHOT, &published.snapshot())
                    {
                        say!("logd: inspect reader {} was not answered ({e:?})", p.conn.as_handle().0);
                    }
                    false
                }
                RxStep::Frame { .. } | RxStep::Malformed => {
                    say!(
                        "logd: dropping inspect reader {} — it sent something that is not a \
                         request",
                        p.conn.as_handle().0
                    );
                    false
                }
            }
        });
    }
}
