//! What `inspect` reads: the device soundd drives, and what the mix loop has
//! counted since soundd started.
//!
//! **The mix loop publishes and the control thread answers**, so a reader
//! costs the loop that feeds the device a handful of relaxed stores per wake
//! and never a lock, a syscall or a wait. The counters are the same
//! `toyos_mixer::MixStats` the console line reports every window, summed over
//! every window reported so far plus the one still open — no new measurement,
//! and the running total is what the console's windows add up to.

use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

use toyos_inspect::Snapshot;
use toyos_mixer::MixStats;

const _: () = assert!(
    toyos_inspect::MAX_SNAPSHOT_BYTES == toyos::ipc::MAX_FRAME_LEN as usize,
    "a snapshot is one frame"
);

/// What the loop is doing with the device.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub(crate) enum State {
    /// A device loop with the stream stopped: no client, nothing on the wire.
    Suspended = 0,
    /// A device loop whose stream is started.
    Running = 1,
    /// The null sink with no client.
    Idle = 2,
    /// The null sink draining at least one client.
    Streaming = 3,
}

impl State {
    fn word(raw: u8) -> &'static str {
        match raw {
            0 => "suspended",
            1 => "running",
            2 => "idle",
            3 => "streaming",
            other => unreachable!("soundd: stream state {other} was never published"),
        }
    }
}

/// The device, as it was when the loop started. Fixed for soundd's life.
pub(crate) struct Device {
    /// `virtio-sound`, `hda` or `null`.
    pub(crate) kind: &'static str,
    pub(crate) rate: u32,
    pub(crate) channels: u16,
    pub(crate) period_frames: u32,
    pub(crate) buffers: u32,
}

/// The sums of every window the console has reported.
#[derive(Default)]
pub(crate) struct Totals {
    underruns: u64,
    drains: u64,
    submitted: u64,
    late_wakes: u64,
}

impl Totals {
    /// Add a window the console has just reported, before it is reset.
    pub(crate) fn fold(&mut self, window: &MixStats) {
        self.underruns += u64::from(window.underruns);
        self.drains += u64::from(window.drains);
        self.submitted += u64::from(window.submitted);
        self.late_wakes += u64::from(window.late_wakes);
    }
}

/// The mix loop's side of the snapshot. One writer, the mix loop; one reader,
/// the control thread.
pub(crate) struct Published {
    state: AtomicU8,
    clients: AtomicU64,
    underruns: AtomicU64,
    drains: AtomicU64,
    submitted: AtomicU64,
    late_wakes: AtomicU64,
}

impl Published {
    pub(crate) fn new(state: State) -> Self {
        Self {
            state: AtomicU8::new(state as u8),
            clients: AtomicU64::new(0),
            underruns: AtomicU64::new(0),
            drains: AtomicU64::new(0),
            submitted: AtomicU64::new(0),
            late_wakes: AtomicU64::new(0),
        }
    }

    /// Where the loop is now: every reported window plus the open one.
    ///
    /// Relaxed, and deliberately: each word is a reading of its own, and a
    /// reader that took `underruns` from one wake and `submitted` from the next
    /// is reading two instants a period apart, which is what "now" is here.
    pub(crate) fn publish(&self, totals: &Totals, open: &MixStats, clients: usize, state: State) {
        self.state.store(state as u8, Ordering::Relaxed);
        self.clients.store(clients as u64, Ordering::Relaxed);
        self.underruns.store(totals.underruns + u64::from(open.underruns), Ordering::Relaxed);
        self.drains.store(totals.drains + u64::from(open.drains), Ordering::Relaxed);
        self.submitted.store(totals.submitted + u64::from(open.submitted), Ordering::Relaxed);
        self.late_wakes.store(totals.late_wakes + u64::from(open.late_wakes), Ordering::Relaxed);
    }
}

/// The encoded answer.
pub(crate) fn snapshot(device: &Device, published: &Published) -> Vec<u8> {
    let mut snap = Snapshot::new(toyos_inspect::SOUND);
    snap.put("device", device.kind);
    snap.put("rate_hz", device.rate);
    snap.put("channels", u32::from(device.channels));
    snap.put("period_frames", device.period_frames);
    snap.put("buffers", device.buffers);
    snap.put("stream.state", State::word(published.state.load(Ordering::Relaxed)));
    snap.put("stream.clients", published.clients.load(Ordering::Relaxed));
    snap.put("periods.submitted", published.submitted.load(Ordering::Relaxed));
    snap.put("periods.underruns", published.underruns.load(Ordering::Relaxed));
    snap.put("periods.drains", published.drains.load(Ordering::Relaxed));
    snap.put("wakes.late", published.late_wakes.load(Ordering::Relaxed));
    snap.encode().unwrap_or_else(|why| panic!("soundd: its snapshot: {why}"))
}
