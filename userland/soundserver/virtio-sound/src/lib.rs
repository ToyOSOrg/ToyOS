//! soundserver's virtio-sound driver: one output stream through the device's
//! three queues, every decision about them, and none of the instructions that
//! carry them out.
//!
//! Every `§` here is a section of *Virtual I/O Device (VIRTIO) Version 1.2*,
//! OASIS Committee Specification 01. [`wire`] is §5.14.6's messages; this
//! module is the queues they travel on, laid out in one grant, over
//! `toyos-virtio`'s split virtqueue and the [`Doorbell`] its live transport
//! rings.
//!
//! # The device is not trusted
//!
//! Every word it writes back — a used element, a status, a response, an
//! event — is input from outside the driver. What `toyos-virtio` bounds (a
//! head, a length no more than a chain may be written) it bounds; what is the
//! sound device's is bounded here, and one that fails is a [`Refusal`] by
//! name. §2.7.4 has a driver believe nothing past the `len` a used element
//! gives, so a status, a response or an event shorter than its structure is a
//! refusal and is not read.
//!
//! **A refusal is the end of the device's use**, as `toyos-virtio`'s are: one
//! at bring-up leaves soundserver on its null sink, and one after it ends
//! soundserver, since no conforming device writes it and every period after it
//! is one the mixer would count as played on the device's word.
//!
//! # What is not here
//!
//! - **One stream, stream 0.** The device numbers its streams and reports
//!   only how many, so there is nothing here to choose a second one by.
//! - **No feature of §5.14.6.6.2 is selected**, no channel map or jack is
//!   asked about, and the receive queue is never configured.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod wire;

#[cfg(test)]
mod tests;

use toyos_device_memory::{DmaBuffers, Registers};
use toyos_virtio::pci::Live;
use toyos_virtio::queue::{Buffer, Parts, Published, Used, UsedRefusal, Virtqueue};

use wire::{Event, Failed, Params, PcmInfo, Request};

/// The pipeline, in periods and bytes: 128 frames of 16-bit stereo each.
pub const PERIODS: usize = 8;
pub const PERIOD_BYTES: usize = 512;

/// The one stream this driver opens.
pub const STREAM_ID: u32 = 0;

/// Descriptors per queue, powers of two (§2.7). The transmit queue holds one
/// three-descriptor chain per period, its transfer header, its PCM and its
/// status; the control queue one request and its response; the event queue
/// one buffer per descriptor.
pub const TX_QUEUE_SIZE: u16 = 32;
pub const CONTROL_QUEUE_SIZE: u16 = 2;
pub const EVENT_QUEUE_SIZE: u16 = 8;
const TX_CHAIN: u16 = 3;
const EVENT_BUFS: u16 = EVENT_QUEUE_SIZE;

/// The longest response this driver asks for: a header and one stream's
/// information (§5.14.6.2 has the driver provide exactly that).
const RESPONSE_BYTES: u32 = wire::HDR_BYTES + wire::PCM_INFO_BYTES;
const REQUEST_BYTES: u32 = wire::SET_PARAMS_BYTES;

/// The grant's layout: the periods, then each queue on a page of its own with
/// the buffers its chains name.
const PCM: usize = 0x0000;
const TX_PARTS: Parts = Parts::contiguous(0x1000, TX_QUEUE_SIZE);
const TX_XFER: usize = 0x1800;
const TX_STATUS: usize = 0x1C00;
const CONTROL_PARTS: Parts = Parts::contiguous(0x2000, CONTROL_QUEUE_SIZE);
const REQUEST: usize = 0x2800;
const RESPONSE: usize = 0x2C00;
const EVENT_PARTS: Parts = Parts::contiguous(0x3000, EVENT_QUEUE_SIZE);
const EVENTS: usize = 0x3800;
pub const GRANT_BYTES: usize = 0x4000;

const _: () = {
    assert!(PCM + PERIODS * PERIOD_BYTES <= TX_PARTS.desc);
    assert!(TX_PARTS.end(TX_QUEUE_SIZE) <= TX_XFER);
    assert!(TX_XFER + PERIODS * wire::XFER_BYTES as usize <= TX_STATUS);
    assert!(TX_STATUS + PERIODS * wire::STATUS_BYTES as usize <= CONTROL_PARTS.desc);
    assert!(CONTROL_PARTS.end(CONTROL_QUEUE_SIZE) <= REQUEST);
    assert!(REQUEST + REQUEST_BYTES as usize <= RESPONSE);
    assert!(RESPONSE + RESPONSE_BYTES as usize <= EVENT_PARTS.desc);
    assert!(EVENT_PARTS.end(EVENT_QUEUE_SIZE) <= EVENTS);
    assert!(EVENTS + EVENT_BUFS as usize * wire::EVENT_BYTES as usize <= GRANT_BYTES);
    assert!(PERIODS * TX_CHAIN as usize <= TX_QUEUE_SIZE as usize);
    assert!(PERIODS <= u32::BITS as usize);
    // §5.14.6.6.3.2: `buffer_bytes % period_bytes == 0`, and every period
    // whole 16-bit stereo frames.
    assert!(PERIOD_BYTES % 4 == 0);
};

