//! soundserver IPC protocol and slot-ring shared memory audio streaming.

use core::sync::atomic::Ordering;
use toyos_abi::audio::AudioSlotHeader;
use crate::ipc::{self, FrameRx, IpcError};
use crate::shm::SharedMemory;

pub const MSG_STREAM_OPEN: u32 = 1;
pub const MSG_STREAM_OPENED: u32 = 2;
pub const MSG_STREAM_SET_VOLUME: u32 = 3;
pub const MSG_STREAM_CLOSE: u32 = 4;
/// soundserver rejected `MSG_STREAM_OPEN` (unsupported format/channels/rate).
pub const MSG_STREAM_ERROR: u32 = 5;
/// On a connection that carries no stream: ask what device soundserver runs.
/// Answered once with [`MSG_DEVICE_DESCRIBED`] by a write that does not wait,
/// and the connection is closed.
pub const MSG_DEVICE_DESCRIBE: u32 = 6;
/// [`DeviceDescription`].
pub const MSG_DEVICE_DESCRIBED: u32 = 7;

/// On soundserver's `volume` port, which the compositor alone holds: move the
/// master level by [`VolumeStep::delta`] positions. A step while muted unmutes.
pub const MSG_VOLUME_STEP: u32 = 1;
/// On the `volume` port: mute, or unmute at the level it had. A bare header.
pub const MSG_VOLUME_MUTE: u32 = 2;
/// The answer to either: [`Volume`], read back from the device after the set.
pub const MSG_VOLUME: u32 = 3;
/// The answer to either where the device has no master, or a set did not
/// read back: a bare header.
pub const MSG_VOLUME_UNAVAILABLE: u32 = 4;

/// The only sample format currently implemented end-to-end.
pub const FORMAT_S16LE: u16 = 0;

crate::ipc_payload! {
    pub struct StreamOpenRequest {
        pub sample_rate: u32,
        pub channels: u16,
        pub format: u16,
    }

    /// What the ring is shaped like. **The ring itself and the signal
    /// arrive as handles**, sent with this frame — see
    /// [`STREAM_OPENED_HANDLES`].
    pub struct StreamOpenResponse {
        pub client_period_frames: u32,
        pub client_period_bytes: u32,
        pub device_sample_rate: u32,
        pub device_channels: u16,
        pub slot_count: u16,
        /// The device frames soundserver fades a closed stream out over, so a
        /// client can derive how long it may take to let go of one.
        pub fade_frames: u32,
    }

    /// The device soundserver runs, and the client rates it takes: a client at
    /// any of them is resampled to the device's rate.
    pub struct DeviceDescription {
        pub sample_rate: u32,
        pub period_frames: u32,
        pub min_client_rate: u32,
        pub max_client_rate: u32,
        pub channels: u16,
        pub _pad: u16,
    }

    pub struct VolumeStep {
        pub delta: i32,
    }

    /// The master as the device reads back: `position` positions below the
    /// top, of `bottom`, and whether it is muted (`1`) or not (`0`).
    pub struct Volume {
        pub position: u8,
        pub bottom: u8,
        pub muted: u8,
        pub _pad: u8,
    }

    pub struct StreamSetVolume {
        pub gain: f32,
    }
}

/// The two handles `MSG_STREAM_OPENED` is sent with: the slot ring, then the
/// wait side of the event soundserver signals on.
///
/// **Both travel soundserver → client**, which is the direction that makes a dead
/// client detectable: the wait side goes with the client's table and
/// soundserver's next signal answers `Gone`, by construction rather than by
/// bookkeeping.
pub const STREAM_OPENED_HANDLES: usize = 2;

pub struct AudioSlotWriter {
    shm: SharedMemory,
    period_bytes: u32,
    slot_count: u32,
}

/// The next free slot, held until it is committed or dropped. Holding it
/// mutably borrows the writer, so a second one cannot be handed out meanwhile.
pub struct SlotWriteGuard<'a> {
    writer: &'a mut AudioSlotWriter,
    idx: u32,
}

impl SlotWriteGuard<'_> {
    /// Copy one period of `data` into the slot and publish it to soundserver.
    ///
    /// # Panics
    /// Unless `data` is one period long.
    pub fn commit(self, data: &[u8]) {
        let slot = self.idx % self.writer.slot_count;
        assert_eq!(data.len(), self.writer.period_bytes as usize, "audio: a slot filled with part of a period");
        self.writer.slot(slot).write(0, data);
        self.writer
            .header()
            .write_idx
            .store(self.idx.wrapping_add(1), Ordering::Release);
    }
}

