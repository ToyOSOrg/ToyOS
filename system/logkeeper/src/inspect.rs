//! The `log` port: `inspect`'s answer about where this boot's log is going,
//! and a reader on this machine handed the log itself ([`READ`]).
//!
//! **On a thread of its own, so no reader can reach the loop or the file.**
//! They publish what they know — a few relaxed stores, and a lock only when
//! the file changes — and this thread
//! owns the acceptor, reads each reader's request without ever blocking on it,
//! answers in one non-blocking write and closes. A reader that connects and
//! says nothing costs a slot of [`READERS`] for its limit, never the log.

use std::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use toyos::admission::{Admission, Event, Left, Policy};
use toyos::ipc;
use toyos::poller::{Poller, READABLE};
use toyos::port::Acceptor;
use toyos::say;
use toyos_inspect::Snapshot;

use toyos_logstream::READ;

use crate::serve::Hub;
use crate::store::Volume;

const _: () = assert!(
    toyos_inspect::MAX_SNAPSHOT_BYTES == ipc::MAX_FRAME_LEN as usize,
    "a snapshot is one frame"
);

/// Readers accepted and not yet asking: a reader asks once and goes. Any
/// holder of `log` may hold them all, since its connector names no holder.
const READERS: Policy = Policy::unshared(8);

/// The poll token of the acceptor, clear of every handle's own value.
const ACCEPT: u64 = u64::MAX;

/// Where the file stands.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum State {
    /// Every batch written and synced as it falls due.
    Writing = 0,
    /// The volume answers slower than a log is worth, or refused the part
    /// and is asked again: the replay keeps what the file is owed.
    Degraded = 1,
    /// No volume, or one given up on: the log is on the console only.
    ConsoleOnly = 2,
    /// The volume is not open yet: its file takes the boot whole once it is.
    Opening = 3,
}

impl State {
    fn word(raw: u8) -> &'static str {
        match raw {
            0 => "writing",
            1 => "degraded",
            2 => "console-only",
            3 => "opening",
            other => unreachable!("logkeeper: volume state {other} was never published"),
        }
    }
}

/// The answer's facts: the file's, published by the file's thread, and the
/// records lost, by the loop; one reader, this module's thread.
pub struct Published {
    state: AtomicU8,
    /// The part last published, or 0 for none: the file thread's own record
    /// of when `file` changes, and read by nothing else.
    part: AtomicU32,
    /// The part being written and its path, one pair under one lock, so no
    /// reader pairs one file's part number with another's path.
    file: Mutex<Option<(u32, String)>>,
    bytes: AtomicU64,
    lost: AtomicU64,
}

impl Published {
    pub fn new() -> Self {
        Self {
            state: AtomicU8::new(State::ConsoleOnly as u8),
            part: AtomicU32::new(0),
            file: Mutex::new(None),
            bytes: AtomicU64::new(0),
            lost: AtomicU64::new(0),
        }
    }

    /// Where the file stands, from the file's thread.
    pub fn publish(&self, volume: Option<&Volume>, state: State) {
        self.state.store(state as u8, Ordering::Relaxed);
        let part = volume.map_or(0, Volume::part);
        self.bytes.store(volume.map_or(0, Volume::bytes), Ordering::Relaxed);
        // The lock is taken when the file changes and at no other batch.
        if self.part.swap(part, Ordering::Relaxed) != part {
            *self.file.lock().expect("the inspect thread does not panic holding this") =
                volume.map(|v| (v.part(), v.path()));
        }
    }

    /// The records the kernel overwrote before the loop reached them.
    pub fn publish_lost(&self, lost: u64) {
        self.lost.store(lost, Ordering::Relaxed);
    }

    fn snapshot(&self) -> Vec<u8> {
        let mut snap = Snapshot::new(toyos_inspect::LOG);
        snap.put("volume.state", State::word(self.state.load(Ordering::Relaxed)));
        let file = self.file.lock().expect("the file's thread does not panic holding this").clone();
        if let Some((part, path)) = file {
            snap.put("volume.path", path);
            snap.put("volume.part", part);
            snap.put("volume.bytes", self.bytes.load(Ordering::Relaxed));
        }
        snap.put("records.lost", self.lost.load(Ordering::Relaxed));
        snap.encode().unwrap_or_else(|why| panic!("logkeeper: its snapshot: {why}"))
    }
}

/// Serve `inspect` and this machine's readers on `acceptor` for the life of
/// the process.
pub fn serve(acceptor: Acceptor, published: Arc<Published>, hub: Arc<Hub>) {
    std::thread::Builder::new()
        .name("log-inspect".into())
        .spawn(move || run(&acceptor, &published, &hub))
        .expect("logkeeper: the inspect thread could not be started");
}

fn run(acceptor: &Acceptor, published: &Published, hub: &Hub) -> ! {
    let poller = Poller::new(1 + READERS.slots as u32);
    let mut readers: Admission<(), (), Instant, 0> = Admission::new(READERS);
    let mut accept_armed = false;
    let mut ready: Vec<u64> = Vec::new();
    loop {
        if !accept_armed && readers.has_room() {
            poller.watch(acceptor, READABLE, ACCEPT);
            accept_armed = true;
        }
        readers.watch(|conn, slot| poller.watch(conn, READABLE, slot));
        let timeout = readers.left(Instant::now()).map_or(u64::MAX, |left| left.as_nanos() as u64);
        ready.clear();
        poller.wait(1, timeout, |token| ready.push(token));

        let now = Instant::now();
        for &token in &ready {
            if token != ACCEPT {
                readers.woke(token);
                continue;
            }
            accept_armed = false;
            // One past the table stays queued until a slot is free, and a
            // table with room and no shares refuses nobody. A reader the
            // kernel would not hand over is told by its connection closing.
            if let Some(Ok(conn)) = readers.accept(acceptor) {
                let _ = readers.admit(conn, (), (), now);
            }
        }
        while let Some(event) = readers.next(now) {
            match event {
                Event::Admitted(reader) => match reader.msg_type() {
                    toyos_inspect::MSG_INSPECT => {
                        let _ = reader.try_send_bytes(toyos_inspect::MSG_SNAPSHOT, &published.snapshot());
                    }
                    READ => hub.read(&reader.into_connection()),
                    _ => {}
                },
                // A reader that hung up or spoke another protocol is told by
                // its closing, and said of nowhere: every holder of `log`
                // would write into it at its own rate.
                Event::Left { why: why @ Left::Silent, say: Some(unsaid), .. } => {
                    say!("logkeeper: letting a reader go: {why}{unsaid}")
                }
                Event::Left { .. } => {}
            }
        }
    }
}