/// The rates this driver can encode, best first. 44100 leads because it is
/// what the mixer, the resampler and the recorded counters are sized against;
/// 48000 is the one every other device offers.
const RATES: [(u32, u8); 2] = [(44100, wire::RATE_44100), (48000, wire::RATE_48000)];

/// How many times a control command's completion is polled before the device
/// is called silent, and the spins between two looks.
///
/// **A count and not a deadline**: a wall clock keeps running while this guest
/// is not, so a duration here would measure the host's scheduler and call a
/// healthy device gone. Policy, not physics — the specification has no number,
/// and a device answering at all answers in one round trip.
const CONTROL_POLLS: u32 = 100_000;
const SPINS_PER_POLL: u32 = 256;

/// Where a chain made available is told to the device: the live transport.
pub trait Doorbell {
    fn ring(&self, published: Published);
}

impl<R: Registers> Doorbell for Live<R> {
    fn ring(&self, published: Published) {
        self.notify(published);
    }
}

/// Why the device is not driven. Each is something it answered, or did not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Its configuration says it has no PCM stream (§5.14.4).
    NoStream,
    /// A used ring `toyos-virtio` did not believe.
    Used(u16, UsedRefusal),
    /// It never answered a control request.
    Silent(Request),
    /// It answered fewer bytes than the response's structure.
    ShortAnswer { request: Request, written: u32, wanted: u32 },
    /// It answered a status §5.14.6 does not define.
    UnknownStatus(Request, u32),
    /// It answered, and said no.
    Rejected(Request, Failed),
    /// Stream 0 does not carry output (§5.14.6.6.2).
    NotOutput { direction: u8 },
    /// It offers no format, rate or channel count this driver writes.
    NoFormat { formats: u64 },
    NoRate { rates: u64 },
    NoChannels { min: u8, max: u8 },
    /// A period's status is shorter than its structure.
    ShortStatus { period: usize, written: u32 },
    /// A period's status is not `S_OK` (§5.14.6.8), or no status at all.
    PeriodFailed { period: usize, status: Result<Failed, u32> },
    /// An event is shorter than its structure.
    ShortEvent { written: u32 },
}

impl core::fmt::Display for Refusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoStream => write!(f, "its configuration offers no PCM stream"),
            Self::Used(queue, why) => write!(f, "its queue {queue}'s used ring: {why}"),
            Self::Silent(request) => write!(f, "it never answered {}", request.name()),
            Self::ShortAnswer { request, written, wanted } => write!(
                f,
                "it answered {} with {written} byte(s), where the response is {wanted}",
                request.name()
            ),
            Self::UnknownStatus(request, word) => {
                write!(f, "it answered {} with the status {word:#x}, which is none", request.name())
            }
            Self::Rejected(request, failed) => {
                write!(f, "it refused {} ({})", request.name(), failed.name())
            }
            Self::NotOutput { direction } => {
                write!(f, "its stream {STREAM_ID} has direction {direction}, not output")
            }
            Self::NoFormat { formats } => write!(
                f,
                "it offers the formats {formats:#x}, without S16 (bit {})",
                wire::FMT_S16
            ),
            Self::NoRate { rates } => write!(
                f,
                "it offers the rates {rates:#x}, without 44100 (bit {}) or 48000 (bit {})",
                wire::RATE_44100,
                wire::RATE_48000
            ),
            Self::NoChannels { min, max } => {
                write!(f, "it takes {min} to {max} channel(s), and this driver writes 1 or 2")
            }
            Self::ShortStatus { period, written } => write!(
                f,
                "it gave period {period} back with {written} byte(s) of its {}-byte status",
                wire::STATUS_BYTES
            ),
            Self::PeriodFailed { period, status: Ok(failed) } => {
                write!(f, "it failed period {period} ({})", failed.name())
            }
            Self::PeriodFailed { period, status: Err(word) } => {
                write!(f, "it gave period {period} back with the status {word:#x}, which is none")
            }
            Self::ShortEvent { written } => write!(
                f,
                "it wrote an event of {written} byte(s), where one is {}",
                wire::EVENT_BYTES
            ),
        }
    }
}