impl AudioSlotWriter {
    pub fn new(shm: SharedMemory, period_bytes: u32, slot_count: u32) -> Self {
        Self { shm, period_bytes, slot_count }
    }

    fn header(&self) -> &AudioSlotHeader {
        unsafe { &*(self.shm.as_ptr() as *const AudioSlotHeader) }
    }

    fn slot(&self, slot_idx: u32) -> crate::shm::Bytes<'_> {
        slot_bytes(&self.shm, slot_idx, self.period_bytes)
    }

    /// Acquire the next free slot for writing. Returns None if the ring is full.
    pub fn begin_fill(&mut self) -> Option<SlotWriteGuard<'_>> {
        // Only this side writes write_idx; read_idx needs Acquire so the slot
        // data reads soundserver finished before releasing the slot are ordered.
        let w = self.header().write_idx.load(Ordering::Relaxed);
        let r = self.header().read_idx.load(Ordering::Acquire);
        if w.wrapping_sub(r) >= self.slot_count {
            return None;
        }
        Some(SlotWriteGuard { writer: self, idx: w })
    }
}

/// Slot `slot_idx`'s period of the ring.
fn slot_bytes(shm: &SharedMemory, slot_idx: u32, period_bytes: u32) -> crate::shm::Bytes<'_> {
    let offset = AudioSlotHeader::SIZE + slot_idx as usize * period_bytes as usize;
    shm.bytes().sub(offset, period_bytes as usize)
}

pub struct AudioSlotReader {
    shm: SharedMemory,
    period_bytes: u32,
    slot_count: u32,
}

impl AudioSlotReader {
    pub fn new(shm: SharedMemory, period_bytes: u32, slot_count: u32) -> Self {
        Self { shm, period_bytes, slot_count }
    }

    fn header(&self) -> &AudioSlotHeader {
        unsafe { &*(self.shm.as_ptr() as *const AudioSlotHeader) }
    }

    /// The oldest filled slot, or None if the ring is empty (underrun).
    ///
    /// The slot stays owned by soundserver — the client may not refill it — until
    /// [`SlotReadGuard::advance`] publishes the consumption. Advancing before
    /// the data is copied out lets a concurrently-filling client overwrite the
    /// slot mid-read (torn audio).
    pub fn peek(&self) -> Option<SlotReadGuard<'_>> {
        let h = self.header();
        let w = h.write_idx.load(Ordering::Acquire);
        let r = h.read_idx.load(Ordering::Relaxed);
        if w == r {
            return None;
        }
        Some(SlotReadGuard { reader: self, idx: r })
    }
}

/// Access to the oldest filled slot. Advancing consumes the guard, so an
/// advance without a successful peek is unrepresentable — and the release
/// uses the index captured at peek time, never re-reading header state the
/// untrusted client can scribble on (a hostile peer rewinding `write_idx`
/// must only garble its own stream, not abort soundserver).
pub struct SlotReadGuard<'a> {
    reader: &'a AudioSlotReader,
    idx: u32,
}

impl SlotReadGuard<'_> {
    /// Copy the slot's period out: the client may be writing it, whatever the
    /// protocol says.
    ///
    /// # Panics
    /// Unless `out` is one period long.
    pub fn read(&self, out: &mut [u8]) {
        assert_eq!(out.len(), self.reader.period_bytes as usize, "audio: a slot read into part of a period");
        let slot = self.idx % self.reader.slot_count;
        slot_bytes(&self.reader.shm, slot, self.reader.period_bytes).read(0, out);
    }

    /// Release the slot back to the client.
    pub fn advance(self) {
        self.reader
            .header()
            .read_idx
            .store(self.idx.wrapping_add(1), Ordering::Release);
    }
}

#[derive(Debug)]
pub enum AudioError {
    NotFound,
    /// soundserver rejected the requested format/channels/rate.
    Rejected,
    /// soundserver let go of the stream's signal (daemon exit or client removal).
    Disconnected,
    /// soundserver announced the stream and did not send the ring and the signal
    /// with it. Handles travel in the frame that names them, so a short
    /// batch is a protocol violation and never something to wait for.
    MissingHandles,
    /// The slot ring soundserver sent is refused ([`SharedMemory::lent`]).
    Lent(crate::shm::Lent),
    Ipc(IpcError),
    Protocol(u32),
}

pub struct AudioStream {
    control: crate::Connection,
    slot_writer: AudioSlotWriter,
    /// The period the fill callback writes, copied into each slot it commits.
    period: alloc::vec::Vec<u8>,
    signal: crate::wake::Bell,
    period_frames: u32,
    device_sample_rate: u32,
    device_channels: u16,
    slot_count: u32,
    fade_frames: u32,
}

/// What device soundserver runs, asked on a connection of its own before any
/// stream exists.
pub fn describe_device() -> Result<DeviceDescription, AudioError> {
    let control = AudioStream::connect_soundserver()?;
    control.signal(MSG_DEVICE_DESCRIBE).map_err(AudioError::Ipc)?;
    let mut rx = FrameRx::<{ core::mem::size_of::<DeviceDescription>() }>::new();
    let frame = rx.recv(&control).map_err(AudioError::Ipc)?;
    match frame.msg_type {
        MSG_DEVICE_DESCRIBED => ipc::decode_payload(rx.payload(frame.payload_len)).map_err(AudioError::Ipc),
        other => Err(AudioError::Protocol(other)),
    }
}

impl AudioStream {
    pub fn open(sample_rate: u32, channels: u16, format: u16) -> Result<Self, AudioError> {
        let control = Self::connect_soundserver()?;
        let req = StreamOpenRequest { sample_rate, channels, format };
        control.send(MSG_STREAM_OPEN, &req).map_err(AudioError::Ipc)?;

        let mut rx = FrameRx::<{ core::mem::size_of::<StreamOpenResponse>() }>::new();
        let frame = rx.recv(&control).map_err(AudioError::Ipc)?;
        let resp: StreamOpenResponse = match frame.msg_type {
            MSG_STREAM_OPENED => ipc::decode_payload(rx.payload(frame.payload_len)).map_err(AudioError::Ipc)?,
            MSG_STREAM_ERROR => return Err(AudioError::Rejected),
            other => return Err(AudioError::Protocol(other)),
        };

        let [shm, signal] = rx.take_handles::<STREAM_OPENED_HANDLES>().ok_or(AudioError::MissingHandles)?;
        let signal = crate::wake::Bell::from(signal);

        let slot_count = resp.slot_count as u32;
        let shm_size = AudioSlotHeader::SIZE + slot_count as usize * resp.client_period_bytes as usize;
        let shm = SharedMemory::lent(shm, shm_size)
            .map_err(AudioError::Lent)?;
        let slot_writer = AudioSlotWriter::new(shm, resp.client_period_bytes, slot_count);

        Ok(Self {
            control,
            slot_writer,
            period: alloc::vec![0; resp.client_period_bytes as usize],
            signal,
            period_frames: resp.client_period_frames,
            device_sample_rate: resp.device_sample_rate,
            device_channels: resp.device_channels,
            slot_count,
            fade_frames: resp.fade_frames,
        })
    }

    /// Block until soundserver signals, then fill all available ring slots via the
    /// callback. Each callback invocation receives one period-sized buffer.
    ///
    /// Returns `Err(Disconnected)` once soundserver has let go of the signal
    /// (it is gone or removed this client) — the caller must stop the stream,
    /// not retry.
    pub fn wait_and_fill(&mut self, mut callback: impl FnMut(&mut [u8])) -> Result<(), AudioError> {
        if self.signal.wait_for(u64::MAX).is_err() {
            return Err(AudioError::Disconnected);
        }
        while let Some(slot) = self.slot_writer.begin_fill() {
            callback(&mut self.period);
            slot.commit(&self.period);
        }
        Ok(())
    }

    pub fn period_frames(&self) -> u32 {
        self.period_frames
    }

    pub fn device_sample_rate(&self) -> u32 {
        self.device_sample_rate
    }

    pub fn device_channels(&self) -> u16 {
        self.device_channels
    }

    /// The periods of the stream's ring.
    pub fn slot_count(&self) -> u32 {
        self.slot_count
    }

    /// The device frames soundserver fades this stream out over once it closes.
    pub fn fade_frames(&self) -> u32 {
        self.fade_frames
    }

    pub fn set_volume(&self, gain: f32) -> Result<(), AudioError> {
        self.control.send(MSG_STREAM_SET_VOLUME, &StreamSetVolume { gain })
            .map_err(AudioError::Ipc)
    }

    pub fn close(&self) {
        let _ = self.control.signal(MSG_STREAM_CLOSE);
    }

    /// One connection to soundserver, through this process's own namespace.
    ///
    /// **There was a retry loop here and it is gone**, along with its twin in
    /// `net`. A `soundserver` connector is live from this process's first
    /// instruction, so `NotFound` now means the manifest did not give this
    /// program sound.
    fn connect_soundserver() -> Result<crate::Connection, AudioError> {
        crate::endow::service("soundserver").map_err(|_| AudioError::NotFound)
    }
}