/// The three queues, laid out in the grant before the transport is given
/// them: [`toyos_virtio::pci::Setup::enable`] takes each, and [`Sound::open`]
/// takes them back once the device is live.
pub struct Queues<M: DmaBuffers> {
    pub control: Virtqueue<M>,
    pub events: Virtqueue<M>,
    pub tx: Virtqueue<M>,
}

impl<M: DmaBuffers + Clone> Queues<M> {
    /// # Panics
    /// If the grant is shorter than [`GRANT_BYTES`]: the caller's own mistake.
    pub fn new(mem: M) -> Self {
        Self {
            control: Virtqueue::new(mem.clone(), wire::CONTROL_QUEUE, CONTROL_QUEUE_SIZE, CONTROL_PARTS),
            events: Virtqueue::new(mem.clone(), wire::EVENT_QUEUE, EVENT_QUEUE_SIZE, EVENT_PARTS),
            tx: Virtqueue::new(mem, wire::TX_QUEUE, TX_QUEUE_SIZE, TX_PARTS),
        }
    }
}

/// What the device said stream 0 is, and what this driver chose of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stream {
    pub info: PcmInfo,
    pub rate: u32,
    pub channels: u8,
}

/// The device, live and configured: stream 0 prepared, every event buffer
/// posted.
pub struct Sound<M: DmaBuffers, D: Doorbell> {
    mem: M,
    bell: D,
    queues: Queues<M>,
    running: bool,
}

impl<M: DmaBuffers, D: Doorbell> Sound<M, D> {
    /// §5.14.5 on a live device whose configuration counts `streams`: post
    /// the event buffers, ask what stream 0 is, choose a rate and a channel
    /// count it offers, and set and prepare it (§5.14.6.6.1).
    pub fn open(queues: Queues<M>, mem: M, bell: D, streams: u32) -> Result<(Self, Stream), Refusal> {
        if streams == 0 {
            return Err(Refusal::NoStream);
        }
        let mut sound = Self { mem, bell, queues, running: false };
        for buffer in 0..EVENT_BUFS {
            sound.post_event(buffer);
        }
        let info = sound.pcm_info()?;
        let (rate, code, channels) = choose(&info)?;
        let params = Params {
            buffer_bytes: (PERIODS * PERIOD_BYTES) as u32,
            period_bytes: PERIOD_BYTES as u32,
            channels,
            format: wire::FMT_S16,
            rate: code,
        };
        sound.control(Request::SetParams, &wire::set_params(STREAM_ID, params), wire::HDR_BYTES)?;
        sound.simple(Request::Prepare, wire::R_PCM_PREPARE)?;
        Ok((sound, Stream { info, rate, channels }))
    }

    /// Where period `idx`'s samples go in the grant.
    pub fn period(idx: usize) -> usize {
        assert!(idx < PERIODS, "virtio-sound: there is no period {idx}");
        PCM + idx * PERIOD_BYTES
    }

    pub fn running(&self) -> bool {
        self.running
    }

    /// Put period `idx` on the wire, starting the stream first if it is
    /// stopped (§5.14.6.6.1).
    ///
    /// # Panics
    /// If period `idx` is still the device's: the caller's own mistake.
    pub fn submit(&mut self, idx: usize) -> Result<(), Refusal> {
        let pcm = Self::period(idx);
        if !self.running {
            self.simple(Request::Start, wire::R_PCM_START)?;
            self.running = true;
        }
        let xfer = TX_XFER + idx * wire::XFER_BYTES as usize;
        let status = TX_STATUS + idx * wire::STATUS_BYTES as usize;
        self.mem.write32(xfer, STREAM_ID);
        // What a status the device does not write leaves behind is no `S_OK`.
        self.mem.write32(status, 0);
        let chain = [
            Buffer::readable(self.mem.device_addr(xfer), wire::XFER_BYTES),
            Buffer::readable(self.mem.device_addr(pcm), PERIOD_BYTES as u32),
            Buffer::writable(self.mem.device_addr(status), wire::STATUS_BYTES),
        ];
        let published = self.queues.tx.publish(idx as u16 * TX_CHAIN, &chain);
        self.bell.ring(published);
        Ok(())
    }

    /// Stop the stream; nothing when it is stopped.
    pub fn stop(&mut self) -> Result<(), Refusal> {
        if self.running {
            self.simple(Request::Stop, wire::R_PCM_STOP)?;
            self.running = false;
        }
        Ok(())
    }

    /// Every period the device has given back since the last call, as a mask.
    pub fn completed(&mut self) -> Result<u32, Refusal> {
        let mut mask = 0;
        while let Some(Used { head, written }) = used(&mut self.queues.tx)? {
            // A head `toyos-virtio` answered is one a chain was published at,
            // and this driver publishes one only at a period's.
            let period = (head / TX_CHAIN) as usize;
            if written < wire::STATUS_BYTES {
                return Err(Refusal::ShortStatus { period, written });
            }
            let word = self.mem.read32(TX_STATUS + period * wire::STATUS_BYTES as usize);
            match wire::status(word) {
                Ok(Ok(())) => mask |= 1 << period,
                Ok(Err(failed)) => return Err(Refusal::PeriodFailed { period, status: Ok(failed) }),
                Err(word) => return Err(Refusal::PeriodFailed { period, status: Err(word) }),
            }
        }
        Ok(mask)
    }

    /// Every event the device has written since the last call, oldest first,
    /// each buffer posted again once it is read.
    pub fn events(&mut self, mut said: impl FnMut(Event)) -> Result<(), Refusal> {
        while let Some(Used { head, written }) = used(&mut self.queues.events)? {
            if written < wire::EVENT_BYTES {
                return Err(Refusal::ShortEvent { written });
            }
            let at = EVENTS + head as usize * wire::EVENT_BYTES as usize;
            said(Event { code: self.mem.read32(at), data: self.mem.read32(at + 4) });
            self.post_event(head);
        }
        Ok(())
    }

    fn post_event(&mut self, buffer: u16) {
        let at = EVENTS + buffer as usize * wire::EVENT_BYTES as usize;
        let chain = [Buffer::writable(self.mem.device_addr(at), wire::EVENT_BYTES)];
        let published = self.queues.events.publish(buffer, &chain);
        self.bell.ring(published);
    }

    fn pcm_info(&mut self) -> Result<PcmInfo, Refusal> {
        let request = Request::PcmInfo;
        self.control(request, &wire::pcm_info_query(STREAM_ID, 1), RESPONSE_BYTES)?;
        let at = RESPONSE + wire::HDR_BYTES as usize;
        Ok(PcmInfo::decode(core::array::from_fn(|word| self.mem.read32(at + 4 * word))))
    }

    fn simple(&mut self, request: Request, code: u32) -> Result<(), Refusal> {
        self.control(request, &wire::pcm_hdr(code, STREAM_ID), wire::HDR_BYTES)
    }

    /// One control round trip: the request's words into the request buffer, a
    /// chain of it and a `response`-byte response, published and rung, and
    /// the answer's length and status held to §5.14.6.
    fn control(&mut self, request: Request, words: &[u32], response: u32) -> Result<(), Refusal> {
        for (nth, word) in words.iter().enumerate() {
            self.mem.write32(REQUEST + 4 * nth, *word);
        }
        let chain = [
            Buffer::readable(self.mem.device_addr(REQUEST), 4 * words.len() as u32),
            Buffer::writable(self.mem.device_addr(RESPONSE), response),
        ];
        let published = self.queues.control.publish(0, &chain);
        self.bell.ring(published);

        let mut answer = None;
        for _ in 0..CONTROL_POLLS {
            answer = used(&mut self.queues.control)?;
            if answer.is_some() {
                break;
            }
            for _ in 0..SPINS_PER_POLL {
                core::hint::spin_loop();
            }
        }
        let Used { written, .. } = answer.ok_or(Refusal::Silent(request))?;
        if written < response {
            return Err(Refusal::ShortAnswer { request, written, wanted: response });
        }
        match wire::status(self.mem.read32(RESPONSE)) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(failed)) => Err(Refusal::Rejected(request, failed)),
            Err(word) => Err(Refusal::UnknownStatus(request, word)),
        }
    }
}

/// The next chain `queue`'s device has finished with, or the used ring's
/// refusal under the queue's own index.
fn used<M: DmaBuffers>(queue: &mut Virtqueue<M>) -> Result<Option<Used>, Refusal> {
    let index = queue.index();
    queue.poll_used().map_err(|why| Refusal::Used(index, why))
}

/// A rate and a channel count stream 0 offers that this driver writes, and
/// the rate's code.
fn choose(info: &PcmInfo) -> Result<(u32, u8, u8), Refusal> {
    if info.direction != wire::D_OUTPUT {
        return Err(Refusal::NotOutput { direction: info.direction });
    }
    if info.formats & (1 << wire::FMT_S16) == 0 {
        return Err(Refusal::NoFormat { formats: info.formats });
    }
    let (rate, code) = *RATES
        .iter()
        .find(|(_, code)| info.rates & (1 << code) != 0)
        .ok_or(Refusal::NoRate { rates: info.rates })?;
    // Stereo where the device takes it; the mixer converts either way.
    let (min, max) = (info.channels_min, info.channels_max);
    let channels = max.min(2);
    if channels == 0 || min > channels {
        return Err(Refusal::NoChannels { min, max });
    }
    Ok((rate, code, channels))
}
